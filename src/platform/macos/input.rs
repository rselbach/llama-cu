//! Synthetic mouse and keyboard input with CGEvent.

use std::collections::HashMap;
use std::ffi::c_void;
use std::thread;
use std::time::Duration;

use objc2_core_foundation::{CFData, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType, CGKeyCode, CGMouseButton,
    CGScrollEventUnit,
};
use objc2_foundation::{NSString, NSUserDefaults};
use unicode_segmentation::UnicodeSegmentation;

use crate::error::{Error, ErrorCode, Result};
use crate::keys::{Key, KeyCombo, NamedKey};
use crate::model::{MouseButton, Point};
use crate::platform::InputTarget;

/// Pause that lets the target app process one event before the next.
const EVENT_GAP: Duration = Duration::from_millis(10);
/// Pause after moving the pointer so hover state can update.
const HOVER_GAP: Duration = Duration::from_millis(40);
/// Pause between typing events.
const TYPE_GAP: Duration = Duration::from_millis(10);
/// Most UTF-16 units in one typing event. Events carry whole characters, so
/// one character longer than this, such as a family emoji, goes alone.
const TYPE_CHUNK_UNITS: usize = 20;
/// Pointer moves per drag.
const DRAG_STEPS: u32 = 20;

const KEY_RETURN: CGKeyCode = 0x24;
const KEY_TAB: CGKeyCode = 0x30;
const KEY_COMMAND: CGKeyCode = 0x37;
const KEY_SHIFT: CGKeyCode = 0x38;
const KEY_OPTION: CGKeyCode = 0x3A;
const KEY_CONTROL: CGKeyCode = 0x3B;
const KEY_FUNCTION: CGKeyCode = 0x3F;

/// Clicks `count` times at a screen point.
pub fn click(target: InputTarget, at: Point, button: MouseButton, count: u32) -> Result<()> {
    let (down, up, cg_button) = match button {
        MouseButton::Left => (
            CGEventType::LeftMouseDown,
            CGEventType::LeftMouseUp,
            CGMouseButton::Left,
        ),
        MouseButton::Right => (
            CGEventType::RightMouseDown,
            CGEventType::RightMouseUp,
            CGMouseButton::Right,
        ),
        MouseButton::Middle => (
            CGEventType::OtherMouseDown,
            CGEventType::OtherMouseUp,
            CGMouseButton::Center,
        ),
    };
    move_to(target, at)?;
    thread::sleep(HOVER_GAP);
    for n in 1..=count {
        for kind in [down, up] {
            let event = mouse_event(target, kind, at, cg_button)?;
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventClickState,
                i64::from(n),
            );
            post(target, &event);
            thread::sleep(EVENT_GAP);
        }
    }
    Ok(())
}

/// Drags with the left button between two screen points.
pub fn drag(target: InputTarget, from: Point, to: Point) -> Result<()> {
    validate_point(target, to)?;
    move_to(target, from)?;
    thread::sleep(HOVER_GAP);
    post(
        target,
        &*mouse_event(
            target,
            CGEventType::LeftMouseDown,
            from,
            CGMouseButton::Left,
        )?,
    );
    thread::sleep(HOVER_GAP);
    for step in 1..=DRAG_STEPS {
        let t = f64::from(step) / f64::from(DRAG_STEPS);
        let p = Point {
            x: from.x + (to.x - from.x) * t,
            y: from.y + (to.y - from.y) * t,
        };
        post(
            target,
            &*mouse_event(
                target,
                CGEventType::LeftMouseDragged,
                p,
                CGMouseButton::Left,
            )?,
        );
        thread::sleep(EVENT_GAP);
    }
    thread::sleep(HOVER_GAP);
    post(
        target,
        &*mouse_event(target, CGEventType::LeftMouseUp, to, CGMouseButton::Left)?,
    );
    Ok(())
}

