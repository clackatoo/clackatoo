//! [`Span`], the text an operator acts on, its ends in order however it
//! was taken, as Vim's `do_pending()` puts an operator's start before its
//! end.

use super::text::Text;
use crate::motion::{Cursor, Point};

/// The text an operator acts on: from byte [`start`](Self::start) to byte
/// [`end`](Self::end), in that order. Its ends are its own, so it's made
/// only by the constructors here, which put them in order.
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    start: usize,
    end: usize,
    /// Whole lines, `first` to `last`.
    pub(super) lines: Option<(u64, u64)>,
    /// Where the motion that took it went, if one did: where `y` leaves
    /// the cursor, if it went back.
    pub(super) moved: Option<Cursor>,
    /// Whether `d` leaves the cursor at the first non-blank of the line
    /// after the lines it takes, `startofline` or not, as Vim's does after
    /// `:d`, or a motion by characters that became whole lines.
    pub(super) first_non_blank: bool,
}

impl Span {
    /// The characters from byte `a` to byte `b`, whichever is first.
    pub(super) fn bytes(a: usize, b: usize) -> Self {
        Self {
            start: a.min(b),
            end: a.max(b),
            lines: None,
            moved: None,
            first_non_blank: false,
        }
    }

    /// Lines `first` to `last` of `text`, whole, whichever is first.
    pub(super) fn lines(text: &Text<'_>, first: u64, last: u64) -> Self {
        let (first, last) = (first.min(last), first.max(last));
        Self {
            lines: Some((first, last)),
            ..Self::bytes(text.line_start(first), text.line_start(last + 1))
        }
    }

    /// Nothing, at `at` in `text`, as an exclusive motion that goes
    /// nowhere takes.
    pub(super) fn empty(text: &Text<'_>, at: Point) -> Self {
        let byte = text.byte_at(at);
        Self::bytes(byte, byte)
    }

    /// The characters of `text` from `start` to `last`, both in it,
    /// whichever is first.
    pub(super) fn chars(text: &Text<'_>, start: Point, last: Point) -> Self {
        let (start, last) = (start.min(last), start.max(last));
        Self::bytes(text.byte_at(start), text.char_end(last))
    }

    /// As [`chars`](Self::chars), but `last` past its line's last
    /// character, as on an empty line, is the line break, as Vim's visual
    /// mode and `d` with Backspace take it.
    pub(super) fn chars_to_break(text: &Text<'_>, start: Point, last: Point) -> Self {
        let (start, last) = (start.min(last), start.max(last));
        // The last line's break isn't the text's to take.
        let end = if last.col >= text.line_cells(last.line) && last.line < text.last_line() {
            text.line_start(last.line + 1)
        } else {
            text.char_end(last)
        };
        Self::bytes(text.byte_at(start), end)
    }

    /// The byte it starts at.
    pub(super) fn start(&self) -> usize {
        self.start
    }

    /// The byte it ends at, not in it.
    pub(super) fn end(&self) -> usize {
        self.end
    }

    /// It, with the motion that took it gone to `moved`.
    pub(super) fn went(self, moved: Cursor) -> Self {
        Self {
            moved: Some(moved),
            ..self
        }
    }

    /// It, with `d` leaving the cursor at the first non-blank after it.
    pub(super) fn leaving_first_non_blank(self) -> Self {
        Self {
            first_non_blank: true,
            ..self
        }
    }
}
