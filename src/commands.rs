//! Command behavior shared by every platform.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::Serialize;

use crate::error::{Error, ErrorCode, Result};
use crate::keys::KeyCombo;
use crate::model::{AppInfo, MouseButton, NodeInfo, Point, Rect, SnapshotOptions, WindowInfo};
use crate::platform::{Permissions, Platform};
use crate::render::TextLimit;
use crate::session::{ElementRef, Fingerprint, Session, SnapshotRefs, Store};
use crate::text;

/// How long to wait after sending the paste shortcut before restoring the
/// previous clipboard.
const PASTE_SETTLE: Duration = Duration::from_millis(500);

/// How long to let the UI react to an action before observing it.
const OBSERVE_SETTLE: Duration = Duration::from_millis(300);

/// Default for the most elements `get-ax-state` lists.
pub const DEFAULT_MAX_NODES: usize = 1000;

/// Longest screenshot edge, in pixels. Model APIs shrink large images before
/// the model sees them, which would break the match between screenshot
/// pixels and coordinates, so larger windows are captured scaled down.
const MAX_SCREENSHOT_EDGE: f64 = 1280.0;

/// Most pixels in a screenshot, for the same reason.
const MAX_SCREENSHOT_PIXELS: f64 = 1_150_000.0;

/// Password managers that llama-cu refuses to operate, by bundle ID.
const BLOCKED_APPS: &[&str] = &[
    "com.1password.1password",
    "com.1password.safari",
    "com.agilebits.onepassword7",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.bitwarden.desktop",
    "com.dashlane.dashlanephonefinal",
    "com.lastpass.LastPass",
    "com.nordsec.nordpass",
    "me.proton.pass.catalyst",
    "me.proton.pass.electron",
];

/// Result of `list-apps`.
#[derive(Serialize)]
pub struct AppList {
    pub apps: Vec<ListedApp>,
}

/// One app in `list-apps` output.
#[derive(Serialize)]
pub struct ListedApp {
    #[serde(flatten)]
    pub app: AppInfo,
    /// Whether the app is in front.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub frontmost: bool,
}

/// Result of `get-app`.
#[derive(Serialize)]
pub struct AppDetails {
    pub app: AppInfo,
    pub windows: Vec<WindowInfo>,
}

/// One element in `get-ax-state` output.
#[derive(Serialize)]
pub struct StateNode {
    pub id: usize,
    pub depth: usize,
    /// Element details; `frame` is relative to the window.
    #[serde(flatten)]
    pub info: NodeInfo,
}

/// Result of `get-ax-state`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AxState {
    pub app: AppInfo,
    pub window: Option<WindowInfo>,
    pub other_windows: Vec<WindowInfo>,
    /// Screenshot pixels per window point. Frames and coordinates are in
    /// screenshot pixels.
    pub scale: f64,
    pub nodes: Vec<StateNode>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_text: Option<String>,
    /// Longest text shown in the rendered tree; JSON output is never cut.
    #[serde(skip)]
    pub text_limit: TextLimit,
}

/// Result of `get-screenshot`.
#[derive(Serialize)]
pub struct Screenshot {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub window: WindowInfo,
}

/// Result of `get-ax-state-and-screenshot`.
#[derive(Serialize)]
pub struct StateAndScreenshot {
    pub state: AxState,
    #[serde(flatten)]
    pub screenshot: Capture,
}

/// The screenshot part of `get-ax-state-and-screenshot`. A failed capture
/// still returns the state.
#[derive(Serialize)]
pub enum Capture {
    #[serde(rename = "screenshot")]
    Taken(Screenshot),
    #[serde(rename = "screenshotError")]
    Failed(Error),
}

/// Result of an action command.
#[derive(Serialize)]
pub struct Action {
    pub message: String,
    /// State after the action, when requested with `--observe`.
    #[serde(flatten)]
    pub observed: Option<Observation>,
}

/// What `--observe` saw after an action. A failure here does not undo the
/// action, so it does not fail the command.
#[derive(Serialize)]
pub enum Observation {
    #[serde(rename = "observed")]
    State(Box<StateAndScreenshot>),
    #[serde(rename = "observeError")]
    Failed(Error),
}

