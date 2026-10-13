//! Moving the normal-mode cursor through a terminal's history and screen.
//!
//! The cursor moves over the whole document: history lines, numbered as
//! they scrolled off, then the screen's rows numbered on from there. A
//! line keeps its number as output scrolls it into history, so a cursor on
//! a line stays on it. Motions follow Vim's.

use std::rc::Rc;

use clackatoo_term::Row;

/// A position in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub(crate) line: u64,
    pub(crate) col: usize,
}

/// The lines the cursor moves over.
pub(crate) trait Lines {
    /// A line: borrowed from a terminal's screen or history, or for a
    /// file's text, made as it's asked for.
    type Row<'a>: std::ops::Deref<Target = Row>
    where
        Self: 'a;

    /// Line `line`, or `None` if it isn't to hand (not fetched yet), which
    /// counts as blank.
    fn row(&self, line: u64) -> Option<Self::Row<'_>>;
    /// The first and last lines, inclusive.
    fn bounds(&self) -> (u64, u64);

    /// Whether line `line` is empty, as a paragraph ends at. A terminal's
    /// line can't tell blanks typed from none, so it's empty if blank.
    fn empty(&self, line: u64) -> bool {
        text_end(self.row(line).as_deref()) == 0
    }

    /// The screen rows line `line` is drawn on, in order: one, unless it
    /// wraps, as an editor's long lines do with `wrap`.
    fn screen_rows(&self, _line: u64) -> Rc<[ScreenRow]> {
        Rc::new([ScreenRow::WHOLE])
    }

    /// Where line `line`'s row stops, if the line goes on past it: an
    /// editor's line longer than a row can hold. Motions that read the row
    /// can't see past it.
    fn overflows(&self, _line: u64) -> Option<usize> {
        None
    }

    /// The column of line `line`'s last character, as `$` goes to, or 0 for
    /// a blank line.
    fn last_cell(&self, line: u64) -> usize {
        last_char(self.row(line).as_deref())
    }

    /// The cells a screen row has, where lines wrap: `None` where they
    /// don't.
    fn row_width(&self) -> Option<usize> {
        None
    }
}

/// One of the screen rows a line is drawn on, when it wraps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScreenRow {
    /// The line's first cell on it.
    pub(crate) start: usize,
    /// The cells drawn before its text: on a row the line goes on over,
    /// its indent and `showbreak`.
    pub(crate) indent: usize,
}

impl ScreenRow {
    /// A line's only row, when it doesn't wrap.
    pub(crate) const WHOLE: Self = Self {
        start: 0,
        indent: 0,
    };
}

/// A screen row of the document: line `line`'s `row`th, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RowAt {
    pub(crate) line: u64,
    pub(crate) row: usize,
}

/// Which of `rows` cell `col` of their line is on.
pub(crate) fn row_index(rows: &[ScreenRow], col: usize) -> usize {
    rows.partition_point(|row| row.start <= col)
        .saturating_sub(1)
}

/// The screen row `at` is on, in `lines`.
pub(crate) fn row_at(at: Point, lines: &impl Lines) -> RowAt {
    RowAt {
        line: at.line,
        row: row_index(&lines.screen_rows(at.line), at.col),
    }
}

/// Line `line`'s last screen row, in `lines`.
pub(crate) fn last_row(lines: &impl Lines, line: u64) -> RowAt {
    RowAt {
        line,
        row: lines.screen_rows(line).len() - 1,
    }
}

/// The screen row `n` rows down from `from`, or line `end`'s last if
/// that's sooner. Lays out only the lines it passes.
pub(crate) fn rows_down(from: RowAt, mut n: u64, end: u64, lines: &impl Lines) -> RowAt {
    let mut at = from;
    loop {
        let count = lines.screen_rows(at.line).len();
        let left = (count - 1).saturating_sub(at.row) as u64;
        if n <= left {
            return RowAt {
                line: at.line,
                row: at.row + usize::try_from(n).unwrap_or(usize::MAX),
            };
        }
        if at.line >= end {
            return RowAt {
                line: at.line,
                row: count - 1,
            };
        }
        n -= left + 1;
        at = RowAt {
            line: at.line + 1,
            row: 0,
        };
    }
}

