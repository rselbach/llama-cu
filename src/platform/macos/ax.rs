//! Accessibility (AXUIElement) access.

use std::collections::HashSet;
use std::ffi::c_void;
use std::ops::Range;
use std::ptr::{self, NonNull};
use std::thread;
use std::time::{Duration, Instant};

use objc2_application_services::{
    AXCopyMultipleAttributeOptions, AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType,
};
use objc2_core_foundation::{
    CFArray, CFAttributedString, CFBoolean, CFIndex, CFNumber, CFRange, CFRetained, CFString,
    CFType, CFURL, CGPoint, CGSize, Type,
};

use crate::error::{Error, ErrorCode, Result};
use crate::model::{NodeInfo, Rect, Snapshot, SnapshotNode, SnapshotOptions, WindowInfo};

/// Native element handle.
pub type Element = CFRetained<AXUIElement>;

/// Most elements a snapshot visits before giving up, rendered or not.
const MAX_VISITED: usize = 8000;

/// Seconds to wait for an app to answer one accessibility request.
const MESSAGING_TIMEOUT: f32 = 3.0;

/// How long to wait for an app to build its tree after it is asked to.
const TREE_BUILD_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long to give an Electron app to respond to `AXManualAccessibility`.
const ELECTRON_WAIT: Duration = Duration::from_millis(600);

/// Attributes fetched for every element, in `Field` order.
const INFO_ATTRIBUTES: [&str; 14] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXValue",
    "AXPlaceholderValue",
    "AXIdentifier",
    "AXEnabled",
    "AXFocused",
    "AXSelected",
    "AXExpanded",
    "AXPosition",
    "AXSize",
    "AXURL",
];

/// Attributes that list an element's children, in `Field` order after
/// `INFO_ATTRIBUTES`. Open panels and Finder column views list some children
/// only under AXContents or AXVisibleChildren.
const CHILD_ATTRIBUTES: [&str; 3] = ["AXChildren", "AXContents", "AXVisibleChildren"];

/// Largest width and height of an unlabeled clickable element that is
/// listed for its action alone, such as an icon-only web button. Larger
/// ones are layout containers.
const ICON_ACTION_MAX: (f64, f64) = (240.0, 120.0);

#[derive(Clone, Copy)]
enum Field {
    Role,
    Subrole,
    Title,
    Description,
    Value,
    Placeholder,
    Identifier,
    Enabled,
    Focused,
    Selected,
    Expanded,
    Position,
    Size,
    Url,
    Children,
    Contents,
    VisibleChildren,
}

/// Roles that always appear in the tree because they give it structure.
const STRUCTURAL_ROLES: &[&str] = &[
    "AXWindow",
    "AXSheet",
    "AXDrawer",
    "AXPopover",
    "AXToolbar",
    "AXTabGroup",
    "AXTable",
    "AXOutline",
    "AXList",
    "AXBrowser",
    "AXGrid",
    "AXScrollArea",
    "AXWebArea",
    "AXMenuBar",
    "AXMenuBarItem",
    "AXMenu",
    "AXMenuItem",
];

/// Roles that appear only when they carry a label, value, or useful action.
/// Otherwise their children are shown in their place.
const GENERIC_ROLES: &[&str] = &[
    "AXGroup",
    "AXSplitGroup",
    "AXLayoutArea",
    "AXLayoutItem",
    "AXCell",
    "AXStaticText",
    "AXImage",
    "AXGrowArea",
    "AXMatte",
    "AXUnknown",
];

/// Roles whose subtrees are skipped: scroll bars, splitters, and rulers are
/// noise for agents, and table columns repeat the cells already listed under
/// rows.
const SKIPPED_ROLES: &[&str] = &["AXScrollBar", "AXSplitter", "AXRuler", "AXColumn"];

/// Subroles that add nothing to the role name.
const PLAIN_SUBROLES: &[&str] = &["AXStandardWindow", "AXUnknown", "AXTextAttachment"];

/// Actions that do not make an otherwise generic element worth listing.
/// Browsers expose press on nearly every group.
const COMMON_ACTIONS: &[&str] = &["AXShowMenu", "AXScrollToVisible", "AXPress"];

unsafe extern "C-unwind" {
    /// Private but long-stable API that maps an AX window to its window ID.
    fn _AXUIElementGetWindow(element: &AXUIElement, window: *mut u32) -> AXError;
}

/// Sets the global timeout for accessibility requests.
pub fn configure() {
    unsafe {
        AXUIElement::new_system_wide().set_messaging_timeout(MESSAGING_TIMEOUT);
    }
}

