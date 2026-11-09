//! Keys in the kitty keyboard protocol's encoding
//! (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>), for a program
//! that asked for it (see [`clackatoo_term::keyboard`]).
//!
//! A key is sent as `CSI code[:shifted[:base]] [; modifiers [; text]] u`,
//! or for the keys xterm already sends as `CSI … ~` or `CSI 1 ; m A`, in
//! those forms. With [`DISAMBIGUATE`](clackatoo_term::keyboard::DISAMBIGUATE) alone, only what xterm's encoding
//! can't tell apart changes: Esc, and keys with Ctrl or Alt. Enter, Tab
//! and Backspace with no modifiers stay as they were, so a shell left with
//! the flags on by a program that died can still be typed `reset` into.
//! With [`ALL_KEYS`], every key is a sequence, text and all.
//!
//! What windows don't do yet: key repeats and releases
//! ([`EVENT_TYPES`](clackatoo_term::keyboard::EVENT_TYPES)), which the
//! emulator therefore never keeps; Super, Hyper, Meta, Caps Lock and Num
//! Lock as modifiers; and modifier keys pressed alone. The key Shift
//! makes, and the key in the base layout, are as the keyboard's own
//! layout has them for letters, and as a US layout has them otherwise.

use clackatoo_term::keyboard::{ALL_KEYS, ALTERNATE_KEYS, TEXT};
use winit::keyboard::{Key, KeyCode, KeyLocation, NamedKey};

use super::{KeyPress, Modifiers};

/// What a key is, as the protocol numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Code {
    /// A key that types `char`, numbered by it as typed without Shift.
    Text(char),
    /// A key on the numeric keypad that types `char`, by its number.
    Keypad(u32, char),
    /// Any other key: its number, and the sequence's last character.
    Functional(u32, char),
}

/// Appends what `press` sends to a program that asked for the kitty
/// keyboard protocol's `flags`, which include [`DISAMBIGUATE`](clackatoo_term::keyboard::DISAMBIGUATE) or
/// [`ALL_KEYS`].
pub(super) fn encode(press: &KeyPress<'_>, flags: u8, out: &mut Vec<u8>) {
    let modifiers = press.modifiers;
    let all = flags & ALL_KEYS != 0;
    let Some(code) = code(press) else {
        // Text an input method committed whole, which is sent as it is.
        if let Key::Character(text) = press.key
            && text.chars().nth(1).is_some()
        {
            out.extend_from_slice(text.as_bytes());
        }
        return;
    };
    let value = modifier_value(modifiers);
    match code {
        Code::Text(ch) if !all => {
            // Text with no modifiers but Shift is just text, as before.
            if !modifiers.ctrl && !modifiers.alt {
                let text = typed(press).unwrap_or_else(|| ch.to_string());
                out.extend_from_slice(text.as_bytes());
                return;
            }
            sequence(
                out,
                u32::from(ch),
                alternates(press, ch, flags),
                value,
                None,
                'u',
            );
        }
        Code::Text(ch) => {
            let text = (flags & TEXT != 0).then(|| typed(press)).flatten();
            let alternates = alternates(press, ch, flags);
            sequence(out, u32::from(ch), alternates, value, text.as_deref(), 'u');
        }
        // The keypad's text is text, as the rest of the keyboard's.
        Code::Keypad(_, ch) if !all && !modifiers.ctrl && !modifiers.alt => {
            let text = typed(press).unwrap_or_else(|| ch.to_string());
            out.extend_from_slice(text.as_bytes());
        }
        Code::Keypad(number, _) => {
            let text = (all && flags & TEXT != 0).then(|| typed(press)).flatten();
            sequence(out, number, (None, None), value, text.as_deref(), 'u');
        }
        Code::Functional(number, trailer) => {
            if !all && value == 1 {
                // Kept as they were, as the module's documentation says.
                // By its number and `u`: F3 is 13 too, as `CSI 13 ~`.
                let kept: &[u8] = match (number, trailer) {
                    // Enter, on the keypad too.
                    (13 | 57414, 'u') => b"\r",
                    (9, 'u') => b"\t",
                    (127, 'u') => b"\x7f",
                    _ => &[],
                };
                if !kept.is_empty() {
                    out.extend_from_slice(kept);
                    return;
                }
            }
            sequence(out, number, (None, None), value, None, trailer);
        }
    }
}

