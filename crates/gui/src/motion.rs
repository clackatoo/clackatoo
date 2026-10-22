//! What Vim's motions do, for every pane with a cursor: where each goes
//! over [`Lines`], and what an operator takes with it.
//!
//! A pane gives its text as `Lines`: a terminal's history then screen, as
//! one document whose lines keep their numbers as output scrolls them into
//! history, or an editor's file, laid out as it's drawn. What keys make a
//! motion is [`crate::vim`]'s.

use std::cmp::Ordering;
use std::ops::Deref;
use std::rc::Rc;

use clackatoo_term::{Cell, Row};

/// A position in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Point {
    pub(crate) line: u64,
    pub(crate) col: usize,
}

/// One cell of a line, as the cursor reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Spot {
    /// The character there: a tab in each of a tab's cells, as a file's
    /// line has them, and a blank on a wide character's right half.
    pub(crate) ch: char,
    /// Whether it's a wide character's left half.
    pub(crate) wide: bool,
    /// Whether it's a cell after a character's first, with nothing of its
    /// own: a wide character's right half, or a tab's.
    pub(crate) spacer: bool,
}

impl Spot {
    /// A cell holding a character one cell wide.
    pub(crate) const fn narrow(ch: char) -> Self {
        Self {
            ch,
            wide: false,
            spacer: false,
        }
    }

    /// A wide character's right half.
    pub(crate) const SPACER: Self = Self {
        ch: ' ',
        wide: false,
        spacer: true,
    };

    /// A tab's cells after its first.
    pub(crate) const TAB_REST: Self = Self {
        ch: '\t',
        wide: false,
        spacer: true,
    };

    fn of(cell: Cell) -> Self {
        Self {
            ch: cell.ch(),
            wide: cell.is_wide(),
            spacer: cell.is_wide_spacer(),
        }
    }

    /// Whether it's blank: a space, or a tab's.
    pub(crate) fn blank(self) -> bool {
        self.ch == '\t' || (self.ch == ' ' && !self.spacer)
    }
}

/// A line's cells, as the cursor reads them: a terminal's row, or a file's
/// line laid out, which can be any length.
pub(crate) trait Cells {
    /// How many cells it has.
    fn width(&self) -> usize;

    /// Cell `col`, if it has one.
    fn cell(&self, col: usize) -> Option<Spot>;

    /// The cells from `col` on, in order, with their columns: quicker than
    /// asking for each, as a long line's are.
    fn on_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)>;

    /// The cells from `col` back to the first, with their columns, from the
    /// last if `col` is past it.
    fn back_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)>;

    /// The first and last cells of the character in cell `col`: both `col`
    /// for most, both halves of a wide character, and past the last cell,
    /// `col` itself. A file's line has tabs too, which take several.
    fn extent(&self, col: usize) -> (usize, usize)
    where
        Self: Sized,
    {
        let start = start_of_cell(Some(self), col);
        let wide = self.cell(start).is_some_and(|spot| spot.wide);
        (start, start + usize::from(wide))
    }
}

impl Cells for Row {
    fn width(&self) -> usize {
        self.cells().len()
    }

    fn cell(&self, col: usize) -> Option<Spot> {
        self.cells().get(col).copied().map(Spot::of)
    }

    fn on_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
        let cells = self.cells().get(col..).unwrap_or_default();
        (cells.iter().enumerate()).map(move |(offset, &cell)| (col + offset, Spot::of(cell)))
    }

    fn back_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
        let cells = self.cells();
        let cells = &cells[..cells.len().min(col.saturating_add(1))];
        (cells.iter().enumerate().rev()).map(|(col, &cell)| (col, Spot::of(cell)))
    }
}

/// The lines the cursor moves over.
pub(crate) trait Lines {
    /// How a line's cells are read.
    type Line: Cells;

    /// A line: borrowed from a terminal's screen or history, or for a
    /// file's text, made as it's asked for.
    type Row<'a>: Deref<Target = Self::Line>
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

    /// The characters words are made of besides letters, digits and `_`:
    /// an editor's, as its file's language says; a terminal's, none.
    fn words(&self) -> Words {
        Words::NONE
    }

    /// The part of them in view, for `H`, `M` and `L`, if they know.
    fn view(&self) -> Option<View> {
        None
    }
}

/// The part of the lines in view, for `H`, `M` and `L`: from screen row
/// `top`, `rows` tall, keeping `margin` rows, `scrolloff`, from its edges
/// as far as they fit, short of the text's ends. A line going on past its
/// bottom isn't shown whole, so `L` goes to the one before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct View {
    pub(crate) top: RowAt,
    pub(crate) rows: u64,
    pub(crate) margin: u64,
}

/// The characters words are made of besides letters, digits and `_`, as a
/// file's language says, as Vim's `iskeyword` adds them: `-` in CSS, so
/// `w` goes over `margin-top` whole. ASCII punctuation only, kept as one
/// bit each, so asking about a character costs no more than asking whether
/// it's `_`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Words(u128);

impl Words {
    /// None: words are letters, digits and `_`, as in plain text and in
    /// terminals.
    pub(crate) const NONE: Self = Self(0);

    /// The ASCII punctuation in `chars`, as a language pack gives it. Any
    /// other character is passed over: letters and digits are in words
    /// already.
    pub(crate) fn new(chars: &str) -> Self {
        Self(
            (chars.chars())
                .filter(char::is_ascii_punctuation)
                .fold(0, |bits, ch| bits | 1 << u32::from(ch)),
        )
    }

    /// Whether there are none.
    pub(crate) fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Whether `ch` is one of them.
    pub(crate) fn has(self, ch: char) -> bool {
        let ch = u32::from(ch);
        ch < 128 && self.0 >> ch & 1 == 1
    }

    /// Whether `ch` is part of a word: a letter, a digit, `_` or one of
    /// them.
    pub(crate) fn is_word(self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_' || self.has(ch)
    }

