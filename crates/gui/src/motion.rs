//! Moving the normal-mode cursor through a terminal's history and screen.
//!
//! The cursor moves over the whole document: history lines, numbered as
//! they scrolled off, then the screen's rows numbered on from there. A
//! line keeps its number as output scrolls it into history, so a cursor on
//! a line stays on it. Motions follow Vim's.

use clackatoo_term::Row;

/// A position in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub(crate) line: u64,
    pub(crate) col: usize,
}

/// The lines the cursor moves over.
pub(crate) trait Lines {
    /// Line `line`, or `None` if it isn't to hand (not fetched yet), which
    /// counts as blank.
    fn row(&self, line: u64) -> Option<&Row>;
    /// The first and last lines, inclusive.
    fn bounds(&self) -> (u64, u64);
}

/// A way to move the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    Left,
    Right,
    Up(u64),
    Down(u64),
    /// `0`: the line's first column.
    LineStart,
    /// `$`: the line's last character, and stay at line ends when moving
    /// up and down.
    LineEnd,
    /// `w`: the start of the next word.
    WordStart,
    /// `b`: the start of this word, or the previous one.
    WordBack,
    /// `e`: the end of this word, or the next one.
    WordEnd,
    /// `gg`: the first line.
    First,
    /// `G`: the last line.
    Last,
}

/// The normal-mode cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) at: Point,
    /// The column moving up and down aims for, as in Vim: it survives
    /// passing through shorter lines. `usize::MAX` keeps to line ends.
    want: usize,
}

/// What kind of character a cell holds, for word motions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Blank,
    /// Letters, digits and `_`.
    Word,
    /// Any other printing character.
    Punctuation,
}

impl Cursor {
    pub(crate) fn new(at: Point) -> Self {
        Self { at, want: at.col }
    }

    /// Moves by `motion`, staying within `lines`.
    pub(crate) fn apply(&mut self, motion: Motion, lines: &impl Lines) {
        let (first, last) = lines.bounds();
        let at = self.at;
        match motion {
            Motion::Left => {
                self.at.col = previous_cell(lines.row(at.line), at.col).unwrap_or(at.col);
                self.want = self.at.col;
            }
            Motion::Right => {
                let row = lines.row(at.line);
                if let Some(col) = next_cell(row, at.col).filter(|&col| col < text_end(row)) {
                    self.at.col = col;
                }
                self.want = self.at.col;
            }
            Motion::Up(n) => self.go_to_line(at.line.saturating_sub(n).max(first), lines),
            Motion::Down(n) => self.go_to_line(at.line.saturating_add(n).min(last), lines),
            Motion::LineStart => {
                self.at.col = 0;
                self.want = 0;
            }
            Motion::LineEnd => {
                self.at.col = last_char(lines.row(at.line));
                self.want = usize::MAX;
            }
            Motion::First => {
                self.at = Point {
                    line: first,
                    col: 0,
                };
                self.want = 0;
            }
            Motion::Last => {
                self.at = Point { line: last, col: 0 };
                self.want = 0;
            }
            Motion::WordStart => self.word_start(lines),
            Motion::WordBack => self.word_back(lines),
            Motion::WordEnd => self.word_end(lines),
        }
    }

    /// Moves to `line`, aiming for the wanted column.
    fn go_to_line(&mut self, line: u64, lines: &impl Lines) {
        let row = lines.row(line);
        let col = self.want.min(last_char(row));
        self.at = Point {
            line,
            col: start_of_cell(row, col),
        };
    }

    fn word_start(&mut self, lines: &impl Lines) {
        let mut walk = Walk::new(lines, self.at);
        let class = walk.class();
        while class != Class::Blank && walk.class() == class {
            if !walk.forward() {
                return self.settle(walk.at);
            }
        }
        while walk.class() == Class::Blank {
            if !walk.forward() {
                break;
            }
        }
        self.settle(walk.at);
    }

    fn word_end(&mut self, lines: &impl Lines) {
        let mut walk = Walk::new(lines, self.at);
        if !walk.forward() {
            return;
        }
        while walk.class() == Class::Blank {
            if !walk.forward() {
                return self.settle(walk.at);
            }
        }
        let class = walk.class();
        while walk.peek_forward() == Some(class) {
            walk.forward();
        }
        self.settle(walk.at);
    }