/// The screen row `n` rows up from `from`, or line `first`'s first if
/// that's sooner. Lays out only the lines it passes.
pub(crate) fn rows_up(from: RowAt, mut n: u64, first: u64, lines: &impl Lines) -> RowAt {
    let mut at = from;
    loop {
        if n <= at.row as u64 {
            return RowAt {
                line: at.line,
                row: at.row - usize::try_from(n).unwrap_or(0),
            };
        }
        if at.line <= first {
            return RowAt {
                line: at.line,
                row: 0,
            };
        }
        n -= at.row as u64 + 1;
        at.line -= 1;
        at.row = lines.screen_rows(at.line).len() - 1;
    }
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
    /// `^`: the line's first character that isn't blank.
    FirstNonBlank,
    /// `W`: the start of the next run of anything but blanks.
    BigWordStart,
    /// `B`: the start of this run of anything but blanks, or the previous.
    BigWordBack,
    /// `E`: the end of this run of anything but blanks, or the next.
    BigWordEnd,
    /// `f`, `t`, `F` and `T`: the next `ch` on the line, or the previous
    /// one if not `forward`, or the character just before it if `till`.
    Find {
        ch: char,
        forward: bool,
        till: bool,
    },
    /// `%`: the bracket that pairs with the one under the cursor, or with
    /// the next one on its line.
    Match,
    /// `}`: the next blank line after the paragraph, or `{` the blank line
    /// before it if not `forward`.
    Paragraph {
        forward: bool,
    },
    /// `gk`: up by screen rows, where lines wrap, rather than by lines.
    RowUp(u64),
    /// `gj`: down by screen rows.
    RowDown(u64),
    /// `g0`: the screen row's first character.
    RowStart,
    /// `g$`: the screen row's last character.
    RowEnd,
}

/// What an operator such as `y` takes when given a motion, as in Vim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// The characters up to where the motion goes, but not that one.
    Exclusive,
    /// The characters up to where the motion goes, and that one.
    Inclusive,
    /// Every line from the cursor's to the motion's, whole.
    Lines,
}

impl Motion {
    /// What an operator takes with it from `from` to `to`, in `lines`; or
    /// `None` if the motion went nowhere it could, as `%` with no bracket,
    /// which takes nothing.
    pub(crate) fn reach_from(self, from: Point, to: Point, lines: &impl Lines) -> Option<Reach> {
        match self {
            Self::Match | Self::Find { .. } if from == to => None,
            // `}` with no empty line after takes the last line's end.
            Self::Paragraph { forward: true } if !lines.empty(to.line) => Some(Reach::Inclusive),
            _ => Some(self.reach()),
        }
    }

    pub(crate) fn reach(self) -> Reach {
        match self {
            Self::Left
            | Self::Right
            | Self::LineStart
            | Self::FirstNonBlank
            | Self::WordStart
            | Self::WordBack
            | Self::BigWordStart
            | Self::BigWordBack
            | Self::Paragraph { .. }
            // As in Vim, moving by rows isn't by lines.
            | Self::RowUp(_)
            | Self::RowDown(_)
            | Self::RowStart => Reach::Exclusive,
            Self::Find { forward, .. } => {
                if forward {
                    Reach::Inclusive
                } else {
                    Reach::Exclusive
                }
            }
            Self::LineEnd | Self::WordEnd | Self::BigWordEnd | Self::Match | Self::RowEnd => {
                Reach::Inclusive
            }
            Self::Up(_) | Self::Down(_) | Self::First | Self::Last => Reach::Lines,
        }
    }
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
pub(crate) enum Class {
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

    /// A cursor at `at`, aiming for column `want` moving up and down.
    pub(crate) fn wanting(at: Point, want: usize) -> Self {
        Self { at, want }
    }

    /// The column moving up and down aims for: `usize::MAX` after `$`.
    pub(crate) fn want(&self) -> usize {
        self.want
    }

