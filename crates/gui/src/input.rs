//! Turning key presses into the bytes a terminal program reads.
//!
//! This is xterm's encoding, which nearly every program understands:
//! printable keys send their text, control keys send C0 bytes, Alt adds an
//! Escape before the key, and named keys (arrows, function keys…) send
//! escape sequences, with modifiers as a parameter.

use clackatoo_protocol::InputModes;
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey};

/// The modifier keys held during a key press.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Modifiers {
    pub(crate) shift: bool,
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
}

impl Modifiers {
    /// xterm's modifier parameter: 1, plus 1 for Shift, 2 for Alt and 4 for
    /// Ctrl. `None` when no modifier is held, since sequences then leave the
    /// parameter out.
    fn parameter(self) -> Option<u8> {
        let bits = u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.ctrl) << 2;
        (bits != 0).then_some(bits + 1)
    }
}

/// One key press, as the window saw it.
#[derive(Clone, Debug)]
pub(crate) struct KeyPress<'a> {
    /// The key, with Shift and the keyboard layout applied, but not Ctrl or
    /// Alt.
    pub(crate) key: &'a Key,
    /// The text the key types, if any, after dead keys and compose.
    pub(crate) text: Option<&'a str>,
    pub(crate) location: KeyLocation,
    /// Where the key is on the keyboard, whatever the layout, if known.
    pub(crate) code: Option<KeyCode>,
    pub(crate) modifiers: Modifiers,
}

/// A key press kept for later, as one waiting for the rest of a binding.
#[derive(Clone, Debug)]
pub(crate) struct KeptPress {
    key: Key,
    text: Option<String>,
    location: KeyLocation,
    code: Option<KeyCode>,
    modifiers: Modifiers,
}

impl KeptPress {
    pub(crate) fn of(press: &KeyPress<'_>) -> Self {
        Self {
            key: press.key.clone(),
            text: press.text.map(str::to_owned),
            location: press.location,
            code: press.code,
            modifiers: press.modifiers,
        }
    }

    pub(crate) fn press(&self) -> KeyPress<'_> {
        KeyPress {
            key: &self.key,
            text: self.text.as_deref(),
            location: self.location,
            code: self.code,
            modifiers: self.modifiers,
        }
    }
}

/// Whether `key` is only a modifier, such as Shift or Ctrl, which programs
/// are never sent on its own.
pub(crate) fn is_modifier(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::AltGraph
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::Fn
        )
    )
}

/// Appends what `press` sends to the program to `out`. Keys that send
/// nothing, such as a modifier pressed alone, append nothing.
pub(crate) fn encode(press: &KeyPress<'_>, modes: InputModes, out: &mut Vec<u8>) {
    let modifiers = press.modifiers;
    if press.location == KeyLocation::Numpad
        && modes.app_keypad
        && modifiers == Modifiers::default()
        && let Some(byte) = keypad_application(press.key)
    {
        out.extend_from_slice(&[0x1b, b'O', byte]);
        return;
    }
    match press.key {
        Key::Named(named) => encode_named(*named, modifiers, modes, out),
        Key::Character(text) => {
            let start = out.len();
            if modifiers.alt {
                out.push(0x1b);
            }
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if modifiers.ctrl => match control(ch) {
                    Some(byte) => out.push(byte),
                    // Ctrl with a key that has no control code types the key.
                    None => push_str(out, text),
                },
                _ => push_str(out, press.text.filter(|_| !modifiers.alt).unwrap_or(text)),
            }
            if out.len() == start + usize::from(modifiers.alt) {
                out.truncate(start);
            }
        }
        _ => {}
    }
}

