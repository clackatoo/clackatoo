//! A file's text as the Vim engine and the screen see it: each line laid
//! out in cells, as a terminal's rows are, with where in the text each
//! cell's character is.
//!
//! Lines end at `\n` only, as Vim's do, and the line break that ends the
//! last line isn't the start of another. A tab takes the cells to the next
//! multiple of the tab stop (the `tabstop` option), drawn blank; a control
//! character is drawn as `^` and a letter, as Vim draws it.
//!
//! With `wrap`, a line too long for the pane is cut into screen rows (see
//! [`wrap`]), which the Vim engine asks for with [`Lines::screen_rows`].
//!
//! Inlay hints take cells on screen, but none of a line laid out: the Vim
//! engine never sees them (see [`hints`](super::hints)). Where a line's
//! are drawn, in its cells, is [`Text::inlay_cells`] and
//! [`Text::inlays_in`].
//!
//! The Vim engine reads a laid out line's cells straight from where its
//! characters start (see [`Cells`]), as it reads a terminal's row, so it
//! moves through a line of any length, and nothing is kept for each cell.

use std::rc::Rc;

use clackatoo_protocol::BufferId;
use clackatoo_term::{MAX_EXTRA, RowBuilder, Style, Underline};
use clackatoo_text::{Diagnostic, Rope, Span};

use super::Prose;
use super::hints::{Hint, Hints};
use super::kept::{Kept, LineLog};
use super::wrap::{Placed, Wrap};
use crate::colors;
use crate::motion::{Cells, Lines, Point, ScreenRow, Spot, Words};

/// The column after a tab that starts at column `col`, with tabs stopping
/// at multiples of `tabstop`.
pub(crate) fn tab_end(col: usize, tabstop: usize) -> usize {
    (col / tabstop + 1) * tabstop
}

/// What the characters laid out so far say of the next, as a terminal's
/// row keeps it ([`RowBuilder`]).
#[derive(Clone, Copy, Default)]
struct Joining {
    /// Whether the last character drawn in the cell before is a joiner, so
    /// the next is drawn in it too.
    joins: bool,
    /// How many characters are drawn in the cell before after its first:
    /// past [`MAX_EXTRA`], more are dropped.
    extras: usize,
}

/// The cells `ch` takes, laid out at cell `col` after what `joining` says,
/// as a terminal's row lays it out ([`RowBuilder`]): none if it's drawn in
/// the cell before, as an accent is, or after a joiner, or if it's dropped,
/// as it is with no cell before, or with that cell full. `joining` is kept
/// up to date.
fn cells_of(ch: char, col: usize, tabstop: usize, joining: &mut Joining) -> usize {
    let width = match ch {
        '\t' => tab_end(col, tabstop) - col,
        // `^` and a letter, or U+FFFD.
        '\0'..='\x1f' | '\x7f' => 2,
        ch if ch.is_control() => 1,
        ch => match clackatoo_term::char_width(ch) {
            Some(width) if width > 0 && !joining.joins => width,
            // Dropped, changing nothing: with no width at all, with no cell
            // before, or with that cell as full as a cell can be.
            None => return 0,
            Some(_) if col == 0 || joining.extras >= MAX_EXTRA => return 0,
            Some(_) => {
                joining.extras += 1;
                joining.joins = ch == '\u{200d}';
                return 0;
            }
        },
    };
    *joining = Joining::default();
    width
}

/// Whether `bytes` are all plain ASCII, one cell each: no tabs, no
/// controls.
fn plain(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| (b' '..=b'~').contains(byte))
}

/// What has a line under it as a line is drawn: a wavy one under what
/// diagnostics are about, and a straight one under what a hover is
/// about, as bytes of the text.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Underlines<'a> {
    pub(crate) diagnostics: &'a [&'a Diagnostic],
    pub(crate) hovered: Option<(usize, usize)>,
}

/// An inlay hint, placed on a laid out line: drawn just before the
/// character at cell `col`, or after the line's end if that's its end.
/// It's the `index`th of the buffer's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Inlay<'h> {
    pub(crate) col: usize,
    pub(crate) hint: &'h Hint,
    pub(crate) index: usize,
}

/// One line, laid out in cells.
#[derive(Clone, Debug)]
pub(crate) struct Laid {
    /// The line's text, without its line break.
    text: String,
    /// Each character's first cell, and its byte in the line, in order.
    /// Characters drawn in the cell before, such as accents, have none.
    starts: Starts,
    /// The cells the line fills.
    cells: usize,
}

/// Where each character of a laid out line starts: its first cell, and
/// its byte.
#[derive(Clone, Debug)]
enum Starts {
    /// As many characters, each starting where one byte and one cell after
    /// the last does, as plain ASCII without tabs: the `n`th starts at cell
    /// and byte `n`, so nothing need be kept, which matters on a minified
    /// file's long lines. The last may still be wider, take more bytes or
    /// have accents after it: see [`Laid::is_plain`].
    Plain(usize),
    /// Each one's, in 32 bits: no line is 4 GiB long.
    Table(Vec<(u32, u32)>),
}

impl Starts {
    fn len(&self) -> usize {
        match self {
            Self::Plain(count) => *count,
            Self::Table(table) => table.len(),
        }
    }

    fn get(&self, index: usize) -> Option<(usize, usize)> {
        match self {
            Self::Plain(count) => (index < *count).then_some((index, index)),
            Self::Table(table) => {
                (table.get(index)).map(|&(cell, byte)| (cell as usize, byte as usize))
            }
        }
    }

    /// How many characters start at or before cell `col`.
    fn to_cell(&self, col: usize) -> usize {
        match self {
            Self::Plain(count) => (col + 1).min(*count),
            Self::Table(table) => table.partition_point(|&(cell, _)| cell as usize <= col),
        }
    }

    /// How many characters start at or before byte `byte`.
    fn to_byte(&self, byte: usize) -> usize {
        match self {
            Self::Plain(count) => (byte + 1).min(*count),
            Self::Table(table) => table.partition_point(|&(_, start)| start as usize <= byte),
        }
    }

    /// Notes that the next character, a tab if `tab`, starts at `cell` and
    /// `byte`. A tab is never plain, even one a cell wide: moved along by
    /// an edit before it, it can take more.
    fn push(&mut self, cell: usize, byte: usize, tab: bool) {
        let narrow = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
        match self {
            Self::Plain(count) if cell == *count && byte == *count && !tab => *count += 1,
            Self::Plain(count) => {
                let mut table: Vec<(u32, u32)> =
                    (0..*count).map(|at| (narrow(at), narrow(at))).collect();
                table.push((narrow(cell), narrow(byte)));
                *self = Self::Table(table);
            }
            Self::Table(table) => table.push((narrow(cell), narrow(byte))),
        }
    }