/// Reports whether this process may use the accessibility API.
pub fn is_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Returns the application element for `pid`.
pub fn application(pid: i32) -> Element {
    unsafe { AXUIElement::new_application(pid) }
}

/// Converts an accessibility error into a client-facing error.
pub fn ax_error(err: AXError, doing: &str) -> Error {
    let (code, reason) = match err {
        AXError::APIDisabled => (
            ErrorCode::PermissionDenied,
            "accessibility permission is missing; run `llama-cu doctor`",
        ),
        AXError::InvalidUIElement => (ErrorCode::StaleElement, "the element no longer exists"),
        AXError::CannotComplete => (ErrorCode::Timeout, "the app did not respond"),
        AXError::ActionUnsupported | AXError::AttributeUnsupported => {
            (ErrorCode::Unsupported, "the element does not support it")
        }
        AXError::NoValue => (ErrorCode::Unsupported, "the element has no value"),
        AXError::IllegalArgument => (ErrorCode::InvalidArgument, "the app rejected the value"),
        _ => (ErrorCode::Platform, "the accessibility request failed"),
    };
    Error::new(code, format!("{doing}: {reason} (AXError {})", err.0))
}

/// Reads one attribute. Returns `Ok(None)` when the element has no value
/// for it.
pub fn attribute(element: &AXUIElement, name: &str) -> Result<Option<CFRetained<CFType>>> {
    let name_cf = CFString::from_str(name);
    let mut value: *const CFType = ptr::null();
    let err = unsafe { element.copy_attribute_value(&name_cf, NonNull::from(&mut value)) };
    match err {
        AXError::Success => {
            Ok(NonNull::new(value.cast_mut()).map(|p| unsafe { CFRetained::from_raw(p) }))
        }
        AXError::NoValue | AXError::AttributeUnsupported => Ok(None),
        err => Err(ax_error(err, &format!("reading {name}"))),
    }
}

/// Writes one attribute.
pub fn set_attribute(element: &AXUIElement, name: &str, value: &CFType) -> Result<()> {
    let err = unsafe { element.set_attribute_value(&CFString::from_str(name), value) };
    match err {
        AXError::Success => Ok(()),
        err => Err(ax_error(err, &format!("setting {name}"))),
    }
}

/// Reads an attribute that holds an element.
pub fn element_attribute(element: &AXUIElement, name: &str) -> Result<Option<Element>> {
    Ok(attribute(element, name)?.and_then(|v| v.downcast::<AXUIElement>().ok()))
}

/// Reads an attribute that holds an array of elements.
pub fn elements_attribute(element: &AXUIElement, name: &str) -> Result<Vec<Element>> {
    Ok(attribute(element, name)?
        .map(|v| to_elements(&v))
        .unwrap_or_default())
}

/// Returns the element's children.
pub fn children(element: &AXUIElement) -> Result<Vec<Element>> {
    elements_attribute(element, "AXChildren")
}

/// Finds an element by child indices from the application root.
pub fn resolve(pid: i32, path: &[usize]) -> Result<Element> {
    let mut element = application(pid);
    for &index in path {
        element = child_list(&element)?
            .into_iter()
            .nth(index)
            .ok_or_else(|| {
                Error::new(ErrorCode::ElementNotFound, "element path no longer exists")
            })?;
    }
    Ok(element)
}

/// Returns the element's children in the order snapshot paths count them.
fn child_list(element: &AXUIElement) -> Result<Vec<Element>> {
    let names: Vec<CFRetained<CFString>> = CHILD_ATTRIBUTES
        .iter()
        .map(|n| CFString::from_str(n))
        .collect();
    let values = values(element, &CFArray::from_retained_objects(&names))?;
    Ok(join_children(values.iter().map(|v| {
        v.as_deref().map(to_elements).unwrap_or_default()
    })))
}

/// Joins child lists, keeping the first occurrence of each element: the
/// AXChildren list first, then elements found only in the other lists.
fn join_children(lists: impl IntoIterator<Item = Vec<Element>>) -> Vec<Element> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for element in lists.into_iter().flatten() {
        if seen.insert(element.clone()) {
            out.push(element);
        }
    }
    out
}

/// Returns the window ID of an AX window element.
pub fn window_id(window: &AXUIElement) -> Option<u64> {
    let mut id = 0u32;
    let err = unsafe { _AXUIElementGetWindow(window, &mut id) };
    (err == AXError::Success && id != 0).then_some(u64::from(id))
}

