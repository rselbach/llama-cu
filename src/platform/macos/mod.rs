//! macOS backend: Accessibility API, ScreenCaptureKit, CGEvent, and
//! NSPasteboard.

mod apps;
mod ax;
mod background;
mod capture;
mod clipboard;
mod input;
mod responsibility;

use std::ops::Range;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use objc2_application_services::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

pub use responsibility::become_responsible;

use super::{InputTarget, Permissions, Platform};
use crate::error::{Error, ErrorCode, Result};
use crate::keys::{Key, KeyCombo, Modifiers};
use crate::model::{AppInfo, MouseButton, NodeInfo, Point, Snapshot, SnapshotOptions, WindowInfo};

/// How long to wait for an app to come to the front.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often to repeat the request while the app stays behind. macOS drops
/// the request when another process, such as a system alert, holds the
/// front, and later gives the front back to whoever had it before.
const ACTIVATE_RETRY: Duration = Duration::from_millis(250);

/// The macOS backend.
pub struct MacOs;

impl MacOs {
    /// Creates the backend and configures accessibility timeouts.
    pub fn new() -> Self {
        ax::configure();
        Self
    }
}

fn require_accessibility() -> Result<()> {
    if ax::is_trusted() {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::PermissionDenied,
        "accessibility permission is missing; run `llama-cu doctor`",
    ))
}

fn require_input(target: InputTarget) -> Result<()> {
    require_accessibility()?;
    background::require_on_screen(target)
}

impl Platform for MacOs {
    type Element = ax::Element;
    type Clipboard = clipboard::Saved;

    fn permissions(&self) -> Permissions {
        Permissions {
            accessibility: ax::is_trusted(),
            screen_recording: CGPreflightScreenCaptureAccess(),
            app: responsibility::bundle(),
        }
    }

    fn request_permissions(&self) {
        if !ax::is_trusted() {
            let options = CFDictionary::from_slices(
                &[unsafe { kAXTrustedCheckOptionPrompt }],
                &[CFBoolean::new(true)],
            );
            unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
        }
        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess();
        }
    }

    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        Ok(apps::list())
    }

    fn app_at_path(&self, path: &Path) -> Result<AppInfo> {
        apps::info_at(path)
    }

    fn launch(&self, app: &AppInfo, background: bool) -> Result<i32> {
        apps::launch(app, background)
    }

    fn is_running(&self, pid: i32) -> bool {
        apps::is_running(pid)
    }

    fn frontmost_pid(&self) -> Option<i32> {
        apps::frontmost_pid()
    }

    fn windows(&self, pid: i32) -> Result<Vec<WindowInfo>> {
        require_accessibility()?;
        ax::windows(pid)
    }

    fn activate(&self, pid: i32, window: Option<u64>) -> Result<()> {
        require_accessibility()?;
        let app = ax::application(pid);
        let is_front = || {
            ax::attribute(&app, "AXFrontmost")
                .ok()
                .flatten()
                .and_then(|v| v.downcast::<CFBoolean>().ok())
                .is_some_and(|b| b.as_bool())
        };
        let start = Instant::now();
        let mut requested: Option<Instant> = None;
        while !is_front() {
            if start.elapsed() >= ACTIVATE_TIMEOUT {
                let message = match apps::frontmost_name() {
                    Some(name) => format!("the app did not come to the front; {name} is in front"),
                    None => "the app did not come to the front".to_string(),
                };
                return Err(Error::new(ErrorCode::Timeout, message));
            }
            if requested.is_none_or(|at| at.elapsed() >= ACTIVATE_RETRY) {
                ax::set_attribute(&app, "AXFrontmost", CFBoolean::new(true))?;
                requested = Some(Instant::now());
            }
            thread::sleep(Duration::from_millis(20));
        }
        if let Some(window) = window
            .map(|id| ax::window_element(pid, id))
            .transpose()?
            .flatten()
        {
            let _ = ax::set_attribute(&window, "AXMinimized", CFBoolean::new(false));
            let _ = ax::set_attribute(&window, "AXMain", CFBoolean::new(true));
            let _ = ax::perform_action(&window, "raise", false);
        }
        // Let the window server finish reordering before input arrives.
        thread::sleep(Duration::from_millis(50));
        Ok(())
    }

    fn snapshot(
        &self,
        pid: i32,
        window: Option<u64>,
        options: SnapshotOptions,
    ) -> Result<Snapshot> {
        require_accessibility()?;
        ax::snapshot(pid, window, options)
    }

    fn resolve(&self, pid: i32, path: &[usize]) -> Result<Self::Element> {
        require_accessibility()?;
        ax::resolve(pid, path)
    }

    fn focused_element(&self, pid: i32) -> Result<Self::Element> {
        require_accessibility()?;
        ax::focused_element(pid)
    }

    fn element_info(&self, element: &Self::Element) -> Result<NodeInfo> {
        ax::info(element)
    }

    fn element_text(&self, element: &Self::Element) -> Result<String> {
        ax::text(element)
    }

    fn perform_action(
        &self,
        element: &Self::Element,
        action: &str,
        background: bool,
    ) -> Result<()> {
        ax::perform_action(element, action, background)
    }

    fn set_value(&self, element: &Self::Element, value: &str) -> Result<()> {
        ax::set_value(element, value)
    }

    fn select_range(&self, element: &Self::Element, text: &str, range: Range<usize>) -> Result<()> {
        ax::select_range(element, text, range)
    }

    fn capture_window(
        &self,
        _pid: i32,
        window: u64,
        path: &Path,
        scale: f64,
    ) -> Result<(u32, u32)> {
        capture::capture_window(window, path, scale)
    }

    fn click(&self, target: InputTarget, at: Point, button: MouseButton, count: u32) -> Result<()> {
        require_input(target)?;
        input::click(target, at, button, count)
    }

    fn drag(&self, target: InputTarget, from: Point, to: Point) -> Result<()> {
        require_input(target)?;
        input::drag(target, from, to)
    }

    fn scroll(&self, target: InputTarget, at: Point, dx: i32, dy: i32) -> Result<()> {
        require_input(target)?;
        input::scroll(target, at, dx, dy)
    }

    fn press_key(&self, target: InputTarget, combo: &KeyCombo) -> Result<()> {
        require_input(target)?;
        input::press_key(target, combo)
    }

    fn type_text(&self, target: InputTarget, text: &str) -> Result<()> {
        require_input(target)?;
        input::type_text(target, text)
    }

    fn clipboard_save(&self) -> Result<Self::Clipboard> {
        Ok(clipboard::save())
    }

    fn clipboard_set(&self, plain: &str, html: Option<&str>) -> Result<()> {
        clipboard::set(plain, html)
    }

    fn clipboard_restore(&self, saved: Self::Clipboard) -> Result<()> {
        clipboard::restore(saved)
    }

    fn paste_shortcut(&self) -> KeyCombo {
        KeyCombo {
            modifiers: Modifiers {
                command: true,
                ..Modifiers::default()
            },
            key: Some(Key::Char('v')),
        }
    }
}