fn encode_named(key: NamedKey, modifiers: Modifiers, modes: InputModes, out: &mut Vec<u8>) {
    let parameter = modifiers.parameter();
    let alt_prefix = |out: &mut Vec<u8>| {
        if modifiers.alt {
            out.push(0x1b);
        }
    };
    match key {
        NamedKey::Enter => {
            alt_prefix(out);
            out.push(b'\r');
        }
        NamedKey::Tab if modifiers.shift => out.extend_from_slice(b"\x1b[Z"),
        NamedKey::Tab => {
            alt_prefix(out);
            out.push(b'\t');
        }
        NamedKey::Backspace => {
            alt_prefix(out);
            // Ctrl+Backspace sends ^H, which shells bind to deleting a word.
            out.push(if modifiers.ctrl { 0x08 } else { 0x7f });
        }
        NamedKey::Escape => {
            alt_prefix(out);
            out.push(0x1b);
        }
        NamedKey::Space => {
            alt_prefix(out);
            out.push(if modifiers.ctrl { 0 } else { b' ' });
        }
        NamedKey::ArrowUp => cursor_key(b'A', parameter, modes, out),
        NamedKey::ArrowDown => cursor_key(b'B', parameter, modes, out),
        NamedKey::ArrowRight => cursor_key(b'C', parameter, modes, out),
        NamedKey::ArrowLeft => cursor_key(b'D', parameter, modes, out),
        NamedKey::Home => cursor_key(b'H', parameter, modes, out),
        NamedKey::End => cursor_key(b'F', parameter, modes, out),
        NamedKey::Insert => tilde_key(2, parameter, out),
        NamedKey::Delete => tilde_key(3, parameter, out),
        NamedKey::PageUp => tilde_key(5, parameter, out),
        NamedKey::PageDown => tilde_key(6, parameter, out),
        NamedKey::F1 => function_key(b'P', parameter, out),
        NamedKey::F2 => function_key(b'Q', parameter, out),
        NamedKey::F3 => function_key(b'R', parameter, out),
        NamedKey::F4 => function_key(b'S', parameter, out),
        NamedKey::F5 => tilde_key(15, parameter, out),
        NamedKey::F6 => tilde_key(17, parameter, out),
        NamedKey::F7 => tilde_key(18, parameter, out),
        NamedKey::F8 => tilde_key(19, parameter, out),
        NamedKey::F9 => tilde_key(20, parameter, out),
        NamedKey::F10 => tilde_key(21, parameter, out),
        NamedKey::F11 => tilde_key(23, parameter, out),
        NamedKey::F12 => tilde_key(24, parameter, out),
        _ => {}
    }
}

/// Arrows, Home and End: `CSI A`, or `SS3 A` in application cursor mode,
/// or `CSI 1;m A` with modifiers.
fn cursor_key(letter: u8, parameter: Option<u8>, modes: InputModes, out: &mut Vec<u8>) {
    match parameter {
        Some(parameter) => {
            out.extend_from_slice(format!("\x1b[1;{parameter}").as_bytes());
            out.push(letter);
        }
        None if modes.app_cursor_keys => out.extend_from_slice(&[0x1b, b'O', letter]),
        None => out.extend_from_slice(&[0x1b, b'[', letter]),
    }
}

/// Keys sent as `CSI n ~`, or `CSI n;m ~` with modifiers.
fn tilde_key(number: u8, parameter: Option<u8>, out: &mut Vec<u8>) {
    let sequence = match parameter {
        Some(parameter) => format!("\x1b[{number};{parameter}~"),
        None => format!("\x1b[{number}~"),
    };
    out.extend_from_slice(sequence.as_bytes());
}

/// F1 to F4: `SS3 P`, or `CSI 1;m P` with modifiers.
fn function_key(letter: u8, parameter: Option<u8>, out: &mut Vec<u8>) {
    match parameter {
        Some(parameter) => {
            out.extend_from_slice(format!("\x1b[1;{parameter}").as_bytes());
            out.push(letter);
        }
        None => out.extend_from_slice(&[0x1b, b'O', letter]),
    }
}

/// The C0 control code Ctrl plus `ch` sends, as xterm assigns them.
fn control(ch: char) -> Option<u8> {
    let byte = u8::try_from(ch).ok()?;
    match byte {
        b'a'..=b'z' => Some(byte - b'a' + 1),
        b'@'..=b'_' => Some(byte - b'@'),
        b' ' | b'2' => Some(0),
        b'3' => Some(0x1b),
        b'4' => Some(0x1c),
        b'5' => Some(0x1d),
        b'6' => Some(0x1e),
        b'7' | b'/' | b'-' => Some(0x1f),
        b'8' | b'?' => Some(0x7f),
        _ => None,
    }
}

/// The final byte of a keypad key's application sequence, `SS3 x`.
fn keypad_application(key: &Key) -> Option<u8> {
    match key {
        Key::Named(NamedKey::Enter) => Some(b'M'),
        Key::Character(text) => match text.as_str() {
            "*" => Some(b'j'),
            "+" => Some(b'k'),
            "," => Some(b'l'),
            "-" => Some(b'm'),
            "." => Some(b'n'),
            "/" => Some(b'o'),
            "=" => Some(b'X'),
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                Some(b'p' + (digit.as_bytes()[0] - b'0'))
            }
            _ => None,
        },
        _ => None,
    }
}