/// Scrolls by whole lines at a screen point. Positive `dy` scrolls down and
/// positive `dx` scrolls right.
pub fn scroll(target: InputTarget, at: Point, dx: i32, dy: i32) -> Result<()> {
    move_to(target, at)?;
    thread::sleep(HOVER_GAP);
    // Wheel deltas count positive toward the top and left of the content.
    // With natural scrolling on, the system flips events posted at the HID
    // tap, including synthetic ones, but not events posted to a process.
    let flipped = matches!(target, InputTarget::Foreground) && natural_scrolling();
    let sign = if flipped { 1 } else { -1 };
    let steps = dx.unsigned_abs().max(dy.unsigned_abs());
    for i in 0..steps {
        let vertical = if i < dy.unsigned_abs() {
            sign * dy.signum()
        } else {
            0
        };
        let horizontal = if i < dx.unsigned_abs() {
            sign * dx.signum()
        } else {
            0
        };
        let event = CGEvent::new_scroll_wheel_event2(
            None,
            CGScrollEventUnit::Line,
            2,
            vertical,
            horizontal,
            0,
        )
        .ok_or_else(event_failed)?;
        CGEvent::set_location(Some(&event), cg_point(at));
        if let InputTarget::Background { window, frame, .. } = target {
            CGEvent::set_flags(Some(&event), CGEventFlags::empty());
            super::background::stamp_location(window, frame, at, &event)?;
        }
        post(target, &event);
        thread::sleep(EVENT_GAP);
    }
    Ok(())
}

/// Presses a key combo, holding modifier keys the way a keyboard would.
pub fn press_key(target: InputTarget, combo: &KeyCombo) -> Result<()> {
    super::background::require_keyboard_focus(target)?;
    let (key, key_flags) = match combo.key {
        Some(key) => {
            let (code, flags) = key_code(key)?;
            (Some(code), flags)
        }
        None => (None, CGEventFlags::empty()),
    };
    let m = combo.modifiers;
    let shift = m.shift || key_flags.contains(CGEventFlags::MaskShift);
    let modifiers = [
        (m.command, KEY_COMMAND, CGEventFlags::MaskCommand),
        (m.control, KEY_CONTROL, CGEventFlags::MaskControl),
        (m.option, KEY_OPTION, CGEventFlags::MaskAlternate),
        (shift, KEY_SHIFT, CGEventFlags::MaskShift),
        (m.function, KEY_FUNCTION, CGEventFlags::MaskSecondaryFn),
    ];
    let held: Vec<(CGKeyCode, CGEventFlags)> = modifiers
        .iter()
        .filter(|(on, _, _)| *on)
        .map(|&(_, code, flag)| (code, flag))
        .collect();

    let mut flags = CGEventFlags::empty();
    for &(code, flag) in &held {
        flags |= flag;
        post_key(target, code, true, flags)?;
    }
    if let Some(code) = key {
        let key_flags = flags | (key_flags - CGEventFlags::MaskShift);
        post_key(target, code, true, key_flags)?;
        post_key(target, code, false, key_flags)?;
    }
    for &(code, flag) in held.iter().rev() {
        flags -= flag;
        post_key(target, code, false, flags)?;
    }
    Ok(())
}

/// Types text as Unicode key events. Newlines and tabs press Return and Tab.
pub fn type_text(target: InputTarget, text: &str) -> Result<()> {
    for typed in typing_events(text) {
        // Typing can move focus, such as Return opening a window, so check
        // before each event rather than once.
        super::background::require_keyboard_focus(target)?;
        match typed {
            Typed::Key(code) => tap(target, code)?,
            Typed::Text(units) => {
                for down in [true, false] {
                    let event =
                        CGEvent::new_keyboard_event(None, 0, down).ok_or_else(event_failed)?;
                    CGEvent::set_flags(Some(&event), CGEventFlags::empty());
                    unsafe {
                        CGEvent::keyboard_set_unicode_string(
                            Some(&event),
                            units.len() as _,
                            units.as_ptr(),
                        );
                    }
                    post(target, &event);
                }
            }
        }
        thread::sleep(TYPE_GAP);
    }
    Ok(())
}

/// One typing event: a run of whole characters, or a key press.
#[derive(Debug, PartialEq)]
enum Typed {
    Text(Vec<u16>),
    Key(CGKeyCode),
}