/// Result of `doctor`.
#[derive(Serialize)]
pub struct Doctor {
    #[serde(flatten)]
    pub permissions: Permissions,
}

/// What a pointer command targets.
#[derive(Debug, Clone, Copy)]
pub enum Target {
    /// An element ID from the last `get-ax-state`.
    Element(usize),
    /// A point relative to the window from the last state or screenshot.
    Point(Point),
}

/// A scroll direction.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Content format for `paste`.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum PasteFormat {
    #[default]
    Plain,
    Markdown,
    Html,
}

/// Where `select-text` puts the selection relative to the match.
#[derive(Debug, Clone, Copy, Default, clap::ValueEnum)]
pub enum SelectPosition {
    /// Select the matching text.
    #[default]
    Select,
    /// Place the cursor before the match.
    Before,
    /// Place the cursor after the match.
    After,
}

/// Runs commands against a platform backend with persistent session state.
pub struct Ctx<P: Platform> {
    platform: P,
    store: Store,
    session: Session,
}

impl<P: Platform> Ctx<P> {
    /// Loads the session from `store`.
    pub fn new(platform: P, store: Store) -> Result<Self> {
        let session = store.load()?;
        Ok(Self {
            platform,
            store,
            session,
        })
    }

    /// Reports permission status, prompting for missing permissions if asked.
    pub fn doctor(&self, prompt: bool) -> Doctor {
        let permissions = self.platform.permissions();
        if prompt && !(permissions.accessibility && permissions.screen_recording) {
            self.platform.request_permissions();
        }
        Doctor { permissions }
    }

    /// Lists installed and running apps.
    pub fn list_apps(&self, running_only: bool) -> Result<AppList> {
        let mut apps = self.platform.list_apps()?;
        if running_only {
            apps.retain(|a| a.pid.is_some());
        }
        let front = self.platform.frontmost_pid();
        let mut apps: Vec<ListedApp> = apps
            .into_iter()
            .map(|app| ListedApp {
                frontmost: front.is_some() && app.pid == front,
                app,
            })
            .collect();
        apps.sort_by(|a, b| {
            b.frontmost
                .cmp(&a.frontmost)
                .then_with(|| b.app.pid.is_some().cmp(&a.app.pid.is_some()))
                .then_with(|| a.app.name.to_lowercase().cmp(&b.app.name.to_lowercase()))
        });
        Ok(AppList { apps })
    }

    /// Selects an app by name, path, or bundle ID, launching it if needed.
    pub fn get_app(&mut self, query: &str) -> Result<AppDetails> {
        // Forget the previous app first, so a failed selection cannot leave
        // later commands acting on it.
        self.session = Session::default();
        self.store.save(&self.session)?;

        let mut app = if query.contains('/') {
            let app = self.platform.app_at_path(Path::new(query))?;
            self.platform
                .list_apps()?
                .into_iter()
                .find(|a| a.pid.is_some() && same_app(a, &app))
                .unwrap_or(app)
        } else {
            find_app(self.platform.list_apps()?, query)?
        };
        if is_blocked(&app) {
            return Err(Error::new(
                ErrorCode::AppBlocked,
                format!(
                    "{} is a password manager; llama-cu does not operate it",
                    app.name
                ),
            ));
        }
        if app.pid.is_none() {
            let pid = self.platform.launch(&app)?;
            if !self.platform.is_running(pid) {
                return Err(Error::new(
                    ErrorCode::AppNotRunning,
                    format!("{} quit right after launching", app.name),
                ));
            }
            app.pid = Some(pid);
        }
        let windows = self.platform.windows(pid_of(&app))?;
        self.session = Session {
            app: Some(app.clone()),
            ..Session::default()
        };
        self.store.save(&self.session)?;
        Ok(AppDetails { app, windows })
    }

