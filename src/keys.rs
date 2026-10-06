use crate::error::{Error, ErrorCode, Result};

/// Modifier keys held during a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub command: bool,
    pub control: bool,
    pub option: bool,
    pub shift: bool,
    pub function: bool,
}

/// Keys that do not produce a printable character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedKey {
    Enter,
    Tab,
    Space,
    Backspace,
    Delete,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// The Help key, where PC keyboards have Insert.
    Help,
    F(u8),
    /// A numeric keypad key, by the character it types; `'\n'` is the
    /// keypad Enter key.
    Keypad(char),
}

/// A single key, either named or identified by the character it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Named(NamedKey),
    Char(char),
}

/// A key with modifiers, such as `cmd+shift+t`. `key` is `None` when only
/// modifiers are pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyCombo {
    pub modifiers: Modifiers,
    pub key: Option<Key>,
}

impl KeyCombo {
    /// Parses a combo such as `cmd+shift+t`, `enter`, or `ctrl++`.
    ///
    /// Letters are case-insensitive; add `shift` explicitly when a shortcut
    /// needs it.
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            return Err(invalid(input, "empty key"));
        }
        let (mods, last) = if s == "+" {
            ("", "+")
        } else if let Some(rest) = s.strip_suffix("++") {
            (rest, "+")
        } else {
            s.rsplit_once('+').unwrap_or(("", s))
        };

        let mut modifiers = Modifiers::default();
        for name in mods.split('+').filter(|m| !m.is_empty()) {
            if !set_modifier(&mut modifiers, name) {
                return Err(invalid(input, &format!("unknown modifier {name:?}")));
            }
        }
        if set_modifier(&mut modifiers, last) {
            return Ok(Self {
                modifiers,
                key: None,
            });
        }
        let key =
            parse_key(last).ok_or_else(|| invalid(input, &format!("unknown key {last:?}")))?;
        Ok(Self {
            modifiers,
            key: Some(key),
        })
    }
}

fn set_modifier(m: &mut Modifiers, name: &str) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "cmd" | "command" | "meta" | "super" | "win" => m.command = true,
        "ctrl" | "control" => m.control = true,
        "alt" | "option" | "opt" => m.option = true,
        "shift" => m.shift = true,
        "fn" => m.function = true,
        _ => return false,
    }
    true
}

fn parse_key(name: &str) -> Option<Key> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Key::Char(c.to_ascii_lowercase()));
    }
    let lower = name.to_ascii_lowercase();
    let named = match lower.as_str() {
        "enter" | "return" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "space" => NamedKey::Space,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" | "forwarddelete" => NamedKey::Delete,
        "escape" | "esc" => NamedKey::Escape,
        "up" | "arrowup" => NamedKey::Up,
        "down" | "arrowdown" => NamedKey::Down,
        "left" | "arrowleft" => NamedKey::Left,
        "right" | "arrowright" => NamedKey::Right,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        // Underscored names and Prior and Next follow xdotool, which many
        // models use.
        "pageup" | "page_up" | "prior" => NamedKey::PageUp,
        "pagedown" | "page_down" | "next" => NamedKey::PageDown,
        "help" | "insert" => NamedKey::Help,
        "plus" => return Some(Key::Char('+')),
        "minus" => return Some(Key::Char('-')),
        _ if lower.starts_with("kp_") => NamedKey::Keypad(keypad_char(&lower[3..])?),
        _ => {
            let n: u8 = lower.strip_prefix('f')?.parse().ok()?;
            if !(1..=20).contains(&n) {
                return None;
            }
            NamedKey::F(n)
        }
    };
    Some(Key::Named(named))
}

/// Maps an xdotool keypad name without its `KP_` prefix, such as `7` or
/// `add`, to the character the key types.
fn keypad_char(name: &str) -> Option<char> {
    let c = match name {
        "enter" => '\n',
        "add" | "plus" => '+',
        "subtract" | "minus" => '-',
        "multiply" => '*',
        "divide" => '/',
        "decimal" => '.',
        "equal" => '=',
        digit => {
            let mut chars = digit.chars();
            match (chars.next(), chars.next()) {
                (Some(d @ '0'..='9'), None) => d,
                _ => return None,
            }
        }
    };
    Some(c)
}

fn invalid(input: &str, reason: &str) -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        format!("invalid key combo {input:?}: {reason}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(command: bool, control: bool, option: bool, shift: bool) -> Modifiers {
        Modifiers {
            command,
            control,
            option,
            shift,
            function: false,
        }
    }

    #[test]
    fn parse_valid() {
        let none = Modifiers::default();
        let cases = [
            ("enter", none, Some(Key::Named(NamedKey::Enter))),
            ("Return", none, Some(Key::Named(NamedKey::Enter))),
            ("a", none, Some(Key::Char('a'))),
            (
                "cmd+C",
                mods(true, false, false, false),
                Some(Key::Char('c')),
            ),
            (
                "cmd+shift+t",
                mods(true, false, false, true),
                Some(Key::Char('t')),
            ),
            (
                "ctrl+alt+delete",
                mods(false, true, true, false),
                Some(Key::Named(NamedKey::Delete)),
            ),
            (
                "cmd++",
                mods(true, false, false, false),
                Some(Key::Char('+')),
            ),
            ("+", none, Some(Key::Char('+'))),
            (
                "option+f12",
                mods(false, false, true, false),
                Some(Key::Named(NamedKey::F(12))),
            ),
            ("shift", mods(false, false, false, true), None),
            (
                "cmd+space",
                mods(true, false, false, false),
                Some(Key::Named(NamedKey::Space)),
            ),
            ("Page_Up", none, Some(Key::Named(NamedKey::PageUp))),
            ("Next", none, Some(Key::Named(NamedKey::PageDown))),
            ("Insert", none, Some(Key::Named(NamedKey::Help))),
            ("KP_7", none, Some(Key::Named(NamedKey::Keypad('7')))),
            ("KP_Enter", none, Some(Key::Named(NamedKey::Keypad('\n')))),
            (
                "ctrl+KP_Add",
                mods(false, true, false, false),
                Some(Key::Named(NamedKey::Keypad('+'))),
            ),
        ];
        for (input, want_mods, want_key) in cases {
            let got = KeyCombo::parse(input).unwrap_or_else(|e| panic!("{input}: {e}"));
            assert_eq!(got.modifiers, want_mods, "{input}");
            assert_eq!(got.key, want_key, "{input}");
        }
    }

    #[test]
    fn parse_invalid() {
        for input in [
            "",
            "hyper+a",
            "f21",
            "enterr",
            "cmd+shift+nope",
            "KP_10",
            "KP_",
        ] {
            assert!(KeyCombo::parse(input).is_err(), "{input} should fail");
        }
    }
}
