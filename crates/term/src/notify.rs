//! Notifications programs ask the terminal to show, as a build finishing
//! or a long command ending says, in any of the three ways terminals read:
//!
//! - `OSC 9 ; text`, iTerm2's. `ConEmu` numbers commands of its own in OSC
//!   9: `OSC 9 ; 4` is how far a task has got (see [`crate::TaskProgress`]),
//!   and the others, whose first part is a number, aren't notifications.
//! - `OSC 777 ; notify ; title ; body`, urxvt's, as foot and Ghostty read
//!   it.
//! - `OSC 99 ; metadata ; payload`, kitty's
//!   (<https://sw.kovidgoyal.net/kitty/desktop-notifications/>). The
//!   metadata is `key=value` pairs, separated by `:`. What's read: `i`, the
//!   notification's id, which a later one names to close it (`p=close`);
//!   `p`, whether the payload is the `title` (as when it's left out) or
//!   the `body`; `d=0`, that more of it comes in another sequence;
//!   `e=1`, that it's base64; and `u`, how urgent it is (0, 1 or 2).
//!   Anything else it asks, such as buttons, icons or sounds, is left out.
//!
//! Nothing here is ever answered: not kitty's questions about what's
//! supported (`p=?`), nor its reports of a notification closed or clicked,
//! so nothing a program sends comes back to it as typing.
//!
//! What a program says is cleaned as names are (see [`crate::visible`]):
//! control characters become spaces, what hides or turns text round goes,
//! runs of spaces become one, and it's cut short. The terminal keeps only
//! the latest it was asked for until it's taken; how often one is shown
//! is the owner's to bound, as it knows the time.

use base64::Engine;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT as BASE64;

/// The most characters of a title shown.
pub const MOST_TITLE: usize = 120;

/// The most characters of a body shown.
pub const MOST_BODY: usize = 300;

/// The most bytes of one part, a title or a body, read as a program
/// sends it: a few times what's shown, for what cleaning drops.
const LONGEST_PART: usize = 4096;

/// The longest id of kitty's kept. kitty's own are far shorter.
const LONGEST_ID: usize = 64;

/// How urgent a program says its notification is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Urgency {
    /// It can wait (kitty's `u=0`).
    Low,
    /// As most are.
    #[default]
    Normal,
    /// It needs the user now (kitty's `u=2`).
    Critical,
}

/// A notification a program asked to be shown, cleaned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Notification {
    /// Its first line, at most [`MOST_TITLE`] characters. May be empty,
    /// as iTerm2's have none.
    pub title: String,
    /// What it says, at most [`MOST_BODY`] characters. May be empty.
    pub body: String,
    /// How urgent it is.
    pub urgency: Urgency,
}

/// What a program asked of the terminal's notification.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Notice {
    /// Show this, in place of the terminal's last, if it's still shown.
    Show(Notification),
    /// Take the terminal's last back, as kitty's `p=close` does.
    Close,
}

impl Notification {
    /// The notification titled `title`, saying `body`, both as a program
    /// sent them, cleaned; `None` if nothing's left of either.
    pub(crate) fn cleaned(title: &[u8], body: &[u8], urgency: Urgency) -> Option<Self> {
        let title = cleaned(title, MOST_TITLE);
        let body = cleaned(body, MOST_BODY);
        (!title.is_empty() || !body.is_empty()).then_some(Self {
            title,
            body,
            urgency,
        })
    }

    /// Cleaned again, as a notification from somewhere that isn't
    /// trusted to have, such as a daemon on another machine.
    #[must_use]
    pub fn recleaned(&self) -> Option<Self> {
        Self::cleaned(self.title.as_bytes(), self.body.as_bytes(), self.urgency)
    }
}

/// `text`, as a program sent it, as it's shown: at most [`LONGEST_PART`]
/// bytes of it read, control characters as spaces, what hides or turns
/// text round left out, spaces run together and trimmed, and at most
/// `most` characters.
fn cleaned(text: &[u8], most: usize) -> String {
    let text = String::from_utf8_lossy(&text[..text.len().min(LONGEST_PART)]);
    let spaced = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch });
    let shown = crate::visible(spaced, usize::MAX);
    let mut words = String::with_capacity(shown.len().min(most.saturating_mul(4)));
    for word in shown.split_whitespace() {
        if !words.is_empty() {
            words.push(' ');
        }
        words.push_str(word);
    }
    match words.char_indices().nth(most) {
        Some((end, _)) => {
            words.truncate(end);
            words.truncate(words.trim_end().len());
            words
        }
        None => words,
    }
}

/// A notification of kitty's coming in parts.
#[derive(Debug, Default)]
struct Parts {
    id: Option<String>,
    title: Vec<u8>,
    body: Vec<u8>,
    urgency: Urgency,
}

