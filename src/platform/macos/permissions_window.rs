//! The permissions window. `doctor --prompt` starts it as a helper process so
//! the command returns without waiting for the window to close. The window
//! lists the permissions llama-cu needs, opens the matching Privacy & Security
//! pane, and offers llama-cu.app as a drag source for that pane's list.

use std::cell::Cell;
use std::fs::{self, File, TryLockError};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSButton, NSColor,
    NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSEvent, NSFont, NSImage, NSImageScaling, NSImageView, NSPasteboardWriting,
    NSRunningApplication, NSScreen, NSTextAlignment, NSTextField, NSWindow, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString, NSURL};

use super::MacOs;
use super::ax;
use super::cursor::pump;
use super::responsibility;
use crate::error::{Error, ErrorCode, Result};
use crate::model::Rect;
use crate::platform::{Permissions, Platform};
use crate::session::state_dir;

/// How often the window reads the permissions again, so a grant shows up
/// without a click.
const REFRESH: Duration = Duration::from_millis(250);
/// Keeps a second `doctor --prompt` from opening a second window.
const LOCK: &str = "permissions-window.lock";

const WIDTH: f64 = 480.0;
const HEIGHT: f64 = 400.0;
const MARGIN: f64 = 24.0;
const ICON_SIZE: f64 = 128.0;
const ROWS_TOP: f64 = 180.0;
const ROW_HEIGHT: f64 = 72.0;
const BUTTON_WIDTH: f64 = 120.0;
const BUTTON_HEIGHT: f64 = 28.0;
/// Space between this window and System Settings when they sit side by side.
const GAP: f64 = 12.0;
/// How long to wait for System Settings to show the pane after a Grant click.
const SETTINGS_WAIT: Duration = Duration::from_secs(3);
/// System Settings' bundle ID, which lists its windows through Accessibility.
const SETTINGS_BUNDLE_ID: &str = "com.apple.systempreferences";

/// A permission the window lists, with the pane that grants it. Each tag
/// is the permission's index in [`Permission::ALL`].
#[derive(Clone, Copy)]
enum Permission {
    Accessibility = 0,
    ScreenRecording = 1,
}

impl Permission {
    const ALL: [Self; 2] = [Self::Accessibility, Self::ScreenRecording];

    fn from_tag(tag: isize) -> Option<Self> {
        usize::try_from(tag)
            .ok()
            .and_then(|index| Self::ALL.get(index))
            .copied()
    }

    fn title(self) -> &'static str {
        match self {
            Self::Accessibility => "Accessibility",
            Self::ScreenRecording => "Screen Recording",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Accessibility => "Reads the element tree and sends input.",
            Self::ScreenRecording => "Captures windows.",
        }
    }

    fn pane(self) -> &'static str {
        match self {
            Self::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Self::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
        }
    }

    fn granted(self, permissions: &Permissions) -> bool {
        match self {
            Self::Accessibility => permissions.accessibility,
            Self::ScreenRecording => permissions.screen_recording,
        }
    }
}

/// Starts the window from llama-cu.app in its own process group, so it
/// outlives the command. Does nothing outside the app, where the window
/// has no icon to drag.
pub fn show() -> Result<()> {
    let Some(bundle) = responsibility::bundle() else {
        return Ok(());
    };
    Command::new(bundle.join("Contents/MacOS/llama-cu"))
        .arg("permissions-window")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()?;
    Ok(())
}

/// Shows the window until the user closes it. Returns at once when another
/// window already runs.
pub fn run() -> Result<()> {
    let mtm = MainThreadMarker::new().ok_or_else(|| {
        Error::new(
            ErrorCode::Platform,
            "the permissions window must run on the main thread",
        )
    })?;
    let bundle = responsibility::bundle().ok_or_else(|| {
        Error::new(
            ErrorCode::Unsupported,
            "the permissions window needs llama-cu.app",
        )
    })?;
    fs::create_dir_all(state_dir())?;
    let lock = File::create(state_dir().join(LOCK))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(()),
        Err(TryLockError::Error(err)) => return Err(err.into()),
    }

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    let panel = build(mtm, &bundle)?;
    panel.window.makeKeyAndOrderFront(None);
    // The helper starts behind the terminal, so ask for the front explicitly.
    panel.window.orderFrontRegardless();
    app.activate();
    while panel.window.isVisible() {
        panel.refresh(&MacOs.permissions());
        panel.follow_settings(mtm);
        pump(&app, REFRESH);
    }
    Ok(())
}