    /// Forgets all but the first `count`.
    fn truncate(&mut self, count: usize) {
        match self {
            Self::Plain(kept) => *kept = count.min(*kept),
            Self::Table(table) => table.truncate(count),
        }
    }

    /// The bytes it takes.
    fn size(&self) -> usize {
        match self {
            Self::Plain(_) => 0,
            Self::Table(table) => table.capacity() * size_of::<(u32, u32)>(),
        }
    }
}

#[cfg(test)]
thread_local! {
    /// How many characters have been laid out one by one, for tests that
    /// check a line is laid out again only from where it changed, and a
    /// plain one not even that.
    pub(super) static COUNTED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Lays out `text` from byte `from`, where a character starts, with
/// `starts` holding where the characters before start, and the first
/// `cells` cells theirs. The character before `from`, if there's one,
/// isn't a joiner. From byte `plain` on, the text is known to be plain
/// ASCII without tabs, so once the line is as plain as that so far, the
/// rest needn't be looked at.
fn lay(
    text: String,
    from: usize,
    plain: usize,
    tabstop: usize,
    mut cells: usize,
    mut starts: Starts,
) -> Laid {
    let mut joining = Joining::default();
    for (offset, ch) in text[from..].char_indices() {
        let byte = from + offset;
        if byte >= plain
            && !joining.joins
            && cells == byte
            && matches!(starts, Starts::Plain(n) if n == byte)
        {
            // Plain so far, and from here on: a cell for each byte.
            starts = Starts::Plain(text.len());
            cells = text.len();
            break;
        }
        #[cfg(test)]
        COUNTED.with(|counted| counted.set(counted.get() + 1));
        let width = cells_of(ch, cells, tabstop, &mut joining);
        if width > 0 {
            starts.push(cells, byte, ch == '\t');
            cells += width;
        }
    }
    match &mut starts {
        // Plain after all, as a line is again once the one character that
        // wasn't is taken out: a character to each byte and to each cell,
        // and no tabs, so each starts where its byte is.
        Starts::Table(table)
            if table.len() == text.len()
                && cells == text.len()
                && !text.as_bytes().contains(&b'\t') =>
        {
            starts = Starts::Plain(text.len());
        }
        Starts::Table(table) if table.capacity() > table.len() + table.len() / 4 => {
            table.shrink_to_fit();
        }
        _ => {}
    }
    Laid {
        text,
        starts,
        cells,
    }
}

impl Laid {
    /// Lays out `text`, with tabs stopping at multiples of `tabstop`.
    pub(crate) fn new(text: String, tabstop: usize) -> Self {
        Self::hiding(text, tabstop, 0)
    }

    /// Lays out `text`, with tabs stopping at multiples of `tabstop`, and
    /// its first `hidden` bytes, where a character starts, taking no
    /// cells: nothing's drawn of them, and no cell is theirs, so the
    /// cursor never goes there. A line with any hidden is never plain, as
    /// its first character shown isn't at its first byte.
    pub(crate) fn hiding(text: String, tabstop: usize, hidden: usize) -> Self {
        let hidden = (0..=hidden.min(text.len()))
            .rev()
            .find(|&at| text.is_char_boundary(at))
            .unwrap_or(0);
        // Plain ASCII is seen at a glance, quicker than a character at a
        // time.
        let plain = if hidden == 0 && plain(text.as_bytes()) {
            0
        } else {
            text.len()
        };
        lay(text, hidden, plain, tabstop, 0, Starts::Plain(0))
    }

    /// The line laid out again, its text now its first `from` bytes and
    /// then `rest`. It's as it was before byte `from`, and in its last
    /// `tail` bytes, but may have changed between. What's laid out before
    /// the change is kept: the line is laid out afresh only from the last
    /// character to start before `from`, as a character after it, an
    /// accent, may join it; and a plain line is still one if what changed
    /// is, so its rest needn't be looked at.
    ///
    /// Returns the line, and the cell it's laid out afresh from: before
    /// it, everything is as it was. `from` must be where a character of
    /// the text starts, or its end.
    pub(super) fn again<'r>(
        mut self,
        rest: impl Iterator<Item = &'r str>,
        from: usize,
        tail: usize,
        tabstop: usize,
    ) -> (Self, usize) {
        // Asked before its text is taken.
        let was_plain = self.is_plain();
        // Its text, kept as far as it hasn't changed, which saves copying
        // all of a long line typed at the end of.
        let mut text = std::mem::take(&mut self.text);
        text.truncate(from);
        rest.for_each(|chunk| text.push_str(chunk));
        if from == 0 {
            return (Self::new(text, tabstop), 0);
        }
        // What's before and after the change is as plain as it was.
        let changed = (text.as_bytes()).get(from..text.len().saturating_sub(tail).max(from));
        let plain = if was_plain && changed.is_some_and(plain) {
            0
        } else {
            text.len()
        };
        // The last character to start before `from`.
        let Some(index) = self.starts.to_byte(from - 1).checked_sub(1) else {
            return (Self::new(text, tabstop), 0);
        };
        let (cell, byte) = self.starts.get(index).unwrap_or_default();
        let mut starts = self.starts;
        starts.truncate(index);
        (lay(text, byte, plain, tabstop, cell, starts), cell)
    }

    /// Checks it's laid out as `fresh` is, every cell and start.
    #[cfg(test)]
    pub(super) fn assert_as(&self, fresh: &Self, context: &str) {
        assert_eq!(self.text, fresh.text, "{context}");
        assert_eq!(self.cells, fresh.cells, "{context}: {:?}", self.text);
        let starts = |laid: &Self| -> Vec<_> {
            (0..laid.starts.len())
                .map(|index| laid.starts.get(index))
                .collect()
        };
        assert_eq!(starts(self), starts(fresh), "{context}: {:?}", self.text);
        assert_eq!(self.is_plain(), fresh.is_plain(), "{context}");
        for laid in [self, fresh] {
            let truly = laid.text.chars().count() == laid.text.len()
                && laid.cells == laid.text.len()
                && plain(laid.text.as_bytes());
            assert_eq!(laid.is_plain(), truly, "{context}: {:?}", laid.text);
        }
        assert!(
            self.on_from(0).eq(fresh.on_from(0)),
            "{context}: {:?}",
            self.text
        );
    }

    /// Whether it keeps a table of where each character starts.
    #[cfg(test)]
    pub(super) fn keeps_starts(&self) -> bool {
        matches!(self.starts, Starts::Table(_))
    }

    /// Whether every character is one byte and one cell, as plain ASCII
    /// without tabs is. Where characters start says so only as far as the
    /// last one's start: it may be wider, longer, or followed by an accent.
    pub(crate) fn is_plain(&self) -> bool {
        matches!(self.starts, Starts::Plain(n) if n == self.text.len() && n == self.cells)
    }

    /// How many characters it has apart: not counting those drawn in the
    /// cell before, such as accents.
    pub(crate) fn count(&self) -> usize {
        self.starts.len()
    }

    /// The `index`th character apart: its first cell, the cell after it,
    /// and the character.
    pub(crate) fn piece(&self, index: usize) -> (usize, usize, char) {
        let (cell, byte) = self.starts.get(index).unwrap_or_default();
        let next = (self.starts.get(index + 1)).map_or(self.cells, |(next, _)| next);
        let ch = self.text[byte..].chars().next().unwrap_or(' ');
        (cell, next, ch)
    }

    /// Which character apart cell `col` is in: the last to start at or
    /// before it.
    pub(crate) fn piece_at(&self, col: usize) -> usize {
        self.starts.to_cell(col).saturating_sub(1)
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The cells the line fills.
    pub(crate) fn cells(&self) -> usize {
        self.cells
    }

    /// About how many bytes it takes.
    pub(super) fn size(&self) -> usize {
        self.text.capacity() + self.starts.size()
    }

    /// The byte, in the line, of the character at cell `col`, or the
    /// line's end past its last.
    pub(crate) fn byte_at(&self, col: usize) -> usize {
        if col >= self.cells {
            return self.text.len();
        }
        match self.starts.to_cell(col) {
            0 => 0,
            after => self.starts.get(after - 1).map_or(0, |(_, byte)| byte),
        }
    }

    /// The first cell of the character at `byte`, or past the line's last,
    /// the cell after it.
    pub(crate) fn col_of(&self, byte: usize) -> usize {
        if byte >= self.text.len() {
            return self.cells;
        }
        match self.starts.to_byte(byte) {
            0 => 0,
            after => self.starts.get(after - 1).map_or(0, |(cell, _)| cell),
        }
    }

    /// The first cell of the character before the one at `col`, if there's
    /// one.
    pub(crate) fn col_before(&self, col: usize) -> Option<usize> {
        let index = self.starts.to_cell(col).checked_sub(2)?;
        self.starts.get(index).map(|(cell, _)| cell)
    }

    /// Each character's first cell, the cell after it, and the character,
    /// in order. Characters drawn in the cell before, such as accents,
    /// aren't apart.
    pub(crate) fn pieces(&self) -> impl Iterator<Item = (usize, usize, char)> + '_ {
        (0..self.starts.len()).map(|index| self.piece(index))
    }

    /// The byte after the character at `byte`, with any drawn in its cell.
    pub(crate) fn char_end(&self, byte: usize) -> usize {
        let after = self.starts.to_byte(byte);
        (self.starts.get(after)).map_or(self.text.len(), |(_, end)| end)
    }

    /// Appends the line to `builder` as drawn in `width` cells from cell
    /// `left`, coloured as `spans` say, in order and none overlapping, with
    /// the lines under it `underlines` says, and `inlays`, those from cell
    /// `left` on, in order, each before the character it's at, or after
    /// the last. The line starts at byte `start` of the text they're for.
    pub(crate) fn show<S: Styled>(
        &self,
        builder: &mut RowBuilder,
        (left, width): (usize, usize),
        spans: &[S],
        underlines: &Underlines<'_>,
        inlays: &[Inlay<'_>],
        start: usize,
    ) {
        // The cell the next hint is drawn before: one number to look at
        // for each character, as most lines have none.
        let mut inlays = inlays.iter();
        let mut next_inlay = inlays.next();
        let mut hinted_at = next_inlay.map_or(usize::MAX, |inlay| inlay.col);
        let Underlines {
            diagnostics,
            hovered,
        } = *underlines;
        // From the character at cell `left`, and the spans from its byte
        // on, found by halving: a row far into a long line doesn't step
        // through all that's before it.
        let first = self.starts.to_cell(left).saturating_sub(1);
        let first_byte = self.starts.get(first).map_or(0, |(_, byte)| byte);
        let spans = &spans[spans.partition_point(|span| span.end() <= start + first_byte)..];
        let mut spans = spans.iter().peekable();
        // The most a diagnostic about bytes `from` to `to` of the line
        // matters, if one is. One about no text marks the character where
        // it is, or the last, at the line's end.
        let marked = |from: usize, to: usize| {
            let (from, to) = (start + from, start + to);
            let line_end = start + self.text.len();
            (diagnostics.iter())
                .filter(|diagnostic| {
                    let (first, last) = (diagnostic.start, diagnostic.end);
                    (first < to && last > from)
                        || (first == last && first >= from && (first < to || to == line_end))
                })
                .map(|diagnostic| diagnostic.severity)
                .min()
        };
        let wavy = |style: Style, from: usize, to: usize| match marked(from, to) {
            Some(severity) => Style {
                underline: Underline::Curly,
                underline_color: colors::diagnostic(severity),
                ..style
            },
            None => style,
        };
        for index in first..self.starts.len() {
            let (cell, byte) = self.starts.get(index).unwrap_or_default();
            let (next_cell, next_byte) =
                (self.starts.get(index + 1)).unwrap_or((self.cells, self.text.len()));
            if next_cell <= left {
                continue;
            }
            if cell < left {
                // Half off the left edge: the rest of it, blank.
                let end = next_cell.min(left + width);
                builder.push(&" ".repeat(end - left), &Style::DEFAULT, true);
                continue;
            }
            if cell >= left + width {
                break;
            }
            while cell >= hinted_at {
                if let Some(inlay) = next_inlay {
                    inlay.hint.draw(builder);
                }
                next_inlay = inlays.next();
                hinted_at = next_inlay.map_or(usize::MAX, |inlay| inlay.col);
            }
            let piece = &self.text[byte..next_byte];
            while spans.next_if(|span| span.end() <= start + byte).is_some() {}
            let style = match spans.peek() {
                Some(span) if span.start() <= start + byte => span.style(),
                _ => Style::DEFAULT,
            };
            let style = if diagnostics.is_empty() {
                style
            } else {
                wavy(style, byte, next_byte)
            };
            // What a hover is about, under no wavy line.
            let style = match hovered {
                Some((first, last))
                    if start + byte < last
                        && start + next_byte > first
                        && style.underline == Underline::None =>
                {
                    Style {
                        underline: Underline::Single,
                        ..style
                    }
                }
                _ => style,
            };
            match piece.chars().next() {
                // As much of it as fits: a tab can go on over the next row
                // when lines wrap.
                Some('\t') => builder.push(
                    &" ".repeat(next_cell.min(left + width) - cell),
                    &Style {
                        fg: Style::DEFAULT.fg,
                        ..style
                    },
                    true,
                ),
                Some(ch) if ch.is_control() => push(builder, ch),
                _ => builder.push(piece, &style, false),
            }
        }
        // After the line's end, or past the row's edge, where nothing
        // shows.
        for inlay in next_inlay.into_iter().chain(inlays) {
            inlay.hint.draw(builder);
        }
    }
}

/// The cells of a laid out line are those a terminal's row would have for
/// it, worked out from where each character starts, as they're asked for.
impl Cells for Laid {
    fn width(&self) -> usize {
        self.cells
    }

    fn cell(&self, col: usize) -> Option<Spot> {
        if col >= self.cells {
            return None;
        }
        match self.starts {
            // Not the last, which may not be plain: one byte, one cell.
            Starts::Plain(count) if col + 1 < count => {
                Some(Spot::narrow(char::from(self.text.as_bytes()[col])))
            }
            _ => {
                let (cell, next, ch) = self.piece(self.piece_at(col));
                Some(spot_in(ch, cell, next, col))
            }
        }
    }

    fn on_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
        Scan::new(self, (col < self.cells).then_some(col), true)
    }

    fn back_from(&self, col: usize) -> impl Iterator<Item = (usize, Spot)> {
        let col = self.cells.checked_sub(1).map(|last| col.min(last));
        Scan::new(self, col, false)
    }

    fn extent(&self, col: usize) -> (usize, usize) {
        if col >= self.cells {
            return (col, col);
        }
        let (cell, next, _) = self.piece(self.piece_at(col));
        (cell, next.max(cell + 1) - 1)
    }
}