    /// Them, in the order of their codes.
    pub(crate) fn chars(self) -> impl Iterator<Item = char> {
        (0..128_u8).map(char::from).filter(move |&ch| self.has(ch))
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

impl View {
    /// The view's bottom row, or the text's last if that's sooner.
    pub(crate) fn bottom_row(&self, lines: &impl Lines) -> RowAt {
        let (_, last) = lines.bounds();
        rows_down(self.top, self.rows.saturating_sub(1), last, lines)
    }

    /// The first and last lines shown whole: one going on past the view's
    /// bottom isn't, nor one shown from partway down, as Ctrl+E can leave
    /// it where lines wrap, unless it's the only one.
    pub(crate) fn whole_lines(&self, lines: &impl Lines) -> (u64, u64) {
        let top = self.top;
        let bottom_row = self.bottom_row(lines);
        let cut = bottom_row.row + 1 < lines.screen_rows(bottom_row.line).len();
        let bottom = if cut && bottom_row.line > top.line {
            bottom_row.line - 1
        } else {
            bottom_row.line
        };
        let highest = if top.row > 0 && top.line < bottom {
            top.line + 1
        } else {
            top.line
        };
        (highest, bottom)
    }

    /// Where the cursor on line `line` goes to be in the view `margin` rows
    /// from its edges, as far as they fit, as Vim's `cursor_correct()`
    /// puts it after `H`, `M`, `L` and the pages (see [`within_margin`]).
    pub(crate) fn correct(&self, line: u64, lines: &impl Lines) -> u64 {
        let (first, last) = lines.bounds();
        let (highest, bottom) = self.whole_lines(lines);
        let at_start = self.top
            == (RowAt {
                line: first,
                row: 0,
            });
        let at_end = bottom == last;
        within_margin(line, self, bottom, (at_start, at_end), lines).max(highest)
    }
}

/// Where `line`, gone to by `H`, `M` or `L` in `view`, whose last line
/// shown whole is `bottom`, goes to keep `view.margin` rows from the
/// view's edges, as Vim's `scrolloff` does: lines are taken from the
/// bottom and the top in turn, the bottom first, until there are as many
/// rows as it wants beyond each, or they meet, and `line` is kept between.
/// None are wanted at the text's start, or at its end, as `ends` says
/// each shows, and then no more than half the view at the other.
fn within_margin(
    line: u64,
    view: &View,
    bottom: u64,
    ends: (bool, bool),
    lines: &impl Lines,
) -> u64 {
    let top = view.top;
    let (mut above_wanted, mut below_wanted) = (view.margin, view.margin);
    if ends.0 {
        above_wanted = 0;
        below_wanted = below_wanted.min(view.rows / 2);
    }
    if ends.1 {
        below_wanted = 0;
        above_wanted = above_wanted.min(view.rows.saturating_sub(1) / 2);
    }
    let shown = |line: u64| {
        let rows = lines.screen_rows(line).len() as u64;
        if line == top.line {
            rows - top.row as u64
        } else {
            rows
        }
    };
    let (mut high, mut low, mut above, mut below) = (top.line, bottom, 0, 0);
    while (above < above_wanted || below < below_wanted) && high < low {
        if below < below_wanted && (below <= above || above >= above_wanted) {
            below += shown(low);
            low -= 1;
        } else {
            above += shown(high);
            high += 1;
        }
    }
    match high.cmp(&low) {
        Ordering::Less => line.clamp(high, low),
        Ordering::Equal => high,
        Ordering::Greater => low,
    }
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

/// How many screen rows `to` is below `from`, in `lines`.
pub(crate) fn rows_between(from: RowAt, to: RowAt, lines: &impl Lines) -> u64 {
    if to.line == from.line {
        return to.row.saturating_sub(from.row) as u64;
    }
    let rest_of_first = lines.screen_rows(from.line).len() - from.row;
    let between: usize = (from.line + 1..to.line)
        .map(|line| lines.screen_rows(line).len())
        .sum();
    (rest_of_first + between + to.row) as u64
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

/// A block of columns, as Ctrl+V selects one, Vim's blockwise visual
/// mode: on each line from `first` to `last`, the cells from `left` to
/// `right`, both in it, or to the line's end if `right` is
/// [`TO_END`](Self::TO_END), as after `$`. Columns are cells as drawn, so
/// a tab or a wide character can be cut by its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) first: u64,
    pub(crate) last: u64,
    pub(crate) left: usize,
    pub(crate) right: usize,
}

impl Block {
    /// The `right` of a block that goes to each line's end.
    pub(crate) const TO_END: usize = usize::MAX;

    /// The block with corners at `anchor` and `cursor`, as Vim finds it:
    /// from the first cell either corner's character takes to the last,
    /// so a tab or a wide character at a corner is in it whole; to each
    /// line's end if `to_end`.
    pub(crate) fn between(anchor: Point, cursor: Point, to_end: bool, lines: &impl Lines) -> Self {
        let extent = |at: Point| {
            lines
                .row(at.line)
                .map_or((at.col, at.col), |row| row.extent(at.col))
        };
        let (anchor_cells, cursor_cells) = (extent(anchor), extent(cursor));
        Self {
            first: anchor.line.min(cursor.line),
            last: anchor.line.max(cursor.line),
            left: anchor_cells.0.min(cursor_cells.0),
            right: if to_end {
                Self::TO_END
            } else {
                anchor_cells.1.max(cursor_cells.1)
            },
        }
    }

    /// Whether it goes to each line's end.
    pub(crate) fn to_end(self) -> bool {
        self.right == Self::TO_END
    }

    /// How many columns wide it is, if it doesn't go to each line's end.
    pub(crate) fn width(self) -> Option<usize> {
        (!self.to_end()).then(|| self.right + 1 - self.left)
    }

    /// The cells it takes of a line `cells` wide, from the first to just
    /// after the last: none past the line's end.
    pub(crate) fn on(self, cells: usize) -> Option<std::ops::Range<usize>> {
        let end = self.right.saturating_add(1).min(cells);
        (self.left < end).then_some(self.left..end)
    }
}

/// The biggest count a command takes: more digits than this are kept to
/// it, as Vim keeps them, so no count overflows or takes long to repeat.
pub(crate) const MOST_COUNT: usize = 99_999_999;

/// `count`, with `digit` typed after it, kept to [`MOST_COUNT`].
pub(crate) fn count_with(count: Option<usize>, digit: u32) -> usize {
    (count.unwrap_or(0).saturating_mul(10))
        .saturating_add(digit as usize)
        .min(MOST_COUNT)
}

/// Two counts, one multiplying the other, as in `2d3w`, kept to
/// [`MOST_COUNT`].
pub(crate) fn multiplied(first: Option<usize>, second: Option<usize>) -> Option<usize> {
    match (first, second) {
        (None, None) => None,
        (first, second) => {
            Some((first.unwrap_or(1).saturating_mul(second.unwrap_or(1))).min(MOST_COUNT))
        }
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
    /// Found `again`, by `;` or `,`, a `t` or `T` doesn't stop at one right
    /// by the cursor, as Vim's doesn't.
    Find {
        ch: char,
        forward: bool,
        till: bool,
        again: bool,
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
    /// Ctrl+End: the last line's last character, or with a count, that
    /// line's.
    TextEnd,
    /// `H`: the view's top line, or with a count, so many from the top.
    ViewTop,
    /// `M`: the middle of the lines in view.
    ViewMiddle,
    /// `L`: the view's bottom line, or with a count, so many from the
    /// bottom.
    ViewBottom,
    /// `+` and Enter: the next line's first character that isn't blank.
    NextLine,
    /// `-`: the previous line's first character that isn't blank.
    PreviousLine,
    /// `_`: this line's first character that isn't blank, or with a
    /// count, that many lines less one down.
    LineFirstNonBlank,
    /// `g_`: this line's last character that isn't blank, or with a
    /// count, that many lines less one down.
    LastNonBlank,
    /// `|`: the column a count says, counting from 1, or the first.
    Column,
    /// `ge`: the end of the word before, back over line ends.
    WordEndBack,
    /// `gE`: the end of the run of anything but blanks before.
    BigWordEndBack,
    /// Space: right, and on from a line's last character to the next
    /// line's start, as Vim's does with `whichwrap` having `s`.
    RightOver,
    /// Backspace: left, and back from a line's start to the end of the
    /// line before, as Vim's does with `whichwrap` having `b`.
    LeftOver,
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
    pub(crate) fn reach_from(
        self,
        from: Point,
        to: Point,
        count: Option<usize>,
        lines: &impl Lines,
    ) -> Option<Reach> {
        let once = count.is_none_or(|count| count == 1);
        match self {
            // `dt)` with `)` right after the cursor takes the character
            // it's on, as Vim's.
            Self::Find { .. } if from == to && once && self.found_in_place(from, lines) => {
                Some(Reach::Inclusive)
            }
            Self::Match
            | Self::Find { .. }
            | Self::WordEndBack
            | Self::BigWordEndBack
            | Self::RightOver
            | Self::LeftOver
                if from == to =>
            {
                None
            }
            // `+` on the last line has no line to take.
            Self::NextLine | Self::PreviousLine if from.line == to.line => None,
            // `}` with no empty line after takes the last line's end.
            Self::Paragraph { forward: true } if !lines.empty(to.line) => Some(Reach::Inclusive),
            _ => Some(self.reach()),
        }
    }

    /// Whether it's `t` finding its character right after `at`, so going
    /// nowhere without failing, as Vim's `t)` does with `)` next.
    pub(crate) fn found_in_place(self, at: Point, lines: &impl Lines) -> bool {
        let Self::Find {
            ch,
            forward: true,
            till: true,
            again,
        } = self
        else {
            return false;
        };
        let row = lines.row(at.line);
        find_char(row.as_deref(), at.col, (ch, true, true), 1, again) == Some(at.col)
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
            | Self::RowStart
            | Self::Column
            | Self::RightOver
            | Self::LeftOver => Reach::Exclusive,
            Self::Find { forward, .. } => {
                if forward {
                    Reach::Inclusive
                } else {
                    Reach::Exclusive
                }
            }
            Self::LineEnd
            | Self::WordEnd
            | Self::BigWordEnd
            | Self::Match
            | Self::RowEnd
            | Self::TextEnd
            | Self::LastNonBlank
            | Self::WordEndBack
            | Self::BigWordEndBack => Reach::Inclusive,
            Self::Up(_)
            | Self::Down(_)
            | Self::First
            | Self::Last
            | Self::ViewTop
            | Self::ViewMiddle
            | Self::ViewBottom
            | Self::NextLine
            | Self::PreviousLine
            | Self::LineFirstNonBlank => Reach::Lines,
        }
    }
}

/// What an operator takes with a motion: from `start` to `end`, in order,
/// as `reach` says, and the cursor where the motion went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Taken {
    pub(crate) start: Point,
    pub(crate) end: Point,
    pub(crate) reach: Reach,
    pub(crate) moved: Cursor,
}

impl Taken {
    /// What `motion`, with `count` typed, takes from `from` to `moved`,
    /// where it went, in `lines`: none if it went nowhere it could (see
    /// [`Motion::reach_from`]), or there's no line to take, as `dj` and
    /// `d2_` on the last line.
    pub(crate) fn between(
        motion: Motion,
        count: Option<usize>,
        from: Point,
        moved: Cursor,
        lines: &impl Lines,
    ) -> Option<Self> {
        let counted = count.is_some_and(|count| count > 1);
        let down = match motion {
            Motion::Up(_) | Motion::Down(_) => true,
            Motion::LineFirstNonBlank | Motion::LastNonBlank => counted,
            _ => false,
        };
        if down && moved.at.line == from.line {
            return None;
        }
        let reach = motion.reach_from(from, moved.at, count, lines)?;
        let (start, end) = if moved.at < from {
            (moved.at, from)
        } else {
            (from, moved.at)
        };

        Some(Self {
            start,
            end,
            reach,
            moved,
        })
    }
}

/// The normal-mode cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cursor {
    pub(crate) at: Point,
    /// The column moving up and down aims for, as in Vim: it survives
    /// passing through shorter lines. `usize::MAX` keeps to line ends.
    /// `None` aims for where the cursor is, worked out when it's needed,
    /// as Vim's `w_set_curswant` has it: a tab's last cell, where Vim's
    /// cursor sits on one (see [`aim`](Self::aim)).
    want: Option<usize>,
}

/// What kind of character a cell holds, for word motions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    Blank,
    /// Letters, digits and `_`, and the characters the file's language
    /// adds to them (see [`Words`]).
    Word,
    /// Any other printing character.
    Punctuation,
}

impl Cursor {
    pub(crate) fn new(at: Point) -> Self {
        Self { at, want: None }
    }

