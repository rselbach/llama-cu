//! Window-addressed events and delivery checks for experimental background
//! input.

use std::ffi::c_void;
use std::sync::OnceLock;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFRetained, CFString, CFType, CGPoint,
};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventType, CGMouseButton, CGWindowListCopyWindowInfo,
    CGWindowListOption, kCGWindowIsOnscreen,
};
use objc2_foundation::{NSPoint, NSProcessInfo};

use super::ax;
use super::responsibility::{RTLD_DEFAULT, dlsym};
use crate::error::{Error, ErrorCode, Result};
use crate::model::{Point, Rect};
use crate::platform::InputTarget;

type SetWindowLocation = unsafe extern "C" fn(*const CGEvent, CGPoint);

/// Creates an AppKit event so the window routing metadata is populated before
/// conversion to CGEvent. All mouse event type values match between the APIs.
pub fn mouse_event(
    window: u64,
    frame: Rect,
    kind: CGEventType,
    at: Point,
    button: CGMouseButton,
) -> Result<CFRetained<CGEvent>> {
    // Full pressure while a button is down, as CGEventCreateMouseEvent sets
    // for foreground events.
    let pressure = match kind {
        CGEventType::LeftMouseDown
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDown
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDown
        | CGEventType::OtherMouseDragged => 1.0,
        _ => 0.0,
    };
    let native = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType(kind.0 as usize),
        NSPoint::new(at.x, at.y),
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window as isize,
        None,
        0,
        1,
        pressure,
    ).ok_or_else(event_failed)?;
    let event = native.CGEvent().ok_or_else(event_failed)?;
    CGEvent::set_location(Some(&event), CGPoint { x: at.x, y: at.y });
    CGEvent::set_integer_value_field(Some(&event), CGEventField::MouseEventSubtype, 3);
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::MouseEventButtonNumber,
        i64::from(button.0),
    );
    stamp_location(window, frame, at, &event)?;
    Ok(event.into())
}

/// Sets the addressed window and its local point. The local coordinate setter
/// is private macOS API; refusing when absent avoids silently malformed input.
pub fn stamp_location(window: u64, frame: Rect, at: Point, event: &CGEvent) -> Result<()> {
    static SET_LOCATION: OnceLock<Option<SetWindowLocation>> = OnceLock::new();
    let set_location = SET_LOCATION
        .get_or_init(|| {
            // CoreGraphics is already linked and loaded.
            let pointer = unsafe { dlsym(RTLD_DEFAULT, c"CGEventSetWindowLocation".as_ptr()) };
            if pointer.is_null() {
                return None;
            }
            // The symbol takes a CGEventRef and a CGPoint by value.
            Some(unsafe { std::mem::transmute::<*mut c_void, SetWindowLocation>(pointer) })
        })
        .ok_or_else(|| {
            Error::new(
                ErrorCode::BackgroundUnavailable,
                "this macOS version does not expose CGEventSetWindowLocation",
            )
        })?;
    for field in [
        // NSEvent's windowNumber field, also needed on raw scroll events.
        CGEventField(51),
        CGEventField::MouseEventWindowUnderMousePointer,
        CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
    ] {
        CGEvent::set_integer_value_field(Some(event), field, window as i64);
    }
    unsafe {
        set_location(
            event,
            CGPoint {
                x: at.x - frame.x,
                y: at.y - frame.y,
            },
        );
    }
    Ok(())
}

/// Refuses background input to a window that is not on screen in the
/// current Space. Apps do not handle events posted to minimized windows,
/// hidden apps, or other Spaces, yet posting them succeeds.
pub fn require_on_screen(target: InputTarget) -> Result<()> {
    let InputTarget::Background { window, .. } = target else {
        return Ok(());
    };
    if is_on_screen(window) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::BackgroundUnavailable,
        "the selected window is not on screen; it is minimized, hidden, or on another Space",
    ))
}

/// Refuses background keys unless the selected window has the app's keyboard
/// focus. Keys posted to a process go to its focused window, not the window
/// the target names.
pub fn require_keyboard_focus(target: InputTarget) -> Result<()> {
    let InputTarget::Background { pid, window, .. } = target else {
        return Ok(());
    };
    let focused = ax::element_attribute(&ax::application(pid), "AXFocusedWindow")?;
    if focused.as_deref().and_then(ax::window_id) == Some(window) {
        return Ok(());
    }
    Err(Error::new(
        ErrorCode::BackgroundUnavailable,
        "the selected window does not have the app's keyboard focus; click its text control and observe again",
    ))
}

fn is_on_screen(window: u64) -> bool {
    let Ok(id) = u32::try_from(window) else {
        return false;
    };
    let Some(list) = CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, id)
    else {
        return false;
    };
    // SAFETY: window lists hold dictionaries keyed by strings.
    let list: &CFArray<CFDictionary<CFString, CFType>> = unsafe { list.cast_unchecked() };
    list.get(0)
        .and_then(|info| info.get(unsafe { kCGWindowIsOnscreen }))
        .and_then(|value| value.downcast::<CFBoolean>().ok())
        .is_some_and(|on_screen| on_screen.as_bool())
}

fn event_failed() -> Error {
    Error::new(
        ErrorCode::Platform,
        "creating a background mouse event failed",
    )
}