/// Splits text into typing events. Each text event holds whole characters,
/// as grapheme clusters, so apps never see half an emoji or a combining
/// accent apart from its letter.
fn typing_events(text: &str) -> Vec<Typed> {
    let mut events = Vec::new();
    let mut chunk = Vec::new();
    for grapheme in text.graphemes(true) {
        let key = match grapheme {
            "\r\n" | "\n" | "\r" => Some(KEY_RETURN),
            "\t" => Some(KEY_TAB),
            _ => None,
        };
        let units: Vec<u16> = grapheme.encode_utf16().collect();
        let full = chunk.len() + units.len() > TYPE_CHUNK_UNITS;
        if (key.is_some() || full) && !chunk.is_empty() {
            events.push(Typed::Text(std::mem::take(&mut chunk)));
        }
        match key {
            Some(code) => events.push(Typed::Key(code)),
            None => chunk.extend(units),
        }
    }
    if !chunk.is_empty() {
        events.push(Typed::Text(chunk));
    }
    events
}

/// Reports whether natural scrolling is on, which is the system default.
fn natural_scrolling() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("com.apple.swipescrolldirection");
    defaults.objectForKey(&key).is_none() || defaults.boolForKey(&key)
}

fn tap(target: InputTarget, code: CGKeyCode) -> Result<()> {
    post_key(target, code, true, CGEventFlags::empty())?;
    post_key(target, code, false, CGEventFlags::empty())
}

fn post_key(target: InputTarget, code: CGKeyCode, down: bool, flags: CGEventFlags) -> Result<()> {
    let event = CGEvent::new_keyboard_event(None, code, down).ok_or_else(event_failed)?;
    CGEvent::set_flags(Some(&event), flags);
    post(target, &event);
    thread::sleep(EVENT_GAP);
    Ok(())
}

fn move_to(target: InputTarget, at: Point) -> Result<()> {
    post(
        target,
        &*mouse_event(target, CGEventType::MouseMoved, at, CGMouseButton::Left)?,
    );
    Ok(())
}

fn mouse_event(
    target: InputTarget,
    kind: CGEventType,
    at: Point,
    button: CGMouseButton,
) -> Result<objc2_core_foundation::CFRetained<CGEvent>> {
    validate_point(target, at)?;
    match target {
        InputTarget::Foreground => {
            CGEvent::new_mouse_event(None, kind, cg_point(at), button).ok_or_else(event_failed)
        }
        InputTarget::Background { window, frame, .. } => {
            super::background::mouse_event(window, frame, kind, at, button)
        }
    }
}

/// Reject points outside the addressed window rather than route to a menu or
/// another window in the same process accidentally.
fn validate_point(target: InputTarget, at: Point) -> Result<()> {
    if let InputTarget::Background { frame, .. } = target
        && (!at.x.is_finite()
            || !at.y.is_finite()
            || at.x < frame.x
            || at.y < frame.y
            || at.x >= frame.x + frame.width
            || at.y >= frame.y + frame.height)
    {
        return Err(Error::new(
            ErrorCode::BackgroundUnavailable,
            "background pointer input must be inside the selected window",
        ));
    }
    Ok(())
}

fn post(target: InputTarget, event: &CGEvent) {
    match target {
        InputTarget::Foreground => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event)),
        InputTarget::Background { pid, .. } => CGEvent::post_to_pid(pid, Some(event)),
    }
}

fn cg_point(p: Point) -> CGPoint {
    CGPoint { x: p.x, y: p.y }
}

fn event_failed() -> Error {
    Error::new(ErrorCode::Platform, "creating an input event failed")
}