    /// Reads the accessibility tree of the selected app's window.
    pub fn get_ax_state(
        &mut self,
        window: Option<u64>,
        max_nodes: usize,
        text_limit: TextLimit,
    ) -> Result<AxState> {
        let app = self.app()?;
        let pid = pid_of(&app);
        let windows = self.platform.windows(pid)?;
        let target = match window {
            Some(_) => Some(pick_window(&windows, window, None, &app)?),
            None => pick_window(&windows, None, None, &app).ok(),
        };
        let snapshot = self.platform.snapshot(
            pid,
            target.as_ref().map(|w| w.id),
            SnapshotOptions { max_nodes },
        )?;
        let origin = target.as_ref().map(|w| w.frame.origin());
        let scale = target.as_ref().map_or(1.0, |w| screenshot_scale(&w.frame));

        let elements = snapshot
            .nodes
            .iter()
            .map(|n| ElementRef {
                path: n.path.clone(),
                fingerprint: Fingerprint::of(&n.info, origin),
            })
            .collect();
        self.session.window = target.as_ref().map(|w| w.id);
        self.session.scale = Some(scale);
        self.session.snapshot = Some(SnapshotRefs {
            pid,
            window: self.session.window,
            elements,
        });
        self.store.save(&self.session)?;

        let nodes = snapshot
            .nodes
            .into_iter()
            .enumerate()
            .map(|(i, n)| {
                let mut info = n.info;
                if let Some(origin) = origin {
                    info.frame = info.frame.map(|f| f.relative_to(origin).scaled(scale));
                }
                StateNode {
                    id: i + 1,
                    depth: n.depth,
                    info,
                }
            })
            .collect();
        let other_windows = windows
            .into_iter()
            .filter(|w| Some(w.id) != target.as_ref().map(|t| t.id))
            .collect();
        Ok(AxState {
            app,
            window: target,
            other_windows,
            scale,
            nodes,
            truncated: snapshot.truncated,
            selected_text: snapshot.selected_text,
            text_limit,
        })
    }

    /// Captures the selected app's window.
    pub fn get_screenshot(
        &mut self,
        window: Option<u64>,
        output: Option<PathBuf>,
    ) -> Result<Screenshot> {
        let app = self.app()?;
        let pid = pid_of(&app);
        let target = pick_window(&self.platform.windows(pid)?, window, None, &app)?;
        let path = match output {
            Some(path) => std::path::absolute(path)?,
            None => self.store.screenshot_path()?,
        };
        let scale = screenshot_scale(&target.frame);
        let (width, height) = self.platform.capture_window(pid, target.id, &path, scale)?;
        self.session.window = Some(target.id);
        self.session.scale = Some(scale);
        self.store.save(&self.session)?;
        Ok(Screenshot {
            path,
            width,
            height,
            window: target,
        })
    }

    /// Reads the accessibility tree and captures the same window. A failed
    /// capture is reported alongside the tree instead of discarding it.
    pub fn get_ax_state_and_screenshot(
        &mut self,
        window: Option<u64>,
        max_nodes: usize,
        text_limit: TextLimit,
        output: Option<PathBuf>,
    ) -> Result<StateAndScreenshot> {
        let state = self.get_ax_state(window, max_nodes, text_limit)?;
        let window = state.window.as_ref().map(|w| w.id);
        let screenshot = match self.get_screenshot(window, output) {
            Ok(s) => Capture::Taken(s),
            Err(err) => Capture::Failed(err),
        };
        Ok(StateAndScreenshot { state, screenshot })
    }

    /// Waits for the UI to react to an action, then reads the state and a
    /// screenshot of the app's focused window with default limits.
    pub fn observe(&mut self) -> Observation {
        thread::sleep(OBSERVE_SETTLE);
        match self.get_ax_state_and_screenshot(None, DEFAULT_MAX_NODES, TextLimit::default(), None)
        {
            Ok(state) => Observation::State(Box::new(state)),
            Err(err) => Observation::Failed(err),
        }
    }

