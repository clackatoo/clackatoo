//! [`Found`], where an object was found, its ends in order whichever way
//! they were found, as Vim's `do_pending()` puts an operator's start before
//! its end: `iw` on an empty last line finds the word's end back on the
//! line before.

use crate::motion::{Cells, Lines, Point, line_end, previous_cell};

/// Where an object was found: from [`start`](Self::start) to
/// [`last`](Self::last), in that order, and whether it's whole lines, as a
/// paragraph is, or characters. Unless [`inclusive`](Self::inclusive),
/// `last` isn't in it but just after it, a line's start, which an operator
/// takes up to as it does an exclusive motion, as Vim's: the end of the
/// line before, or whole lines from the first's indent (see
/// [`taken`](Self::taken)). Its fields are its own, so it's made only by
/// [`new`](Self::new), which puts its ends in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    start: Point,
    last: Point,
    lines: bool,
    inclusive: bool,
}

impl Found {
    /// From one end to the other, `ends` in either order.
    pub(crate) fn new(ends: (Point, Point), lines: bool, inclusive: bool) -> Self {
        let (start, last) = if ends.0 <= ends.1 {
            ends
        } else {
            (ends.1, ends.0)
        };
        Self {
            start,
            last,
            lines,
            inclusive,
        }
    }

    /// Where it starts.
    pub(crate) fn start(self) -> Point {
        self.start
    }

    /// Where it ends, in it if [`inclusive`](Self::inclusive).
    pub(crate) fn last(self) -> Point {
        self.last
    }

    /// Whether it's whole lines.
    pub(crate) fn lines(self) -> bool {
        self.lines
    }

    /// Whether [`last`](Self::last) is in it.
    pub(crate) fn inclusive(self) -> bool {
        self.inclusive
    }

    /// What an operator takes of it, as Vim's `do_pending()` takes an
    /// exclusive end at a line's start: to the end of the line before, or
    /// from at or before the first line's first non-blank, whole lines.
    /// None if that's nothing. The editor's operators take it as they do
    /// any exclusive motion; this is for what only copies.
    pub(crate) fn taken(self, lines: &impl Lines) -> Option<Self> {
        if self.inclusive {
            return Some(self);
        }
        let Self { start, last, .. } = self;
        // Up to a character on the line: the one before.
        if last.col > 0 {
            let row = lines.row(last.line);
            let col = previous_cell(row.as_deref(), last.col)?;
            let last = Point { col, ..last };
            return (start <= last).then_some(Self {
                last,
                inclusive: true,
                ..self
            });
        }
        if last.line <= start.line {
            return None;
        }
        let line = last.line - 1;
        let row = lines.row(start.line);
        let indent = row.as_deref().map_or(0, |row| {
            (row.on_from(0))
                .find(|(_, spot)| !spot.blank())
                .map_or(usize::MAX, |(col, _)| col)
        });
        Some(Self {
            start,
            last: Point {
                line,
                col: lines.last_cell(line),
            },
            lines: start.col <= indent,
            inclusive: true,
        })
    }

    /// Where a selection of it ends: just past the end of the line before
    /// a line's start it's up to, so the line break's in it, as Vim's.
    pub(super) fn selected_last(self, lines: &impl Lines) -> Point {
        if self.inclusive || self.last.line == 0 {
            return self.last;
        }
        let line = self.last.line - 1;
        Point {
            line,
            col: line_end(line, lines),
        }
    }
}