/// Lists the app's windows.
pub fn windows(pid: i32) -> Result<Vec<WindowInfo>> {
    let app = application(pid);
    let focused = element_attribute(&app, "AXFocusedWindow")?;
    let mut out = Vec::new();
    for window in elements_attribute(&app, "AXWindows")? {
        let Some(id) = window_id(&window) else {
            continue;
        };
        let title = attribute(&window, "AXTitle")?
            .and_then(|v| to_string(&v))
            .unwrap_or_default();
        let minimized = attribute(&window, "AXMinimized")?
            .and_then(|v| to_bool(&v))
            .unwrap_or(false);
        let Some(frame) = frame_of(&window)? else {
            continue;
        };
        out.push(WindowInfo {
            id,
            title,
            frame,
            focused: focused.as_deref() == Some(&*window),
            minimized,
        });
    }
    Ok(out)
}

/// Returns the AX element for one of the app's windows.
pub fn window_element(pid: i32, id: u64) -> Result<Option<Element>> {
    Ok(elements_attribute(&application(pid), "AXWindows")?
        .into_iter()
        .find(|w| window_id(w) == Some(id)))
}

fn frame_of(element: &AXUIElement) -> Result<Option<Rect>> {
    let position = attribute(element, "AXPosition")?.and_then(|v| to_point(&v));
    let size = attribute(element, "AXSize")?.and_then(|v| to_size(&v));
    Ok(position.zip(size).map(|(p, s)| rect(p, s)))
}

/// Describes an element.
pub fn info(element: &AXUIElement) -> Result<NodeInfo> {
    let names = attribute_names(false);
    Ok(fetch(element, &names)?.info)
}

/// Returns the element's text value.
pub fn text(element: &AXUIElement) -> Result<String> {
    attribute(element, "AXValue")?
        .and_then(|v| to_string(&v))
        .ok_or_else(|| Error::new(ErrorCode::Unsupported, "the element has no text value"))
}

/// Invokes an action matched case-insensitively against the element's
/// actions, with or without the AX prefix. In background mode, refuses the
/// action that raises a window, however it was named.
pub fn perform_action(element: &AXUIElement, wanted: &str, background: bool) -> Result<()> {
    let actions = action_names(element)?;
    let raw = actions
        .iter()
        .find(|raw| {
            raw.eq_ignore_ascii_case(wanted)
                || display_action(raw).eq_ignore_ascii_case(wanted)
                || raw
                    .strip_prefix("AX")
                    .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
        })
        .ok_or_else(|| {
            let available: Vec<String> = actions.iter().map(|a| display_action(a)).collect();
            Error::new(
                ErrorCode::Unsupported,
                format!(
                    "the element does not support {wanted:?}; available: {}",
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                ),
            )
        })?;
    if background && raw == "AXRaise" {
        return Err(Error::new(
            ErrorCode::BackgroundUnavailable,
            "raising a window is unavailable in background mode",
        ));
    }
    let err = unsafe { element.perform_action(&CFString::from_str(raw)) };
    match err {
        AXError::Success => Ok(()),
        // Finder opens the item but reports this error for AXOpen.
        AXError::AttributeUnsupported if raw == "AXOpen" => Ok(()),
        err => Err(ax_error(
            err,
            &format!("performing {}", display_action(raw)),
        )),
    }
}

/// Replaces the element's value, keeping numeric and boolean values typed.
pub fn set_value(element: &AXUIElement, value: &str) -> Result<()> {
    if !is_settable(element, "AXValue") {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "the element's value is not settable; use type-text or paste instead",
        ));
    }
    let current = attribute(element, "AXValue")?;
    let invalid = |kind: &str| {
        Error::new(
            ErrorCode::InvalidArgument,
            format!("the element holds a {kind}; {value:?} is not one"),
        )
    };
    let new_value: CFRetained<CFType> = match current.as_deref() {
        Some(v) if v.downcast_ref::<CFBoolean>().is_some() => {
            let b = value.parse::<bool>().map_err(|_| invalid("boolean"))?;
            CFBoolean::new(b).retain().into()
        }
        Some(v) if v.downcast_ref::<CFNumber>().is_some() => {
            let n = value.trim().parse::<f64>().map_err(|_| invalid("number"))?;
            CFNumber::new_f64(n).into()
        }
        _ => CFString::from_str(value).into(),
    };
    set_attribute(element, "AXValue", &new_value)
}

/// Reports whether an attribute of the element can be written.
fn is_settable(element: &AXUIElement, name: &str) -> bool {
    let mut settable = 0u8;
    let name = CFString::from_str(name);
    let err = unsafe { element.is_attribute_settable(&name, NonNull::from(&mut settable)) };
    err == AXError::Success && settable != 0
}