/// A terminal's notifications: what's waiting to be taken, and what
/// kitty's protocol needs kept between sequences.
#[derive(Debug, Default)]
pub(crate) struct Notifications {
    /// The latest asked for, until it's taken.
    pending: Option<Notice>,
    /// The id of the last shown, as kitty's protocol gave it, if it did.
    shown: Option<String>,
    /// One coming in parts (`d=0`).
    parts: Option<Parts>,
}

impl Notifications {
    /// Takes what was last asked, if anything has been since this was
    /// last called.
    pub(crate) fn take(&mut self) -> Option<Notice> {
        self.pending.take()
    }

    /// Forgets any notification half sent, as at a reset. What was asked
    /// before is still to be done.
    pub(crate) fn reset(&mut self) {
        self.parts = None;
    }

    /// `OSC 9 ; text`: iTerm2's, but for `ConEmu`'s numbered commands.
    pub(crate) fn iterm(&mut self, text: &[&[u8]]) {
        if text
            .first()
            .is_some_and(|first| !first.is_empty() && first.iter().all(u8::is_ascii_digit))
        {
            return;
        }
        self.show(None, b"", &text.join(&b';'), Urgency::Normal);
    }

    /// `OSC 777 ; notify ; title ; body`.
    pub(crate) fn urxvt(&mut self, params: &[&[u8]]) {
        let [b"notify", title, body @ ..] = params else {
            return;
        };
        self.show(None, title, &body.join(&b';'), Urgency::Normal);
    }

    /// `OSC 99 ; metadata ; payload`.
    pub(crate) fn kitty(&mut self, metadata: &[u8], payload: &[&[u8]]) {
        let mut id = None;
        let mut done = true;
        let mut base64 = false;
        let mut body = false;
        let mut close = false;
        let mut urgency = None;
        for pair in metadata.split(|&byte| byte == b':') {
            let (key, value) = match pair.iter().position(|&byte| byte == b'=') {
                Some(at) => (&pair[..at], &pair[at + 1..]),
                None => (pair, &b""[..]),
            };
            match (key, value) {
                (b"i", value) => id = kitty_id(value),
                (b"d", b"0") => done = false,
                (b"e", b"1") => base64 = true,
                (b"p", b"title") => body = false,
                (b"p", b"body") => body = true,
                (b"p", b"close") => close = true,
                // What else a payload can be (`?`, `alive`, `buttons`,
                // `icon`) isn't shown, and gets no answer.
                (b"p", _) => return,
                (b"u", b"0") => urgency = Some(Urgency::Low),
                (b"u", b"1") => urgency = Some(Urgency::Normal),
                (b"u", b"2") => urgency = Some(Urgency::Critical),
                _ => {}
            }
        }
        if close {
            if id.is_some() && id == self.shown {
                self.shown = None;
                self.pending = Some(Notice::Close);
            }
            return;
        }
        let joined = payload.join(&b';');
        let text = if base64 {
            if joined.len() > LONGEST_PART.div_ceil(3) * 4 {
                return;
            }
            let Ok(text) = BASE64.decode(&joined) else {
                return;
            };
            text
        } else {
            joined
        };
        // A part of another notification starts afresh.
        let mut parts = self
            .parts
            .take()
            .filter(|parts| parts.id == id)
            .unwrap_or_else(|| Parts {
                id: id.clone(),
                ..Parts::default()
            });
        let part = if body {
            &mut parts.body
        } else {
            &mut parts.title
        };
        let room = LONGEST_PART.saturating_sub(part.len());
        part.extend_from_slice(&text[..text.len().min(room)]);
        if let Some(urgency) = urgency {
            parts.urgency = urgency;
        }
        if done {
            self.show(parts.id, &parts.title, &parts.body, parts.urgency);
        } else {
            self.parts = Some(parts);
        }
    }

    fn show(&mut self, id: Option<String>, title: &[u8], body: &[u8], urgency: Urgency) {
        if let Some(notification) = Notification::cleaned(title, body, urgency) {
            self.shown = id;
            self.pending = Some(Notice::Show(notification));
        }
    }
}