    fn word_back(&mut self, lines: &impl Lines) {
        let mut walk = Walk::new(lines, self.at);
        if !walk.back() {
            return;
        }
        while walk.class() == Class::Blank {
            if !walk.back() {
                return self.settle(walk.at);
            }
        }
        let class = walk.class();
        while walk.peek_back() == Some(class) {
            walk.back();
        }
        self.settle(walk.at);
    }

    /// Lands on `at`, pulled back from a line's end onto its last
    /// character.
    fn settle(&mut self, at: Point) {
        self.at = at;
        self.want = at.col;
    }
}

/// A position moving cell by cell through the document, with each line's
/// end as a blank between it and the next.
struct Walk<'a, L> {
    lines: &'a L,
    at: Point,
}

impl<'a, L: Lines> Walk<'a, L> {
    fn new(lines: &'a L, at: Point) -> Self {
        Self { lines, at }
    }

    fn class(&self) -> Class {
        class_at(self.lines.row(self.at.line), self.at.col)
    }

    /// Steps to the next cell, or the next line's start. Returns false at
    /// the document's end.
    fn forward(&mut self) -> bool {
        match self.next() {
            Some(next) => {
                self.at = next;
                true
            }
            None => false,
        }
    }

    fn back(&mut self) -> bool {
        match self.previous() {
            Some(previous) => {
                self.at = previous;
                true
            }
            None => false,
        }
    }

    fn peek_forward(&self) -> Option<Class> {
        self.next()
            .map(|at| class_at(self.lines.row(at.line), at.col))
    }

    fn peek_back(&self) -> Option<Class> {
        self.previous()
            .map(|at| class_at(self.lines.row(at.line), at.col))
    }

    fn next(&self) -> Option<Point> {
        let row = self.lines.row(self.at.line);
        // A line's end is the position just past its text.
        match next_cell(row, self.at.col) {
            Some(col) if col <= text_end(row) && self.at.col < text_end(row) => Some(Point {
                line: self.at.line,
                col,
            }),
            _ if self.at.line < self.lines.bounds().1 => Some(Point {
                line: self.at.line + 1,
                col: 0,
            }),
            _ => None,
        }
    }

    fn previous(&self) -> Option<Point> {
        if self.at.col > 0 {
            let row = self.lines.row(self.at.line);
            return previous_cell(row, self.at.col).map(|col| Point {
                line: self.at.line,
                col,
            });
        }
        if self.at.line > self.lines.bounds().0 {
            let line = self.at.line - 1;
            return Some(Point {
                line,
                col: text_end(self.lines.row(line)),
            });
        }
        None
    }
}

/// The class of the cell at `col`, with past a line's text blank.
fn class_at(row: Option<&Row>, col: usize) -> Class {
    let Some(cell) = row.and_then(|row| row.cells().get(col)) else {
        return Class::Blank;
    };
    let ch = cell.ch();
    if ch == ' ' && !cell.is_wide_spacer() {
        Class::Blank
    } else if ch.is_alphanumeric() || ch == '_' || cell.is_wide() {
        Class::Word
    } else {
        Class::Punctuation
    }
}

/// Where the line's text ends: one past its last non-blank cell.
fn text_end(row: Option<&Row>) -> usize {
    row.map_or(0, |row| {
        row.cells()
            .iter()
            .rposition(|cell| cell.ch() != ' ' || cell.is_wide_spacer())
            .map_or(0, |last| last + 1)
    })
}

/// The column of the line's last character, or 0 for a blank line.
fn last_char(row: Option<&Row>) -> usize {
    start_of_cell(row, text_end(row).saturating_sub(1))
}

/// The cell after the one at `col`, stepping over a wide character's
/// right half.
fn next_cell(row: Option<&Row>, col: usize) -> Option<usize> {
    let cells = row?.cells();
    let mut next = col + 1;
    while cells.get(next).is_some_and(|cell| cell.is_wide_spacer()) {
        next += 1;
    }
    (next <= cells.len()).then_some(next)
}

/// The cell before the one at `col`, landing on a wide character's left
/// half.
fn previous_cell(row: Option<&Row>, col: usize) -> Option<usize> {
    let previous = col.checked_sub(1)?;
    Some(start_of_cell(row, previous))
}