/// Focuses the element and selects a byte range of `text`, its current value.
pub fn select_range(element: &AXUIElement, text: &str, range: Range<usize>) -> Result<()> {
    let _ = set_attribute(element, "AXFocused", CFBoolean::new(true));
    let location = text[..range.start].encode_utf16().count();
    let length = text[range.clone()].encode_utf16().count();
    let cf_range = CFRange {
        location: location as CFIndex,
        length: length as CFIndex,
    };
    let value = unsafe {
        AXValue::new(
            AXValueType::CFRange,
            NonNull::from(&cf_range).cast::<c_void>(),
        )
    }
    .ok_or_else(|| Error::new(ErrorCode::Platform, "creating a text range failed"))?;
    set_attribute(element, "AXSelectedTextRange", &value)
}

/// Returns the element with keyboard focus in the app.
pub fn focused_element(pid: i32) -> Result<Element> {
    element_attribute(&application(pid), "AXFocusedUIElement")?
        .ok_or_else(|| Error::new(ErrorCode::ElementNotFound, "no element has keyboard focus"))
}

/// Captures the app's tree: the target window, open context menus, and the
/// menu bar with any open menu.
pub fn snapshot(pid: i32, window: Option<u64>, options: SnapshotOptions) -> Result<Snapshot> {
    let app = application(pid);
    if let Some(id) = window
        && let Some(window) = window_element(pid, id)?
    {
        expose_contents(&app, &window);
    }

    // Browsers fill in web content shortly after the first request for it,
    // so read again while a web area is still empty.
    let start = Instant::now();
    let mut walker = walk_app(&app, window, options)?;
    while walker.empty_web_area && start.elapsed() < TREE_BUILD_TIMEOUT {
        thread::sleep(Duration::from_millis(250));
        walker = walk_app(&app, window, options)?;
    }
    let selected_text = walker
        .focused
        .as_deref()
        .and_then(|focused| attribute(focused, "AXSelectedText").ok().flatten())
        .and_then(|v| to_string(&v))
        .filter(|s| !s.is_empty());
    Ok(Snapshot {
        nodes: walker.nodes,
        truncated: walker.truncated,
        selected_text,
    })
}

fn walk_app(app: &AXUIElement, window: Option<u64>, options: SnapshotOptions) -> Result<Walker> {
    let mut walker = Walker {
        names: attribute_names(true),
        // AppKit reports AXFocused on every cell of a focused outline, so
        // compare against the app's focused element instead.
        focused: element_attribute(app, "AXFocusedUIElement").ok().flatten(),
        max_nodes: options.max_nodes,
        visited: 0,
        nodes: Vec::new(),
        truncated: false,
        empty_web_area: false,
    };
    let top = child_list(app)?;
    let roles: Vec<String> = top.iter().map(|c| role_of(c).unwrap_or_default()).collect();

    for (i, child) in top.iter().enumerate() {
        if roles[i] == "AXWindow" && window.is_some() && window_id(child) == window {
            let clip = frame_of(child)?;
            walker.walk(child, vec![i], 0, clip, None)?;
        }
    }
    for (i, child) in top.iter().enumerate() {
        if roles[i] == "AXMenu" {
            walker.walk(child, vec![i], 0, None, None)?;
        }
    }
    for (i, child) in top.iter().enumerate() {
        if roles[i] == "AXMenuBar" {
            walker.walk_menu_bar(child, vec![i])?;
        }
    }
    Ok(walker)
}

fn role_of(element: &AXUIElement) -> Option<String> {
    attribute(element, "AXRole")
        .ok()
        .flatten()
        .and_then(|v| to_string(&v))
}

/// Asks apps that hide their content from accessibility clients to expose
/// it, when the window shows only title bar buttons and empty groups.
///
/// Electron apps usually respond to `AXManualAccessibility`. Firefox and some
/// Electron apps, such as Signal, need `AXEnhancedUserInterface`, which can
/// slow down window managers' animations for that app, so it is the last
/// resort.
fn expose_contents(app: &AXUIElement, window: &AXUIElement) {
    let empty = || {
        children(window).is_ok_and(|kids| {
            kids.iter().all(|kid| match role_of(kid).as_deref() {
                Some("AXButton") => true,
                Some("AXGroup") => children(kid).is_ok_and(|k| k.is_empty()),
                _ => false,
            })
        })
    };
    let wait_for_content = |timeout: Duration| {
        let start = Instant::now();
        while empty() && start.elapsed() < timeout {
            thread::sleep(Duration::from_millis(100));
        }
    };
    if !empty() {
        return;
    }
    // Apps that implement neither attribute report errors, and Firefox
    // reports `AXEnhancedUserInterface` as unimplemented yet honors it, so
    // the results are ignored.
    let electron = attribute(app, "AXManualAccessibility")
        .ok()
        .flatten()
        .is_some();
    if electron {
        let _ = set_attribute(app, "AXManualAccessibility", CFBoolean::new(true));
        wait_for_content(ELECTRON_WAIT);
        if !empty() {
            return;
        }
    }
    let _ = set_attribute(app, "AXEnhancedUserInterface", CFBoolean::new(true));
    wait_for_content(TREE_BUILD_TIMEOUT);
}