/// kitty's id `value`, if it's one: letters, digits and `-_+.`, as the
/// protocol has them, and not too long. It's only ever compared.
fn kitty_id(value: &[u8]) -> Option<String> {
    let fits = !value.is_empty()
        && value.len() <= LONGEST_ID
        && (value.iter()).all(|&byte| byte.is_ascii_alphanumeric() || b"-_+.".contains(&byte));
    fits.then(|| String::from_utf8_lossy(value).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(notifications: &mut Notifications, sequence: &str) {
        let params: Vec<&[u8]> = sequence.split(';').map(str::as_bytes).collect();
        match params.as_slice() {
            [b"9", text @ ..] => notifications.iterm(text),
            [b"777", rest @ ..] => notifications.urxvt(rest),
            [b"99", metadata, payload @ ..] => notifications.kitty(metadata, payload),
            _ => panic!("not a notification: {sequence}"),
        }
    }

    fn shown(title: &str, body: &str) -> Notice {
        Notice::Show(Notification {
            title: title.into(),
            body: body.into(),
            urgency: Urgency::Normal,
        })
    }

    #[test]
    fn each_kind_is_read() {
        let mut n = Notifications::default();
        osc(&mut n, "9;build done; all green");
        assert_eq!(n.take(), Some(shown("", "build done; all green")));
        assert_eq!(n.take(), None, "taken once");
        osc(&mut n, "777;notify;make;it built; twice");
        assert_eq!(n.take(), Some(shown("make", "it built; twice")));
        osc(&mut n, "99;;Hello");
        assert_eq!(n.take(), Some(shown("Hello", "")));
        // Only the latest waits.
        osc(&mut n, "9;one");
        osc(&mut n, "9;two");
        assert_eq!(n.take(), Some(shown("", "two")));
    }

    #[test]
    fn conemu_commands_and_other_payloads_are_not_notifications() {
        let mut n = Notifications::default();
        osc(&mut n, "9;1;500");
        osc(&mut n, "9;12");
        osc(&mut n, "777;preexec;x");
        osc(&mut n, "99;p=?;");
        osc(&mut n, "99;p=icon;aGk=");
        osc(&mut n, "9; \u{202e} \x07");
        assert_eq!(n.take(), None);
    }

    #[test]
    fn kitty_parts_join_until_done_and_close_by_id() {
        let mut n = Notifications::default();
        osc(&mut n, "99;i=a1:d=0;Tit");
        osc(&mut n, "99;i=a1:d=0;le");
        assert_eq!(n.take(), None, "not done yet");
        osc(&mut n, "99;i=a1:p=body:u=2:e=1;Ym9keQ==");
        assert_eq!(
            n.take(),
            Some(Notice::Show(Notification {
                title: "Title".into(),
                body: "body".into(),
                urgency: Urgency::Critical,
            }))
        );
        // Another id's close, or one without an id, closes nothing.
        osc(&mut n, "99;i=b:p=close;");
        osc(&mut n, "99;p=close;");
        assert_eq!(n.take(), None);
        osc(&mut n, "99;i=a1:p=close;");
        assert_eq!(n.take(), Some(Notice::Close));
        osc(&mut n, "99;i=a1:p=close;");
        assert_eq!(n.take(), None, "closed once");
        // A part of another notification starts afresh.
        osc(&mut n, "99;i=x:d=0;lost");
        osc(&mut n, "99;i=y;kept");
        assert_eq!(n.take(), Some(shown("kept", "")));
    }

    #[test]
    fn what_programs_send_is_cleaned_and_bounded() {
        let mut n = Notifications::default();
        osc(
            &mut n,
            "777;notify;\x1b[2Jfix\u{202e}ed\u{200b} \r\n\t it;\u{85}rm -rf\x15 \u{2028}now\x07",
        );
        assert_eq!(n.take(), Some(shown("[2Jfixed it", "rm -rf now")));
        let long = "é".repeat(LONGEST_PART);
        osc(&mut n, &format!("777;notify;{long};{long}"));
        let Some(Notice::Show(said)) = n.take() else {
            panic!("shown");
        };
        assert_eq!(said.title.chars().count(), MOST_TITLE);
        assert_eq!(said.body.chars().count(), MOST_BODY);
        // Parts stop growing, and base64 too long isn't read.
        for _ in 0..1000 {
            osc(&mut n, &format!("99;i=z:d=0;{long}"));
        }
        assert!(n.parts.as_ref().unwrap().title.len() <= LONGEST_PART);
        osc(&mut n, &format!("99;e=1;{}", "QUFB".repeat(LONGEST_PART)));
        osc(&mut n, "99;e=1;not base64!");
        // A hostile id is never kept to compare.
        osc(&mut n, "99;i=a\x15b;x");
        assert_eq!(n.shown, None);
        assert!(matches!(n.take(), Some(Notice::Show(_))));
    }

    #[test]
    fn nothing_hidden_survives_cleaning() {
        let hostile: String = (0..0x3000u32)
            .chain(0xE0000..0xE0100)
            .filter_map(char::from_u32)
            .collect();
        let said = cleaned(hostile.as_bytes(), usize::MAX);
        assert!(
            said.chars()
                .all(|ch| !ch.is_control() && (!crate::invisible(ch) || crate::joiner(ch)))
        );
    }
}