/// Cell `col` of character `ch`, which takes the cells from `cell` to
/// `next`, as a terminal's row has it ([`push`]): a tab's cells are blank,
/// a control character is `^` and a letter, or U+FFFD, and a wide
/// character's right half is a spacer.
fn spot_in(ch: char, cell: usize, next: usize, col: usize) -> Spot {
    match ch {
        '\t' => Spot::narrow(' '),
        '\0'..='\x1f' | '\x7f' if col == cell => Spot::narrow('^'),
        '\0'..='\x1f' | '\x7f' => Spot::narrow(letter_of(ch)),
        ch if ch.is_control() => Spot::narrow('\u{fffd}'),
        ch if col == cell => Spot {
            ch,
            wide: next - cell == 2,
            spacer: false,
        },
        _ => Spot::SPACER,
    }
}

/// The letter after `^` that control character `ch` is drawn as.
fn letter_of(ch: char) -> char {
    char::from(u8::try_from(ch).unwrap_or(b'?') ^ 0x40)
}

/// A laid out line's cells, one after another from one of them, forward or
/// back: a character at a time, which is quicker than finding each cell's.
struct Scan<'a> {
    laid: &'a Laid,
    /// Whether it's plain, a byte to each cell.
    plain: bool,
    forward: bool,
    /// The next cell, if there's one.
    col: Option<usize>,
    /// The character it's in: its index, then as [`Laid::piece`] has it.
    index: usize,
    piece: (usize, usize, char),
}