/// Returns the virtual key code for a key and any flags it needs, such as
/// Shift for `?` on a US layout.
fn key_code(key: Key) -> Result<(CGKeyCode, CGEventFlags)> {
    let nav = CGEventFlags::MaskSecondaryFn;
    let arrow = CGEventFlags::MaskSecondaryFn | CGEventFlags::MaskNumericPad;
    let named = |code, flags| Ok((code, flags));
    match key {
        Key::Named(NamedKey::Enter) => named(KEY_RETURN, CGEventFlags::empty()),
        Key::Named(NamedKey::Tab) => named(KEY_TAB, CGEventFlags::empty()),
        Key::Named(NamedKey::Space) => named(0x31, CGEventFlags::empty()),
        Key::Named(NamedKey::Backspace) => named(0x33, CGEventFlags::empty()),
        Key::Named(NamedKey::Escape) => named(0x35, CGEventFlags::empty()),
        Key::Named(NamedKey::Delete) => named(0x75, nav),
        Key::Named(NamedKey::Home) => named(0x73, nav),
        Key::Named(NamedKey::End) => named(0x77, nav),
        Key::Named(NamedKey::PageUp) => named(0x74, nav),
        Key::Named(NamedKey::PageDown) => named(0x79, nav),
        Key::Named(NamedKey::Left) => named(0x7B, arrow),
        Key::Named(NamedKey::Right) => named(0x7C, arrow),
        Key::Named(NamedKey::Down) => named(0x7D, arrow),
        Key::Named(NamedKey::Up) => named(0x7E, arrow),
        Key::Named(NamedKey::Help) => named(0x72, nav),
        Key::Named(NamedKey::F(n)) => named(function_key(n), nav),
        Key::Named(NamedKey::Keypad(c)) => named(keypad_key(c), CGEventFlags::MaskNumericPad),
        Key::Char(' ') => named(0x31, CGEventFlags::empty()),
        Key::Char(c) => {
            let (code, shift) = layout_key(c).or_else(|| ansi_key(c)).ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidArgument,
                    format!("no key on the current keyboard layout types {c:?}; use type-text"),
                )
            })?;
            let flags = if shift {
                CGEventFlags::MaskShift
            } else {
                CGEventFlags::empty()
            };
            Ok((code, flags))
        }
    }
}

fn function_key(n: u8) -> CGKeyCode {
    const CODES: [CGKeyCode; 20] = [
        0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F, 0x69, 0x6B, 0x71,
        0x6A, 0x40, 0x4F, 0x50, 0x5A,
    ];
    CODES[usize::from(n - 1)]
}

/// Returns the virtual key code of a keypad key, by the character it types.
fn keypad_key(c: char) -> CGKeyCode {
    match c {
        '.' => 0x41,
        '*' => 0x43,
        '+' => 0x45,
        '/' => 0x4B,
        '\n' => 0x4C,
        '-' => 0x4E,
        '=' => 0x51,
        '8' => 0x5B,
        '9' => 0x5C,
        // 0 through 7 are consecutive.
        d => 0x52 + d.to_digit(10).map_or(0, |n| n as CGKeyCode),
    }
}