fn push_str(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(text.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(key: Key, modifiers: Modifiers) -> Vec<u8> {
        press_in(&key, modifiers, InputModes::default())
    }

    fn press_in(key: &Key, modifiers: Modifiers, modes: InputModes) -> Vec<u8> {
        let text = match key {
            Key::Character(text) => Some(text.to_string()),
            _ => None,
        };
        let mut out = Vec::new();
        encode(
            &KeyPress {
                key,
                text: text.as_deref(),
                location: KeyLocation::Standard,
                code: None,
                modifiers,
            },
            modes,
            &mut out,
        );
        out
    }

    fn ch(text: &str) -> Key {
        Key::Character(text.into())
    }

    const CTRL: Modifiers = Modifiers {
        shift: false,
        ctrl: true,
        alt: false,
    };
    const ALT: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: true,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ctrl: false,
        alt: false,
    };
    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
    };

    #[test]
    fn text_types_itself() {
        assert_eq!(press(ch("a"), NONE), b"a");
        assert_eq!(press(ch("é"), NONE), "é".as_bytes());
        assert_eq!(press(ch("A"), SHIFT), b"A");
    }

    #[test]
    fn ctrl_sends_control_codes() {
        assert_eq!(press(ch("c"), CTRL), [3]);
        assert_eq!(press(ch("["), CTRL), [0x1b]);
        assert_eq!(press(ch("\\"), CTRL), [0x1c]);
        assert_eq!(press(Key::Named(NamedKey::Space), CTRL), [0]);
        assert_eq!(press(Key::Named(NamedKey::Backspace), CTRL), [8]);
    }

    #[test]
    fn alt_adds_an_escape() {
        assert_eq!(press(ch("b"), ALT), b"\x1bb");
        let ctrl_alt = Modifiers { alt: true, ..CTRL };
        assert_eq!(press(ch("c"), ctrl_alt), b"\x1b\x03");
        assert_eq!(press(Key::Named(NamedKey::Backspace), ALT), b"\x1b\x7f");
    }

    #[test]
    fn named_keys() {
        assert_eq!(press(Key::Named(NamedKey::Enter), NONE), b"\r");
        assert_eq!(press(Key::Named(NamedKey::Backspace), NONE), b"\x7f");
        assert_eq!(press(Key::Named(NamedKey::Tab), SHIFT), b"\x1b[Z");
        assert_eq!(press(Key::Named(NamedKey::Delete), NONE), b"\x1b[3~");
        assert_eq!(press(Key::Named(NamedKey::F1), NONE), b"\x1bOP");
        assert_eq!(press(Key::Named(NamedKey::F5), NONE), b"\x1b[15~");
        assert_eq!(press(Key::Named(NamedKey::F12), CTRL), b"\x1b[24;5~");
        assert_eq!(press(Key::Named(NamedKey::Shift), SHIFT), b"");
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let app = InputModes {
            app_cursor_keys: true,
            ..InputModes::default()
        };
        let up = || Key::Named(NamedKey::ArrowUp);
        assert_eq!(press(up(), NONE), b"\x1b[A");
        assert_eq!(press_in(&up(), NONE, app), b"\x1bOA");
        // Modifiers always use the CSI form.
        assert_eq!(press_in(&up(), CTRL, app), b"\x1b[1;5A");
        assert_eq!(
            press(
                Key::Named(NamedKey::ArrowLeft),
                Modifiers { alt: true, ..SHIFT }
            ),
            b"\x1b[1;4D"
        );
    }

    #[test]
    fn text_an_input_method_committed_is_sent_whole() {
        // The window keys a commit as one key with all its text.
        assert_eq!(press(ch("日本語"), NONE), "日本語".as_bytes());
        assert_eq!(press(ch("👩\u{200D}💻"), NONE), "👩\u{200D}💻".as_bytes());
    }

    #[test]
    fn the_keypad_follows_application_keypad_mode() {
        let app = InputModes {
            app_keypad: true,
            ..InputModes::default()
        };
        let mut out = Vec::new();
        let key = ch("5");
        let press = KeyPress {
            key: &key,
            text: Some("5"),
            location: KeyLocation::Numpad,
            code: None,
            modifiers: NONE,
        };
        encode(&press, app, &mut out);
        assert_eq!(out, b"\x1bOu");
        out.clear();
        encode(&press, InputModes::default(), &mut out);
        assert_eq!(out, b"5");
    }
}