struct Fetched {
    role: String,
    info: NodeInfo,
    children: Vec<Element>,
}

struct Walker {
    names: CFRetained<CFArray<CFString>>,
    focused: Option<Element>,
    max_nodes: usize,
    visited: usize,
    nodes: Vec<SnapshotNode>,
    truncated: bool,
    /// Set when a web area had no children, which usually means the browser
    /// is still building it.
    empty_web_area: bool,
}

impl Walker {
    fn full(&mut self) -> bool {
        if self.nodes.len() >= self.max_nodes || self.visited >= MAX_VISITED {
            self.truncated = true;
        }
        self.truncated
    }

    fn walk(
        &mut self,
        element: &AXUIElement,
        path: Vec<usize>,
        depth: usize,
        clip: Option<Rect>,
        parent_label: Option<&str>,
    ) -> Result<()> {
        if self.full() {
            return Ok(());
        }
        self.visited += 1;
        let Some(fetched) = fetch_or_skip(element, &self.names)? else {
            return Ok(());
        };
        let Fetched {
            role,
            mut info,
            children,
        } = fetched;
        info.focused = self.focused.as_deref() == Some(element);
        if SKIPPED_ROLES.contains(&role.as_str()) {
            return Ok(());
        }
        // Closed submenus have empty frames; separators are untitled,
        // disabled menu items.
        let closed_menu = role == "AXMenu" && info.frame.is_some_and(|f| !f.has_area());
        let separator = role == "AXMenuItem" && info.label().is_none() && !info.enabled;
        if closed_menu || separator {
            return Ok(());
        }
        let clip = if role == "AXMenu" { None } else { clip };
        if let (Some(clip), Some(frame)) = (clip, info.frame)
            && frame.has_area()
            && !frame.intersects(&clip)
        {
            return Ok(());
        }
        // Text already shown in the parent's label adds nothing.
        if role == "AXStaticText"
            && let (Some(text), Some(parent)) = (info.value.as_deref(), parent_label)
            && parent.contains(text.trim())
        {
            return Ok(());
        }

        let keep = is_interesting(&role, &info);
        // An unlabeled element that can be clicked is listed only when
        // nothing below it is, so icon-only buttons stay reachable.
        let icon = !keep && is_icon_action(&info);
        let label = info.label().or(info.value.as_deref()).map(str::to_string);
        let child_depth = if keep || icon { depth + 1 } else { depth };
        let position = self.nodes.len();
        if keep || icon {
            self.nodes.push(SnapshotNode {
                depth,
                path: path.clone(),
                info,
            });
        }

        if role == "AXWebArea" && children.is_empty() {
            self.empty_web_area = true;
        }
        let hidden_rows = hidden_rows(element, &role);
        for (i, child) in children.iter().enumerate() {
            if hidden_rows.contains(child) {
                continue;
            }
            let mut child_path = path.clone();
            child_path.push(i);
            let parent = if keep { label.as_deref() } else { parent_label };
            self.walk(child, child_path, child_depth, clip, parent)?;
        }
        if icon && self.nodes.len() > position + 1 {
            self.nodes.remove(position);
            for node in &mut self.nodes[position..] {
                node.depth -= 1;
            }
        }
        Ok(())
    }

    /// Lists the menu bar items, descending only into an open menu.
    fn walk_menu_bar(&mut self, bar: &AXUIElement, path: Vec<usize>) -> Result<()> {
        let fetched = fetch(bar, &self.names)?;
        self.nodes.push(SnapshotNode {
            depth: 0,
            path: path.clone(),
            info: fetched.info,
        });
        for (i, item) in fetched.children.iter().enumerate() {
            if self.full() {
                return Ok(());
            }
            self.visited += 1;
            let Some(item_fetched) = fetch_or_skip(item, &self.names)? else {
                continue;
            };
            let open = item_fetched.info.selected;
            let mut item_path = path.clone();
            item_path.push(i);
            self.nodes.push(SnapshotNode {
                depth: 1,
                path: item_path.clone(),
                info: item_fetched.info,
            });
            if open {
                for (j, menu) in item_fetched.children.iter().enumerate() {
                    let mut menu_path = item_path.clone();
                    menu_path.push(j);
                    self.walk(menu, menu_path, 2, None, None)?;
                }
            }
        }
        Ok(())
    }
}