/// Looks up a character on the US ANSI layout.
fn ansi_key(c: char) -> Option<(CGKeyCode, bool)> {
    const UNSHIFTED: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0\0`";
    const SHIFTED: &str = "ASDFHGZXCV\0BQWERYT!@#$^%+(&_*)}OU{IP\0LJ\"K:|<?NM>\0\0~";
    if let Some(i) = UNSHIFTED.chars().position(|u| u == c && u != '\0') {
        return Some((i as CGKeyCode, false));
    }
    SHIFTED
        .chars()
        .position(|s| s == c && s != '\0')
        .map(|i| (i as CGKeyCode, true))
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, key: *const c_void) -> *const c_void;
    fn LMGetKbdType() -> u8;
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// Looks up a character on the user's keyboard layout, so shortcuts such as
/// cmd+z press the right key on AZERTY or Dvorak.
fn layout_key(c: char) -> Option<(CGKeyCode, bool)> {
    const SHIFT_STATE: u32 = 0x02;
    const NO_DEAD_KEYS: u32 = 1;
    let map = unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return None;
        }
        let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
        let mut map: HashMap<char, (CGKeyCode, bool)> = HashMap::new();
        if !data.is_null() {
            let layout = (*data.cast::<CFData>()).byte_ptr().cast::<c_void>();
            let kbd_type = u32::from(LMGetKbdType());
            for shift in [false, true] {
                for code in 0..0x80u16 {
                    let mut dead = 0u32;
                    let mut len = 0usize;
                    let mut buf = [0u16; 4];
                    let status = UCKeyTranslate(
                        layout,
                        code,
                        0,
                        if shift { SHIFT_STATE } else { 0 },
                        kbd_type,
                        NO_DEAD_KEYS,
                        &mut dead,
                        buf.len(),
                        &mut len,
                        buf.as_mut_ptr(),
                    );
                    if status != 0 || len == 0 {
                        continue;
                    }
                    let mut decoded = char::decode_utf16(buf[..len].iter().copied());
                    if let (Some(Ok(ch)), None) = (decoded.next(), decoded.next())
                        && !ch.is_control()
                    {
                        map.entry(ch).or_insert((code, shift));
                    }
                }
            }
        }
        CFRelease(source);
        map
    };
    map.get(&c)
        .or_else(|| map.get(&c.to_ascii_lowercase()))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_points_stay_inside_the_addressed_window() {
        let target = InputTarget::Background {
            pid: 1,
            window: 1,
            frame: crate::model::Rect {
                x: -500.0,
                y: 200.0,
                width: 300.0,
                height: 400.0,
            },
        };
        let cases = [
            (
                Point {
                    x: -500.0,
                    y: 200.0,
                },
                true,
            ),
            (
                Point {
                    x: -200.1,
                    y: 599.9,
                },
                true,
            ),
            (
                Point {
                    x: -200.0,
                    y: 300.0,
                },
                false,
            ),
            (
                Point {
                    x: -300.0,
                    y: 600.0,
                },
                false,
            ),
            (
                Point {
                    x: -501.0,
                    y: 300.0,
                },
                false,
            ),
            (
                Point {
                    x: -300.0,
                    y: 199.0,
                },
                false,
            ),
            (
                Point {
                    x: f64::NAN,
                    y: 300.0,
                },
                false,
            ),
            (
                Point {
                    x: -300.0,
                    y: f64::INFINITY,
                },
                false,
            ),
        ];
        for (point, want) in cases {
            assert_eq!(validate_point(target, point).is_ok(), want, "{point:?}");
        }
    }

    #[test]
    fn ansi_lookup() {
        let cases = [
            ('a', Some((0x00, false))),
            ('c', Some((0x08, false))),
            ('z', Some((0x06, false))),
            ('1', Some((0x12, false))),
            ('/', Some((0x2C, false))),
            ('?', Some((0x2C, true))),
            ('`', Some((0x32, false))),
            ('é', None),
        ];
        for (c, want) in cases {
            assert_eq!(ansi_key(c), want, "{c:?}");
        }
    }

    #[test]
    fn typing_keeps_characters_whole() {
        let utf16 = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        // One character of 26 UTF-16 units: a letter with 25 accents.
        let long = format!("Z{}", "\u{301}".repeat(25));
        let cases = [
            (
                "newlines and tabs press keys",
                "Troy\r\nAbed\tAnnie\n",
                vec![
                    Typed::Text(utf16("Troy")),
                    Typed::Key(KEY_RETURN),
                    Typed::Text(utf16("Abed")),
                    Typed::Key(KEY_TAB),
                    Typed::Text(utf16("Annie")),
                    Typed::Key(KEY_RETURN),
                ],
            ),
            (
                "long text splits between characters",
                "Greendale Community College",
                vec![
                    Typed::Text(utf16("Greendale Community ")),
                    Typed::Text(utf16("College")),
                ],
            ),
            (
                "an emoji at the boundary stays whole",
                "Greendale College 👩🏽‍💻",
                vec![
                    Typed::Text(utf16("Greendale College ")),
                    Typed::Text(utf16("👩🏽‍💻")),
                ],
            ),
            (
                "a combining accent stays with its letter",
                "Greendale Communitye\u{301}",
                vec![
                    Typed::Text(utf16("Greendale Community")),
                    Typed::Text(utf16("e\u{301}")),
                ],
            ),
            (
                "a character longer than a chunk goes alone",
                &format!("a{long}b"),
                vec![
                    Typed::Text(utf16("a")),
                    Typed::Text(utf16(&long)),
                    Typed::Text(utf16("b")),
                ],
            ),
        ];
        for (name, text, want) in cases {
            assert_eq!(typing_events(text), want, "{name}");
        }
    }

    #[test]
    fn function_keys() {
        assert_eq!(function_key(1), 0x7A);
        assert_eq!(function_key(12), 0x6F);
        assert_eq!(function_key(20), 0x5A);
    }

    #[test]
    fn keypad_keys() {
        let cases = [
            ('0', 0x52),
            ('7', 0x59),
            ('8', 0x5B),
            ('9', 0x5C),
            ('\n', 0x4C),
            ('+', 0x45),
        ];
        for (c, want) in cases {
            assert_eq!(keypad_key(c), want, "{c:?}");
        }
    }
}