/// The window and the controls that change as permissions are granted.
struct Panel {
    window: Retained<NSWindow>,
    buttons: Vec<(Permission, Retained<NSButton>)>,
    /// Targets of the buttons. AppKit does not retain them.
    actions: Retained<Actions>,
}

impl Panel {
    /// Moves the window beside System Settings once a Grant click has opened
    /// it, then brings this window forward. Gives up after `SETTINGS_WAIT`.
    fn follow_settings(&self, mtm: MainThreadMarker) {
        let pending = &self.actions.ivars().pending_since;
        let Some(started) = pending.get() else {
            return;
        };
        let Some(settings) = settings_frame() else {
            if started.elapsed() >= SETTINGS_WAIT {
                pending.set(None);
            }
            return;
        };
        pending.set(None);
        let screens = NSScreen::screens(mtm);
        if let Some(primary) = screens.firstObject() {
            let origin = beside(settings, self.window.frame().size, primary.frame().size);
            self.window.setFrameOrigin(origin);
        }
        self.window.orderFrontRegardless();
    }

    fn refresh(&self, permissions: &Permissions) {
        for (permission, button) in &self.buttons {
            let granted = permission.granted(permissions);
            button.setEnabled(!granted);
            button.setTitle(&NSString::from_str(if granted {
                "Granted"
            } else {
                "Grant…"
            }));
        }
    }
}

fn build(mtm: MainThreadMarker, bundle: &Path) -> Result<Panel> {
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame(0.0, 0.0, WIDTH, HEIGHT),
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Manage Permissions"));
    window.center();
    let content = window.contentView().ok_or_else(|| {
        Error::new(
            ErrorCode::Platform,
            "the permissions window has no content view",
        )
    })?;

    let path = bundle.to_string_lossy();
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(&path));
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
    let icon_view = DragIcon::new(
        mtm,
        frame(
            (WIDTH - ICON_SIZE) / 2.0,
            HEIGHT - MARGIN - ICON_SIZE,
            ICON_SIZE,
            ICON_SIZE,
        ),
        url,
        &icon,
    );
    content.addSubview(&icon_view);

    let caption = label(
        mtm,
        "Drag the icon into each Privacy & Security list.",
        frame(
            MARGIN,
            HEIGHT - MARGIN - ICON_SIZE - 26.0,
            WIDTH - 2.0 * MARGIN,
            18.0,
        ),
    );
    caption.setAlignment(NSTextAlignment::Center);
    caption.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    caption.setTextColor(Some(&NSColor::secondaryLabelColor()));
    content.addSubview(&caption);

    let actions = Actions::alloc(mtm).set_ivars(ActionsIvars {
        pending_since: Cell::new(None),
    });
    let actions: Retained<Actions> = unsafe { msg_send![super(actions), init] };
    let mut buttons = Vec::with_capacity(Permission::ALL.len());
    for (index, permission) in Permission::ALL.into_iter().enumerate() {
        let row_y = ROWS_TOP - (index + 1) as f64 * ROW_HEIGHT;
        let title = label(
            mtm,
            permission.title(),
            frame(MARGIN, row_y + 36.0, 300.0, 18.0),
        );
        title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        content.addSubview(&title);

        let detail = label(
            mtm,
            permission.detail(),
            frame(MARGIN, row_y + 14.0, 300.0, 16.0),
        );
        detail.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        content.addSubview(&detail);

        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Grant…"),
                Some(&actions),
                Some(sel!(grant:)),
                mtm,
            )
        };
        button.setTag(permission as isize);
        button.setFrame(frame(
            WIDTH - MARGIN - BUTTON_WIDTH,
            row_y + (ROW_HEIGHT - BUTTON_HEIGHT) / 2.0,
            BUTTON_WIDTH,
            BUTTON_HEIGHT,
        ));
        content.addSubview(&button);
        buttons.push((permission, button));
    }
    Ok(Panel {
        window,
        buttons,
        actions,
    })
}

fn label(mtm: MainThreadMarker, text: &str, rect: NSRect) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(rect);
    label
}