    /// Moves by `motion`, staying within `lines`.
    pub(crate) fn apply(&mut self, motion: Motion, lines: &impl Lines) {
        let (first, last) = lines.bounds();
        let at = self.at;
        match motion {
            Motion::Left => {
                let row = lines.row(at.line);
                self.at.col = previous_cell(row.as_deref(), at.col).unwrap_or(at.col);
                self.want = self.at.col;
            }
            Motion::Right => {
                let row = lines.row(at.line);
                let row = row.as_deref();
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
                self.at.col = lines.last_cell(at.line);
                self.want = usize::MAX;
            }
            Motion::FirstNonBlank => {
                let row = lines.row(at.line);
                let row = row.as_deref();
                let end = text_end(row);
                let mut col = 0;
                while col < end && class_at(row, col) == Class::Blank {
                    col = next_cell(row, col).unwrap_or(end);
                }
                self.at.col = start_of_cell(row, col.min(end.saturating_sub(1)));
                self.want = self.at.col;
            }
            Motion::Find { ch, forward, till } => {
                let row = lines.row(at.line);
                if let Some(col) = find(row.as_deref(), at.col, ch, forward, till) {
                    self.at.col = col;
                    self.want = col;
                }
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
            Motion::WordStart => self.word_start(lines, Class::of),
            Motion::WordBack => self.word_back(lines, Class::of),
            Motion::WordEnd => self.word_end(lines, Class::of),
            Motion::BigWordStart => self.word_start(lines, Class::big),
            Motion::BigWordBack => self.word_back(lines, Class::big),
            Motion::BigWordEnd => self.word_end(lines, Class::big),
            Motion::Match => {
                if let Some(at) = pair(at, lines) {
                    self.at = at;
                    self.want = at.col;
                }
            }
            Motion::Paragraph { forward } => {
                let line = paragraph_end(at.line, forward, lines);
                // With no empty line after it, `}` goes to the end of the
                // last line, as Vim's does.
                let col = if forward && !lines.empty(line) {
                    lines.last_cell(line)
                } else {
                    0
                };
                self.at = Point { line, col };
                self.want = col;
            }
            Motion::RowUp(n) => self.go_to_row(rows_up(row_at(at, lines), n, first, lines), lines),
            Motion::RowDown(n) => {
                self.go_to_row(rows_down(row_at(at, lines), n, last, lines), lines);
            }
            Motion::RowStart | Motion::RowEnd => {
                let rows = lines.screen_rows(at.line);
                let index = row_index(&rows, at.col);
                let col = match motion {
                    Motion::RowStart => rows[index].start,
                    _ => usize::MAX,
                };
                self.at.col = on_row(lines, at.line, &rows, index, col);
                self.want = self.at.col;
            }
        }
    }

    /// Moves to screen row `to`, as `gj` and `gk` do: as far from its left
    /// edge as the wanted column is from the left edge of the row it's on,
    /// so passing a short row doesn't lose it, but no further than a row
    /// is wide, as Vim keeps it. `$` keeps to rows' ends.
    pub(crate) fn go_to_row(&mut self, to: RowAt, lines: &impl Lines) {
        let here = lines.screen_rows(self.at.line);
        let here = here[row_index(&here, self.at.col)];
        let rows = lines.screen_rows(to.line);
        let index = to.row.min(rows.len() - 1);
        let col = if self.want == usize::MAX {
            usize::MAX
        } else {
            let offset = here.indent + self.want.saturating_sub(here.start);
            let offset = match lines.row_width() {
                Some(width) => offset.min(width.saturating_sub(1)),
                None => offset,
            };
            rows[index].start + offset.saturating_sub(rows[index].indent)
        };
        self.at = Point {
            line: to.line,
            col: on_row(lines, to.line, &rows, index, col),
        };
        self.want = col;
    }

    /// Moves to `line`, aiming for the wanted column.
    fn go_to_line(&mut self, line: u64, lines: &impl Lines) {
        let row = lines.row(line);
        let row = row.as_deref();
        let col = self.want.min(lines.last_cell(line));
        self.at = Point {
            line,
            col: start_of_cell(row, col),
        };
    }

    fn word_start(&mut self, lines: &impl Lines, kind: fn(Class) -> Class) {
        let mut walk = Walk::new(lines, self.at).seeing(kind);
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

    fn word_end(&mut self, lines: &impl Lines, kind: fn(Class) -> Class) {
        let mut walk = Walk::new(lines, self.at).seeing(kind);
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

    fn word_back(&mut self, lines: &impl Lines, kind: fn(Class) -> Class) {
        let mut walk = Walk::new(lines, self.at).seeing(kind);
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

impl Class {
    fn of(class: Self) -> Self {
        class
    }

    /// For `W`, `B` and `E`: anything but blanks is one kind.
    fn big(class: Self) -> Self {
        match class {
            Self::Blank => Self::Blank,
            Self::Word | Self::Punctuation => Self::Word,
        }
    }
}

/// A position moving cell by cell through the document, with each line's
/// end as a blank between it and the next.
pub(crate) struct Walk<'a, L> {
    lines: &'a L,
    at: Point,
    kind: fn(Class) -> Class,
}

impl<'a, L: Lines> Walk<'a, L> {
    pub(crate) fn new(lines: &'a L, at: Point) -> Self {
        Self {
            lines,
            at,
            kind: Class::of,
        }
    }

    /// Sees classes as `kind` does: as `W` sees them, say.
    fn seeing(self, kind: fn(Class) -> Class) -> Self {
        Self { kind, ..self }
    }

    pub(crate) fn at(&self) -> Point {
        self.at
    }

    fn class(&self) -> Class {
        self.class_of(self.at)
    }

    fn class_of(&self, at: Point) -> Class {
        (self.kind)(class_at(self.lines.row(at.line).as_deref(), at.col))
    }

    /// Steps to the next cell, or the next line's start. Returns false at
    /// the document's end.
    pub(crate) fn forward(&mut self) -> bool {
        match self.next() {
            Some(next) => {
                self.at = next;
                true
            }
            None => false,
        }
    }

    pub(crate) fn back(&mut self) -> bool {
        match self.previous() {
            Some(previous) => {
                self.at = previous;
                true
            }
            None => false,
        }
    }

    fn peek_forward(&self) -> Option<Class> {
        self.next().map(|at| self.class_of(at))
    }

    fn peek_back(&self) -> Option<Class> {
        self.previous().map(|at| self.class_of(at))
    }

    fn next(&self) -> Option<Point> {
        let row = self.lines.row(self.at.line);
        let row = row.as_deref();
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
            return previous_cell(row.as_deref(), self.at.col).map(|col| Point {
                line: self.at.line,
                col,
            });
        }
        if self.at.line > self.lines.bounds().0 {
            let line = self.at.line - 1;
            return Some(Point {
                line,
                col: text_end(self.lines.row(line).as_deref()),
            });
        }
        None
    }
}

/// The brackets `%` pairs, opening first.
const PAIRS: [(char, char); 3] = [('(', ')'), ('[', ']'), ('{', '}')];

/// How many lines `%` looks through for a pair, so a lone bracket in a
/// terminal's long history doesn't read all of it.
const MATCH_REACH: u64 = 10_000;

/// Where the bracket that pairs with the one at `at` is, or with the next
/// bracket on its line after `at`, counting brackets of its kind between.
fn pair(at: Point, lines: &impl Lines) -> Option<Point> {
    let row = lines.row(at.line)?;
    let (col, ch) = (row.cells().iter().enumerate().skip(at.col))
        .map(|(col, cell)| (col, cell.ch()))
        .find(|(_, ch)| {
            PAIRS
                .iter()
                .any(|&(open, close)| *ch == open || *ch == close)
        })?;
    let &(open, close) = PAIRS
        .iter()
        .find(|&&(open, close)| ch == open || ch == close)?;
    let forward = ch == open;
    let (first, last) = lines.bounds();
    let mut depth = 0_usize;
    let mut line = at.line;
    let mut from = Some(col);
    loop {
        // A line not to hand might hold a bracket that pairs.
        let row = lines.row(line)?;
        let cells = row.cells();
        let cols: Box<dyn Iterator<Item = usize>> = match (forward, from) {
            (true, Some(from)) => Box::new(from..cells.len()),
            (true, None) => Box::new(0..cells.len()),
            (false, Some(from)) => Box::new((0..=from).rev()),
            (false, None) => Box::new((0..cells.len()).rev()),
        };
        for col in cols {
            let ch = cells[col].ch();
            if ch == ch_of(forward, open, close) {
                depth += 1;
            } else if ch == ch_of(!forward, open, close) {
                depth -= 1;
                if depth == 0 {
                    return Some(Point { line, col });
                }
            }
        }
        from = None;
        let reach = line.abs_diff(at.line) >= MATCH_REACH;
        line = match forward {
            true if line < last && !reach => line + 1,
            false if line > first && !reach => line - 1,
            _ => return None,
        };
    }
}

/// The bracket going `forward` counts in, `open` or `close`.
fn ch_of(forward: bool, open: char, close: char) -> char {
    if forward { open } else { close }
}

/// The line `{` or `}` goes to from `line`: past the empty lines next to
/// it, then past the paragraph's lines, to the first empty one, or the
/// first or last line.
fn paragraph_end(line: u64, forward: bool, lines: &impl Lines) -> u64 {
    let (first, last) = lines.bounds();
    let blank = |line| lines.empty(line);
    let step = |line: u64| {
        if forward {
            (line < last).then(|| line + 1)
        } else {
            (line > first).then(|| line - 1)
        }
    };
    let mut line = line;
    while let Some(next) = step(line).filter(|&next| blank(next)) {
        line = next;
    }
    while let Some(next) = step(line) {
        line = next;
        if blank(line) {
            break;
        }
    }
    line
}

/// The class of the cell at `col`, with past a line's text blank.
pub(crate) fn class_at(row: Option<&Row>, col: usize) -> Class {
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
pub(crate) fn text_end(row: Option<&Row>) -> usize {
    row.map_or(0, |row| {
        row.cells()
            .iter()
            .rposition(|cell| cell.ch() != ' ' || cell.is_wide_spacer())
            .map_or(0, |last| last + 1)
    })
}

/// The column of the line's last character, or 0 for a blank line.
pub(crate) fn last_char(row: Option<&Row>) -> usize {
    start_of_cell(row, text_end(row).saturating_sub(1))
}

/// The character nearest `col` on screen row `index` of line `line` of
/// `lines`, which is drawn on `rows`.
fn on_row(lines: &impl Lines, line: u64, rows: &[ScreenRow], index: usize, col: usize) -> usize {
    let start = rows[index].start;
    let col = match rows.get(index + 1) {
        Some(next) => col.clamp(start, next.start.saturating_sub(1).max(start)),
        None => col.min(lines.last_cell(line).max(start)),
    };
    start_of_cell(lines.row(line).as_deref(), col)
}

/// The cell after the one at `col`, stepping over a wide character's
/// right half.
pub(crate) fn next_cell(row: Option<&Row>, col: usize) -> Option<usize> {
    let cells = row?.cells();
    let mut next = col + 1;
    while cells.get(next).is_some_and(|cell| cell.is_wide_spacer()) {
        next += 1;
    }
    (next <= cells.len()).then_some(next)
}

/// The cell before the one at `col`, landing on a wide character's left
/// half.
pub(crate) fn previous_cell(row: Option<&Row>, col: usize) -> Option<usize> {
    let previous = col.checked_sub(1)?;
    Some(start_of_cell(row, previous))
}

/// The cell of `ch` after `col` on the row, or before it if not `forward`,
/// or the cell next to that one, towards `col`, if `till`.
fn find(row: Option<&Row>, col: usize, ch: char, forward: bool, till: bool) -> Option<usize> {
    let cells = row?.cells();
    let found = if forward {
        (col + 1..cells.len()).find(|&at| cells[at].ch() == ch && !cells[at].is_wide_spacer())?
    } else {
        (0..col.min(cells.len()))
            .rev()
            .find(|&at| cells[at].ch() == ch && !cells[at].is_wide_spacer())?
    };
    if !till {
        return Some(found);
    }
    let near = if forward {
        previous_cell(row, found)?
    } else {
        next_cell(row, found)?
    };
    (near != col).then_some(near)
}

/// The last character before `end`, from `start`, for an exclusive
/// motion: none if they're the same place. As in Vim, a motion that ends
/// at the start of a line stops at the end of the line before.
pub(crate) fn before(lines: &impl Lines, start: Point, end: Point) -> Option<Point> {
    if end <= start {
        return None;
    }
    if end.col == 0 && end.line > start.line {
        let line = end.line - 1;
        let col = text_end(lines.row(line).as_deref()).saturating_sub(1);
        return Some(Point { line, col }.max(start));
    }
    let col = previous_cell(lines.row(end.line).as_deref(), end.col)?;
    Some(Point {
        line: end.line,
        col,
    })
}

/// `col`, moved left onto a wide character's left half if it's on the
/// right.
pub(crate) fn start_of_cell(row: Option<&Row>, col: usize) -> usize {
    let spacer = row
        .and_then(|row| row.cells().get(col))
        .is_some_and(|cell| cell.is_wide_spacer());
    if spacer { col.saturating_sub(1) } else { col }
}

#[cfg(test)]
pub(crate) mod tests {
    use clackatoo_term::{RowBuilder, Style};

    use super::*;

    /// Lines of text, 20 columns wide, numbered from 0.
    pub(crate) struct Text(Vec<Row>);

    impl Text {
        pub(crate) fn new(lines: &[&str]) -> Self {
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
        type Row<'a> = &'a Row;

        fn row(&self, line: u64) -> Option<&Row> {
            self.0.get(usize::try_from(line).ok()?)
        }

        fn bounds(&self) -> (u64, u64) {
            (0, self.0.len() as u64 - 1)
        }
    }

    pub(crate) fn at(line: u64, col: usize) -> Point {
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
