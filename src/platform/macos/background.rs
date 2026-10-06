//! Window-addressed mouse events for experimental background delivery.

use std::ffi::{c_char, c_void};
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{CGEvent, CGEventField, CGEventType, CGMouseButton};
use objc2_foundation::{NSPoint, NSProcessInfo};

use crate::error::{Error, ErrorCode, Result};
use crate::model::{Point, Rect};

type SetWindowLocation = unsafe extern "C" fn(*const CGEvent, CGPoint);

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// Creates an AppKit event so the window routing metadata is populated before
/// conversion to CGEvent. All mouse event type values match between the APIs.
pub fn mouse_event(
    window: u64,
    frame: Rect,
    kind: CGEventType,
    at: Point,
    button: CGMouseButton,
) -> Result<CFRetained<CGEvent>> {
    let native = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        NSEventType(kind.0 as usize),
        NSPoint::new(at.x, at.y),
        NSEventModifierFlags::empty(),
        NSProcessInfo::processInfo().systemUptime(),
        window as isize,
        None,
        0,
        1,
        0.0,
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
    // NSEvent owns the converted event; retain it for the caller.
    Ok(unsafe { CFRetained::retain(NonNull::from(&*event)) })
}

/// Sets the addressed window and its local point. The local coordinate setter
/// is private macOS API; refusing when absent avoids silently malformed input.
pub fn stamp_location(window: u64, frame: Rect, at: Point, event: &CGEvent) -> Result<()> {
    static SET_LOCATION: OnceLock<Option<SetWindowLocation>> = OnceLock::new();
    let set_location = SET_LOCATION
        .get_or_init(|| {
            // RTLD_DEFAULT on Darwin. CoreGraphics is already linked and loaded.
            let pointer =
                unsafe { dlsym(-2isize as *mut c_void, c"CGEventSetWindowLocation".as_ptr()) };
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

fn event_failed() -> Error {
    Error::new(
        ErrorCode::Platform,
        "creating a background mouse event failed",
    )
}