/// The protocol's modifier value: 1, plus 1 for Shift, 2 for Alt and 4 for
/// Ctrl, as xterm's.
fn modifier_value(modifiers: Modifiers) -> u8 {
    1 + u8::from(modifiers.shift) + (u8::from(modifiers.alt) << 1) + (u8::from(modifiers.ctrl) << 2)
}

/// What key `press` is, if the protocol has a number for it.
fn code(press: &KeyPress<'_>) -> Option<Code> {
    if press.location == KeyLocation::Numpad {
        match press.key {
            Key::Named(NamedKey::Enter) => return Some(Code::Functional(57414, 'u')),
            Key::Character(text) => {
                if let Some(number) = keypad(text)
                    && let Some(ch) = text.chars().next()
                {
                    return Some(Code::Keypad(number, ch));
                }
            }
            _ => {}
        }
    }
    match press.key {
        Key::Named(NamedKey::Space) => Some(Code::Text(' ')),
        Key::Named(named) => {
            functional(*named).map(|(number, trailer)| Code::Functional(number, trailer))
        }
        Key::Character(text) => {
            let mut chars = text.chars();
            let (Some(ch), None) = (chars.next(), chars.next()) else {
                return None;
            };
            Some(Code::Text(unshifted(press, ch)))
        }
        _ => None,
    }
}

/// `ch`, the key Shift made, as typed without it: its lower case, or else
/// the key in the base layout.
fn unshifted(press: &KeyPress<'_>, ch: char) -> char {
    let mut lower = ch.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(lower), None) if lower != ch => lower,
        _ if press.modifiers.shift => press.code.and_then(base_layout).unwrap_or(ch),
        _ => ch,
    }
}

/// The key Shift made, and the key in the base layout, as
/// [`ALTERNATE_KEYS`] has them said: each only when it's another than
/// `ch`, the key's own.
fn alternates(press: &KeyPress<'_>, ch: char, flags: u8) -> (Option<u32>, Option<u32>) {
    if flags & ALTERNATE_KEYS == 0 {
        return (None, None);
    }
    let shifted = match press.key {
        Key::Character(text) if press.modifiers.shift => text.chars().next(),
        _ => None,
    };
    let base = press.code.and_then(base_layout);
    (
        shifted.filter(|&shifted| shifted != ch).map(u32::from),
        base.filter(|&base| base != ch).map(u32::from),
    )
}

/// The text `press` types, if it types any: none with Ctrl or Alt held.
fn typed(press: &KeyPress<'_>) -> Option<String> {
    if press.modifiers.ctrl || press.modifiers.alt {
        return None;
    }
    let text = press.text.or(match press.key {
        Key::Character(text) => Some(text.as_str()),
        Key::Named(NamedKey::Space) => Some(" "),
        _ => None,
    })?;
    Some(text.to_owned()).filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
}

/// Appends `CSI number[:shifted[:base]] [; modifiers [; text]] trailer`.
/// The number 1 is left out of `CSI 1 A` and its like when nothing
/// follows it, as the 1 of modifiers is when no text does.
fn sequence(
    out: &mut Vec<u8>,
    number: u32,
    (shifted, base): (Option<u32>, Option<u32>),
    modifiers: u8,
    text: Option<&str>,
    trailer: char,
) {
    use std::fmt::Write;

    let mut sequence = String::from("\x1b[");
    let implied = trailer != 'u' && number == 1 && modifiers == 1 && text.is_none();
    if !implied {
        write!(sequence, "{number}").expect("writing to a string never fails");
    }
    let written = match (shifted, base) {
        (Some(shifted), Some(base)) => write!(sequence, ":{shifted}:{base}"),
        (Some(shifted), None) => write!(sequence, ":{shifted}"),
        (None, Some(base)) => write!(sequence, "::{base}"),
        (None, None) => Ok(()),
    };
    written.expect("writing to a string never fails");
    if modifiers != 1 || text.is_some() {
        sequence.push(';');
        if modifiers != 1 {
            write!(sequence, "{modifiers}").expect("writing to a string never fails");
        }
    }
    if let Some(text) = text {
        sequence.push(';');
        let codepoints: Vec<String> = text.chars().map(|ch| u32::from(ch).to_string()).collect();
        sequence.push_str(&codepoints.join(":"));
    }
    sequence.push(trailer);
    out.extend_from_slice(sequence.as_bytes());
}

