//! The clipboard, as programs reach it with OSC 52.
//!
//! `OSC 52 ; selection ; data ST` puts `data`, base64-encoded text, on the
//! clipboard, and `OSC 52 ; selection ; ? ST` asks what's there. Neovim
//! (its `osc52` provider, and over ssh), tmux's `set-clipboard` and many
//! other programs copy this way, which works wherever the terminal is,
//! however many machines away the program runs. `selection` names which
//! of the desktop's clipboards: `c` the clipboard, `p` the primary
//! selection (and `s`, as xterm reads it). Both may be named. None means
//! the clipboard.
//!
//! The emulator only reads what was asked. It has no clipboard: the daemon
//! takes the requests with [`Terminal::take_clipboard`](crate::Terminal::take_clipboard)
//! and passes them on to a window following the terminal, which owns a
//! clipboard and decides what to allow.

use base64::Engine;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT as BASE64;

use crate::parser::MAX_OSC_LEN;

/// The most text a program may put on the clipboard at once, in bytes,
/// and the most it's told when it asks what's there. Far more than any
/// selection copied by hand, and bounded, so a program can't flood every
/// window with a clipboard of gigabytes.
pub const MAX_CLIPBOARD_TEXT: usize = 1 << 20;

/// The most base64 that [`MAX_CLIPBOARD_TEXT`] bytes encode to.
const MAX_ENCODED: usize = MAX_CLIPBOARD_TEXT.div_ceil(3) * 4;

/// The longest selection parameter read. xterm's are a few letters long.
const MAX_SELECTION_PARAM: usize = 16;

// The parser keeps the first `MAX_OSC_LEN` bytes of a string and drops the
// rest. A request cut short must never decode to the start of the text,
// so the longest one allowed has to fit whole: then one that was cut is
// always longer than allowed, and is refused.
const _: () = assert!(MAX_OSC_LEN > "52;".len() + MAX_SELECTION_PARAM + ";".len() + MAX_ENCODED);

/// The most requests to read the clipboard that wait to be taken. More are
/// answered at once, with nothing, so a program asking in a loop can't
/// queue up asks without end.
const MAX_READS_WAITING: usize = 4;

/// Which of the desktop's clipboards a program means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Selection {
    /// The clipboard, which Ctrl+V pastes.
    Clipboard,
    /// The primary selection, which the middle button pastes.
    Primary,
}

impl Selection {
    /// The letter OSC 52 names it by.
    fn letter(self) -> char {
        match self {
            Self::Clipboard => 'c',
            Self::Primary => 'p',
        }
    }
}

/// What a program asked of the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardRequest {
    /// Put `text` on `selection`.
    Write {
        /// Which clipboard.
        selection: Selection,
        /// The text, at most [`MAX_CLIPBOARD_TEXT`] bytes.
        text: String,
    },
    /// Say what's on a clipboard. The program waits for the answer.
    Read(ClipboardRead),
}

/// A program's request to be told what's on a clipboard, which must be
/// answered with [`answer`](Self::answer), even if only with nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipboardRead {
    /// Which clipboard.
    pub selection: Selection,
    /// The request ended with BEL rather than ST, so the answer does too,
    /// as xterm's does.
    bell_terminated: bool,
}

impl ClipboardRead {
    /// The answer to write back to the program: `text`, or, if there's
    /// none to give or it's longer than [`MAX_CLIPBOARD_TEXT`], an empty
    /// one. Programs waiting for an answer (Neovim waits ten seconds)
    /// take an empty one as an empty clipboard and carry on.
    #[must_use]
    pub fn answer(&self, text: Option<&str>) -> Vec<u8> {
        let mut answer = format!("\x1b]52;{};", self.selection.letter());
        if let Some(text) = text.filter(|text| text.len() <= MAX_CLIPBOARD_TEXT) {
            BASE64.encode_string(text, &mut answer);
        }
        answer.push_str(if self.bell_terminated {
            "\x07"
        } else {
            "\x1b\\"
        });
        answer.into_bytes()
    }
}

/// The requests a program has made that haven't been taken yet. A write
/// replaces any earlier write to the same clipboard that's waiting, since
/// only the last would show, so at most two writes and
/// [`MAX_READS_WAITING`] reads ever wait.
#[derive(Clone, Debug, Default)]
pub(crate) struct Requests {
    waiting: Vec<ClipboardRequest>,
}

impl Requests {
    /// Reads `OSC 52 ; selection ; data`, ended with BEL if
    /// `bell_terminated`. Returns the answer to write back at once, for a
    /// read that can't wait.
    pub(crate) fn osc52(
        &mut self,
        selection: &[u8],
        data: &[&[u8]],
        bell_terminated: bool,
    ) -> Option<Vec<u8>> {
        let selections = parse_selection(selection)?;
        // Base64 has no `;`, so data the parser split is no request.
        let [data] = data else {
            log::trace!("ignoring OSC 52 with {} parts of data", data.len());
            return None;
        };
        if *data == b"?" {
            let read = ClipboardRead {
                selection: selections[0],
                bell_terminated,
            };
            let reads = (self.waiting.iter())
                .filter(|request| matches!(request, ClipboardRequest::Read(_)))
                .count();
            if reads >= MAX_READS_WAITING {
                log::debug!("too many reads of the clipboard waiting: answering with nothing");
                return Some(read.answer(None));
            }
            self.waiting.push(ClipboardRequest::Read(read));
            return None;
        }
        let text = decode(data)?;
        for selection in selections {
            self.waiting.retain(|request| {
                !matches!(request, ClipboardRequest::Write { selection: old, .. } if *old == selection)
            });
            self.waiting.push(ClipboardRequest::Write {
                selection,
                text: text.clone(),
            });
        }
        None
    }

    /// Takes every request waiting, oldest first.
    pub(crate) fn take(&mut self) -> Vec<ClipboardRequest> {
        std::mem::take(&mut self.waiting)
    }
}

/// The clipboards a selection parameter names, the one to read from
/// first, if it names any. Cut buffers (`0` to `7`) are xterm's alone,
/// and left out.
fn parse_selection(param: &[u8]) -> Option<Vec<Selection>> {
    if param.len() > MAX_SELECTION_PARAM {
        log::trace!(
            "ignoring OSC 52 with a selection parameter of {} bytes",
            param.len()
        );
        return None;
    }
    // None at all means the clipboard.
    if param.is_empty() {
        return Some(vec![Selection::Clipboard]);
    }
    let mut named = Vec::with_capacity(2);
    for &letter in param {
        let selection = match letter {
            b'c' => Selection::Clipboard,
            b'p' | b's' => Selection::Primary,
            _ => continue,
        };
        if !named.contains(&selection) {
            named.push(selection);
        }
    }
    (!named.is_empty()).then_some(named)
}

/// The text `data` encodes, unless it's empty, too long or not base64.
/// Bytes that aren't UTF-8 become U+FFFD, as clipboards hold text.
fn decode(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    if data.len() > MAX_ENCODED {
        log::debug!("ignoring OSC 52 of {} bytes: too long", data.len());
        return None;
    }
    let bytes = BASE64
        .decode(data)
        .inspect_err(|err| log::debug!("ignoring OSC 52 that isn't base64: {err}"))
        .ok()?;
    // The longest base64 allowed can hold two bytes more.
    if bytes.len() > MAX_CLIPBOARD_TEXT {
        log::debug!("ignoring OSC 52 of {} bytes of text: too long", bytes.len());
        return None;
    }
    Some(
        String::from_utf8(bytes)
            .unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned()),
    )
}
