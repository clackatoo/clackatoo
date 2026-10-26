//! [`Steps`]: a position stepped a character at a time as Vim's `inc()`,
//! `dec()`, `incl()` and `decl()` step one, each line's end a position of
//! its own, just past its last character, where Vim reads its NUL. Ports
//! of Vim's C that walk text this way, as its sentences and tags do, read
//! as the C reads with it.

use std::cell::RefCell;

use super::{Cells, Lines, Point, line_end_in, next_cell, previous_cell};

/// What a step did, as Vim's `inc()` and `dec()` say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Along the line, onto a character.
    Along,
    /// Onto another line: its start going on, its end going back.
    Line,
    /// Along the line, onto its end, going on.
    End,
    /// Nowhere: the text's start or end.
    Stuck,
}

impl Step {
    /// Whether it went nowhere, as Vim's -1.
    pub(crate) fn stuck(self) -> bool {
        self == Self::Stuck
    }
}

#[cfg(test)]
thread_local! {
    /// Every step asked for of any `Steps` on this thread, for tests to
    /// count what a command costs through its public way in.
    pub(crate) static STEPS_ASKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The character Vim reads at a line's end.
pub(crate) const NUL: char = '\0';

/// A line's row and where its end is, kept while steps stay on it: an
/// editor lays out a line each time it's asked for.
struct Kept<R> {
    line: u64,
    row: Option<R>,
    end: usize,
}

/// Steps through `lines` as Vim's position functions do, as many as
/// a budget allows: past it, every step is [`Step::Stuck`], as at the
/// text's ends, so a search walking with them fails there.
pub(crate) struct Steps<'a, L: Lines> {
    lines: &'a L,
    kept: RefCell<Option<Kept<L::Row<'a>>>>,
    left: std::cell::Cell<usize>,
    /// Whether a step was asked for past the budget: what was found then
    /// was found short of looking, and is nothing to go by.
    ran_out: std::cell::Cell<bool>,
    /// Every step asked for, past the budget too.
    #[cfg(test)]
    asked: std::cell::Cell<usize>,
}

impl<'a, L: Lines> Steps<'a, L> {
    pub(crate) fn new(lines: &'a L) -> Self {
        Self::budgeted(lines, usize::MAX)
    }

    /// Steps through `lines`, no more than `steps` of them.
    pub(crate) fn budgeted(lines: &'a L, steps: usize) -> Self {
        Self {
            lines,
            kept: RefCell::new(None),
            left: std::cell::Cell::new(steps),
            ran_out: std::cell::Cell::new(false),
            #[cfg(test)]
            asked: std::cell::Cell::new(0),
        }
    }

    /// Spends a step of the budget: false if there's none left.
    fn spend(&self) -> bool {
        #[cfg(test)]
        {
            self.asked.set(self.asked.get() + 1);
            STEPS_ASKED.with(|asked| asked.set(asked.get() + 1));
        }
        let left = self.left.get();
        if left == 0 {
            self.ran_out.set(true);
            return false;
        }
        self.left.set(left - 1);
        true
    }

    /// Whether the budget ran out: a search that did can't say what it
    /// found, as it stopped short.
    pub(crate) fn ran_out(&self) -> bool {
        self.ran_out.get()
    }

    /// How many steps were asked for, past the budget too.
    #[cfg(test)]
    pub(crate) fn asked(&self) -> usize {
        self.asked.get()
    }

    pub(crate) fn lines(&self) -> &'a L {
        self.lines
    }

    /// `f` of line `line`'s row and where its end is.
    fn on<T>(&self, line: u64, f: impl FnOnce(Option<&L::Line>, usize) -> T) -> T {
        let mut kept = self.kept.borrow_mut();
        if kept.as_ref().is_none_or(|kept| kept.line != line) {
            let row = self.lines.row(line);
            let end = line_end_in(line, row.as_deref(), self.lines);
            *kept = Some(Kept { line, row, end });
        }
        let kept = kept.as_ref().expect("just kept");
        f(kept.row.as_deref(), kept.end)
    }

    /// Where line `line` ends: just past its last character, 0 if it's
    /// empty.
    pub(crate) fn end(&self, line: u64) -> usize {
        self.on(line, |_, end| end)
    }

    /// The character at `at`, as Vim's `gchar_pos()` reads it: [`NUL`] at
    /// a line's end.
    pub(crate) fn char(&self, at: Point) -> char {
        self.on(at.line, |row, end| {
            if at.col >= end {
                return NUL;
            }
            row.and_then(|row| row.cell(at.col))
                .map_or(' ', |spot| spot.ch)
        })
    }

    /// Whether line `line` is empty, as Vim's `LINEEMPTY()`.
    pub(crate) fn empty(&self, line: u64) -> bool {
        self.end(line) == 0
    }

    /// Vim's `inc()`: on a character, or to the line's end, or from it to
    /// the next line's start.
    pub(crate) fn inc(&self, at: &mut Point) -> Step {
        if !self.spend() {
            return Step::Stuck;
        }
        let (end, next) = self.on(at.line, |row, end| (end, next_cell(row, at.col)));
        if at.col < end {
            at.col = next.unwrap_or(end).min(end);
            return if at.col >= end {
                Step::End
            } else {
                Step::Along
            };
        }
        if at.line < self.lines.bounds().1 {
            *at = Point {
                line: at.line + 1,
                col: 0,
            };
            return Step::Line;
        }
        Step::Stuck
    }

    /// Vim's `incl()`: as [`inc`](Self::inc), but past a line's end, but
    /// an empty line's.
    pub(crate) fn incl(&self, at: &mut Point) -> Step {
        let step = self.inc(at);
        if matches!(step, Step::Line | Step::End) && at.col != 0 {
            return self.inc(at);
        }
        step
    }

    /// Vim's `dec()`: back a character, or from a line's start to the end
    /// of the line before.
    pub(crate) fn dec(&self, at: &mut Point) -> Step {
        if !self.spend() {
            return Step::Stuck;
        }
        if at.col > 0 {
            let back = self.on(at.line, |row, end| previous_cell(row, at.col.min(end)));
            at.col = back.unwrap_or(0);
            return Step::Along;
        }
        if at.line > self.lines.bounds().0 {
            let line = at.line - 1;
            *at = Point {
                line,
                col: self.end(line),
            };
            return Step::Line;
        }
        Step::Stuck
    }

    /// Vim's `decl()`: as [`dec`](Self::dec), but past the end of the line
    /// before, but an empty line's.
    pub(crate) fn decl(&self, at: &mut Point) -> Step {
        let step = self.dec(at);
        if step == Step::Line && at.col != 0 {
            return self.dec(at);
        }
        step
    }
}

/// Whether `ch` is a blank, as Vim's `ascii_iswhite()`.
pub(crate) fn white(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}