    /// Clicks an element or a window-relative point.
    pub fn click(&mut self, target: Target, button: MouseButton, count: u32) -> Result<Action> {
        if count == 0 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "count must be at least 1",
            ));
        }
        let (pid, window, point, what) = match target {
            Target::Element(id) => {
                let (pid, element, info) = self.element(id)?;
                // These actions work on covered windows and leave the pointer
                // alone. Each stands in for exactly one kind of click. Not
                // showMenu: apps such as Finder answer it only after the menu
                // closes.
                let semantic = match (button, count) {
                    (MouseButton::Left, 1) => Some(("press", "pressed")),
                    (MouseButton::Left, 2) => Some(("open", "opened")),
                    _ => None,
                };
                if let Some((name, verb)) = semantic.filter(|(name, _)| info.has_action(name)) {
                    self.platform.perform_action(&element, name)?;
                    return Ok(action(format!("{verb} [{id}] {}", describe(&info))));
                }
                let point = self.visible_center(&element, &info, id)?;
                (
                    pid,
                    self.snapshot_window(),
                    point,
                    format!("[{id}] {}", describe(&info)),
                )
            }
            Target::Point(p) => {
                let (pid, window) = self.coordinate_window()?;
                (
                    pid,
                    Some(window.id),
                    self.to_screen(&window, p),
                    format!("{},{}", p.x, p.y),
                )
            }
        };
        self.platform.activate(pid, window)?;
        self.platform.click(point, button, count)?;
        let clicks = match count {
            1 => String::new(),
            n => format!(" x{n}"),
        };
        let button = match button {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        };
        Ok(action(format!("{button}-clicked{clicks} {what}")))
    }

    /// Drags between two window-relative points.
    pub fn drag(&mut self, from: Point, to: Point) -> Result<Action> {
        let (pid, window) = self.coordinate_window()?;
        self.platform.activate(pid, Some(window.id))?;
        self.platform
            .drag(self.to_screen(&window, from), self.to_screen(&window, to))?;
        Ok(action(format!(
            "dragged from {},{} to {},{}",
            from.x, from.y, to.x, to.y
        )))
    }

    /// Scrolls at an element, a window-relative point, or the window center.
    pub fn scroll(
        &mut self,
        target: Option<Target>,
        direction: Direction,
        amount: u32,
    ) -> Result<Action> {
        let (pid, window, point) = match target {
            Some(Target::Element(id)) => {
                let (pid, element, info) = self.element(id)?;
                let point = self.visible_center(&element, &info, id)?;
                (pid, self.snapshot_window(), point)
            }
            Some(Target::Point(p)) => {
                let (pid, window) = self.coordinate_window()?;
                (pid, Some(window.id), self.to_screen(&window, p))
            }
            None => {
                let (pid, window) = self.coordinate_window()?;
                (pid, Some(window.id), window.frame.center())
            }
        };
        let n = i32::try_from(amount)
            .map_err(|_| Error::new(ErrorCode::InvalidArgument, "amount is too large"))?;
        let (dx, dy) = match direction {
            Direction::Up => (0, -n),
            Direction::Down => (0, n),
            Direction::Left => (-n, 0),
            Direction::Right => (n, 0),
        };
        self.platform.activate(pid, window)?;
        self.platform.scroll(point, dx, dy)?;
        Ok(action(
            format!("scrolled {direction:?} {amount}").to_lowercase(),
        ))
    }

    /// Presses keys and shortcuts in order.
    pub fn press_key(&mut self, keys: &[String]) -> Result<Action> {
        let combos = keys
            .iter()
            .map(|k| KeyCombo::parse(k))
            .collect::<Result<Vec<_>>>()?;
        let pid = pid_of(&self.app()?);
        self.platform.activate(pid, self.session.window)?;
        for combo in &combos {
            self.platform.press_key(combo)?;
        }
        Ok(action(format!("pressed {}", keys.join(" "))))
    }

    /// Types text into the focused control.
    pub fn type_text(&mut self, text: &str) -> Result<Action> {
        let pid = pid_of(&self.app()?);
        self.platform.activate(pid, self.session.window)?;
        self.platform.type_text(text)?;
        Ok(action(format!("typed {} characters", text.chars().count())))
    }

    /// Pastes text through the clipboard, then restores the clipboard.
    pub fn paste(&mut self, content: &str, format: PasteFormat) -> Result<Action> {
        let (plain, html) = match format {
            PasteFormat::Plain => (content.to_string(), None),
            PasteFormat::Markdown => (content.to_string(), Some(text::markdown_to_html(content))),
            PasteFormat::Html => (text::html_to_text(content), Some(content.to_string())),
        };
        let pid = pid_of(&self.app()?);
        self.platform.activate(pid, self.session.window)?;

        let saved = self.platform.clipboard_save()?;
        let pasted = self
            .platform
            .clipboard_set(&plain, html.as_deref())
            .and_then(|()| self.platform.press_key(&self.platform.paste_shortcut()));
        thread::sleep(PASTE_SETTLE);
        let restored = self.platform.clipboard_restore(saved);
        pasted?;
        restored?;
        Ok(action(format!(
            "pasted {} characters as {format:?}",
            content.chars().count()
        )))
    }

    /// Replaces the value of an element, or of the focused element.
    pub fn set_value(&mut self, element: Option<usize>, value: &str) -> Result<Action> {
        let (el, what) = self.element_or_focused(element)?;
        self.platform.set_value(&el, value)?;
        Ok(action(format!("set value of {what}")))
    }

    /// Selects text in an element, or in the focused element, or places the
    /// cursor before or after it.
    pub fn select_text(
        &mut self,
        element: Option<usize>,
        needle: &str,
        position: SelectPosition,
        occurrence: usize,
    ) -> Result<Action> {
        if needle.is_empty() || occurrence == 0 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "text must be non-empty and occurrence must be at least 1",
            ));
        }
        let (el, what) = self.element_or_focused(element)?;
        let haystack = self.platform.element_text(&el)?;
        let start = haystack
            .match_indices(needle)
            .nth(occurrence - 1)
            .map(|(i, _)| i)
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::ElementNotFound,
                    format!("{needle:?} (occurrence {occurrence}) not found in {what}"),
                )
            })?;
        let end = start + needle.len();
        let range = match position {
            SelectPosition::Select => start..end,
            SelectPosition::Before => start..start,
            SelectPosition::After => end..end,
        };
        self.platform.select_range(&el, &haystack, range)?;
        let verb = match position {
            SelectPosition::Select => "selected",
            SelectPosition::Before => "placed cursor before",
            SelectPosition::After => "placed cursor after",
        };
        Ok(action(format!("{verb} {needle:?} in {what}")))
    }

    /// Invokes a named accessibility action on an element.
    pub fn perform_secondary_action(&mut self, id: usize, name: &str) -> Result<Action> {
        let (_, element, info) = self.element(id)?;
        self.platform.perform_action(&element, name)?;
        Ok(action(format!(
            "performed {name} on [{id}] {}",
            describe(&info)
        )))
    }

    /// Returns the selected app, re-finding it if it was relaunched.
    fn app(&mut self) -> Result<AppInfo> {
        let app = self.session.app.clone().ok_or_else(|| {
            Error::new(
                ErrorCode::NoAppSelected,
                "no app selected; run `llama-cu get-app <name>` first",
            )
        })?;
        if app.pid.is_some_and(|pid| self.platform.is_running(pid)) {
            return Ok(app);
        }
        let relaunched = self
            .platform
            .list_apps()?
            .into_iter()
            .find(|a| a.pid.is_some() && same_app(a, &app))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::AppNotRunning,
                    format!(
                        "{} is no longer running; run `llama-cu get-app` again",
                        app.name
                    ),
                )
            })?;
        self.session = Session {
            app: Some(relaunched.clone()),
            ..Session::default()
        };
        self.store.save(&self.session)?;
        Ok(relaunched)
    }

    /// Returns the window that window-relative coordinates refer to.
    fn coordinate_window(&mut self) -> Result<(i32, WindowInfo)> {
        let app = self.app()?;
        let pid = pid_of(&app);
        let windows = self.platform.windows(pid)?;
        let window = pick_window(&windows, None, self.session.window, &app)?;
        Ok((pid, window))
    }

    /// Converts a point in screenshot pixels of `window` to the screen. The
    /// scale is the one used when the window was last observed.
    fn to_screen(&self, window: &WindowInfo, p: Point) -> Point {
        let scale = match self.session.scale {
            Some(scale) if self.session.window == Some(window.id) => scale,
            _ => screenshot_scale(&window.frame),
        };
        Point {
            x: window.frame.x + p.x / scale,
            y: window.frame.y + p.y / scale,
        }
    }

    fn snapshot_window(&self) -> Option<u64> {
        self.session.snapshot.as_ref().and_then(|s| s.window)
    }

    /// Finds an element from the last snapshot and checks it has not changed.
    fn element(&self, id: usize) -> Result<(i32, P::Element, NodeInfo)> {
        let snapshot = self.session.snapshot.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::ElementNotFound,
                "no element IDs yet; run `llama-cu get-ax-state` first",
            )
        })?;
        let entry = id
            .checked_sub(1)
            .and_then(|i| snapshot.elements.get(i))
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::ElementNotFound,
                    format!(
                        "element [{id}] is not in the last get-ax-state output (IDs 1-{})",
                        snapshot.elements.len()
                    ),
                )
            })?;
        if !self.platform.is_running(snapshot.pid) {
            return Err(Error::new(
                ErrorCode::AppNotRunning,
                "the app from the last get-ax-state is no longer running",
            ));
        }
        let stale = || {
            Error::new(
                ErrorCode::StaleElement,
                format!(
                    "element [{id}] changed since the last get-ax-state; run it again to get fresh IDs"
                ),
            )
        };
        let element = self
            .platform
            .resolve(snapshot.pid, &entry.path)
            .map_err(|_| stale())?;
        let info = self.platform.element_info(&element)?;
        let origin = match (entry.fingerprint.frame, snapshot.window) {
            (Some(_), Some(window)) => self
                .platform
                .windows(snapshot.pid)?
                .into_iter()
                .find(|w| w.id == window)
                .map(|w| w.frame.origin()),
            _ => None,
        };
        if !entry.fingerprint.matches(&info, origin) {
            return Err(stale());
        }
        Ok((snapshot.pid, element, info))
    }

    fn element_or_focused(&mut self, id: Option<usize>) -> Result<(P::Element, String)> {
        match id {
            Some(id) => {
                let (_, element, info) = self.element(id)?;
                Ok((element, format!("[{id}] {}", describe(&info))))
            }
            None => {
                let pid = pid_of(&self.app()?);
                let element = self.platform.focused_element(pid)?;
                let info = self.platform.element_info(&element)?;
                Ok((element, format!("focused {}", describe(&info))))
            }
        }
    }

    /// Returns the screen point at the center of the element's part inside
    /// its window, scrolling it into view first when none of it is visible.
    fn visible_center(&self, element: &P::Element, info: &NodeInfo, id: usize) -> Result<Point> {
        let no_frame = || {
            Error::new(
                ErrorCode::Unsupported,
                format!("element [{id}] has no on-screen frame; try perform-secondary-action"),
            )
        };
        let frame = info.frame.filter(Rect::has_area).ok_or_else(no_frame)?;
        let window = match (self.session.snapshot.as_ref(), self.snapshot_window()) {
            (Some(s), Some(id)) => self
                .platform
                .windows(s.pid)?
                .into_iter()
                .find(|w| w.id == id),
            _ => None,
        };
        let Some(window) = window else {
            return Ok(frame.center());
        };
        if let Some(visible) = frame.intersection(&window.frame) {
            return Ok(visible.center());
        }
        // Menus and popovers live outside the window; use their own frame.
        if !info.has_action("scrollToVisible") {
            return Ok(frame.center());
        }
        self.platform.perform_action(element, "scrollToVisible")?;
        let frame = self
            .platform
            .element_info(element)?
            .frame
            .filter(Rect::has_area)
            .ok_or_else(no_frame)?;
        Ok(frame.intersection(&window.frame).unwrap_or(frame).center())
    }
}

