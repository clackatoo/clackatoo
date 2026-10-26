//! Keys typed on a phone, as the bytes a terminal's program reads: the
//! encoding a window uses (xterm's, see the window's `input.rs`), for the
//! keys a phone has. Printable keys send their text, Ctrl with a letter
//! its control code, Alt an Escape first, and named keys their escape
//! sequences, arrows as the program's cursor mode asks.

use clackatoo_protocol::InputModes;

/// A key, from the phone's keyboard, a keyboard attached to it, or the
/// row of keys above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Text typed: a character, or more at once, as dictation types.
    Text(String),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// A function key, F1 to F12.
    F(u8),
}

impl Key {
    /// The key a browser's `KeyboardEvent.key` names, if it's one that
    /// doesn't type text, or text typed with Ctrl or Alt held, which the
    /// keyboard's text events don't carry.
    #[must_use]
    pub fn named(key: &str) -> Option<Self> {
        Some(match key {
            "Enter" => Self::Enter,
            "Tab" => Self::Tab,
            "Backspace" => Self::Backspace,
            "Escape" | "Esc" => Self::Escape,
            "ArrowUp" | "Up" => Self::Up,
            "ArrowDown" | "Down" => Self::Down,
            "ArrowRight" | "Right" => Self::Right,
            "ArrowLeft" | "Left" => Self::Left,
            "Home" => Self::Home,
            "End" => Self::End,
            "Insert" => Self::Insert,
            "Delete" | "Del" => Self::Delete,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            function => {
                let number: u8 = function.strip_prefix('F')?.parse().ok()?;
                if !(1..=12).contains(&number) {
                    return None;
                }
                Self::F(number)
            }
        })
    }
}

/// The modifier keys held, or tapped on the row of keys, with a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools, reason = "each held or not")]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    /// xterm's modifier parameter: 1, plus 1 for Shift, 2 for Alt and 4
    /// for Ctrl, or `None` with none held, as sequences then leave it out.
    fn parameter(self) -> Option<u8> {
        let bits = u8::from(self.shift) | u8::from(self.alt) << 1 | u8::from(self.ctrl) << 2;
        (bits != 0).then_some(bits + 1)
    }
}

/// What `key`, with `modifiers` held, sends to a program in `modes`.
#[must_use]
pub fn encode(key: &Key, modifiers: Modifiers, modes: InputModes) -> Vec<u8> {
    let mut out = Vec::new();
    let parameter = modifiers.parameter();
    let alt = |out: &mut Vec<u8>| {
        if modifiers.alt {
            out.push(0x1b);
        }
    };
    match key {
        Key::Text(text) => {
            alt(&mut out);
            let text = straight(text);
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) if modifiers.ctrl => match control(ch) {
                    Some(byte) => out.push(byte),
                    // Ctrl with a key that has no control code types the key.
                    None => out.extend_from_slice(text.as_bytes()),
                },
                // Text typed on a phone ends lines with Enter, as typing does.
                _ => out.extend_from_slice(text.replace('\n', "\r").as_bytes()),
            }
        }
        Key::Enter => {
            alt(&mut out);
            out.push(b'\r');
        }
        Key::Tab if modifiers.shift => out.extend_from_slice(b"\x1b[Z"),
        Key::Tab => {
            alt(&mut out);
            out.push(b'\t');
        }
        Key::Backspace => {
            alt(&mut out);
            // Ctrl+Backspace sends ^H, which shells bind to deleting a word.
            out.push(if modifiers.ctrl { 0x08 } else { 0x7f });
        }
        Key::Escape => {
            alt(&mut out);
            out.push(0x1b);
        }
        Key::Up => cursor_key(b'A', parameter, modes, &mut out),
        Key::Down => cursor_key(b'B', parameter, modes, &mut out),
        Key::Right => cursor_key(b'C', parameter, modes, &mut out),
        Key::Left => cursor_key(b'D', parameter, modes, &mut out),
        Key::Home => cursor_key(b'H', parameter, modes, &mut out),
        Key::End => cursor_key(b'F', parameter, modes, &mut out),
        Key::Insert => tilde_key(2, parameter, &mut out),
        Key::Delete => tilde_key(3, parameter, &mut out),
        Key::PageUp => tilde_key(5, parameter, &mut out),
        Key::PageDown => tilde_key(6, parameter, &mut out),
        Key::F(number @ 1..=4) => {
            let letter = b'P' + (number - 1);
            match parameter {
                Some(parameter) => {
                    out.extend_from_slice(format!("\x1b[1;{parameter}").as_bytes());
                    out.push(letter);
                }
                None => out.extend_from_slice(&[0x1b, b'O', letter]),
            }
        }
        Key::F(number) => {
            let code = match number {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            tilde_key(code, parameter, &mut out);
        }
    }
    out
}

/// What pasting `text` sends to a program in `modes`: as a window
/// pastes it (see `clackatoo_term::paste`), marked as a paste if the
/// program asked, and then unable to end itself early.
#[must_use]
pub fn paste(text: &str, modes: InputModes) -> Vec<u8> {
    clackatoo_term::paste(text, modes)
}