    /// A cursor at `at`, aiming for column `want` moving up and down.
    pub(crate) fn wanting(at: Point, want: usize) -> Self {
        Self {
            at,
            want: Some(want),
        }
    }

    /// This cursor at `at`, aiming for the same column moving up and down.
    pub(crate) fn moved(self, at: Point) -> Self {
        Self { at, ..self }
    }

    /// The column moving up and down aims for: `usize::MAX` after `$`.
    /// Where it aims for where the cursor is, its column, which on a tab
    /// is the tab's first cell: [`aim`](Self::aim) knows better.
    pub(crate) fn want(&self) -> usize {
        self.want.unwrap_or(self.at.col)
    }

    /// The column moving up and down aims for, in `lines`: where it aims
    /// for where the cursor is, on a tab that's its last cell, as Vim's
    /// cursor sits there in normal mode.
    pub(crate) fn aim(&self, lines: &impl Lines) -> usize {
        self.want.unwrap_or_else(|| {
            let row = lines.row(self.at.line);
            match row.as_deref() {
                Some(row) if row.cell(self.at.col).is_some_and(|spot| spot.ch == '\t') => {
                    row.extent(self.at.col).1
                }
                _ => self.at.col,
            }
        })
    }

    /// Whether the column moving up and down aims for is where the cursor
    /// is, not settled yet.
    pub(crate) fn aims_here(&self) -> bool {
        self.want.is_none()
    }

