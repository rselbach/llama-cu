//! Operating-system backends.
//!
//! Each backend implements [`Platform`] with thin primitives over the native
//! accessibility, capture, input, and clipboard APIs. Command behavior that
//! is the same everywhere lives in `commands.rs`.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::keys::KeyCombo;
use crate::model::{
    AppInfo, MouseButton, NodeInfo, Point, Rect, Snapshot, SnapshotOptions, WindowInfo,
};

/// Where synthetic input is delivered. Background input must never fall back
/// to the global event stream or activate the application.
#[derive(Debug, Clone, Copy)]
pub enum InputTarget {
    Foreground,
    Background { pid: i32, window: u64, frame: Rect },
}

/// What the agent cursor shows once it arrives, so people can follow
/// background input that never moves the real pointer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Gesture {
    /// Rests at the point, as for setting a value.
    Point,
    /// Clicks `count` times, with the right or middle button if `secondary`.
    Click { count: u32, secondary: bool },
    /// Presses at the point and drags to `to`, in screen points.
    Drag { to: Point },
    /// Scrolls; positive `dy` scrolls down and positive `dx` right.
    Scroll { dx: i32, dy: i32 },
}

#[cfg(target_os = "macos")]
mod macos;

/// The backend for the operating system this binary was built for.
#[cfg(target_os = "macos")]
pub fn current() -> impl Platform {
    macos::MacOs::new()
}

/// Prepares the process before any command runs. A llama-cu inside
/// llama-cu.app re-executes itself so the app, not the terminal or agent
/// host that started it, owns its permissions.
#[cfg(target_os = "macos")]
pub fn prepare_process() -> Result<()> {
    macos::become_responsible()
}

/// Runs the helper process that draws the agent cursor. Commands start it
/// as needed, and it exits once the cursor has been idle for a while.
#[cfg(target_os = "macos")]
pub fn run_cursor() -> Result<()> {
    macos::run_cursor()
}

/// Permissions the backend needs from the operating system.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub accessibility: bool,
    pub screen_recording: bool,
    /// The app that holds the permissions, when it is llama-cu's own app
    /// rather than the one that started llama-cu.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<PathBuf>,
}

/// Native primitives a backend provides. All points and frames use screen
/// coordinates in points with a top-left origin.
pub trait Platform {
    /// Native accessibility element handle.
    type Element;
    /// Saved clipboard contents.
    type Clipboard;

    /// Reports which required permissions are granted.
    fn permissions(&self) -> Permissions;
    /// Asks the operating system to prompt for missing permissions.
    fn request_permissions(&self);

    /// Lists installed apps and running apps, with `pid` set when running.
    fn list_apps(&self) -> Result<Vec<AppInfo>>;
    /// Describes the app bundle or executable at `path`.
    fn app_at_path(&self, path: &Path) -> Result<AppInfo>;
    /// Launches the app and waits until it accepts accessibility requests.
    /// Requests no activation when `background` is true.
    /// Returns its process ID.
    fn launch(&self, app: &AppInfo, background: bool) -> Result<i32>;
    /// Reports whether `pid` is a running app.
    fn is_running(&self, pid: i32) -> bool;
    /// Returns the process ID of the app in front, if any.
    fn frontmost_pid(&self) -> Option<i32>;
    /// Lists the app's top-level windows.
    fn windows(&self, pid: i32) -> Result<Vec<WindowInfo>>;
    /// Brings the app, and optionally one of its windows, to the front.
    fn activate(&self, pid: i32, window: Option<u64>) -> Result<()>;

    /// Captures the accessibility tree of `window`, plus the menu bar and
    /// any open menus. Node paths must work with [`Platform::resolve`].
    fn snapshot(&self, pid: i32, window: Option<u64>, options: SnapshotOptions)
    -> Result<Snapshot>;
    /// Finds the element at a snapshot path.
    fn resolve(&self, pid: i32, path: &[usize]) -> Result<Self::Element>;
    /// Returns the element that has keyboard focus in the app.
    fn focused_element(&self, pid: i32) -> Result<Self::Element>;
    /// Describes an element.
    fn element_info(&self, element: &Self::Element) -> Result<NodeInfo>;
    /// Returns the full text value of an element.
    fn element_text(&self, element: &Self::Element) -> Result<String>;
    /// Invokes a named accessibility action, matched case-insensitively.
    /// Refuses actions that raise a window when `background` is true.
    fn perform_action(&self, element: &Self::Element, action: &str, background: bool)
    -> Result<()>;
    /// Replaces an element's value.
    fn set_value(&self, element: &Self::Element, value: &str) -> Result<()>;
    /// Selects `range`, given in bytes of `text`, which is the element's
    /// current text. An empty range places the cursor.
    fn select_range(&self, element: &Self::Element, text: &str, range: Range<usize>) -> Result<()>;

    /// Captures a window at `scale` pixels per point and writes it as PNG.
    /// Returns the image size in pixels.
    fn capture_window(&self, pid: i32, window: u64, path: &Path, scale: f64) -> Result<(u32, u32)>;

    /// Clicks `count` times at a screen point.
    fn click(&self, target: InputTarget, at: Point, button: MouseButton, count: u32) -> Result<()>;
    /// Drags with the left button between two screen points.
    fn drag(&self, target: InputTarget, from: Point, to: Point) -> Result<()>;
    /// Scrolls at a screen point by whole lines. Positive `dy` scrolls down
    /// and positive `dx` scrolls right.
    fn scroll(&self, target: InputTarget, at: Point, dx: i32, dy: i32) -> Result<()>;
    /// Presses and releases a key combo. Background keys need the target
    /// window to have the app's keyboard focus.
    fn press_key(&self, target: InputTarget, combo: &KeyCombo) -> Result<()>;
    /// Types text into the focused control. Background typing stops if the
    /// target window loses the app's keyboard focus.
    fn type_text(&self, target: InputTarget, text: &str) -> Result<()>;
    /// Moves the agent cursor to the screen point `at` above `window`, waits
    /// until it arrives, then plays `gesture` while the caller sends input.
    fn show_cursor(&self, window: u64, at: Point, gesture: Gesture) -> Result<()>;

    /// Saves the clipboard contents.
    fn clipboard_save(&self) -> Result<Self::Clipboard>;
    /// Replaces the clipboard with plain text and optional HTML.
    fn clipboard_set(&self, plain: &str, html: Option<&str>) -> Result<()>;
    /// Restores saved clipboard contents.
    fn clipboard_restore(&self, saved: Self::Clipboard) -> Result<()>;
    /// Returns the platform's paste shortcut.
    fn paste_shortcut(&self) -> KeyCombo;
}