/// Fetches an element, returning `None` for elements that vanished or
/// cannot be read. An unresponsive app or a missing permission still fails
/// the whole snapshot.
fn fetch_or_skip(element: &AXUIElement, names: &CFArray<CFString>) -> Result<Option<Fetched>> {
    match fetch(element, names) {
        Ok(f) => Ok(Some(f)),
        Err(e) if matches!(e.code, ErrorCode::Timeout | ErrorCode::PermissionDenied) => Err(e),
        Err(_) => Ok(None),
    }
}

/// Returns rows of a table or outline that are scrolled out of view, so
/// huge lists cost a few requests instead of one per row.
fn hidden_rows(element: &AXUIElement, role: &str) -> HashSet<Element> {
    if role != "AXTable" && role != "AXOutline" {
        return HashSet::new();
    }
    let visible: HashSet<Element> = elements_attribute(element, "AXVisibleRows")
        .unwrap_or_default()
        .into_iter()
        .collect();
    if visible.is_empty() {
        return HashSet::new();
    }
    elements_attribute(element, "AXRows")
        .unwrap_or_default()
        .into_iter()
        .filter(|row| !visible.contains(row))
        .collect()
}

fn is_interesting(role: &str, info: &NodeInfo) -> bool {
    if STRUCTURAL_ROLES.contains(&role) {
        return true;
    }
    let has_text = info.label().is_some()
        || info.value.as_deref().is_some_and(|v| !v.is_empty())
        || info.placeholder.is_some();
    if has_text || info.focused || info.checked.is_some() {
        return true;
    }
    if !GENERIC_ROLES.contains(&role) {
        return true;
    }
    info.actions
        .iter()
        .any(|a| !COMMON_ACTIONS.iter().any(|c| display_action(c) == *a))
}

fn is_icon_action(info: &NodeInfo) -> bool {
    let clickable = ["press", "confirm", "open"]
        .iter()
        .any(|a| info.has_action(a));
    clickable
        && info.frame.is_some_and(|f| {
            f.has_area() && f.width <= ICON_ACTION_MAX.0 && f.height <= ICON_ACTION_MAX.1
        })
}

fn attribute_names(with_children: bool) -> CFRetained<CFArray<CFString>> {
    let children: &[&str] = if with_children {
        &CHILD_ATTRIBUTES
    } else {
        &[]
    };
    let names: Vec<CFRetained<CFString>> = INFO_ATTRIBUTES
        .iter()
        .chain(children)
        .map(|n| CFString::from_str(n))
        .collect();
    CFArray::from_retained_objects(&names)
}