/// `col`, moved left onto a wide character's left half if it's on the
/// right.
fn start_of_cell(row: Option<&Row>, col: usize) -> usize {
    let spacer = row
        .and_then(|row| row.cells().get(col))
        .is_some_and(|cell| cell.is_wide_spacer());
    if spacer { col.saturating_sub(1) } else { col }
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{RowBuilder, Style};

    use super::*;

    struct Text(Vec<Row>);

    impl Text {
        fn new(lines: &[&str]) -> Self {
            Self(
                lines
                    .iter()
                    .map(|text| {
                        let mut builder = RowBuilder::new(20);
                        builder.push(text, &Style::DEFAULT, false);
                        builder.finish(false)
                    })
                    .collect(),
            )
        }
    }

    impl Lines for Text {
        fn row(&self, line: u64) -> Option<&Row> {
            self.0.get(usize::try_from(line).ok()?)
        }

        fn bounds(&self) -> (u64, u64) {
            (0, self.0.len() as u64 - 1)
        }
    }

    fn at(line: u64, col: usize) -> Point {
        Point { line, col }
    }

    /// Where `motions` take a cursor starting at `start`.
    fn moved(text: &Text, start: Point, motions: &[Motion]) -> Point {
        let mut cursor = Cursor::new(start);
        for &motion in motions {
            cursor.apply(motion, text);
        }
        cursor.at
    }

    #[test]
    fn hjkl_stay_on_the_text_and_remember_the_column() {
        let text = Text::new(&["hello world", "hi", "", "a longer line"]);
        assert_eq!(moved(&text, at(0, 0), &[Motion::Left]), at(0, 0));
        assert_eq!(
            moved(&text, at(0, 9), &[Motion::Right, Motion::Right]),
            at(0, 10)
        );
        // Down through shorter lines, and back out to the same column.
        let path = [Motion::Down(1), Motion::Down(1), Motion::Down(1)];
        assert_eq!(moved(&text, at(0, 8), &path[..1]), at(1, 1));
        assert_eq!(moved(&text, at(0, 8), &path[..2]), at(2, 0));
        assert_eq!(moved(&text, at(0, 8), &path), at(3, 8));
        // Not past the first or last line.
        assert_eq!(moved(&text, at(0, 0), &[Motion::Up(5)]), at(0, 0));
        assert_eq!(moved(&text, at(3, 0), &[Motion::Down(5)]), at(3, 0));
    }

    #[test]
    fn line_ends_stick_when_moving_up_and_down() {
        let text = Text::new(&["hello world", "hi", "a longer line"]);
        let end = [Motion::LineEnd, Motion::Down(1), Motion::Down(1)];
        assert_eq!(moved(&text, at(0, 0), &end[..1]), at(0, 10));
        assert_eq!(moved(&text, at(0, 0), &end), at(2, 12));
        assert_eq!(moved(&text, at(2, 5), &[Motion::LineStart]), at(2, 0));
    }

    #[test]
    fn word_motions_cross_punctuation_and_lines() {
        let text = Text::new(&["let x = foo(bar);", "  next_line"]);
        let w = |n: usize, from: Point| moved(&text, from, &vec![Motion::WordStart; n]);
        assert_eq!(w(1, at(0, 0)), at(0, 4));
        assert_eq!(w(3, at(0, 0)), at(0, 8));
        assert_eq!(w(4, at(0, 0)), at(0, 11));
        // From the last word, w moves to the next line's first word.
        assert_eq!(w(1, at(0, 16)), at(1, 2));

        assert_eq!(moved(&text, at(0, 8), &[Motion::WordEnd]), at(0, 10));
        assert_eq!(moved(&text, at(0, 10), &[Motion::WordEnd]), at(0, 11));
        // ");" is one run of punctuation, so one word.
        assert_eq!(moved(&text, at(1, 2), &[Motion::WordBack]), at(0, 15));
        assert_eq!(moved(&text, at(0, 10), &[Motion::WordBack]), at(0, 8));
    }

    #[test]
    fn wide_characters_are_one_position() {
        let text = Text::new(&["a字b"]);
        assert_eq!(moved(&text, at(0, 0), &[Motion::Right]), at(0, 1));
        assert_eq!(moved(&text, at(0, 1), &[Motion::Right]), at(0, 3));
        assert_eq!(moved(&text, at(0, 3), &[Motion::Left]), at(0, 1));
    }

    #[test]
    fn missing_lines_count_as_blank() {
        let text = Text::new(&["one", "two"]);
        let mut cursor = Cursor::new(at(1, 2));
        cursor.apply(Motion::First, &text);
        assert_eq!(cursor.at, at(0, 0));
        cursor.apply(Motion::Last, &text);
        assert_eq!(cursor.at, at(1, 0));
    }
}