/// A key that types no text: its number, and the sequence's last
/// character. Esc, Enter, Tab and Backspace are their C0 codes; the keys
/// xterm sends as `CSI … ~` or `CSI 1 … A` keep those, but F3, which is
/// `CSI 13 ~` so as not to be taken for a cursor position report.
fn functional(key: NamedKey) -> Option<(u32, char)> {
    Some(match key {
        NamedKey::Escape => (27, 'u'),
        NamedKey::Enter => (13, 'u'),
        NamedKey::Tab => (9, 'u'),
        NamedKey::Backspace => (127, 'u'),
        NamedKey::Insert => (2, '~'),
        NamedKey::Delete => (3, '~'),
        NamedKey::ArrowLeft => (1, 'D'),
        NamedKey::ArrowRight => (1, 'C'),
        NamedKey::ArrowUp => (1, 'A'),
        NamedKey::ArrowDown => (1, 'B'),
        NamedKey::PageUp => (5, '~'),
        NamedKey::PageDown => (6, '~'),
        NamedKey::Home => (1, 'H'),
        NamedKey::End => (1, 'F'),
        NamedKey::CapsLock => (57358, 'u'),
        NamedKey::ScrollLock => (57359, 'u'),
        NamedKey::NumLock => (57360, 'u'),
        NamedKey::PrintScreen => (57361, 'u'),
        NamedKey::Pause => (57362, 'u'),
        NamedKey::ContextMenu => (57363, 'u'),
        NamedKey::F1 => (1, 'P'),
        NamedKey::F2 => (1, 'Q'),
        NamedKey::F3 => (13, '~'),
        NamedKey::F4 => (1, 'S'),
        NamedKey::F5 => (15, '~'),
        NamedKey::F6 => (17, '~'),
        NamedKey::F7 => (18, '~'),
        NamedKey::F8 => (19, '~'),
        NamedKey::F9 => (20, '~'),
        NamedKey::F10 => (21, '~'),
        NamedKey::F11 => (23, '~'),
        NamedKey::F12 => (24, '~'),
        NamedKey::F13 => (57376, 'u'),
        NamedKey::F14 => (57377, 'u'),
        NamedKey::F15 => (57378, 'u'),
        NamedKey::F16 => (57379, 'u'),
        NamedKey::F17 => (57380, 'u'),
        NamedKey::F18 => (57381, 'u'),
        NamedKey::F19 => (57382, 'u'),
        NamedKey::F20 => (57383, 'u'),
        NamedKey::F21 => (57384, 'u'),
        NamedKey::F22 => (57385, 'u'),
        NamedKey::F23 => (57386, 'u'),
        NamedKey::F24 => (57387, 'u'),
        _ => return None,
    })
}

/// The protocol's number for the key on the numeric keypad that types
/// `text`: each its own, apart from the keys elsewhere that type the same.
fn keypad(text: &str) -> Option<u32> {
    match text {
        "." | "," => Some(57409),
        "/" => Some(57410),
        "*" => Some(57411),
        "-" => Some(57412),
        "+" => Some(57413),
        "=" => Some(57415),
        number if number.len() == 1 && number.as_bytes()[0].is_ascii_digit() => {
            Some(57399 + u32::from(number.as_bytes()[0] - b'0'))
        }
        _ => None,
    }
}