/// Reads several attributes in one request. Attributes that failed or have
/// no value come back as `None`.
fn values(
    element: &AXUIElement,
    names: &CFArray<CFString>,
) -> Result<Vec<Option<CFRetained<CFType>>>> {
    let mut values: *const CFArray = ptr::null();
    let err = unsafe {
        element.copy_multiple_attribute_values(
            names.as_opaque(),
            AXCopyMultipleAttributeOptions::empty(),
            NonNull::from(&mut values),
        )
    };
    if err != AXError::Success {
        return Err(ax_error(err, "reading element"));
    }
    let values: CFRetained<CFArray> = NonNull::new(values.cast_mut())
        .map(|p| unsafe { CFRetained::from_raw(p) })
        .ok_or_else(|| Error::new(ErrorCode::Platform, "reading element returned nothing"))?;
    Ok(items(&values)
        .into_iter()
        .map(|v| {
            // Failed attributes come back as AXValues that wrap an AXError.
            let is_error = v
                .downcast_ref::<AXValue>()
                .is_some_and(|ax| unsafe { ax.r#type() } == AXValueType::AXError);
            (!is_error).then_some(v)
        })
        .collect())
}

fn fetch(element: &AXUIElement, names: &CFArray<CFString>) -> Result<Fetched> {
    let values = values(element, names)?;
    let get = |f: Field| values.get(f as usize).and_then(Option::as_deref);
    let string = |f: Field| get(f).and_then(to_string).filter(|s| !s.is_empty());
    let flag = |f: Field| get(f).and_then(to_bool);

    let role = string(Field::Role).unwrap_or_default();
    let subrole = string(Field::Subrole);
    let mut value = get(Field::Value).and_then(to_string);
    let mut checked = None;
    if role == "AXCheckBox" || role == "AXRadioButton" {
        checked = get(Field::Value).and_then(to_bool);
        value = None;
    }
    // Menus report AXExpanded on every item; whether a menu is open shows in
    // the tree itself. Safari and Chrome report it as false on every web
    // element but list it only on those that can expand.
    let expanded = match get(Field::Expanded).and_then(to_bool) {
        _ if role.starts_with("AXMenu") => None,
        Some(false) if !lists_attribute(element, "AXExpanded") => None,
        expanded => expanded,
    };
    let frame = get(Field::Position)
        .and_then(to_point)
        .zip(get(Field::Size).and_then(to_size))
        .map(|(p, s)| rect(p, s));

    let info = NodeInfo {
        role: display_role(&role, subrole.as_deref()),
        title: string(Field::Title),
        description: string(Field::Description),
        value,
        placeholder: string(Field::Placeholder),
        identifier: string(Field::Identifier),
        url: string(Field::Url).filter(|_| role == "AXLink" || role == "AXWebArea"),
        frame,
        enabled: flag(Field::Enabled).unwrap_or(true),
        focused: flag(Field::Focused).unwrap_or(false),
        selected: flag(Field::Selected).unwrap_or(false),
        expanded,
        checked,
        // Static text is never settable, and checking costs a request.
        settable: get(Field::Value).is_some()
            && role != "AXStaticText"
            && is_settable(element, "AXValue"),
        actions: action_names(element)
            .unwrap_or_default()
            .iter()
            .map(|a| display_action(a))
            .collect(),
    };
    let children = join_children(
        [Field::Children, Field::Contents, Field::VisibleChildren]
            .map(|f| get(f).map(to_elements).unwrap_or_default()),
    );
    Ok(Fetched {
        role,
        info,
        children,
    })
}

fn action_names(element: &AXUIElement) -> Result<Vec<String>> {
    let mut names: *const CFArray = ptr::null();
    let err = unsafe { element.copy_action_names(NonNull::from(&mut names)) };
    if err != AXError::Success {
        return Err(ax_error(err, "reading actions"));
    }
    let Some(names) = NonNull::new(names.cast_mut()) else {
        return Ok(Vec::new());
    };
    let names: CFRetained<CFArray> = unsafe { CFRetained::from_raw(names) };
    Ok(items(&names)
        .iter()
        .filter_map(|v| v.downcast_ref::<CFString>().map(|s| s.to_string()))
        .collect())
}

/// Reports whether the element lists `name` among its attributes.
fn lists_attribute(element: &AXUIElement, name: &str) -> bool {
    let mut names: *const CFArray = ptr::null();
    let err = unsafe { element.copy_attribute_names(NonNull::from(&mut names)) };
    if err != AXError::Success {
        return false;
    }
    let Some(names) = NonNull::new(names.cast_mut()) else {
        return false;
    };
    let names: CFRetained<CFArray> = unsafe { CFRetained::from_raw(names) };
    items(&names).iter().any(|v| {
        v.downcast_ref::<CFString>()
            .is_some_and(|s| s.to_string() == name)
    })
}

/// Turns `AXPress` into `press` and a custom action such as
/// `Name:Archive\nTarget:...` into `Archive`.
fn display_action(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("Name:") {
        return rest.lines().next().unwrap_or(rest).to_string();
    }
    let name = raw.strip_prefix("AX").unwrap_or(raw);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Turns `AXButton` with subrole `AXCloseButton` into `close button`.
fn display_role(role: &str, subrole: Option<&str>) -> String {
    let base = match subrole {
        Some(s) if !PLAIN_SUBROLES.contains(&s) => s,
        _ => role,
    };
    if base == "AXStaticText" {
        return "text".into();
    }
    if base.is_empty() {
        return "unknown".into();
    }
    words(base.strip_prefix("AX").unwrap_or(base))
}

/// Splits CamelCase into lowercase words, keeping acronyms together.
fn words(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1);
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase() || (p.is_uppercase() && next.is_some_and(|n| n.is_lowercase()))
            });
        if boundary {
            out.push(' ');
        }
        out.extend(c.to_lowercase());
    }
    out
}

fn rect(p: CGPoint, s: CGSize) -> Rect {
    Rect {
        x: p.x,
        y: p.y,
        width: s.width,
        height: s.height,
    }
}