impl<'a> Scan<'a> {
    /// From cell `col` of `laid`, if it has that cell.
    fn new(laid: &'a Laid, col: Option<usize>, forward: bool) -> Self {
        let plain = laid.is_plain();
        let index = match col {
            Some(col) if !plain => laid.piece_at(col),
            _ => 0,
        };
        Self {
            laid,
            plain,
            forward,
            col,
            index,
            piece: laid.piece(index),
        }
    }
}

impl Iterator for Scan<'_> {
    type Item = (usize, Spot);

    fn next(&mut self) -> Option<(usize, Spot)> {
        let col = self.col?;
        if self.plain {
            let byte = self.laid.text.as_bytes()[col];
            self.col = if self.forward {
                Some(col + 1).filter(|&next| next < self.laid.cells)
            } else {
                col.checked_sub(1)
            };
            return Some((col, Spot::narrow(char::from(byte))));
        }
        let (cell, next, ch) = self.piece;
        self.col = if self.forward {
            if col + 1 < next {
                Some(col + 1)
            } else if self.index + 1 < self.laid.count() {
                self.index += 1;
                self.piece = self.laid.piece(self.index);
                Some(self.piece.0)
            } else {
                None
            }
        } else if col > cell {
            Some(col - 1)
        } else if let Some(index) = self.index.checked_sub(1) {
            self.index = index;
            self.piece = self.laid.piece(index);
            Some(self.piece.1 - 1)
        } else {
            None
        };
        Some((col, spot_in(ch, cell, next, col)))
    }
}

/// Lays `ch`, which isn't a tab, out at the end of `builder`.
fn push(builder: &mut RowBuilder, ch: char) {
    match ch {
        '\0'..='\x1f' | '\x7f' => {
            builder.push(&format!("^{}", letter_of(ch)), &Style::DEFAULT, true);
        }
        ch if ch.is_control() => builder.push("\u{fffd}", &Style::DEFAULT, true),
        ch => {
            let mut bytes = [0; 4];
            builder.push(ch.encode_utf8(&mut bytes), &Style::DEFAULT, false);
        }
    }
}

/// What colours some of a line, as [`Laid::show`] draws it: a highlight of
/// the syntax, or a style of the editor's own.
pub(crate) trait Styled {
    /// Its first byte, in the text.
    fn start(&self) -> usize;
    /// The byte after its last.
    fn end(&self) -> usize;
    /// How it's drawn.
    fn style(&self) -> Style;
}

impl Styled for Span {
    fn start(&self) -> usize {
        self.start
    }

    fn end(&self) -> usize {
        self.end
    }

    fn style(&self) -> Style {
        colors::syntax(self.kind)
    }
}

/// Bytes of the text drawn in a style of their own, as a lesson's titles
/// are bold and a listing's folders are coloured as the explorer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stretch {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) style: Style,
}

impl Styled for Stretch {
    fn start(&self) -> usize {
        self.start
    }

    fn end(&self) -> usize {
        self.end
    }

    fn style(&self) -> Style {
        self.style
    }
}

/// A buffer's text, laid out a line at a time as it's asked for. Lines
/// laid out, and the rows they wrap onto, are kept in a [`Kept`], which an
/// editor keeps from one frame to the next.
pub(crate) struct Text<'a> {
    rope: &'a Rope,
    /// The columns tabs stop at multiples of.
    tabstop: usize,
    /// How long lines wrap, if they do.
    wrap: Option<Wrap>,
    /// Where the cursor is typing in insert mode: at a line's end, the line
    /// has a cell more after it, for the cursor, when lines wrap.
    typing: Option<Point>,
    /// How much of the start of each line is hidden, if any is.
    hide: Option<fn(&str) -> usize>,
    /// What words are made of besides letters, digits and `_`.
    words: Words,
    /// The inlay hints drawn between the text, if they're shown.
    hints: Option<&'a Hints>,
    /// The lesson it is, if it's one, whose code is kept on one row as
    /// it wraps as prose.
    prose: Option<Rc<Prose>>,
    kept: Kept,
}