/// Picks the requested window, else the remembered one, else the focused or
/// first visible window.
fn pick_window(
    windows: &[WindowInfo],
    requested: Option<u64>,
    remembered: Option<u64>,
    app: &AppInfo,
) -> Result<WindowInfo> {
    if let Some(id) = requested {
        return windows.iter().find(|w| w.id == id).cloned().ok_or_else(|| {
            Error::new(
                ErrorCode::WindowNotFound,
                format!("{} has no window {id}", app.name),
            )
        });
    }
    remembered
        .and_then(|id| windows.iter().find(|w| w.id == id))
        .or_else(|| windows.iter().find(|w| w.focused && !w.minimized))
        .or_else(|| windows.iter().find(|w| !w.minimized))
        .or_else(|| windows.first())
        .cloned()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::WindowNotFound,
                format!("{} has no open windows", app.name),
            )
        })
}

/// Matches an app by exact bundle ID, exact name, or unique name substring,
/// all case-insensitive. Running apps win ties.
fn find_app(apps: Vec<AppInfo>, query: &str) -> Result<AppInfo> {
    let q = query.to_lowercase();
    let mut apps = apps;
    apps.sort_by_key(|a| a.pid.is_none());

    if let Some(i) = apps.iter().position(|a| {
        a.bundle_id
            .as_deref()
            .is_some_and(|b| b.to_lowercase() == q)
            || a.name.to_lowercase() == q
    }) {
        return Ok(apps.swap_remove(i));
    }
    let mut matches: Vec<AppInfo> = apps
        .into_iter()
        .filter(|a| a.name.to_lowercase().contains(&q))
        .collect();
    matches.dedup_by(|a, b| same_app(a, b));
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(Error::new(
            ErrorCode::AppNotFound,
            format!("no app matches {query:?}; run `llama-cu list-apps`"),
        )),
        _ => {
            let names: Vec<String> = matches
                .iter()
                .take(10)
                .map(|a| match &a.bundle_id {
                    Some(id) => format!("{} ({id})", a.name),
                    None => a.name.clone(),
                })
                .collect();
            Err(Error::new(
                ErrorCode::AppNotFound,
                format!("{query:?} matches several apps: {}", names.join(", ")),
            ))
        }
    }
}