    /// Settles the column moving up and down aims for, if it aims for
    /// where the cursor is: at its first cell if `at_start`, as Vim's
    /// cursor sits on a tab in insert mode, or in visual mode at or before
    /// the selection's other end, or else as [`aim`](Self::aim) has it.
    /// Vim settles it before each key, in the mode the key finds.
    pub(crate) fn settle_want(&mut self, lines: &impl Lines, at_start: bool) {
        if self.want.is_none() {
            self.want = Some(if at_start {
                self.at.col
            } else {
                self.aim(lines)
            });
        }
    }

    /// Moves by `motion`, staying within `lines`.
    pub(crate) fn apply(&mut self, motion: Motion, lines: &impl Lines) {
        let (first, last) = lines.bounds();
        let at = self.at;
        match motion {
            Motion::Left => {
                let row = lines.row(at.line);
                self.at.col = previous_cell(row.as_deref(), at.col).unwrap_or(at.col);
                self.want = None;
            }
            Motion::Right => {
                let row = lines.row(at.line);
                let last = lines.last_cell(at.line);
                if let Some(col) = next_cell(row.as_deref(), at.col).filter(|&col| col <= last) {
                    self.at.col = col;
                }
                self.want = None;
            }
            Motion::Up(n) => self.go_to_line(at.line.saturating_sub(n).max(first), lines),
            Motion::Down(n) => self.go_to_line(at.line.saturating_add(n).min(last), lines),
            Motion::LineStart => {
                self.at.col = 0;
                self.want = None;
            }
            Motion::LineEnd => {
                self.at.col = lines.last_cell(at.line);
                self.want = Some(usize::MAX);
            }
            Motion::FirstNonBlank => {
                let row = lines.row(at.line);
                let row = row.as_deref();
                let end = text_end(row);
                let mut col = 0;
                while col < end && class_at(row, col, Words::NONE) == Class::Blank {
                    col = next_cell(row, col).unwrap_or(end);
                }
                self.at.col = start_of_cell(row, col.min(end.saturating_sub(1)));
                self.want = None;
            }
            Motion::Find { .. } => self.find(motion, 1, lines),
            Motion::NextLine
            | Motion::PreviousLine
            | Motion::LineFirstNonBlank
            | Motion::LastNonBlank
            | Motion::Column
            | Motion::RightOver
            | Motion::LeftOver => self.apply_over_lines(motion, lines),
            Motion::WordEndBack => self.word_end_back(lines, Class::of),
            Motion::BigWordEndBack => self.word_end_back(lines, Class::big),
            // Keeping the column, as Neovim does: `go` puts them at the
            // first non-blank, with `startofline`.
            Motion::First => self.go_to_line(first, lines),
            Motion::Last => self.go_to_line(last, lines),
            Motion::WordStart => self.word_start(lines, Class::of),
            Motion::WordBack => self.word_back(lines, Class::of),
            Motion::WordEnd => self.word_end(lines, Class::of),
            Motion::BigWordStart => self.word_start(lines, Class::big),
            Motion::BigWordBack => self.word_back(lines, Class::big),
            Motion::BigWordEnd => self.word_end(lines, Class::big),
            Motion::Match => {
                if let Some(at) = pair(at, lines) {
                    self.at = at;
                    self.want = None;
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
                self.want = None;
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
                self.want = None;
            }
            Motion::TextEnd => self.onto_line_end(last, lines),
            Motion::ViewTop | Motion::ViewMiddle | Motion::ViewBottom => {
                self.go_in_view(motion, 1, lines, true);
            }
        }
    }

    /// Moves to line `line`'s last character, keeping to lines' ends
    /// moving up and down, as `$` does.
    fn onto_line_end(&mut self, line: u64, lines: &impl Lines) {
        self.at = Point {
            line,
            col: lines.last_cell(line),
        };
        self.want = Some(usize::MAX);
    }

    /// Moves by `motion` as a command with `count` typed before it does:
    /// `G` and `gg` with a count go to that line, `H` and `L` so many lines
    /// from the view's top or bottom, and any other motion goes `count`
    /// times (see [`repeat`](Self::repeat)). `gg`, `G`, `H`, `M` and `L`
    /// land at the line's first character that isn't blank if
    /// `start_of_line`, as Vim's `startofline` puts them, or else aim for
    /// the wanted column.
    pub(crate) fn go(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        lines: &impl Lines,
        start_of_line: bool,
    ) {
        self.going(motion, count, lines, start_of_line, false);
    }

    /// Where `motion` goes, `count` typed, when an operator takes it: as
    /// [`go`](Self::go) goes, but `H` and `L` to the view's edges, as
    /// Vim's do, `scrolloff` or not.
    pub(crate) fn go_to_take(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        lines: &impl Lines,
        start_of_line: bool,
    ) {
        self.going(motion, count, lines, start_of_line, true);
    }

    /// [`go`](Self::go), or [`go_to_take`](Self::go_to_take) if `taking`:
    /// as an operator takes the motion.
    fn going(
        &mut self,
        motion: Motion,
        count: Option<usize>,
        lines: &impl Lines,
        start_of_line: bool,
        taking: bool,
    ) {
        let (first, last) = lines.bounds();
        // Line `number`, counting the first as 1, or the last if there
        // aren't so many.
        let numbered =
            |number: usize| (first.saturating_add((number as u64).saturating_sub(1))).min(last);
        match (motion, count) {
            (Motion::First | Motion::Last, Some(number)) => {
                self.onto_line(numbered(number), start_of_line, lines);
            }
            (Motion::First, None) => self.onto_line(first, start_of_line, lines),
            (Motion::TextEnd, Some(number)) => self.onto_line_end(numbered(number), lines),
            (Motion::Last, None) => self.onto_line(last, start_of_line, lines),
            (Motion::ViewTop | Motion::ViewMiddle | Motion::ViewBottom, count) => {
                self.go_in_view(motion, count.unwrap_or(1), lines, taking);
                if start_of_line {
                    *self = Self::at_first_non_blank(self.at.line, lines);
                }
            }
            // A count less one lines down, as Vim's: none from the last
            // line.
            (Motion::LineFirstNonBlank | Motion::LastNonBlank, Some(count))
                if count > 1 && self.at.line >= last => {}
            (Motion::LineFirstNonBlank | Motion::LastNonBlank, count) => {
                let down = (count.unwrap_or(1) as u64).saturating_sub(1);
                let line = self.at.line.saturating_add(down).min(last);
                if motion == Motion::LineFirstNonBlank {
                    *self = Self::at_first_non_blank(line, lines);
                } else {
                    let row = lines.row(line);
                    let end = text_end(row.as_deref());
                    let col = start_of_cell(row.as_deref(), end.saturating_sub(1));
                    self.settle(Point { line, col });
                }
            }
            (Motion::Column, count) => {
                let col = count.unwrap_or(1).saturating_sub(1);
                let row = lines.row(self.at.line);
                self.at.col = start_of_cell(row.as_deref(), col.min(lines.last_cell(self.at.line)));
                self.want = Some(col);
            }
            // Taken by an operator, Space stops at a line's end, past its
            // last character, before going on, as Vim's counts the line
            // break as one.
            (Motion::RightOver, count) if taking => {
                for _ in 0..count.unwrap_or(1) {
                    match over_on(self.at, lines) {
                        Some(at) => self.settle(at),
                        None => break,
                    }
                }
            }
            (Motion::WordStart | Motion::BigWordStart, count) if taking => {
                let kind = if motion == Motion::WordStart {
                    Class::of
                } else {
                    Class::big
                };
                self.repeat(motion, count.unwrap_or(1) - 1, lines);
                self.word_start_taken(lines, kind);
            }
            (motion, count) => self.repeat(motion, count.unwrap_or(1), lines),
        }
    }

    /// What an operator takes with `motion`, `count` typed, from here:
    /// as far as [`go`](Self::go) moves (see [`Taken`]).
    pub(crate) fn taken(
        &self,
        motion: Motion,
        count: Option<usize>,
        lines: &impl Lines,
        start_of_line: bool,
    ) -> Option<Taken> {
        let mut moved = *self;
        moved.go_to_take(motion, count, lines, start_of_line);
        Taken::between(motion, count, self.at, moved, lines)
    }

    /// Moves to line `line`: to its first character that isn't blank if
    /// `start_of_line`, or else aiming for the wanted column.
    pub(crate) fn onto_line(&mut self, line: u64, start_of_line: bool, lines: &impl Lines) {
        if start_of_line {
            *self = Self::at_first_non_blank(line, lines);
        } else {
            self.go_to_line(line, lines);
        }
    }

    /// Moves to the line of the view that `motion`, `H`, `M` or `L`, goes
    /// to, as Vim's do: `count` lines from the first line in view or the
    /// last shown whole, counting from 1, or the line half the rows shown
    /// reach; aiming for the wanted column. Moving, as opposed to being
    /// taken by an operator (`taking`), it keeps the view's margin of
    /// screen rows from the view's edges, as Vim's `scrolloff` does,
    /// unless the view is at the text's start or end. Nothing without a
    /// view.
    fn go_in_view(&mut self, motion: Motion, count: usize, lines: &impl Lines, taking: bool) {
        let Some(view) = lines.view() else {
            return;
        };
        let (first, last) = lines.bounds();
        let top = view.top;
        let bottom_row = view.bottom_row(lines);
        let (highest, bottom) = view.whole_lines(lines);
        let from_edge = (count as u64).saturating_sub(1);
        // A count past the view's other edge goes no further than it, but
        // an operator takes as far as the text goes, as Vim's: `d99L` to
        // its start.
        let mut line = match motion {
            Motion::ViewTop if !taking => highest.saturating_add(from_edge).min(bottom),
            Motion::ViewTop => highest.saturating_add(from_edge).min(last),
            Motion::ViewBottom if !taking => bottom.saturating_sub(from_edge).max(highest),
            Motion::ViewBottom => bottom.saturating_sub(from_edge).max(first),
            _ => {
                // Half the rows of the lines shown whole, as Vim's: rows
                // past the text's end, or of a line cut off at the bottom,
                // don't count.
                let end = last_row(lines, bottom).min(bottom_row);
                let half = (rows_between(top, end, lines) + 1).div_ceil(2);
                let mut used = 0;
                let mut line = top.line;
                loop {
                    let rows = lines.screen_rows(line).len() as u64;
                    used += if line == top.line {
                        rows - top.row as u64
                    } else {
                        rows
                    };
                    if used >= half || line >= bottom {
                        break line;
                    }
                    line += 1;
                }
            }
        };
        if !taking {
            line = view.correct(line, lines);
        }
        self.go_to_line(line, lines);
    }

    /// Moves by `motion` `times` times, as a count before it asks (`5k`,
    /// `3w`, `2}`): up and down by `times` times its lines, or rows, at
    /// once, and anything else again and again.
    pub(crate) fn repeat(&mut self, motion: Motion, times: usize, lines: &impl Lines) {
        let n = times as u64;
        match motion {
            Motion::Up(by) => self.apply(Motion::Up(by.saturating_mul(n)), lines),
            Motion::Down(by) => self.apply(Motion::Down(by.saturating_mul(n)), lines),
            Motion::RowUp(by) => self.apply(Motion::RowUp(by.saturating_mul(n)), lines),
            Motion::RowDown(by) => self.apply(Motion::RowDown(by.saturating_mul(n)), lines),
            Motion::Find { .. } => self.find(motion, times, lines),
            // Until it goes nowhere, so a count bigger than the text is
            // quick.
            _ => {
                for _ in 0..times {
                    let from = self.at;
                    self.apply(motion, lines);
                    if self.at == from {
                        break;
                    }
                }
            }
        }
    }

    /// `f`, `t`, `F` or `T`, `find`, to the `times`th of its character on
    /// the line, as Vim's go: nowhere if there aren't that many.
    fn find(&mut self, find: Motion, times: usize, lines: &impl Lines) {
        let Motion::Find {
            ch,
            forward,
            till,
            again,
        } = find
        else {
            return;
        };
        let row = lines.row(self.at.line);
        let skip = till && again && times == 1;
        if let Some(col) = find_char(
            row.as_deref(),
            self.at.col,
            (ch, forward, till),
            times,
            skip,
        ) {
            self.at.col = col;
            self.want = None;
        }
    }

    /// A cursor at line `line`'s first character that isn't blank.
    pub(crate) fn at_first_non_blank(line: u64, lines: &impl Lines) -> Self {
        let mut cursor = Self::new(Point { line, col: 0 });
        cursor.apply(Motion::FirstNonBlank, lines);
        cursor
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
        let want = self.aim(lines);
        let col = if want == usize::MAX {
            usize::MAX
        } else {
            let offset = here.indent + want.saturating_sub(here.start);
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
        self.want = Some(col);
    }

    /// Moves to `line`, aiming for the wanted column.
    fn go_to_line(&mut self, line: u64, lines: &impl Lines) {
        let row = lines.row(line);
        let row = row.as_deref();
        let want = self.aim(lines);
        self.want = Some(want);
        let col = want.min(lines.last_cell(line));
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
        // Over blanks and line ends, stopping at an empty line, as Vim's.
        while walk.class() == Class::Blank {
            if !walk.forward() || (walk.at.col == 0 && lines.empty(walk.at.line)) {
                break;
            }
        }
        self.settle(walk.at);
    }

    /// The last `w` or `W` of a count an operator takes, as Vim's
    /// `fwd_word` goes for one: it stops at the end of the line it's on,
    /// past the last character, rather than going over the line break, so
    /// `dw` on a line's last word leaves the break. From an empty line,
    /// it's at the next line's start.
    fn word_start_taken(&mut self, lines: &impl Lines, kind: fn(Class) -> Class) {
        let mut walk = Walk::new(lines, self.at).seeing(kind);
        let class = walk.class();
        // A step on, or false where the word stops instead.
        let step = |walk: &mut Walk<'_, _>| {
            let line = walk.at.line;
            if walk.forward() && walk.at.line == line {
                return true;
            }
            if !lines.empty(line) {
                walk.at = Point {
                    line,
                    col: line_end(line, lines),
                };
            }
            false
        };
        let mut going = step(&mut walk);
        while going && class != Class::Blank && walk.class() == class {
            going = step(&mut walk);
        }
        while going && walk.class() == Class::Blank {
            going = step(&mut walk);
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
        // Back over blanks and line ends, stopping at an empty line, as
        // Vim's.
        while walk.class() == Class::Blank {
            if (walk.at.col == 0 && lines.empty(walk.at.line)) || !walk.back() {
                return self.settle(walk.at);
            }
        }
        let class = walk.class();
        while walk.peek_back() == Some(class) {
            walk.back();
        }
        self.settle(walk.at);
    }

    /// [`apply`](Self::apply) for the motions that go to other lines'
    /// starts, ends and first non-blanks: `+`, `-`, `_`, `g_`, `|`,
    /// Space and Backspace.
    fn apply_over_lines(&mut self, motion: Motion, lines: &impl Lines) {
        let (first, last) = lines.bounds();
        let at = self.at;
        match motion {
            Motion::NextLine if at.line < last => {
                *self = Self::at_first_non_blank(at.line + 1, lines);
            }
            Motion::PreviousLine if at.line > first => {
                *self = Self::at_first_non_blank(at.line - 1, lines);
            }
            Motion::LineFirstNonBlank | Motion::LastNonBlank | Motion::Column => {
                self.going(motion, None, lines, false, false);
            }
            Motion::RightOver => {
                self.apply(Motion::Right, lines);
                if self.at == at && at.line < last {
                    self.settle(Point {
                        line: at.line + 1,
                        col: 0,
                    });
                }
            }
            Motion::LeftOver => {
                self.apply(Motion::Left, lines);
                if self.at == at && at.line > first {
                    let line = at.line - 1;
                    self.settle(Point {
                        line,
                        col: lines.last_cell(line),
                    });
                }
            }
            _ => {}
        }
    }

    /// `ge`, or `gE` as `kind` sees classes: back to before the start of
    /// the word the cursor's on, then on back over blanks and line ends to
    /// the end of the word before, stopping at an empty line, as Vim's.
    fn word_end_back(&mut self, lines: &impl Lines, kind: fn(Class) -> Class) {
        let mut walk = Walk::new(lines, self.at).seeing(kind);
        let class = walk.class();
        if !walk.back() {
            return;
        }
        if class != Class::Blank {
            while walk.class() == class {
                if !walk.back() {
                    return self.settle(walk.at);
                }
            }
        }
        while walk.class() == Class::Blank {
            if walk.at.col == 0 && lines.empty(walk.at.line) {
                break;
            }
            if !walk.back() {
                break;
            }
        }
        self.settle(walk.at);
    }

    /// Lands on `at`, pulled back from a line's end onto its last
    /// character.
    fn settle(&mut self, at: Point) {
        self.at = at;
        self.want = None;
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
    /// What words are made of in `lines`, asked once.
    words: Words,
    /// The last line whose text's end was asked for, and where that is:
    /// finding it reads back over the blanks after it, which a step along
    /// a line of many shouldn't do each time.
    end: std::cell::Cell<Option<(u64, usize)>>,
}

impl<'a, L: Lines> Walk<'a, L> {
    pub(crate) fn new(lines: &'a L, at: Point) -> Self {
        Self {
            lines,
            at,
            kind: Class::of,
            words: lines.words(),
            end: std::cell::Cell::new(None),
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
        (self.kind)(class_at(
            self.lines.row(at.line).as_deref(),
            at.col,
            self.words,
        ))
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

    /// Where the text of line `line`, whose row is `row`, ends.
    fn text_end(&self, line: u64, row: Option<&L::Line>) -> usize {
        match self.end.get() {
            Some((known, end)) if known == line => end,
            _ => {
                let end = text_end(row);
                self.end.set(Some((line, end)));
                end
            }
        }
    }

    fn next(&self) -> Option<Point> {
        let row = self.lines.row(self.at.line);
        let row = row.as_deref();
        let end = self.text_end(self.at.line, row);
        // A line's end is the position just past its text.
        match next_cell(row, self.at.col) {
            Some(col) if col <= end && self.at.col < end => Some(Point {
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
                col: self.text_end(line, self.lines.row(line).as_deref()),
            });
        }
        None
    }
}

/// The column just after line `line`'s last character, or 0 if the line
/// is empty. Space and Backspace stop there when an operator takes them,
/// so the line break counts as a character.
pub(crate) fn line_end(line: u64, lines: &impl Lines) -> usize {
    if lines.empty(line) {
        return 0;
    }
    let last = lines.last_cell(line);
    lines.row(line).map_or(last, |row| row.extent(last).1) + 1
}

/// The step Space takes from `at` for an operator: on along the line, to
/// its end past its last character, then to the next line's start.
fn over_on(at: Point, lines: &impl Lines) -> Option<Point> {
    let end = line_end(at.line, lines);
    if at.col < end {
        let row = lines.row(at.line);
        let col = next_cell(row.as_deref(), at.col).map_or(end, |col| col.min(end));
        return Some(Point { col, ..at });
    }
    (at.line < lines.bounds().1).then_some(Point {
        line: at.line + 1,
        col: 0,
    })
}

/// What `d` or `c` with `times` Backspaces from `at` deletes, as Vim's
/// does: the characters from where the cursor ends up to the one just
/// before `at`, both included. Going back over the start of a line onto
/// one that isn't empty, the line break counts as a character, so `d`
/// and Backspace at a line's start joins it to the line before.
///
/// `None` if it never goes back onto a line that isn't empty: then the
/// usual rules for an exclusive motion apply, which take an empty line
/// whole.
pub(crate) fn deleted_back(at: Point, times: usize, lines: &impl Lines) -> Option<(Point, Point)> {
    let last = over_back(at, lines)?;
    let mut start = last;
    let mut joins = start.line < at.line && !lines.empty(start.line);
    for _ in 1..times {
        let Some(back) = over_back(start, lines) else {
            break;
        };
        joins |= back.line < start.line && !lines.empty(back.line);
        start = back;
    }
    joins.then_some((start, last))
}

/// One step back for `deleted_back`: to the character before, or from a
/// line's start to just after the previous line's last character.
fn over_back(at: Point, lines: &impl Lines) -> Option<Point> {
    if at.col > 0 {
        let row = lines.row(at.line);
        let col = previous_cell(row.as_deref(), at.col.min(line_end(at.line, lines))).unwrap_or(0);
        return Some(Point { col, ..at });
    }
    (at.line > lines.bounds().0).then(|| Point {
        line: at.line - 1,
        col: line_end(at.line - 1, lines),
    })
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
    let (col, ch) = (row.on_from(at.col))
        .map(|(col, spot)| (col, spot.ch))
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
    // Counts the bracket at `col` in, if it's one, returning where the
    // pair is if this is it.
    let mut count = |line: u64, (col, spot): (usize, Spot)| {
        if spot.ch == ch_of(forward, open, close) {
            depth += 1;
        } else if spot.ch == ch_of(!forward, open, close) {
            depth -= 1;
            if depth == 0 {
                return Some(Point { line, col });
            }
        }
        None
    };
    loop {
        // A line not to hand might hold a bracket that pairs.
        let row = lines.row(line)?;
        let found = if forward {
            (row.on_from(from.unwrap_or(0))).find_map(|cell| count(line, cell))
        } else {
            (row.back_from(from.unwrap_or(usize::MAX))).find_map(|cell| count(line, cell))
        };
        if found.is_some() {
            return found;
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
    // From a blank line, past the blank lines first. From a paragraph's
    // line, the blank line just after it is where it goes, even after a
    // paragraph of one line.
    while blank(line)
        && let Some(next) = step(line)
    {
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

/// The class of the cell at `col`, with past a line's text blank, and
/// `words` part of words.
pub(crate) fn class_at(row: Option<&impl Cells>, col: usize, words: Words) -> Class {
    let Some(spot) = row.and_then(|row| row.cell(col)) else {
        return Class::Blank;
    };
    if spot.blank() {
        Class::Blank
    } else if words.is_word(spot.ch) || spot.wide {
        Class::Word
    } else {
        Class::Punctuation
    }
}

/// Where the line's text ends: one past its last non-blank cell.
pub(crate) fn text_end(row: Option<&impl Cells>) -> usize {
    row.map_or(0, |row| {
        (row.back_from(usize::MAX))
            .find(|(_, spot)| !spot.blank())
            .map_or(0, |(last, _)| last + 1)
    })
}

/// The column of the line's last character, or 0 for a blank line.
pub(crate) fn last_char(row: Option<&impl Cells>) -> usize {
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
    // On the row: a tab begun on the row before is left to the caller
    // (see `wrap::onto_row`).
    start_of_cell(lines.row(line).as_deref(), col).max(start)
}

/// The cell after the one at `col`, stepping over a wide character's
/// right half.
pub(crate) fn next_cell(row: Option<&impl Cells>, col: usize) -> Option<usize> {
    let row = row?;
    let mut next = col + 1;
    while row.cell(next).is_some_and(|spot| spot.spacer) {
        next += 1;
    }
    (next <= row.width()).then_some(next)
}

/// The cell before the one at `col`, landing on a wide character's left
/// half.
pub(crate) fn previous_cell(row: Option<&impl Cells>, col: usize) -> Option<usize> {
    let previous = col.checked_sub(1)?;
    Some(start_of_cell(row, previous))
}

/// The `times`th character `ch` after `col` on the row, or before it if not
/// `forward`, not counting the one right by `col` if `skip`; or the
/// character next to that one, towards `col`, if `till`.
fn find_char(
    row: Option<&impl Cells>,
    col: usize,
    (ch, forward, till): (char, bool, bool),
    times: usize,
    skip: bool,
) -> Option<usize> {
    let cells = row?;
    // A character at a time: its first cell, not the rest of a tab's or a
    // wide character's.
    let first = |(_, spot): &(usize, Spot)| !spot.spacer;
    let found = |(_, spot): &(usize, Spot)| spot.ch == ch;
    // The character right by the cursor isn't looked at if `skip`.
    let skipped = usize::from(skip);
    let nth = times.max(1) - 1;
    let (found, _) = if forward {
        (cells
            .on_from(col + 1)
            .filter(first)
            .skip(skipped)
            .filter(found))
        .nth(nth)?
    } else {
        (cells.back_from(col.checked_sub(1)?))
            .filter(first)
            .skip(skipped)
            .filter(found)
            .nth(nth)?
    };
    if !till {
        return Some(found);
    }
    // Right by `col`, `t` stays where it is, having found it.
    if forward {
        previous_cell(row, found)
    } else {
        next_cell(row, found)
    }
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
pub(crate) fn start_of_cell(row: Option<&impl Cells>, mut col: usize) -> usize {
    // Back over a wide character's right half, or a tab's cells.
    while col > 0
        && row
            .and_then(|row| row.cell(col))
            .is_some_and(|spot| spot.spacer)
    {
        col -= 1;
    }
    col
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
        type Line = Row;
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

    /// `text`, with `view` of it shown.
    struct Viewed(Text, View);

    impl Lines for Viewed {
        type Line = Row;
        type Row<'a> = &'a Row;

        fn row(&self, line: u64) -> Option<&Row> {
            self.0.row(line)
        }

        fn bounds(&self) -> (u64, u64) {
            self.0.bounds()
        }

        fn view(&self) -> Option<View> {
            Some(self.1)
        }
    }

    #[test]
    fn h_m_and_l_go_to_lines_of_the_view_keeping_its_margin() {
        let lines: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let viewed = |top, margin| {
            let view = View {
                top: RowAt { line: top, row: 0 },
                rows: 5,
                margin,
            };
            Viewed(Text::new(&lines), view)
        };
        let went = |lines: &Viewed, motion, count| {
            let mut cursor = Cursor::new(at(12, 3));
            cursor.go(motion, count, lines, false);
            cursor.at
        };
        // Lines 5 to 9 in view.
        let plain = viewed(5, 0);
        assert_eq!(went(&plain, Motion::ViewTop, None), at(5, 3));
        assert_eq!(went(&plain, Motion::ViewTop, Some(2)), at(6, 3));
        assert_eq!(went(&plain, Motion::ViewTop, Some(9)), at(9, 3));
        assert_eq!(went(&plain, Motion::ViewMiddle, None), at(7, 3));
        assert_eq!(went(&plain, Motion::ViewBottom, None), at(9, 3));
        assert_eq!(went(&plain, Motion::ViewBottom, Some(3)), at(7, 3));
        assert_eq!(went(&plain, Motion::ViewBottom, Some(9)), at(5, 3));
        // A margin of a line, but not at the text's start.
        let margin = viewed(5, 1);
        assert_eq!(went(&margin, Motion::ViewTop, None), at(6, 3));
        assert_eq!(went(&margin, Motion::ViewTop, Some(3)), at(7, 3));
        assert_eq!(went(&margin, Motion::ViewBottom, None), at(8, 3));
        // A count no further than the margin from the other edge.
        assert_eq!(went(&margin, Motion::ViewTop, Some(9)), at(8, 3));
        assert_eq!(went(&margin, Motion::ViewBottom, Some(9)), at(6, 3));
        assert_eq!(went(&plain, Motion::ViewBottom, Some(usize::MAX)), at(5, 3));
        let start = viewed(0, 1);
        assert_eq!(went(&start, Motion::ViewTop, None), at(0, 3));
        assert_eq!(went(&start, Motion::ViewBottom, None), at(3, 3));
        // The text ending in view: the middle of what's shown.
        let end = viewed(17, 0);
        assert_eq!(went(&end, Motion::ViewBottom, None), at(19, 3));
        assert_eq!(went(&end, Motion::ViewMiddle, None), at(18, 3));
        // Taken by an operator, whole lines.
        assert_eq!(Motion::ViewTop.reach(), Reach::Lines);
        // Without a view, nowhere.
        let text = Text::new(&lines);
        let mut cursor = Cursor::new(at(12, 3));
        cursor.go(Motion::ViewTop, None, &text, false);
        assert_eq!(cursor.at, at(12, 3));
        // With `startofline`, at the line's first non-blank.
        let indented = Viewed(
            Text::new(&["a", "  b", "c"]),
            View {
                top: RowAt { line: 0, row: 0 },
                rows: 3,
                margin: 0,
            },
        );
        let mut cursor = Cursor::new(at(0, 0));
        cursor.go(Motion::ViewMiddle, None, &indented, true);
        assert_eq!(cursor.at, at(1, 2));
        cursor.go(Motion::First, None, &indented, false);
        cursor.go(Motion::Last, Some(2), &indented, false);
        assert_eq!(cursor.at, at(1, 2), "the wanted column");
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
    fn a_count_repeats_a_motion_or_picks_a_line() {
        let text = Text::new(&["one two", "", "  three", "four", "", "five"]);
        let repeated = |motion, times, from| {
            let mut cursor = Cursor::new(from);
            cursor.repeat(motion, times, &text);
            cursor.at
        };
        assert_eq!(repeated(Motion::Down(1), 3, at(0, 0)), at(3, 0));
        assert_eq!(repeated(Motion::Up(1), 9, at(3, 0)), at(0, 0));
        assert_eq!(
            repeated(Motion::Down(2), 2, at(0, 0)),
            at(4, 0),
            "lines at a time"
        );
        // An empty line is a word, as Vim's.
        assert_eq!(repeated(Motion::WordStart, 2, at(2, 2)), at(4, 0));
        let forward = Motion::Paragraph { forward: true };
        assert_eq!(repeated(forward, 2, at(0, 0)), at(4, 0));
        let mut cursor = Cursor::new(at(0, 4));
        cursor.go(Motion::Last, Some(3), &text, true);
        assert_eq!(cursor.at, at(2, 2), "at its first character");
        cursor.go(Motion::First, Some(99), &text, true);
        assert_eq!(cursor.at, at(5, 0), "or the last line");
    }

    #[test]
    fn a_huge_count_stops_where_the_text_does_and_counts_are_kept_small() {
        let text = Text::new(&["one two", "three"]);
        let mut cursor = Cursor::new(at(0, 0));
        // Quick, as it stops once it goes nowhere.
        cursor.repeat(Motion::WordStart, usize::MAX, &text);
        assert_eq!(cursor.at.line, 1);
        cursor.go(Motion::Last, Some(usize::MAX), &text, true);
        assert_eq!(cursor.at, at(1, 0));
        let mut count = None;
        for _ in 0..30 {
            count = Some(count_with(count, 9));
        }
        assert_eq!(count, Some(MOST_COUNT));
        assert_eq!(
            multiplied(Some(MOST_COUNT), Some(MOST_COUNT)),
            Some(MOST_COUNT)
        );
        assert_eq!(multiplied(None, Some(3)), Some(3));
        assert_eq!(multiplied(None, None), None);
    }

    #[test]
    fn words_are_ascii_punctuation_each_a_bit() {
        let words = Words::new("-?é a-_");
        assert_eq!(words.chars().collect::<String>(), "-?_");
        assert!(words.has('-') && words.has('?') && !words.has('é') && !words.has('a'));
        assert!(words.is_word('-') && words.is_word('é') && words.is_word('7'));
        assert!(!words.is_word(':') && !words.is_word(' '));
        assert!(Words::NONE.is_none() && !words.is_none());
        assert!(!Words::NONE.is_word('-') && Words::NONE.is_word('_'));
        assert_eq!(Words::new("~").chars().collect::<String>(), "~");
    }

    #[test]
    fn wide_characters_are_one_position() {
        let text = Text::new(&["a字b"]);
        assert_eq!(moved(&text, at(0, 0), &[Motion::Right]), at(0, 1));
        assert_eq!(moved(&text, at(0, 1), &[Motion::Right]), at(0, 3));
        assert_eq!(moved(&text, at(0, 3), &[Motion::Left]), at(0, 1));
    }

    /// A line that counts how often it's read back from.
    struct Counted {
        row: Row,
        backs: std::cell::Cell<usize>,
    }

    impl Cells for Counted {
        fn width(&self) -> usize {
            self.row.width()
        }

        fn cell(&self, col: usize) -> Option<Spot> {
            self.row.cell(col)
        }

        fn on_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
            self.row.on_from(col)
        }

        fn back_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
            self.backs.set(self.backs.get() + 1);
            self.row.back_from(col)
        }
    }

    /// A document of one line.
    struct One(Counted);

    impl Lines for One {
        type Line = Counted;
        type Row<'a> = &'a Counted;

        fn row(&self, line: u64) -> Option<&Counted> {
            (line == 0).then_some(&self.0)
        }

        fn bounds(&self) -> (u64, u64) {
            (0, 0)
        }
    }

    #[test]
    fn a_word_motion_reads_back_over_a_lines_blanks_once() {
        // A long word, and many more blanks after it.
        let mut builder = RowBuilder::new(20_000);
        builder.push(&"a".repeat(1000), &Style::DEFAULT, false);
        let one = One(Counted {
            row: builder.finish(false),
            backs: std::cell::Cell::new(0),
        });
        let mut cursor = Cursor::new(at(0, 0));
        cursor.apply(Motion::WordEnd, &one);
        assert_eq!(cursor.at, at(0, 999));
        assert_eq!(one.0.backs.get(), 1);
    }

    #[test]
    fn missing_lines_count_as_blank() {
        let text = Text::new(&["one", "two"]);
        let mut cursor = Cursor::new(at(1, 2));
        // Keeping the column.
        cursor.apply(Motion::First, &text);
        assert_eq!(cursor.at, at(0, 2));
        cursor.apply(Motion::Last, &text);
        assert_eq!(cursor.at, at(1, 2));
    }
}