impl<'a> Text<'a> {
    /// `rope`, with tabs stopping at multiples of `tabstop`.
    pub(crate) fn new(rope: &'a Rope, tabstop: usize) -> Self {
        Self {
            rope,
            tabstop: tabstop.max(1),
            wrap: None,
            typing: None,
            hide: None,
            words: Words::NONE,
            hints: None,
            prose: None,
            kept: Kept::default(),
        }
    }

    /// The text as the lesson `prose` marks it, if it's one: its code
    /// kept on one row as it wraps as prose.
    pub(crate) fn for_lesson(self, prose: Option<Rc<Prose>>) -> Self {
        Self { prose, ..self }
    }

    /// The cells of line `line`, laid out as `laid`, that prose keeps
    /// together, each the first and the one after the last, in order.
    fn together_in(&self, line: u64, laid: &Laid) -> Vec<std::ops::Range<usize>> {
        let Some(prose) = &self.prose else {
            return Vec::new();
        };
        let start = self.line_start(line);
        let end = start + laid.text().len();
        let from = prose.code.partition_point(|&(_, last)| last <= start);
        (prose.code[from..].iter())
            .take_while(|&&(first, _)| first < end)
            .map(|&(first, last)| {
                laid.col_of(first.max(start) - start)..laid.col_of(last.min(end) - start)
            })
            .collect()
    }

    /// The text with `hints` drawn between it, if they're shown: but on
    /// the line the cursor types on, if it does, which shows none (see
    /// [`wrapping`](Self::wrapping)).
    pub(crate) fn hinted(self, hints: Option<&'a Hints>) -> Self {
        Self {
            hints: hints.filter(|hints| !hints.is_empty()),
            ..self
        }
    }

    /// Whether it has inlay hints drawn between it.
    pub(crate) fn has_hints(&self) -> bool {
        self.hints.is_some()
    }

    /// Where the inlay hints drawn on line `line`, laid out as `laid`, go,
    /// in order: each the cell it's drawn before, and the cells it takes,
    /// as its rows are wrapped with. None, at no cost, for a line with
    /// none, or for the line the cursor types on in insert mode.
    pub(crate) fn inlay_cells(&self, line: u64, laid: &Laid) -> Vec<(usize, usize)> {
        let Some(hints) = self.hints else {
            return Vec::new();
        };
        if self.typing.is_some_and(|at| at.line == line) {
            return Vec::new();
        }
        let start = self.line_start(line);
        (hints.all()[hints.range(start, start + laid.text().len())].iter())
            .map(|hint| (laid.col_of(hint.at - start), hint.cells))
            .collect()
    }

    /// The inlay hints drawn on line `line`, laid out as `laid`, before its
    /// cells from `from` to before `to`, or to its end and after it: those
    /// of one row, found by halving, so a long line's thousands aren't all
    /// looked at for each row it goes on over.
    pub(crate) fn inlays_in(
        &self,
        line: u64,
        laid: &Laid,
        (from, to): (usize, Option<usize>),
    ) -> Vec<Inlay<'a>> {
        let Some(hints) = self.hints else {
            return Vec::new();
        };
        if self.typing.is_some_and(|at| at.line == line) {
            return Vec::new();
        }
        let start = self.line_start(line);
        // The bytes of the characters those cells are in, both counted:
        // one that starts before `from`, or at `to`, is left out below.
        let last = to.map_or(laid.text().len(), |to| laid.byte_at(to));
        let on = hints.range(start + laid.byte_at(from), start + last);
        (hints.all()[on.clone()].iter().zip(on))
            .map(|(hint, index)| Inlay {
                col: laid.col_of(hint.at - start),
                hint,
                index,
            })
            .filter(|inlay| from <= inlay.col && to.is_none_or(|to| inlay.col < to))
            .collect()
    }

    /// The `index`th of its inlay hints, as [`inlays_in`](Self::inlays_in)
    /// and [`Inlay::index`] count them, if it has them.
    pub(crate) fn inlay(&self, Placed { col, hint, .. }: Placed) -> Option<Inlay<'a>> {
        Some(Inlay {
            col,
            hint: self.hints?.all().get(hint)?,
            index: hint,
        })
    }

    /// The text with `words` part of words, as its language says, for
    /// moving by words and finding them.
    pub(crate) fn with_words(self, words: Words) -> Self {
        Self { words, ..self }
    }

    /// The text with as much of the start of each line hidden as `hide`
    /// says, if it says.
    pub(crate) fn hiding(self, hide: Option<fn(&str) -> usize>) -> Self {
        Self { hide, ..self }
    }

    /// How many bytes of the start of `line`, a line of the text, are
    /// hidden.
    pub(crate) fn hidden(&self, line: &str) -> usize {
        self.hide.map_or(0, |hide| hide(line))
    }

    /// The byte what's shown of line `line` starts at: its start, but for
    /// what's hidden of it.
    pub(crate) fn shown_start(&self, line: u64) -> usize {
        let start = self.line_start(line);
        match self.hide {
            Some(_) => start + self.hidden(&self.line_text(line)),
            None => start,
        }
    }

    /// The text with long lines wrapped as `wrap` says, if it says, and
    /// room after the end of the line for the cursor typing at `typing`,
    /// in insert mode, if it's there.
    pub(crate) fn wrapping(self, wrap: Option<Wrap>, typing: Option<Point>) -> Self {
        Self {
            wrap,
            typing,
            ..self
        }
    }

    /// The text, revision `revision` of `buffer`'s, laid out into `kept`,
    /// which holds what's been laid out of it before, brought up to date
    /// through the edits since, as `log` has them.
    pub(crate) fn keeping(
        self,
        kept: &Kept,
        buffer: BufferId,
        revision: u64,
        log: &LineLog,
    ) -> Self {
        kept.bring_up_to(buffer, revision, self.tabstop, self.wrap, log);
        Self {
            kept: kept.clone(),
            ..self
        }
    }

    /// Whether long lines wrap.
    pub(crate) fn wraps(&self) -> bool {
        self.wrap.is_some()
    }

    pub(crate) fn rope(&self) -> &'a Rope {
        self.rope
    }

    /// How many lines there are: at least one, even for no text.
    pub(crate) fn lines(&self) -> u64 {
        let breaks = self.rope.len_lines() as u64;
        let ends_with_break =
            self.rope.len_bytes() > 0 && self.rope.byte(self.rope.len_bytes() - 1) == b'\n';
        if ends_with_break { breaks - 1 } else { breaks }
    }

    pub(crate) fn last_line(&self) -> u64 {
        self.lines() - 1
    }

    /// Line `line`, laid out, if there is one.
    pub(crate) fn laid(&self, line: u64) -> Option<Rc<Laid>> {
        if line >= self.lines() {
            return None;
        }
        let bytes = || (self.line_start(line), self.line_end(line));
        Some(
            self.kept
                .laid(line, self.rope, bytes, self.tabstop, self.hide),
        )
    }

    /// The text of line `line`, without its line break.
    pub(crate) fn line_text(&self, line: u64) -> String {
        let Ok(index) = usize::try_from(line) else {
            return String::new();
        };
        if index >= self.rope.len_lines() {
            return String::new();
        }
        let mut text = self.rope.line(index).to_string();
        if text.ends_with('\n') {
            text.pop();
        }
        text
    }

    /// The byte line `line` starts at. Past the last, the text's end.
    pub(crate) fn line_start(&self, line: u64) -> usize {
        match usize::try_from(line) {
            Ok(index) if index < self.rope.len_lines() => self.rope.line_to_byte(index),
            _ => self.rope.len_bytes(),
        }
    }

    /// The byte line `line` ends at, before its line break.
    pub(crate) fn line_end(&self, line: u64) -> usize {
        self.line_start(line) + self.line_len(line)
    }

    /// How many bytes line `line` has, without its line break.
    fn line_len(&self, line: u64) -> usize {
        match usize::try_from(line) {
            Ok(index) if index < self.rope.len_lines() => {
                let text = self.rope.line(index);
                let len = text.len_bytes();
                if len > 0 && text.byte(len - 1) == b'\n' {
                    len - 1
                } else {
                    len
                }
            }
            _ => 0,
        }
    }

    /// The byte of the character at `at`.
    pub(crate) fn byte_at(&self, at: Point) -> usize {
        let start = self.line_start(at.line);
        start + self.laid(at.line).map_or(0, |laid| laid.byte_at(at.col))
    }

    /// The byte after the character at `at`.
    pub(crate) fn char_end(&self, at: Point) -> usize {
        let start = self.line_start(at.line);
        let Some(laid) = self.laid(at.line) else {
            return start;
        };
        start + laid.char_end(laid.byte_at(at.col))
    }

    /// Where `byte` is: its line, and its character's first cell. Just
    /// after the line break that ends the last line, it's the start of the
    /// line after, which is there for typing on, as after Enter at the end.
    pub(crate) fn point_of(&self, byte: usize) -> Point {
        let len = self.rope.len_bytes();
        let byte = byte.min(len);
        if byte == len && len > 0 && self.rope.byte(len - 1) == b'\n' {
            return Point {
                line: self.lines(),
                col: 0,
            };
        }
        let line = (self.rope.byte_to_line(byte) as u64).min(self.last_line());
        let start = self.line_start(line);
        let col = self.laid(line).map_or(0, |laid| laid.col_of(byte - start));
        Point { line, col }
    }

    /// The text from byte `start` to byte `end`.
    pub(crate) fn slice(&self, start: usize, end: usize) -> String {
        self.rope.byte_slice(start..end).to_string()
    }
}

