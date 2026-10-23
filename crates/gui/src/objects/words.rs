//! `iw`, `aw`, `iW` and `aW`, as Vim's `current_word()` finds them, with
//! the word functions it's made of ported as they are: `fwd_word`,
//! `end_word`, `bck_word` and `bckend_word`, which move a position a
//! character at a time, as Vim's `inc` and `dec` do. A line's end, just
//! past its last character, is a position too, Vim's NUL, and blank.
//!
//! These take `stop` and `eol` as Vim's objects pass them, which the
//! motions don't: `end_word` stopping stays on a word's last character,
//! and `bckend_word` with `eol` stops at a line's end. So the motions keep
//! their own (see [`crate::motion::Cursor`]).

use std::cell::RefCell;

use crate::motion::{Class, Lines, Point, Words, class_at, line_end, next_cell, previous_cell};

/// What a step of a [`Pos`] did, as Vim's `inc` and `dec` return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Onto another character of the line.
    Along,
    /// Forward, onto the line's end, just past its last character.
    End,
    /// Onto another line: forward, its start; back, its end.
    Line,
    /// Nowhere, at the text's end or start.
    Stuck,
}

impl Step {
    /// Whether it went onto a line's end or another line, as Vim's `i >= 1`
    /// asks.
    fn over(self) -> bool {
        matches!(self, Self::End | Self::Line)
    }
}

/// A position as Vim's word functions move one, and its block objects.
pub(super) struct Pos<'a, L: Lines> {
    lines: &'a L,
    pub(super) at: Point,
    big: bool,
    words: Words,
    /// The line it was last on, with its row and where it ends, so a step
    /// along it costs a step, not a lookup of the line, however long.
    line: RefCell<Option<Line<'a, L>>>,
}

/// A line of `L`: its number, its row, and where it ends.
type Line<'a, L> = (u64, Option<<L as Lines>::Row<'a>>, usize);

impl<'a, L: Lines> Pos<'a, L> {
    /// At `at` of `lines`, to step a character at a time.
    pub(super) fn new(lines: &'a L, at: Point) -> Self {
        Self::with(lines, at, false, Words::NONE)
    }

    /// At `at` of `lines`, with `words`, and punctuation a word's if
    /// `big`.
    fn with(lines: &'a L, at: Point, big: bool, words: Words) -> Self {
        Self {
            lines,
            at,
            big,
            words,
            line: RefCell::new(None),
        }
    }

    /// `f` of line `line`'s row and where it ends, just past its last
    /// character.
    fn on_line<T>(&self, line: u64, f: impl FnOnce(Option<&L::Line>, usize) -> T) -> T {
        let mut kept = self.line.borrow_mut();
        if kept.as_ref().is_none_or(|(kept, _, _)| *kept != line) {
            #[cfg(test)]
            LINES_LOOKED_UP.with(|looked| looked.set(looked.get() + 1));
            *kept = Some((line, self.lines.row(line), line_end(line, self.lines)));
        }
        let (_, row, end) = kept.as_ref().expect("just kept");
        f(row.as_deref(), *end)
    }

    /// Where line `line` ends, just past its last character.
    fn end(&self, line: u64) -> usize {
        self.on_line(line, |_, end| end)
    }

    /// Vim's `cls()`: the class of the character here, a line's end blank,
    /// and punctuation a word's for the big objects.
    fn class(&self) -> Class {
        let class = self.on_line(self.at.line, |row, end| {
            if self.at.col >= end {
                Class::Blank
            } else {
                class_at(row, self.at.col, self.words)
            }
        });
        match class {
            Class::Punctuation | Class::Script(_) if self.big => Class::Word,
            class => class,
        }
    }

    /// Whether it's on an empty line, which Vim's word functions stop at.
    fn on_empty_line(&self) -> bool {
        self.at.col == 0 && self.lines.empty(self.at.line)
    }

    /// Vim's `inc`.
    pub(super) fn inc(&mut self) -> Step {
        let (at, end) = (self.at, self.end(self.at.line));
        if at.col < end {
            let next = self.on_line(at.line, |row, end| {
                next_cell(row, at.col).map_or(end, |col| col.min(end))
            });
            self.at.col = next;
            return if next < end { Step::Along } else { Step::End };
        }
        if self.at.line < self.lines.bounds().1 {
            self.at = Point {
                line: self.at.line + 1,
                col: 0,
            };
            return Step::Line;
        }
        Step::Stuck
    }

