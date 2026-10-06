//! macOS backend: Accessibility API, ScreenCaptureKit, CGEvent, and
//! NSPasteboard.

mod apps;
mod ax;
mod capture;
mod clipboard;
mod input;

use std::ops::Range;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use objc2_application_services::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary};
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

use super::{Permissions, Platform};
use crate::error::{Error, ErrorCode, Result};
use crate::keys::{Key, KeyCombo, Modifiers};
use crate::model::{AppInfo, MouseButton, NodeInfo, Point, Snapshot, SnapshotOptions, WindowInfo};

/// How long to wait for an app to come to the front.
const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(2);

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
        "accessibility permission is missing for the app that runs llama-cu; run `llama-cu doctor`",
    ))
}

impl Platform for MacOs {
    type Element = ax::Element;
    type Clipboard = clipboard::Saved;

    fn permissions(&self) -> Permissions {
        Permissions {
            accessibility: ax::is_trusted(),
            screen_recording: CGPreflightScreenCaptureAccess(),
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

    fn launch(&self, app: &AppInfo) -> Result<i32> {
        apps::launch(app)
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
        if !is_front() {
            ax::set_attribute(&app, "AXFrontmost", CFBoolean::new(true))?;
            let start = Instant::now();
            while !is_front() && start.elapsed() < ACTIVATE_TIMEOUT {
                thread::sleep(Duration::from_millis(20));
            }
            if !is_front() {
                return Err(Error::new(
                    ErrorCode::Timeout,
                    "the app did not come to the front",
                ));
            }
        }
        if let Some(window) = window
            .map(|id| ax::window_element(pid, id))
            .transpose()?
            .flatten()
        {
            let _ = ax::set_attribute(&window, "AXMinimized", CFBoolean::new(false));
            let _ = ax::set_attribute(&window, "AXMain", CFBoolean::new(true));
            let _ = ax::perform_action(&window, "raise");
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

    fn perform_action(&self, element: &Self::Element, action: &str) -> Result<()> {
        ax::perform_action(element, action)
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

    fn click(&self, at: Point, button: MouseButton, count: u32) -> Result<()> {
        require_accessibility()?;
        input::click(at, button, count)
    }

    fn drag(&self, from: Point, to: Point) -> Result<()> {
        require_accessibility()?;
        input::drag(from, to)
    }

    fn scroll(&self, at: Point, dx: i32, dy: i32) -> Result<()> {
        require_accessibility()?;
        input::scroll(at, dx, dy)
    }

    fn press_key(&self, combo: &KeyCombo) -> Result<()> {
        require_accessibility()?;
        input::press_key(combo)
    }

    fn type_text(&self, text: &str) -> Result<()> {
        require_accessibility()?;
        input::type_text(text)
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