fn to_string(v: &CFType) -> Option<String> {
    if let Some(s) = v.downcast_ref::<CFString>() {
        return Some(s.to_string());
    }
    if let Some(s) = v.downcast_ref::<CFAttributedString>() {
        return s.string().map(|s| s.to_string());
    }
    if let Some(n) = v.downcast_ref::<CFNumber>() {
        return match (n.as_i64(), n.as_f64()) {
            (_, Some(f)) if f.fract() != 0.0 => {
                let rounded = format!("{f:.2}");
                Some(rounded.trim_end_matches('0').to_string())
            }
            (Some(i), _) => Some(i.to_string()),
            _ => None,
        };
    }
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool().to_string());
    }
    if let Some(url) = v.downcast_ref::<CFURL>() {
        return Some(url.string().to_string());
    }
    None
}

fn to_bool(v: &CFType) -> Option<bool> {
    if let Some(b) = v.downcast_ref::<CFBoolean>() {
        return Some(b.as_bool());
    }
    v.downcast_ref::<CFNumber>()
        .and_then(|n| n.as_i64())
        .map(|n| n != 0)
}

fn to_point(v: &CFType) -> Option<CGPoint> {
    let ax = v.downcast_ref::<AXValue>()?;
    let mut point = CGPoint { x: 0.0, y: 0.0 };
    let ok = unsafe { ax.value(AXValueType::CGPoint, NonNull::from(&mut point).cast()) };
    ok.then_some(point)
}

fn to_size(v: &CFType) -> Option<CGSize> {
    let ax = v.downcast_ref::<AXValue>()?;
    let mut size = CGSize {
        width: 0.0,
        height: 0.0,
    };
    let ok = unsafe { ax.value(AXValueType::CGSize, NonNull::from(&mut size).cast()) };
    ok.then_some(size)
}

fn to_elements(v: &CFType) -> Vec<Element> {
    let Some(array) = v.downcast_ref::<CFArray>() else {
        return Vec::new();
    };
    items(array)
        .into_iter()
        .filter_map(|item| item.downcast::<AXUIElement>().ok())
        .collect()
}

/// Returns the objects in an untyped array.
fn items(array: &CFArray) -> Vec<CFRetained<CFType>> {
    // SAFETY: arrays returned by the accessibility API hold CF objects.
    let typed: &CFArray<CFType> = unsafe { array.cast_unchecked() };
    (0..typed.len()).filter_map(|i| typed.get(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_names() {
        let cases = [
            ("AXButton", None, "button"),
            ("AXButton", Some("AXCloseButton"), "close button"),
            ("AXWindow", Some("AXStandardWindow"), "window"),
            ("AXStaticText", None, "text"),
            ("AXTextField", Some("AXSearchField"), "search field"),
            ("AXMenuBarItem", None, "menu bar item"),
            ("AXHTMLContent", None, "html content"),
            ("", None, "unknown"),
        ];
        for (role, subrole, want) in cases {
            assert_eq!(display_role(role, subrole), want, "{role} {subrole:?}");
        }
    }

    #[test]
    fn children_join_in_order_without_duplicates() {
        let (a, b, c) = (application(1), application(2), application(3));
        let joined = join_children([vec![a.clone(), b.clone()], vec![b.clone()], vec![c, a]]);
        let pids: Vec<i32> = joined
            .iter()
            .map(|e| {
                let mut pid = 0;
                unsafe { e.pid(NonNull::from(&mut pid)) };
                pid
            })
            .collect();
        assert_eq!(pids, [1, 2, 3]);
    }

    #[test]
    fn icon_actions() {
        let node = |actions: &[&str], width, height| NodeInfo {
            role: "group".into(),
            frame: Some(Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            }),
            actions: actions.iter().map(|a| a.to_string()).collect(),
            ..NodeInfo::default()
        };
        let cases = [
            ("icon button", node(&["press"], 24.0, 24.0), true),
            ("open action", node(&["open"], 24.0, 24.0), true),
            ("no click action", node(&["showMenu"], 24.0, 24.0), false),
            ("layout container", node(&["press"], 800.0, 400.0), false),
            ("zero size", node(&["press"], 0.0, 24.0), false),
        ];
        for (name, info, want) in cases {
            assert_eq!(is_icon_action(&info), want, "{name}");
        }
    }

    #[test]
    fn action_names_display() {
        let cases = [
            ("AXPress", "press"),
            ("AXShowMenu", "showMenu"),
            ("AXShowAlternateUI", "showAlternateUI"),
            ("Name:Archive\nTarget:0x0\nSelector:(null)", "Archive"),
            ("custom", "custom"),
        ];
        for (raw, want) in cases {
            assert_eq!(display_action(raw), want, "{raw}");
        }
    }
}