fn frame(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

/// Opens the Privacy & Security pane for `permission`.
fn open_pane(permission: Permission) {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(permission.pane())) else {
        return;
    };
    NSWorkspace::sharedWorkspace().openURL(&url);
}

/// Returns the frame of System Settings' largest window, in top-left screen
/// coordinates. None when Settings is not running or Accessibility is missing.
fn settings_frame() -> Option<Rect> {
    let bundle_id = NSString::from_str(SETTINGS_BUNDLE_ID);
    let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&bundle_id);
    let pid = apps.firstObject()?.processIdentifier();
    ax::windows(pid)
        .ok()?
        .into_iter()
        .filter(|window| !window.minimized)
        .map(|window| window.frame)
        .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
}

/// Returns the origin, in bottom-left screen coordinates, of a window of
/// `size` beside `settings`, with their tops aligned. `screen` is the primary
/// screen's size. The window goes on the right when it fits and on the left
/// otherwise.
fn beside(settings: Rect, size: NSSize, screen: NSSize) -> NSPoint {
    let right = settings.x + settings.width + GAP;
    let x = if right + size.width <= screen.width {
        right
    } else {
        (settings.x - GAP - size.width).max(0.0)
    };
    let y = screen.height - settings.y - size.height;
    NSPoint::new(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beside_places_the_window_next_to_settings() {
        let screen = NSSize::new(1512.0, 982.0);
        let size = NSSize::new(480.0, 400.0);
        let cases = [
            (
                "fits on the right",
                Rect {
                    x: 100.0,
                    y: 50.0,
                    width: 800.0,
                    height: 600.0,
                },
                (912.0, 532.0),
            ),
            (
                "moves left when the right is off screen",
                Rect {
                    x: 900.0,
                    y: 50.0,
                    width: 600.0,
                    height: 600.0,
                },
                (408.0, 532.0),
            ),
            (
                "stays on screen when both sides are too narrow",
                Rect {
                    x: 10.0,
                    y: 50.0,
                    width: 1400.0,
                    height: 600.0,
                },
                (0.0, 532.0),
            ),
        ];
        for (name, settings, want) in cases {
            let got = beside(settings, size, screen);
            assert_eq!((got.x, got.y), want, "{name}");
        }
    }
}

/// Per-click state of the Grant buttons.
struct ActionsIvars {
    /// When a Grant click opened System Settings and the window has not yet
    /// been placed beside it.
    pending_since: Cell<Option<Instant>>,
}

// Target of the Grant buttons. The button's tag names the permission.
define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ActionsIvars]
    struct Actions;

    impl Actions {
        #[unsafe(method(grant:))]
        fn grant(&self, sender: &NSButton) {
            if let Some(permission) = Permission::from_tag(sender.tag()) {
                open_pane(permission);
                self.ivars().pending_since.set(Some(Instant::now()));
            }
        }
    }

    unsafe impl NSObjectProtocol for Actions {}
);

/// The data a [`DragIcon`] drags.
struct DragIvars {
    /// The llama-cu.app bundle, which Privacy & Security adds when dropped.
    url: Retained<NSURL>,
}

// The app icon. Dragging it starts a file drag of llama-cu.app.
define_class!(
    #[unsafe(super(NSImageView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = DragIvars]
    struct DragIcon;

    impl DragIcon {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let url: &ProtocolObject<dyn NSPasteboardWriting> =
                ProtocolObject::from_ref(&*self.ivars().url);
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), url);
            let image = self.image();
            let contents: Option<&AnyObject> = image.as_deref().map(|image| image.as_ref());
            unsafe { item.setDraggingFrame_contents(self.bounds(), contents) };
            let items = NSArray::from_retained_slice(&[item]);
            let source: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(self);
            self.beginDraggingSessionWithItems_event_source(&items, event, source);
        }
    }

    unsafe impl NSObjectProtocol for DragIcon {}

    unsafe impl NSDraggingSource for DragIcon {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl DragIcon {
    fn new(
        mtm: MainThreadMarker,
        rect: NSRect,
        url: Retained<NSURL>,
        icon: &NSImage,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DragIvars { url });
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: rect] };
        view.setImage(Some(icon));
        // Scale up as well as down, so the icon fills the frame.
        view.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        view
    }
}