/// The character the key at `code` types in a US layout without Shift.
fn base_layout(code: KeyCode) -> Option<char> {
    Some(match code {
        KeyCode::KeyA => 'a',
        KeyCode::KeyB => 'b',
        KeyCode::KeyC => 'c',
        KeyCode::KeyD => 'd',
        KeyCode::KeyE => 'e',
        KeyCode::KeyF => 'f',
        KeyCode::KeyG => 'g',
        KeyCode::KeyH => 'h',
        KeyCode::KeyI => 'i',
        KeyCode::KeyJ => 'j',
        KeyCode::KeyK => 'k',
        KeyCode::KeyL => 'l',
        KeyCode::KeyM => 'm',
        KeyCode::KeyN => 'n',
        KeyCode::KeyO => 'o',
        KeyCode::KeyP => 'p',
        KeyCode::KeyQ => 'q',
        KeyCode::KeyR => 'r',
        KeyCode::KeyS => 's',
        KeyCode::KeyT => 't',
        KeyCode::KeyU => 'u',
        KeyCode::KeyV => 'v',
        KeyCode::KeyW => 'w',
        KeyCode::KeyX => 'x',
        KeyCode::KeyY => 'y',
        KeyCode::KeyZ => 'z',
        KeyCode::Digit0 => '0',
        KeyCode::Digit1 => '1',
        KeyCode::Digit2 => '2',
        KeyCode::Digit3 => '3',
        KeyCode::Digit4 => '4',
        KeyCode::Digit5 => '5',
        KeyCode::Digit6 => '6',
        KeyCode::Digit7 => '7',
        KeyCode::Digit8 => '8',
        KeyCode::Digit9 => '9',
        KeyCode::Minus => '-',
        KeyCode::Equal => '=',
        KeyCode::BracketLeft => '[',
        KeyCode::BracketRight => ']',
        KeyCode::Backslash => '\\',
        KeyCode::Semicolon => ';',
        KeyCode::Quote => '\'',
        KeyCode::Backquote => '`',
        KeyCode::Comma => ',',
        KeyCode::Period => '.',
        KeyCode::Slash => '/',
        KeyCode::Space => ' ',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use clackatoo_term::keyboard::DISAMBIGUATE;

    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    const CTRL: Modifiers = Modifiers { ctrl: true, ..NONE };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };

    /// What a press of `key` at `code`, typing `text`, sends with `flags`.
    fn sent(
        key: &Key,
        text: Option<&str>,
        code: Option<KeyCode>,
        modifiers: Modifiers,
        flags: u8,
    ) -> String {
        let mut out = Vec::new();
        encode(
            &KeyPress {
                key,
                text,
                location: KeyLocation::Standard,
                code,
                modifiers,
            },
            flags,
            &mut out,
        );
        String::from_utf8(out).expect("keys send UTF-8")
    }

    fn named(key: NamedKey, modifiers: Modifiers, flags: u8) -> String {
        sent(&Key::Named(key), None, None, modifiers, flags)
    }

    fn letter(typed: &str, code: KeyCode, modifiers: Modifiers, flags: u8) -> String {
        let key = Key::Character(typed.into());
        let text = (!modifiers.ctrl).then_some(typed);
        sent(&key, text, Some(code), modifiers, flags)
    }

    #[test]
    fn disambiguating_changes_only_what_xterm_cant_tell_apart() {
        let flags = DISAMBIGUATE;
        assert_eq!(letter("a", KeyCode::KeyA, NONE, flags), "a");
        assert_eq!(letter("A", KeyCode::KeyA, SHIFT, flags), "A");
        assert_eq!(letter("c", KeyCode::KeyC, CTRL, flags), "\x1b[99;5u");
        assert_eq!(letter("b", KeyCode::KeyB, ALT, flags), "\x1b[98;3u");
        assert_eq!(
            letter(
                "I",
                KeyCode::KeyI,
                Modifiers {
                    shift: true,
                    ..CTRL
                },
                flags
            ),
            "\x1b[105;6u"
        );
        assert_eq!(named(NamedKey::Escape, NONE, flags), "\x1b[27u");
        assert_eq!(named(NamedKey::Enter, NONE, flags), "\r");
        assert_eq!(named(NamedKey::Tab, NONE, flags), "\t");
        assert_eq!(named(NamedKey::Backspace, NONE, flags), "\x7f");
        assert_eq!(named(NamedKey::Enter, SHIFT, flags), "\x1b[13;2u");
        assert_eq!(named(NamedKey::Tab, SHIFT, flags), "\x1b[9;2u");
        assert_eq!(named(NamedKey::Backspace, CTRL, flags), "\x1b[127;5u");
        assert_eq!(
            sent(&Key::Named(NamedKey::Space), Some(" "), None, CTRL, flags),
            "\x1b[32;5u"
        );
    }

    #[test]
    fn keys_xterm_sends_as_sequences_keep_their_forms() {
        let flags = DISAMBIGUATE;
        assert_eq!(named(NamedKey::ArrowUp, NONE, flags), "\x1b[A");
        assert_eq!(named(NamedKey::ArrowLeft, CTRL, flags), "\x1b[1;5D");
        assert_eq!(named(NamedKey::Delete, NONE, flags), "\x1b[3~");
        assert_eq!(named(NamedKey::PageDown, SHIFT, flags), "\x1b[6;2~");
        assert_eq!(named(NamedKey::F1, NONE, flags), "\x1b[P");
        assert_eq!(named(NamedKey::F3, NONE, flags), "\x1b[13~");
        assert_eq!(named(NamedKey::F5, ALT, flags), "\x1b[15;3~");
        assert_eq!(named(NamedKey::F13, NONE, flags), "\x1b[57376u");
        assert_eq!(
            named(NamedKey::Shift, SHIFT, flags),
            "",
            "modifiers alone send nothing"
        );
    }

    #[test]
    fn every_key_is_a_sequence_when_asked() {
        let flags = DISAMBIGUATE | ALL_KEYS;
        assert_eq!(letter("a", KeyCode::KeyA, NONE, flags), "\x1b[97u");
        assert_eq!(letter("A", KeyCode::KeyA, SHIFT, flags), "\x1b[97;2u");
        assert_eq!(named(NamedKey::Enter, NONE, flags), "\x1b[13u");
        assert_eq!(named(NamedKey::Escape, NONE, flags), "\x1b[27u");
        // With the text each types.
        let flags = flags | TEXT;
        assert_eq!(letter("a", KeyCode::KeyA, NONE, flags), "\x1b[97;;97u");
        assert_eq!(letter("A", KeyCode::KeyA, SHIFT, flags), "\x1b[97;2;65u");
        assert_eq!(letter("c", KeyCode::KeyC, CTRL, flags), "\x1b[99;5u");
        assert_eq!(letter("é", KeyCode::Digit2, NONE, flags), "\x1b[233;;233u");
    }

    #[test]
    fn alternate_keys_say_what_shift_made_and_the_base_layout() {
        let flags = DISAMBIGUATE | ALL_KEYS | ALTERNATE_KEYS;
        assert_eq!(letter("A", KeyCode::KeyA, SHIFT, flags), "\x1b[97:65;2u");
        // A US layout's Shift+1.
        assert_eq!(letter("!", KeyCode::Digit1, SHIFT, flags), "\x1b[49:33;2u");
        // A French layout's A key, where a US layout has Q.
        assert_eq!(letter("a", KeyCode::KeyQ, CTRL, flags), "\x1b[97::113;5u");
        assert_eq!(letter("a", KeyCode::KeyA, NONE, flags), "\x1b[97u");
    }

    #[test]
    fn the_keypad_has_numbers_of_its_own() {
        let on_keypad = |key: &Key, text, modifiers, flags| {
            let mut out = Vec::new();
            let press = KeyPress {
                key,
                text,
                location: KeyLocation::Numpad,
                code: None,
                modifiers,
            };
            encode(&press, flags, &mut out);
            String::from_utf8(out).expect("keys send UTF-8")
        };
        let five = Key::Character("5".into());
        assert_eq!(on_keypad(&five, Some("5"), NONE, DISAMBIGUATE), "5");
        assert_eq!(on_keypad(&five, None, CTRL, DISAMBIGUATE), "\x1b[57404;5u");
        assert_eq!(on_keypad(&five, Some("5"), NONE, ALL_KEYS), "\x1b[57404u");
        assert_eq!(
            on_keypad(&five, Some("5"), NONE, ALL_KEYS | TEXT),
            "\x1b[57404;;53u"
        );
        let enter = Key::Named(NamedKey::Enter);
        assert_eq!(on_keypad(&enter, None, NONE, DISAMBIGUATE), "\r");
        assert_eq!(
            on_keypad(&enter, None, SHIFT, DISAMBIGUATE),
            "\x1b[57414;2u"
        );
    }

    #[test]
    fn text_an_input_method_committed_is_sent_whole() {
        let key = Key::Character("日本語".into());
        assert_eq!(
            sent(&key, Some("日本語"), None, NONE, DISAMBIGUATE | ALL_KEYS),
            "日本語"
        );
    }
}