/// `text` with the curly quotes a phone's keyboard may put in place of
/// straight ones made straight again: a shell reads only straight ones.
fn straight(text: &str) -> String {
    text.replace(['\u{2018}', '\u{2019}'], "'")
        .replace(['\u{201C}', '\u{201D}'], "\"")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    fn text(text: &str) -> Key {
        Key::Text(text.to_owned())
    }

    #[test]
    fn ctrl_with_a_letter_sends_its_control_code() {
        let modes = InputModes::default();
        assert_eq!(encode(&text("c"), ctrl(), modes), [0x03]);
        assert_eq!(encode(&text("a"), ctrl(), modes), [0x01]);
        assert_eq!(encode(&text("z"), ctrl(), modes), [0x1a]);
        // A phone's keyboard may have Shift on: Ctrl+C all the same.
        assert_eq!(encode(&text("C"), ctrl(), modes), [0x03]);
        assert_eq!(encode(&text("["), ctrl(), modes), [0x1b]);
        assert_eq!(encode(&text(" "), ctrl(), modes), [0x00]);
        assert_eq!(encode(&text("/"), ctrl(), modes), [0x1f]);
        assert_eq!(encode(&text("é"), ctrl(), modes), "é".as_bytes());
        assert_eq!(encode(&text("c"), Modifiers::default(), modes), b"c");
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(encode(&text("b"), alt, modes), b"\x1bb");
    }

    #[test]
    fn named_keys_send_their_sequences() {
        let modes = InputModes::default();
        let none = Modifiers::default();
        assert_eq!(encode(&Key::Escape, none, modes), [0x1b]);
        assert_eq!(encode(&Key::Tab, none, modes), b"\t");
        assert_eq!(encode(&Key::Enter, none, modes), b"\r");
        assert_eq!(encode(&Key::Backspace, none, modes), [0x7f]);
        assert_eq!(encode(&Key::Delete, none, modes), b"\x1b[3~");
        assert_eq!(encode(&Key::PageUp, none, modes), b"\x1b[5~");
        assert_eq!(encode(&Key::F(1), none, modes), b"\x1bOP");
        assert_eq!(encode(&Key::F(12), none, modes), b"\x1b[24~");
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(encode(&Key::Tab, shift, modes), b"\x1b[Z");
    }

    #[test]
    fn arrows_follow_the_programs_cursor_mode() {
        let normal = InputModes::default();
        let application = InputModes {
            app_cursor_keys: true,
            ..InputModes::default()
        };
        let none = Modifiers::default();
        for (key, letter) in [
            (Key::Up, b'A'),
            (Key::Down, b'B'),
            (Key::Right, b'C'),
            (Key::Left, b'D'),
        ] {
            assert_eq!(encode(&key, none, normal), [0x1b, b'[', letter]);
            assert_eq!(encode(&key, none, application), [0x1b, b'O', letter]);
            // With Ctrl, the modifier's form, in either mode.
            let held = [b'\x1b', b'[', b'1', b';', b'5', letter];
            assert_eq!(encode(&key, ctrl(), normal), held);
            assert_eq!(encode(&key, ctrl(), application), held);
        }
    }

    #[test]
    fn keys_are_named_as_browsers_name_them() {
        assert_eq!(Key::named("ArrowLeft"), Some(Key::Left));
        assert_eq!(Key::named("Escape"), Some(Key::Escape));
        assert_eq!(Key::named("F5"), Some(Key::F(5)));
        assert_eq!(Key::named("F13"), None);
        assert_eq!(Key::named("a"), None);
        assert_eq!(Key::named("Shift"), None);
    }

    #[test]
    fn curly_quotes_go_straight_and_newlines_as_enter() {
        let modes = InputModes::default();
        let none = Modifiers::default();
        assert_eq!(
            encode(
                &text("echo \u{201C}hi\u{201D} \u{2018}x\u{2019}"),
                none,
                modes
            ),
            b"echo \"hi\" 'x'"
        );
        assert_eq!(encode(&text("a\nb"), none, modes), b"a\rb");
    }

    #[test]
    fn pastes_are_marked_when_the_program_asks() {
        let plain = InputModes::default();
        // The newline ending it is dropped, so it runs nothing as it lands.
        assert_eq!(paste("ls\nexit\r\n", plain), b"ls\rexit");
        let bracketed = InputModes {
            bracketed_paste: true,
            ..InputModes::default()
        };
        assert_eq!(
            paste("a\n\x1b[201~b", bracketed),
            b"\x1b[200~a\n[201~b\x1b[201~"
        );
        // A terminator hidden inside another is no way out either.
        assert_eq!(
            paste("\x1b[20\x1b[201~1~; cmd\n", bracketed),
            b"\x1b[200~[20[201~1~; cmd\n\x1b[201~"
        );
    }
}