/// Returns screenshot pixels per point for a window: 1, or less for a window
/// too large to send to a model unscaled.
fn screenshot_scale(window: &Rect) -> f64 {
    if !window.has_area() {
        return 1.0;
    }
    let edge = MAX_SCREENSHOT_EDGE / window.width.max(window.height);
    let area = (MAX_SCREENSHOT_PIXELS / (window.width * window.height)).sqrt();
    edge.min(area).min(1.0)
}

fn is_blocked(app: &AppInfo) -> bool {
    app.bundle_id
        .as_deref()
        .is_some_and(|id| BLOCKED_APPS.iter().any(|b| b.eq_ignore_ascii_case(id)))
}

fn same_app(a: &AppInfo, b: &AppInfo) -> bool {
    match (&a.bundle_id, &b.bundle_id) {
        (Some(x), Some(y)) => x == y,
        _ => a.path.is_some() && a.path == b.path,
    }
}

fn pid_of(app: &AppInfo) -> i32 {
    app.pid.expect("running app has a pid")
}

fn action(message: String) -> Action {
    Action {
        message,
        observed: None,
    }
}

/// Describes an element briefly, such as `button "Save"`.
pub fn describe(info: &NodeInfo) -> String {
    match info.label() {
        Some(label) => format!("{} {label:?}", info.role),
        None => info.role.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, bundle: &str, pid: Option<i32>) -> AppInfo {
        AppInfo {
            name: name.into(),
            bundle_id: Some(bundle.into()),
            path: Some(format!("/Applications/{name}.app")),
            pid,
        }
    }

    fn catalog() -> Vec<AppInfo> {
        vec![
            app("Safari", "com.apple.Safari", None),
            app(
                "Safari Technology Preview",
                "com.apple.SafariTechnologyPreview",
                None,
            ),
            app("Notes", "com.apple.Notes", Some(42)),
            app("TextEdit", "com.apple.TextEdit", None),
        ]
    }

    #[test]
    fn find_app_matches() {
        let cases = [
            ("exact name", "safari", "com.apple.Safari"),
            ("bundle id", "COM.APPLE.TEXTEDIT", "com.apple.TextEdit"),
            (
                "unique substring",
                "technology",
                "com.apple.SafariTechnologyPreview",
            ),
            ("running app", "notes", "com.apple.Notes"),
        ];
        for (name, query, want) in cases {
            let got = find_app(catalog(), query).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(got.bundle_id.as_deref(), Some(want), "{name}");
        }
    }

    #[test]
    fn find_app_errors() {
        let cases = [("ambiguous", "saf"), ("missing", "Greendale")];
        for (name, query) in cases {
            let err = find_app(catalog(), query).expect_err(name);
            assert_eq!(err.code, ErrorCode::AppNotFound, "{name}");
        }
    }

    #[test]
    fn pick_window_preference() {
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let window = |id, focused, minimized| WindowInfo {
            id,
            title: format!("w{id}"),
            frame,
            focused,
            minimized,
        };
        let windows = vec![
            window(1, false, true),
            window(2, false, false),
            window(3, true, false),
        ];
        let a = app("Notes", "com.apple.Notes", Some(1));
        let cases = [
            ("requested", Some(1), None, 1),
            ("remembered", None, Some(2), 2),
            ("stale remembered falls back to focused", None, Some(9), 3),
            ("focused", None, None, 3),
        ];
        for (name, requested, remembered, want) in cases {
            let got = pick_window(&windows, requested, remembered, &a).expect(name);
            assert_eq!(got.id, want, "{name}");
        }
        assert!(pick_window(&windows, Some(9), None, &a).is_err());
        assert!(pick_window(&[], None, None, &a).is_err());
    }

    #[test]
    fn screenshot_scales() {
        let size = |width, height| Rect {
            x: 0.0,
            y: 0.0,
            width,
            height,
        };
        let cases = [
            ("small window is unscaled", size(800.0, 600.0), 1.0),
            ("wide window fits the edge", size(2560.0, 400.0), 0.5),
            (
                "square window fits the pixel budget",
                size(1200.0, 1200.0),
                (1_150_000.0f64 / 1_440_000.0).sqrt(),
            ),
            ("empty window", size(0.0, 0.0), 1.0),
        ];
        for (name, window, want) in cases {
            let got = screenshot_scale(&window);
            assert!((got - want).abs() < 1e-9, "{name}: got {got}, want {want}");
            let (w, h) = (window.width * got, window.height * got);
            assert!(w.max(h) <= MAX_SCREENSHOT_EDGE + 1e-6, "{name}: edge");
            assert!(w * h <= MAX_SCREENSHOT_PIXELS + 1e-6, "{name}: pixels");
        }
    }

    #[test]
    fn blocked_apps() {
        let cases = [
            ("1Password", "com.1password.1password", true),
            ("LastPass in another case", "com.lastpass.lastpass", true),
            ("Notes", "com.apple.Notes", false),
        ];
        for (name, bundle, want) in cases {
            assert_eq!(is_blocked(&app(name, bundle, None)), want, "{name}");
        }
    }

    #[test]
    fn partial_results_serialize_flat() {
        let failed = Action {
            message: "clicked".into(),
            observed: Some(Observation::Failed(Error::new(
                ErrorCode::WindowNotFound,
                "gone",
            ))),
        };
        let got = serde_json::to_value(&failed).expect("serialize");
        assert_eq!(
            got,
            serde_json::json!({
                "message": "clicked",
                "observeError": {"code": "window_not_found", "message": "gone"},
            })
        );
        let plain = serde_json::to_value(action("clicked".into())).expect("serialize");
        assert_eq!(plain, serde_json::json!({"message": "clicked"}));
    }
}
