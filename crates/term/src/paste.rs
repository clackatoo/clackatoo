//! Pasted text, as the bytes a program reads: shared by every client, so
//! a window and a phone paste alike.
//!
//! A program that turned on bracketed paste (mode 2004) gets the text
//! between `ESC [200~` and `ESC [201~`, and takes it as text, not keys:
//! a shell doesn't run a pasted line. The text must never end the paste
//! early, or what follows the end it carries runs as typed. Taking out
//! `ESC [201~` alone isn't enough, as taking it out of `ESC [20ESC [201~1~`
//! leaves another. So, as xterm and kitty do, the text loses every escape
//! and other control character but tab, carriage return and newline,
//! which nothing pasted needs and which can't make the terminator.

use crate::InputModes;

/// What pasting `text` sends to a program in `modes`: bracketed, with
/// control characters taken out, if the program asked for that;
/// otherwise as typed, with its newlines as Enter.
#[must_use]
pub fn paste(text: &str, modes: InputModes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut bytes = Vec::with_capacity(text.len() + 12);
        bytes.extend_from_slice(b"\x1b[200~");
        for ch in text.chars() {
            if ch.is_control() && !matches!(ch, '\t' | '\r' | '\n') {
                continue;
            }
            let mut buffer = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bracketed() -> InputModes {
        InputModes {
            bracketed_paste: true,
            ..InputModes::default()
        }
    }

    #[test]
    fn a_paste_cant_end_itself_early_however_its_terminator_is_hidden() {
        for text in [
            "a\x1b[201~; rm -rf ~\n",
            "\x1b[20\x1b[201~1~; cmd\n",
            "\x1b[2\x1b[20\x1b[201~1~1~x",
            "\u{9b}201~x",
        ] {
            let bytes = paste(text, bracketed());
            let inner = &bytes[6..bytes.len() - 6];
            assert!(
                !inner.contains(&0x1b) && !String::from_utf8_lossy(inner).contains('\u{9b}'),
                "{text:?} gave {bytes:?}"
            );
            assert!(bytes.starts_with(b"\x1b[200~") && bytes.ends_with(b"\x1b[201~"));
        }
        assert_eq!(
            paste("\x1b[20\x1b[201~1~; cmd\n", bracketed()),
            b"\x1b[200~[20[201~1~; cmd\n\x1b[201~"
        );
    }

    #[test]
    fn tabs_and_line_ends_stay_and_other_controls_go() {
        assert_eq!(
            paste("a\tb\r\nc\x07\x00d é", bracketed()),
            "\x1b[200~a\tb\r\ncd é\x1b[201~".as_bytes()
        );
    }

    #[test]
    fn without_bracketing_newlines_are_enter() {
        assert_eq!(paste("ls\nexit\r\n", InputModes::default()), b"ls\rexit\r");
    }
}