    /// Vim's `dec`.
    pub(super) fn dec(&mut self) -> Step {
        if self.at.col > 0 {
            let at = self.at;
            self.at.col = self.on_line(at.line, |row, end| {
                previous_cell(row, at.col.min(end)).unwrap_or(0)
            });
            return Step::Along;
        }
        if self.at.line > self.lines.bounds().0 {
            let line = self.at.line - 1;
            self.at = Point {
                line,
                col: self.end(line),
            };
            return Step::Line;
        }
        Step::Stuck
    }

    /// Vim's `incl`: forward, over a line's end.
    pub(super) fn incl(&mut self) -> Step {
        match self.inc() {
            Step::End => self.inc(),
            step => step,
        }
    }

    /// Vim's `decl`: back, over a line's end.
    pub(super) fn decl(&mut self) -> Step {
        match self.dec() {
            Step::Line if self.at.col > 0 => self.dec(),
            step => step,
        }
    }

    /// Vim's `oneleft`: a character back on the line, false at its start.
    fn oneleft(&mut self) -> bool {
        if self.at.col == 0 {
            return false;
        }
        self.dec();
        true
    }

    /// Vim's `skip_chars`: on, or back if not `forward`, over characters
    /// of `class`. False if it reaches the text's end or start.
    fn skip(&mut self, class: Class, forward: bool) -> bool {
        while self.class() == class {
            let step = if forward { self.inc() } else { self.dec() };
            if step == Step::Stuck {
                return false;
            }
        }
        true
    }

    /// Vim's `back_in_line`: back to the start of the word or blanks here,
    /// on this line.
    fn back_in_line(&mut self) {
        let class = self.class();
        while self.at.col > 0 {
            self.dec();
            if self.class() != class {
                self.inc();
                break;
            }
        }
    }

    /// Vim's `fwd_word`, `count` times; with `eol`, the last stops at its
    /// line's end. False if it started at the text's end.
    fn fwd_word(&mut self, count: usize, eol: bool) -> bool {
        for left in (0..count).rev() {
            let stops = eol && left == 0;
            let class = self.class();
            let last_line = self.at.line == self.lines.bounds().1;
            let step = self.inc();
            if step == Step::Stuck || (step.over() && last_line) {
                return false;
            }
            if step.over() && stops {
                return true;
            }
            if class != Class::Blank {
                while self.class() == class {
                    let step = self.inc();
                    if step == Step::Stuck || (step.over() && stops) {
                        return true;
                    }
                }
            }
            while self.class() == Class::Blank {
                if self.on_empty_line() {
                    break;
                }
                let step = self.inc();
                if step == Step::Stuck || (step.over() && stops) {
                    return true;
                }
            }
        }
        true
    }

    /// Vim's `end_word`, `count` times: if `stop`, the first stays on a
    /// word's last character; if `empty`, an empty line is a word's end.
    fn end_word(&mut self, count: usize, mut stop: bool, empty: bool) -> bool {
        for _ in 0..count {
            let class = self.class();
            if self.inc() == Step::Stuck {
                return false;
            }
            if self.class() == class && class != Class::Blank {
                if !self.skip(class, true) {
                    return false;
                }
            } else if !stop || class == Class::Blank {
                let mut at_empty_line = false;
                while self.class() == Class::Blank {
                    if empty && self.on_empty_line() {
                        at_empty_line = true;
                        break;
                    }
                    if self.inc() == Step::Stuck {
                        return false;
                    }
                }
                if at_empty_line {
                    stop = false;
                    continue;
                }
                if !self.skip(self.class(), true) {
                    return false;
                }
            }
            // Overshot: back a character.
            self.dec();
            stop = false;
        }
        true
    }

    /// Vim's `bck_word`, `count` times: if `stop`, the first stays at a
    /// word's start.
    fn bck_word(&mut self, count: usize, mut stop: bool) -> bool {
        for _ in 0..count {
            let class = self.class();
            if self.dec() == Step::Stuck {
                return false;
            }
            if !stop || class == self.class() || class == Class::Blank {
                let mut at_empty_line = false;
                while self.class() == Class::Blank {
                    if self.on_empty_line() {
                        at_empty_line = true;
                        break;
                    }
                    if self.dec() == Step::Stuck {
                        return true;
                    }
                }
                if at_empty_line {
                    stop = false;
                    continue;
                }
                if !self.skip(self.class(), false) {
                    return true;
                }
            }
            // Overshot: on a character.
            self.inc();
            stop = false;
        }
        true
    }