impl Lines for Text<'_> {
    type Line = Laid;
    type Row<'r>
        = Rc<Laid>
    where
        Self: 'r;

    fn row(&self, line: u64) -> Option<Rc<Laid>> {
        self.laid(line)
    }

    fn bounds(&self) -> (u64, u64) {
        (0, self.last_line())
    }

    fn empty(&self, line: u64) -> bool {
        self.line_len(line) == 0
    }

    fn screen_rows(&self, line: u64) -> Rc<[ScreenRow]> {
        let Some((wrap, laid)) = self.wrap.and_then(|wrap| Some((wrap, self.laid(line)?))) else {
            return Rc::new([ScreenRow::WHOLE]);
        };
        let typing = self.typing.filter(|at| at.line == line);
        let typed_at_end = typing.is_some_and(|at| at.col >= laid.cells());
        // Which hints there are, and whether the line hides them, as its
        // rows are kept for.
        let hinted = (self.hints).map(|hints| (hints.generation(), typing.is_some()));
        let inlays = || self.inlay_cells(line, &laid);
        let together = || self.together_in(line, &laid);
        self.kept
            .rows(line, &laid, wrap, typed_at_end, (hinted, inlays), together)
    }

    fn row_width(&self) -> Option<usize> {
        self.wrap.map(|wrap| wrap.width)
    }

    fn words(&self) -> Words {
        self.words
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_term::Row;

    use super::super::kept::tests::Random;
    use super::*;
    use crate::motion::{Cursor, Motion, text_end};
    use crate::objects::Object;

    #[test]
    fn lines_end_at_line_breaks_and_the_last_break_ends_the_last_line() {
        for (text, lines) in [("", 1), ("a", 1), ("a\n", 1), ("a\nb", 2), ("a\n\n", 2)] {
            let rope = Rope::from_str(text);
            assert_eq!(Text::new(&rope, 4).lines(), lines, "{text:?}");
        }
        let rope = Rope::from_str("one\ntwo\n");
        let text = Text::new(&rope, 4);
        assert_eq!(text.line_text(1), "two");
        assert_eq!((text.line_start(1), text.line_end(1)), (4, 7));
    }

    #[test]
    fn tabs_wide_characters_and_controls_take_their_cells() {
        let laid = Laid::new("\tx字\x01é".to_owned(), 4);
        assert_eq!(laid.cells(), 4 + 1 + 2 + 2 + 1);
        // Every cell of the tab is the tab.
        assert_eq!((laid.byte_at(0), laid.byte_at(3)), (0, 0));
        assert_eq!(laid.byte_at(4), 1);
        assert_eq!((laid.byte_at(5), laid.byte_at(6)), (2, 2));
        assert_eq!(laid.byte_at(7), 5, "^A");
        assert_eq!(laid.col_of(6), 9, "é");
        assert_eq!(laid.col_of(99), laid.cells());
        assert_eq!(laid.char_end(2), 5);
        assert_eq!(
            shown(&laid, 0, laid.cells(), &[], 0).text().trim_start(),
            "x字^Aé"
        );
    }

    #[test]
    fn a_tab_goes_to_the_next_tab_stop() {
        for (tabstop, cells) in [(4, [4, 4, 12]), (8, [8, 8, 16]), (3, [3, 3, 9])] {
            // A tab at the start, after two characters, and after eight.
            let starts = ["\t", "ab\t", "abcdefgh\t"];
            let laid: Vec<usize> = (starts.iter())
                .map(|text| Laid::new((*text).to_owned(), tabstop).cells())
                .collect();
            assert_eq!(laid, cells, "tabstop {tabstop}");
        }
    }

    /// Nothing underlined.
    const NO_UNDERLINES: Underlines<'static> = Underlines {
        diagnostics: &[],
        hovered: None,
    };

    /// `laid` as drawn in a row of `width` cells of its own.
    fn shown(laid: &Laid, left: usize, width: usize, spans: &[Span], start: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        laid.show(
            &mut builder,
            (left, width),
            spans,
            &NO_UNDERLINES,
            &[],
            start,
        );
        builder.finish(false)
    }

    #[test]
    fn a_line_is_drawn_from_any_column() {
        let laid = Laid::new("ab字cd".to_owned(), 4);
        assert_eq!(shown(&laid, 0, 4, &[], 0).text(), "ab字");
        assert_eq!(shown(&laid, 1, 3, &[], 0).text(), "b字");
        // Half of 字 off the left edge.
        assert_eq!(shown(&laid, 3, 3, &[], 0).text(), " cd");
    }

    #[test]
    fn a_line_is_coloured_as_its_spans_say() {
        use clackatoo_text::Kind;
        // The line "fn x()" starts at byte 10 of its text.
        let laid = Laid::new("fn x()".to_owned(), 4);
        let spans = [
            Span {
                start: 2,
                end: 9,
                kind: Kind::Comment,
            },
            Span {
                start: 10,
                end: 12,
                kind: Kind::Keyword,
            },
            Span {
                start: 13,
                end: 14,
                kind: Kind::Function,
            },
        ];
        let row = shown(&laid, 0, 6, &spans, 10);
        let fg = |col: usize| row.style(row.cells()[col]).fg;
        assert_eq!(fg(0), colors::syntax(Kind::Keyword).fg);
        assert_eq!(fg(1), colors::syntax(Kind::Keyword).fg);
        assert_eq!(fg(2), Style::DEFAULT.fg);
        assert_eq!(fg(3), colors::syntax(Kind::Function).fg);
        assert_eq!(fg(4), Style::DEFAULT.fg);
    }

    #[test]
    fn places_go_between_points_and_bytes() {
        let rope = Rope::from_str("a\tb\n字x\n");
        let text = Text::new(&rope, 4);
        let point = Point { line: 1, col: 2 };
        assert_eq!(text.byte_at(point), 7);
        assert_eq!(text.point_of(7), point);
        assert_eq!(text.char_end(Point { line: 1, col: 0 }), 7);
        assert_eq!(text.point_of(2), Point { line: 0, col: 4 });
        // After the last line break: the line after, to type on.
        assert_eq!(text.point_of(9), Point { line: 2, col: 0 });
        assert_eq!(text.byte_at(Point { line: 2, col: 0 }), 9);
    }

    #[test]
    fn a_line_longer_than_a_terminal_row_is_laid_out_whole() {
        let laid = Laid::new(format!("{}字\tb", "a".repeat(70_000)), 4);
        assert_eq!(laid.cells(), 70_000 + 2 + 2 + 1);
        assert_eq!(laid.col_of(70_004), 70_004, "b, after the tab");
        assert_eq!(laid.byte_at(70_001), 70_000, "字's second cell");
        // The Vim engine reads all its cells, past the 65,535 a terminal's
        // row can have.
        let wide = Spot {
            ch: '字',
            wide: true,
            spacer: false,
        };
        assert_eq!(laid.cell(70_000), Some(wide));
        assert_eq!(laid.cell(70_001), Some(Spot::SPACER));
        assert_eq!(laid.cell(70_003), Some(Spot::narrow(' ')), "the tab");
        assert_eq!(laid.cell(70_004), Some(Spot::narrow('b')));
        assert_eq!(laid.cell(70_005), None);
        let back: Vec<_> = laid.back_from(usize::MAX).take(3).collect();
        assert_eq!(
            back,
            [
                (70_004, Spot::narrow('b')),
                (70_003, Spot::narrow(' ')),
                (70_002, Spot::narrow(' '))
            ]
        );
        // Drawn from far along, as a row it wraps onto is.
        let mut builder = RowBuilder::new(5);
        let spans: &[Span] = &[];
        laid.show(&mut builder, (69_998, 5), spans, &NO_UNDERLINES, &[], 0);
        assert_eq!(builder.finish(false).text(), "aa字");
    }

    #[test]
    fn plain_lines_keep_no_starts() {
        // Plain ASCII needs no table of where characters start.
        let plain = Laid::new("ab c~".to_owned(), 4);
        assert!(matches!(plain.starts, Starts::Plain(5)));
        assert_eq!((plain.byte_at(3), plain.col_of(4)), (3, 4));
        let mixed = Laid::new("a\tb字".to_owned(), 4);
        assert!(matches!(mixed.starts, Starts::Table(_)));
        assert_eq!(mixed.col_before(5), Some(4));
        // A tab is never plain, even one a cell wide.
        assert!(!Laid::new("abc\td".to_owned(), 4).is_plain());
    }

    /// Characters of every kind the cells of a line are worked out for, and
    /// those motions and objects look for.
    const PIECES: &[&str] = &[
        "a",
        "bc",
        " ",
        "  ",
        "\t",
        "字",
        "é",
        "e\u{301}",
        "\u{301}",
        "\u{200d}",
        // As many accents as a cell can hold: any more are dropped.
        FULL,
        "👩\u{200d}💻",
        "\u{2764}\u{fe0f}",
        "\u{fe0f}",
        "🇬🇧",
        "\x01",
        "\x1c",
        "\x7f",
        "\u{85}",
        "(",
        ")",
        "\"",
        "\\",
        "_",
        "-",
    ];

    /// [`MAX_EXTRA`] accents.
    const FULL: &str = "\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";

    /// Plain ASCII, of the kinds motions look for.
    const PLAIN: &[&str] = &["a", "bc", " ", "(", ")", "\"", "\\", "_", "-"];

    /// A line of up to `most` pieces of `random`'s choosing: plain for
    /// some seeds, or plain but for its last character.
    fn line(random: &mut Random, seed: u64, most: usize) -> String {
        let pieces = if seed.is_multiple_of(3) {
            PLAIN
        } else {
            PIECES
        };
        let mut text: String = (0..random.below(most))
            .map(|_| pieces[random.below(pieces.len())])
            .collect();
        if seed.is_multiple_of(6) {
            text.push_str(PIECES[random.below(PIECES.len())]);
        }
        text
    }

    /// The row a terminal has for `text`, built as [`Laid::show`] draws
    /// it, a character at a time, with how many cells it fills.
    fn as_a_row(text: &str, tabstop: usize) -> (Row, usize) {
        let mut builder = RowBuilder::new(u16::MAX.into());
        for ch in text.chars() {
            if ch == '\t' {
                let col = builder.filled();
                let tab = " ".repeat(tab_end(col, tabstop) - col);
                builder.push(&tab, &Style::DEFAULT, true);
            } else {
                push(&mut builder, ch);
            }
        }
        let filled = builder.filled();
        (builder.finish(false), filled)
    }

    #[test]
    fn what_a_cell_can_hold_no_more_of_is_dropped_as_a_terminals_row_drops_it() {
        assert_eq!(FULL.chars().count(), MAX_EXTRA);
        // A joiner past what the cell holds is dropped, so doesn't join 字.
        let past = format!("a{FULL}\u{200d}字");
        // A joiner as the last the cell holds still joins what's after it,
        // though that's dropped too, as is the accent between.
        let last = format!("a{}\u{200d}\u{301}x", &FULL[2..]);
        for (text, cells) in [(past, 3), (last, 1)] {
            let laid = Laid::new(text.clone(), 4);
            assert_eq!(laid.cells(), cells, "{text:?}");
            assert_eq!(as_a_row(&text, 4).1, cells, "{text:?}");
            // Laid out again from a change after it, as afresh.
            let (again, _) = laid.again(std::iter::once("y"), text.len(), 0, 4);
            let typed = format!("{text}y");
            again.assert_as(&Laid::new(typed.clone(), 4), &typed);
            assert_eq!(again.cells(), as_a_row(&typed, 4).1, "{typed:?}");
        }
    }

    #[test]
    fn the_cells_the_vim_engine_reads_of_a_line_are_a_terminal_rows() {
        for seed in 1..=600_u64 {
            let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
            let tabstop = 1 + random.below(8);
            let text = line(&mut random, seed, 24);
            let laid = Laid::new(text.clone(), tabstop);
            let (row, filled) = as_a_row(&text, tabstop);
            assert_eq!(laid.cells(), filled, "{text:?}");
            let cells: Vec<_> = (0..=filled).map(|col| row.cell(col)).collect();
            let each: Vec<_> = (0..=filled).map(|col| laid.cell(col)).collect();
            assert_eq!(each[..filled], cells[..filled], "{text:?}");
            assert_eq!(each[filled], None, "{text:?}");
            assert_eq!(text_end(Some(&laid)), text_end(Some(&row)), "{text:?}");
            // Read one after another, either way, from anywhere.
            for col in 0..filled + 2 {
                let on: Vec<_> = laid.on_from(col).collect();
                let row_on: Vec<_> = (row.on_from(col))
                    .take_while(|&(at, _)| at < filled)
                    .collect();
                assert_eq!(on, row_on, "{text:?} on from {col}");
                let back: Vec<_> = laid.back_from(col).collect();
                let row_back: Vec<_> = match filled {
                    0 => Vec::new(),
                    _ => row.back_from(col.min(filled - 1)).collect(),
                };
                assert_eq!(back, row_back, "{text:?} back from {col}");
            }
        }
    }

    #[test]
    fn a_long_lines_cells_motions_and_objects_are_a_short_ones_moved_along() {
        // 66,360 cells, more than a terminal's row can have: a whole number
        // of tab stops of any size, ending in a blank, so what's after it
        // is laid out as it is alone.
        let before = "ab cd ".repeat(66_360 / 6);
        let shift = before.len();
        let along = |at: Point| match at.line {
            0 => Point {
                line: 0,
                col: at.col + shift,
            },
            _ => at,
        };
        let motions = [
            Motion::Left,
            Motion::Right,
            Motion::LineEnd,
            Motion::WordStart,
            Motion::WordEnd,
            Motion::WordBack,
            Motion::BigWordStart,
            Motion::BigWordEnd,
            Motion::BigWordBack,
            Motion::Match,
            Motion::Find {
                ch: 'a',
                forward: true,
                till: false,
            },
            Motion::Find {
                ch: '(',
                forward: false,
                till: true,
            },
        ];
        let objects = [
            Object::Word,
            Object::BigWord,
            Object::Quote('"'),
            Object::Bracket {
                open: '(',
                close: ')',
            },
        ];
        for seed in 1..=60_u64 {
            let mut random = Random(seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
            let tabstop = 1 + random.below(8);
            // Starting with a character of its own, which nothing before
            // it can join.
            let text = format!("a{}", line(&mut random, seed, 16));
            let short = Rope::from_str(&format!("{text}\nend\n"));
            let long = Rope::from_str(&format!("{before}{text}\nend\n"));
            let (short, long) = (Text::new(&short, tabstop), Text::new(&long, tabstop));
            let (laid, long_laid) = (short.laid(0).unwrap(), long.laid(0).unwrap());
            assert_eq!(long_laid.cells(), shift + laid.cells(), "{text:?}");
            for col in 0..=laid.cells() {
                assert_eq!(long_laid.cell(shift + col), laid.cell(col), "{text:?}");
            }
            let on = (long_laid.on_from(shift)).map(|(col, spot)| (col - shift, spot));
            assert!(on.eq(laid.on_from(0)), "{text:?}");
            let back = (long_laid.back_from(usize::MAX))
                .take(laid.cells())
                .map(|(col, spot)| (col - shift, spot));
            assert!(back.eq(laid.back_from(usize::MAX)), "{text:?}");

            // From a few of its characters; quotes from one, as they're
            // looked for from the line's start, and brackets from one for
            // some lines, as they're looked for a cell at a time through
            // all that's before, which takes a while.
            let starts: Vec<usize> = laid.pieces().map(|(cell, _, _)| cell).collect();
            for step in 0..starts.len().min(4) {
                let cell = starts[random.below(starts.len())];
                let from = Point { line: 0, col: cell };
                let context = format!("{text:?} from {cell}");
                for motion in motions {
                    let mut moved = Cursor::new(from);
                    moved.apply(motion, &short);
                    let mut far = Cursor::new(along(from));
                    far.apply(motion, &long);
                    // Unless it went back into what's before.
                    if far.at.line > 0 || far.at.col >= shift {
                        assert_eq!(along(moved.at), far.at, "{context}: {motion:?}");
                    }
                }
                let objects = match step {
                    0 if seed.is_multiple_of(3) => &objects[..],
                    0 => &objects[..3],
                    _ => &objects[..2],
                };
                for &object in objects {
                    for inner in [true, false] {
                        let found = object.find(&short, from, inner);
                        let far = object.find(&long, along(from), inner);
                        if far.is_none_or(|(start, _)| start.line > 0 || start.col >= shift) {
                            let found = found.map(|(start, last)| (along(start), along(last)));
                            assert_eq!(found, far, "{context}: {object:?}, {inner}");
                        }
                    }
                }
            }
        }
    }
}