    /// Vim's `bckend_word`, `count` times; with `eol`, stopping at a
    /// line's end.
    fn bckend_word(&mut self, count: usize, eol: bool) -> bool {
        let stops = |step: Step| step == Step::Stuck || (eol && step == Step::Line);
        for _ in 0..count {
            let class = self.class();
            let step = self.dec();
            if step == Step::Stuck {
                return false;
            }
            if eol && step == Step::Line {
                return true;
            }
            if class != Class::Blank {
                while self.class() == class {
                    if stops(self.dec()) {
                        return true;
                    }
                }
            }
            while self.class() == Class::Blank {
                if self.on_empty_line() {
                    break;
                }
                if stops(self.dec()) {
                    return true;
                }
            }
        }
        true
    }
}

/// Whether `b`, or `ge` if `end`, `count` times from `at`, fails as Vim's
/// `bck_word()` or `bckend_word()` does: when one of the count starts at
/// the text's start, which one that runs into it on the way doesn't, as
/// it stops there; `big` for `B` and `gE`.
pub(crate) fn back_fails(
    lines: &impl Lines,
    at: Point,
    (end, big): (bool, bool),
    count: usize,
) -> bool {
    let mut pos = Pos::with(lines, at, big, lines.words());
    !if end {
        pos.bckend_word(count, false)
    } else {
        pos.bck_word(count, false)
    }
}

/// What [`current`] found: from `start` to `end`, which is in it if
/// `inclusive`, or else a line's start just after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Worded {
    pub(super) start: Point,
    pub(super) end: Point,
    pub(super) inclusive: bool,
}

/// Vim's `current_word()`: `count` words, or blanks, from the cursor at
/// `at`, just the word or the blanks if `inner`, with the blanks after
/// (or before) it if not; `big` for `W`. In visual mode, with the
/// selection's other end at `anchor`, a selection of more than a
/// character takes on `count` more, away from the anchor; the start found
/// is then the anchor. Where Vim's fails, at the text's end, an error,
/// with as far as it got, which Vim's leaves the selection at, or the
/// cursor, for an operator.
pub(super) fn current(
    lines: &impl Lines,
    at: Point,
    anchor: Option<Point>,
    (inner, big): (bool, bool),
    count: usize,
) -> Result<Worded, Worded> {
    let include = !inner;
    let mut pos = Pos::with(lines, at, big, lines.words());
    let mut start = anchor.unwrap_or(at);
    let mut first = at;
    let mut inclusive = true;
    let mut include_white = false;
    let mut count = count.max(1);
    let failed = |pos: &Pos<'_, _>, start| Worded {
        start,
        end: pos.at,
        inclusive: true,
    };
    // Not more than a character selected: the word or the blanks here.
    if anchor.is_none_or(|anchor| anchor == at) {
        pos.back_in_line();
        first = pos.at;
        if (pos.class() == Class::Blank) == include {
            if !pos.end_word(1, true, true) {
                return Err(failed(&pos, start));
            }
        } else {
            // A word with the blanks after it, or blanks without the
            // word: to just before the next word, or back onto the end
            // of the line, from the next's start.
            pos.fwd_word(1, true);
            if pos.at.col == 0 {
                pos.decl();
            } else {
                pos.oneleft();
            }
            include_white = include;
        }
        start = first;
        count -= 1;
    }
    // On, `count` more: back if the cursor is before the anchor.
    while count > 0 {
        inclusive = true;
        if anchor.is_some() && pos.at < start {
            if pos.decl() == Step::Stuck {
                return Err(failed(&pos, start));
            }
            if include == (pos.class() == Class::Blank) {
                if !pos.bck_word(1, true) {
                    return Err(failed(&pos, start));
                }
            } else {
                if !pos.bckend_word(1, true) {
                    return Err(failed(&pos, start));
                }
                pos.incl();
            }
        } else {
            if pos.incl() == Step::Stuck {
                return Err(failed(&pos, start));
            }
            if include != (pos.class() == Class::Blank) {
                if !pos.fwd_word(1, true) && count > 1 {
                    return Err(failed(&pos, start));
                }
                // Just past a line break, not the next line's first
                // character.
                if !pos.oneleft() {
                    inclusive = false;
                }
            } else if !pos.end_word(1, true, true) {
                return Err(failed(&pos, start));
            }
        }
        count -= 1;
    }
    // A word with no blanks after it takes those before it, but not a
    // line's indent, as `daw` on a sentence's last word.
    if include_white && (pos.class() != Class::Blank || (pos.at.col == 0 && !inclusive)) {
        let end = pos.at;
        pos.at = first;
        if pos.oneleft() {
            pos.back_in_line();
            if pos.class() == Class::Blank && pos.at.col > 0 {
                start = pos.at;
            }
        }
        pos.at = end;
    }
    Ok(Worded {
        start,
        end: pos.at,
        inclusive,
    })
}

#[cfg(test)]
thread_local! {
    /// How many times a [`Pos`] has looked a line up.
    pub(crate) static LINES_LOOKED_UP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
