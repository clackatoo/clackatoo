//! Soft wrap: a line too long for the pane goes on over as many screen
//! rows as it needs, as Vim's do with `wrap`.
//!
//! A wrapped line is its laid out cells (see [`Laid`]) cut into rows: the
//! Vim engine's columns, and so the cursor's, stay the line's own, and
//! only drawing, scrolling and `gj` and `gk` see the rows. A wide
//! character never splits; a tab, being blank, can. With `linebreak`, a
//! row ends after the last blank or punctuation that fits, where there's
//! one. The rows the line goes on over start with its indent, with
//! `breakindent`, and `showbreak`.
//!
//! Rows are worked out a line at a time, as they're needed, from the
//! view's top or the cursor, never from the file's start, so a long file
//! scrolls as fast as a short one.
//!
//! An inlay hint takes cells on a row without being any of the line's
//! (see [`hints`](super::hints)): it goes with the character it's drawn
//! before, the two never split between rows, and one after the line's
//! end with its last character. A row's start is still one of the line's
//! cells, and the hints on it those drawn before its characters, from its
//! first to its last, and on its last row, those after the line's end.

use clackatoo_term::{RowBuilder, Style};

use super::text::{Laid, Text};
use crate::colors;
use crate::motion::{Cursor, Lines, RowAt, ScreenRow, row_index};
use crate::options::Options;

/// What the options say about wrapping long lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wrapping {
    /// They wrap (`wrap`).
    pub(crate) on: bool,
    linebreak: bool,
    breakindent: bool,
    showbreak: String,
    /// The cells `showbreak` takes.
    showbreak_cells: usize,
}

impl Wrapping {
    pub(crate) fn of(options: &Options) -> Self {
        let mut measure = RowBuilder::new(options.showbreak.chars().count() * 2);
        measure.push(&options.showbreak, &Style::DEFAULT, true);
        Self {
            on: options.wrap,
            linebreak: options.linebreak,
            breakindent: options.breakindent,
            showbreak: options.showbreak.clone(),
            showbreak_cells: measure.filled(),
        }
    }

    /// How lines wrap in rows of `width` cells, if they do.
    pub(crate) fn at(&self, width: usize) -> Option<Wrap> {
        self.on.then_some(Wrap {
            width,
            linebreak: self.linebreak,
            breakindent: self.breakindent,
            showbreak: self.showbreak_cells,
        })
    }

    /// Draws the `indent` cells before a row's text, on a row a line goes
    /// on over: blanks for its indent, then `showbreak`, if it fits.
    pub(crate) fn draw_indent(&self, builder: &mut RowBuilder, indent: usize) {
        if self.showbreak_cells > indent {
            // A row too narrow for it, which keeps a cell for text.
            builder.push(&" ".repeat(indent), &Style::DEFAULT, true);
            return;
        }
        let blanks = indent - self.showbreak_cells;
        builder.push(&" ".repeat(blanks), &Style::DEFAULT, true);
        builder.push(&self.showbreak, &colors::line_number(false), true);
    }
}

/// How long lines wrap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Wrap {
    /// The cells a row has for text: the pane's, right of the gutter.
    pub(crate) width: usize,
    /// Break between words, as `linebreak` says.
    pub(crate) linebreak: bool,
    /// Indent the rows a line goes on over as the line is (`breakindent`).
    pub(crate) breakindent: bool,
    /// The cells `showbreak` takes.
    pub(crate) showbreak: usize,
}

/// The characters `linebreak` breaks a row after, as Vim's `breakat`
/// does by default.
const BREAK_AFTER: &[char] = &[
    ' ', '\t', '!', '@', '*', '-', '+', ';', ':', ',', '.', '/', '?',
];

/// The fewest cells `breakindent` leaves for the text, as Vim's
/// `breakindentopt` does by default.
const LEAST_TEXT: usize = 20;

/// The screen rows `laid` goes on over, wrapped as `wrap` says, with
/// `inlays`, its inlay hints, each the cell it's drawn before and the
/// cells it takes, in order. If `room_after`, the cell after the line's
/// last has to fit too, for the cursor typing there.
pub(crate) fn rows(
    laid: &Laid,
    wrap: Wrap,
    room_after: bool,
    inlays: &[(usize, usize)],
) -> Vec<ScreenRow> {
    match Rows::of(laid, wrap, room_after, inlays) {
        Some(rows) => rows.go_on(laid, vec![ScreenRow::WHOLE], inlays),
        None => vec![ScreenRow::WHOLE],
    }
}

/// The screen rows `laid` goes on over, as [`rows`] has them, worked out
/// character by character even for a plain line: what tests check the
/// rest against.
#[cfg(test)]
pub(crate) fn rows_by_cells(laid: &Laid, wrap: Wrap, room_after: bool) -> Vec<ScreenRow> {
    match Rows::of(laid, wrap, room_after, &[]) {
        Some(rows) => rows.go_on_cells(laid, vec![ScreenRow::WHOLE], &[]),
        None => vec![ScreenRow::WHOLE],
    }
}

/// The screen rows `laid` goes on over, with `inlays`, as [`rows`] has
/// them, given `old`, its rows before what's from cell `changed` on
/// changed, its text or its inlay hints. Each row starts where what was on
/// the row before it decided, looking no further than that row's edge:
/// those whose rows before end before `changed` are kept, and only the
/// rest worked out again, so an edit far along a long line with hints
/// wraps only from there.
pub(crate) fn rows_again(
    laid: &Laid,
    wrap: Wrap,
    room_after: bool,
    old: &[ScreenRow],
    (changed, inlays): (usize, &[(usize, usize)]),
) -> Vec<ScreenRow> {
    let Some(rows) = Rows::of(laid, wrap, room_after, inlays) else {
        return vec![ScreenRow::WHOLE];
    };
    if old.first() != Some(&ScreenRow::WHOLE) {
        return rows.go_on(laid, vec![ScreenRow::WHOLE], inlays);
    }
    // Hints after the line's end go with its last character, wherever an
    // edit has left it.
    let changed = match laid.count().checked_sub(1) {
        Some(last) if inlays.last().is_some_and(|&(col, _)| col >= laid.cells()) => {
            changed.min(laid.piece(last).0)
        }
        _ => changed,
    };
    let mut kept = 1;
    while let Some(row) = old.get(kept)
        && row.indent == rows.indent
        && old[kept - 1].start + rows.room(kept - 1) < changed
    {
        kept += 1;
    }
    rows.go_on(laid, old[..kept].to_vec(), inlays)
}

/// How a line goes on over rows.
struct Rows {
    /// The cells a row has for text.
    width: usize,
    /// The cells before the text on the rows after the first.
    indent: usize,
    linebreak: bool,
    /// Whether the cell after the line's last has to fit too.
    room_after: bool,
}

impl Rows {
    /// How `laid` goes on over rows, wrapped as `wrap` says, if it doesn't
    /// fit on one with `inlays`.
    fn of(laid: &Laid, wrap: Wrap, room_after: bool, inlays: &[(usize, usize)]) -> Option<Self> {
        let width = wrap.width.max(1);
        // The hints counted only for a line that might fit without them.
        let fits = laid.cells() + usize::from(room_after) <= width
            && laid.cells()
                + usize::from(room_after)
                + inlays.iter().map(|&(_, cells)| cells).sum::<usize>()
                <= width;
        if fits {
            return None;
        }
        let indent = if wrap.breakindent {
            let blanks = (laid.pieces())
                .find(|&(_, _, ch)| ch != ' ' && ch != '\t')
                .map_or(laid.cells(), |(cell, _, _)| cell);
            blanks.min(width.saturating_sub(LEAST_TEXT))
        } else {
            0
        };
        Some(Self {
            width,
            // At least a cell for the text, however much is before it.
            indent: (indent + wrap.showbreak).min(width - 1),
            linebreak: wrap.linebreak,
            room_after,
        })
    }

    /// The cells the `row`th row has for text.
    fn room(&self, row: usize) -> usize {
        if row == 0 {
            self.width
        } else {
            self.width - self.indent
        }
    }

    /// `rows`, the first of the line's, worked out on from the last of
    /// them, with `inlays`, its inlay hints, in order.
    fn go_on(
        &self,
        laid: &Laid,
        rows: Vec<ScreenRow>,
        inlays: &[(usize, usize)],
    ) -> Vec<ScreenRow> {
        if laid.is_plain() {
            self.go_on_plain(laid, rows, inlays)
        } else {
            self.go_on_cells(laid, rows, inlays)
        }
    }

    /// [`go_on`](Self::go_on), character by character. A row ends at the
    /// first character that doesn't fit, with the inlay hints drawn before
    /// it, or with `linebreak`, after the last blank or punctuation before
    /// it, not in the blanks the row starts with, which would leave it
    /// empty. The next row starts afresh from there, so where each starts
    /// depends only on what's from there to its edge. Hints after the
    /// line's end go with its last character: a row of nothing but them
    /// would be one the cursor couldn't stop on, and `gj` would stick.
    fn go_on_cells(
        &self,
        laid: &Laid,
        mut rows: Vec<ScreenRow>,
        inlays: &[(usize, usize)],
    ) -> Vec<ScreenRow> {
        let end = laid.cells();
        let count = laid.count() + usize::from(self.room_after);
        let piece = |index| {
            if index < laid.count() {
                laid.piece(index)
            } else {
                (end, end + 1, 'x')
            }
        };
        let last = laid.count().checked_sub(1);
        let after_end: usize = (inlays[inlays.partition_point(|&(col, _)| col < end)..].iter())
            .map(|&(_, cells)| cells)
            .sum();
        let Some(&ScreenRow { mut start, .. }) = rows.last() else {
            return rows;
        };
        // The first hint not before the character being looked at: it only
        // moves on along a row, and is found afresh for each row.
        let mut hint = inlays.partition_point(|&(col, _)| col < start);
        let mut room = self.room(rows.len() - 1);
        // With `linebreak`, the cell after the last character on the row
        // that it can end after, and whether there's been a word yet; and
        // the cells the hints on the row so far take.
        let (mut break_at, mut words, mut used) = (None, false, 0);
        let mut index = laid.piece_at(start);
        while index < count {
            let (cell, next, ch) = piece(index);
            // Those before a character the row starts in the middle of are
            // on the row before.
            // The cell after the end, typed at, has none: those after the
            // end go with the last character.
            let extra = if cell >= start && index < laid.count() && !inlays.is_empty() {
                while inlays.get(hint).is_some_and(|&(col, _)| col < cell) {
                    hint += 1;
                }
                let before: usize = (inlays[hint..].iter())
                    .take_while(|&&(col, _)| col == cell)
                    .map(|&(_, cells)| cells)
                    .sum();
                before + if Some(index) == last { after_end } else { 0 }
            } else {
                0
            };
            if next + used + extra > start + room {
                // The line's cells the rest of the row has room for.
                let edge = (start + room).saturating_sub(used + extra);
                let to = if ch == '\t' && cell < edge {
                    Some(edge)
                } else if let Some(at) = break_at.filter(|&at| at > start && at <= cell) {
                    Some(at)
                } else {
                    // Unless it's wider than a whole row, and has one to
                    // itself.
                    (cell > start).then_some(cell)
                };
                if let Some(to) = to {
                    rows.push(ScreenRow {
                        start: to,
                        indent: self.indent,
                    });
                    (start, room, break_at, words, used) = (to, self.room(1), None, false, 0);
                    index = laid.piece_at(to);
                    hint = inlays.partition_point(|&(col, _)| col < to);
                    continue;
                }
            }
            used += extra;
            words |= ch != ' ' && ch != '\t';
            if self.linebreak && words && BREAK_AFTER.contains(&ch) {
                break_at = Some(next);
            }
            index += 1;
        }
        rows
    }

    /// [`go_on`](Self::go_on) for a plain line, every character a byte and
    /// a cell, a row at a time: the same rows, without looking at every
    /// character of a long line, only at its inlay hints, `inlays`.
    fn go_on_plain(
        &self,
        laid: &Laid,
        mut rows: Vec<ScreenRow>,
        inlays: &[(usize, usize)],
    ) -> Vec<ScreenRow> {
        let bytes = laid.text().as_bytes();
        let len = bytes.len();
        let count = len + usize::from(self.room_after);
        // Those after the line's end go with its last character.
        let (inlays, after) = inlays.split_at(inlays.partition_point(|&(col, _)| col < len));
        let after_end: usize = after.iter().map(|&(_, cells)| cells).sum();
        let Some(&ScreenRow { mut start, .. }) = rows.last() else {
            return rows;
        };
        let mut room = self.room(rows.len() - 1);
        loop {
            // The first cell that doesn't fit on the row, given the hints
            // before the characters that do: each hinted character, in
            // order, as far as the row's edge, less what hints before it
            // take.
            let mut hint = inlays.partition_point(|&(col, _)| col < start);
            let mut end_hinted = after_end > 0 && len > start;
            let mut used = 0;
            let to = loop {
                // At least the row's first character, wider than the row
                // with its hints as it may be.
                let edge = (start + room).saturating_sub(used).max(start + 1);
                let next = inlays.get(hint).map(|&(col, _)| col);
                let (cell, cells) = match next {
                    Some(col) if !end_hinted || col < len - 1 => {
                        let mut cells = 0;
                        while let Some(&(_, more)) = inlays.get(hint).filter(|&&(at, _)| at == col)
                        {
                            cells += more;
                            hint += 1;
                        }
                        if col == len - 1 && end_hinted {
                            end_hinted = false;
                            (col, cells + after_end)
                        } else {
                            (col, cells)
                        }
                    }
                    _ if end_hinted => {
                        end_hinted = false;
                        (len - 1, after_end)
                    }
                    _ => break edge,
                };
                if cell >= edge {
                    break edge;
                }
                // Unless it's wider than a whole row, and has one to
                // itself.
                if cell + 1 + used + cells > start + room && cell > start {
                    break cell;
                }
                used += cells;
            };
            if to >= count {
                return rows;
            }
            let on = &bytes[start..to.min(len)];
            // With `linebreak`, after the last blank or punctuation, if
            // there's a word at or before it.
            let last = self.linebreak.then(|| {
                (on.iter())
                    .rposition(|&byte| BREAK_AFTER.contains(&char::from(byte)))
                    .filter(|&last| on[..=last].iter().any(|&byte| byte != b' '))
            });
            let to = last.flatten().map_or(to, |last| start + last + 1);
            rows.push(ScreenRow {
                start: to,
                indent: self.indent,
            });
            (start, room) = (to, self.room(1));
        }
    }
}

/// Puts `cursor`, moved by screen rows, as `gj`, `gk`, paging and the
/// wheel move it, on a character that starts on the row it's on, where
/// it's in a tab begun on a row before: on the tab, going `up`, and going
/// down, on the character after it, or if there's none, the next line.
/// The column it wants stays as it was.
pub(crate) fn onto_row(cursor: &mut Cursor, text: &Text<'_>, up: bool) {
    let at = cursor.at;
    let Some(laid) = text.laid(at.line) else {
        return;
    };
    let rows = text.screen_rows(at.line);
    let start = rows[row_index(&rows, at.col)].start;
    let byte = laid.byte_at(at.col);
    let begun = laid.col_of(byte);
    if begun >= start {
        return;
    }
    let want = cursor.want();
    let next = laid.char_end(byte);
    if up {
        cursor.at.col = begun;
    } else if next < laid.text().len() {
        cursor.at.col = laid.col_of(next);
    } else if at.line < text.last_line() {
        let line = at.line + 1;
        cursor.go_to_row(RowAt { line, row: 0 }, text);
    } else {
        cursor.at.col = begun;
    }
    *cursor = Cursor::wanting(cursor.at, want);
}

/// Where an editor's view starts: a line, and the cell of it the first
/// row shown starts at, so that when the rows change, as a resize wraps
/// the line afresh, the view still starts at the same text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Top {
    pub(crate) line: u64,
    pub(crate) cell: usize,
}

impl Top {
    /// The screen row it's on, in `text`.
    pub(crate) fn row(self, text: &Text<'_>) -> RowAt {
        let rows = text.screen_rows(self.line);
        RowAt {
            line: self.line,
            row: row_index(&rows, self.cell),
        }
    }

    /// The top that's on `row`, in `text`.
    pub(crate) fn of(row: RowAt, text: &Text<'_>) -> Self {
        let rows = text.screen_rows(row.line);
        Self {
            line: row.line,
            cell: rows[row.row.min(rows.len() - 1)].start,
        }
    }
}

/// An inlay hint on a row shown: drawn before the line's cell `col`, or
/// after its end, taking `cells` cells. It's the `hint`th of the
/// buffer's, as [`Text::inlays_in`] counts them, until they change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub(crate) col: usize,
    pub(crate) cells: usize,
    pub(crate) hint: usize,
}

/// One screen row of an editor's view, showing part of a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) line: u64,
    /// Which of the line's rows it is, from 0.
    pub(crate) row: usize,
    /// The line's first cell on it: when lines don't wrap, the first cell
    /// in view.
    pub(crate) start: usize,
    /// The cells drawn before its text: see [`ScreenRow`].
    pub(crate) indent: usize,
    /// The cell the line's next row starts at, or `None` on its last.
    pub(crate) next: Option<usize>,
    /// The inlay hints on it, in order, as the range of them in the view's
    /// list (see [`view`]).
    pub(crate) hints: (u32, u32),
}

impl Shown {
    /// The cell after the last of the line on it: past the line's end on
    /// its last row.
    pub(crate) fn end(&self) -> usize {
        self.next.unwrap_or(usize::MAX)
    }

    /// Its inlay hints, of `placed`, the view's.
    pub(crate) fn hints<'p>(&self, placed: &'p [Placed]) -> &'p [Placed] {
        let (first, after) = (self.hints.0 as usize, self.hints.1 as usize);
        placed.get(first..after).unwrap_or_default()
    }

    /// Where cell `cell` of the line, on this row, is drawn, counting from
    /// the gutter: after the inlay hints before it if `after_hints`, or
    /// else where they start. Past the line's end, as a whole line's
    /// selection reaches, as far as there is.
    fn drawn_at(&self, cell: usize, placed: &[Placed], after_hints: bool) -> usize {
        let hinted: usize = (self.hints(placed).iter())
            .filter(|hint| hint.col < cell || (after_hints && hint.col == cell))
            .map(|hint| hint.cells)
            .sum();
        (self.indent + hinted).saturating_add(cell - self.start)
    }

    /// The screen column, counting from the gutter, that the line's column
    /// `column` is at on this row, as if it had no inlay hints, if it's on
    /// the row and in the `width` cells right of the gutter: as
    /// `colorcolumn` marks it, a straight line down the pane, as Neovim
    /// keeps it past virtual text.
    pub(crate) fn column(&self, column: usize, width: usize) -> Option<usize> {
        let col = self.indent + column.checked_sub(self.start)?;
        (column < self.end() && col < width).then_some(col)
    }

    /// Where cell `cell` of the line is drawn, counting from the gutter,
    /// if it's on this row and in the `width` cells right of the gutter,
    /// after any inlay hints, of `placed`, the view's, before it.
    pub(crate) fn col_of(&self, cell: usize, width: usize, placed: &[Placed]) -> Option<usize> {
        if cell < self.start || cell >= self.end() {
            return None;
        }
        let col = self.drawn_at(cell, placed, true);
        (col < width).then_some(col)
    }

    /// The cells, counting from the gutter and in the `width` right of it,
    /// that the line's cells from `from` to before `to` are drawn in on
    /// this row, given to `each` as runs: one, or more where inlay hints,
    /// of `placed`, the view's, between them split it. Past the row's end,
    /// to the pane's edge.
    pub(crate) fn runs(
        &self,
        (from, to): (usize, usize),
        width: usize,
        placed: &[Placed],
        mut each: impl FnMut(std::ops::Range<usize>),
    ) {
        let from = from.max(self.start);
        if from >= to.min(self.end()) {
            return;
        }
        let mut first = self.drawn_at(from, placed, true);
        // The cell the hints last passed are drawn before: several at one
        // place are drawn one after another.
        let mut at = None;
        for hint in (self.hints(placed).iter()).filter(|hint| from < hint.col && hint.col < to) {
            if at != Some(hint.col) {
                let before = self.drawn_at(hint.col, placed, false);
                if first < before.min(width) {
                    each(first..before.min(width));
                }
                (first, at) = (before, Some(hint.col));
            }
            first += hint.cells;
        }
        let end = if to > self.end() {
            width
        } else {
            self.drawn_at(to, placed, false).min(width)
        };
        if first < end {
            each(first..end);
        }
    }

    /// The line's cell at `col`, counting from the row's text, as a click
    /// there finds it: on an inlay hint, the character it's drawn before,
    /// or after the line's end, its end.
    pub(crate) fn cell_at(&self, col: usize, placed: &[Placed]) -> usize {
        let mut col = col.saturating_sub(self.indent);
        for hint in self.hints(placed) {
            let drawn = hint.col - self.start;
            if col < drawn {
                break;
            }
            if col < drawn + hint.cells {
                return hint.col;
            }
            col -= hint.cells;
        }
        self.start + col
    }
}

/// The rows of `text` in a view `height` rows high whose top row is
/// `top`, as far as line `end`, with the inlay hints on them put in
/// `placed`, which each row has a range of. When lines don't wrap, `left`
/// is the first cell shown, and the hints before it aren't.
pub(crate) fn view(
    text: &Text<'_>,
    (top, height): (RowAt, usize),
    end: u64,
    left: usize,
    placed: &mut Vec<Placed>,
) -> Vec<Shown> {
    placed.clear();
    let mut shown = Vec::with_capacity(height);
    let mut line = top.line;
    while shown.len() < height && line <= end {
        let rows = text.screen_rows(line);
        // Asked only of a text with hints, so lines without cost nothing.
        let laid = if text.has_hints() {
            text.laid(line)
        } else {
            None
        };
        let first = if line == top.line { top.row } else { 0 };
        for (row, screen) in rows.iter().enumerate().skip(first) {
            if shown.len() == height {
                break;
            }
            let start = if text.wraps() { screen.start } else { left };
            let next = rows.get(row + 1).map(|next| next.start);
            let from = placed.len();
            // Those of the row alone, found by halving: a long line's
            // thousands aren't all looked at for each of its rows.
            if let Some(laid) = &laid {
                placed.extend(
                    text.inlays_in(line, laid, (start, next))
                        .into_iter()
                        .map(|inlay| Placed {
                            col: inlay.col,
                            cells: inlay.hint.cells,
                            hint: inlay.index,
                        }),
                );
            }
            let index = |at: usize| u32::try_from(at).unwrap_or(u32::MAX);
            shown.push(Shown {
                line,
                row,
                start,
                indent: screen.indent,
                next,
                hints: (index(from), index(placed.len())),
            });
        }
        line += 1;
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(width: usize) -> Wrap {
        Wrap {
            width,
            linebreak: false,
            breakindent: false,
            showbreak: 0,
        }
    }

    /// Where `text`'s rows start, and their indents, wrapped as `wrap`
    /// says.
    fn starts(text: &str, wrap: Wrap) -> Vec<(usize, usize)> {
        let laid = Laid::new(text.to_owned(), 4);
        (rows(&laid, wrap, false, &[]).iter())
            .map(|row| (row.start, row.indent))
            .collect()
    }

    #[test]
    fn a_line_breaks_at_the_width_and_one_that_fits_doesnt() {
        assert_eq!(starts("abcdef", wrap(6)), [(0, 0)]);
        assert_eq!(starts("abcdefg", wrap(3)), [(0, 0), (3, 0), (6, 0)]);
        assert_eq!(starts("", wrap(3)), [(0, 0)]);
    }

    #[test]
    fn a_wide_character_goes_whole_onto_the_next_row() {
        // 字 would take cells 3 and 4 of a row of 4.
        assert_eq!(starts("abc字d", wrap(4)), [(0, 0), (3, 0)]);
        // Wider than the row: a row to itself.
        assert_eq!(starts("字字", wrap(1)), [(0, 0), (2, 0)]);
    }

    #[test]
    fn a_tab_goes_on_over_the_row_after() {
        // "ab" and a tab to cell 8, in rows of 5: the tab goes on over.
        let laid = Laid::new("ab\tc".to_owned(), 8);
        let rows = rows(&laid, wrap(5), false, &[]);
        assert_eq!(
            rows,
            [
                ScreenRow::WHOLE,
                ScreenRow {
                    start: 5,
                    indent: 0
                }
            ]
        );
        assert_eq!(laid.byte_at(rows[1].start), 2, "still the tab");
    }

    #[test]
    fn linebreak_breaks_after_blanks_and_punctuation() {
        let linebreak = Wrap {
            linebreak: true,
            ..wrap(12)
        };
        // As Neovim breaks them, at 12 cells.
        assert_eq!(starts("hello world foo", linebreak), [(0, 0), (12, 0)]);
        assert_eq!(
            starts("  indented long text here", linebreak),
            [(0, 0), (11, 0), (21, 0)]
        );
        assert_eq!(starts("abcdefghijkl mno", linebreak), [(0, 0), (12, 0)]);
        assert_eq!(starts("abcde.ghijklmnop", linebreak), [(0, 0), (6, 0)]);
        // A word longer than the row breaks where it has to.
        assert_eq!(
            starts("a abcdefghijklmnop", linebreak),
            [(0, 0), (2, 0), (14, 0)]
        );
        // Not after the blanks a row starts with, which would leave it
        // empty.
        assert_eq!(starts("    abcdefghijklmnop", linebreak), [(0, 0), (12, 0)]);
        assert_eq!(
            starts("abcdefghijkl    mnopqrstuvwxyz", linebreak),
            [(0, 0), (12, 0), (24, 0)]
        );
    }

    #[test]
    fn breakindent_and_showbreak_start_the_rows_after_in() {
        let indented = Wrap {
            breakindent: true,
            ..wrap(30)
        };
        let text = "    abcdefghijklmnopqrstuvwxyz0123456789";
        assert_eq!(starts(text, indented), [(0, 0), (30, 4)]);
        let both = Wrap {
            showbreak: 2,
            ..indented
        };
        assert_eq!(starts(text, both), [(0, 0), (30, 6)]);
        // Each row after has the cells the indent leaves.
        assert_eq!(starts(&format!("{text}{text}"), both).len(), 4);
        // At least 20 cells are kept for the text: 2 to indent by in 22,
        // none in 20, but `showbreak` still shows.
        let narrow = |width| Wrap { width, ..both };
        assert_eq!(starts(text, narrow(22))[1], (22, 4));
        assert_eq!(starts(text, narrow(20))[1], (20, 2));
        // A row always has a cell for text, however long `showbreak` is.
        let long = Wrap {
            showbreak: 50,
            ..wrap(4)
        };
        assert_eq!(starts("abcdef", long), [(0, 0), (4, 3), (5, 3)]);
    }

    #[test]
    fn a_word_carried_onto_a_narrower_row_breaks_at_its_edge() {
        // After "a-", the rest goes on over rows of 5, after `showbreak`.
        let narrow = Wrap {
            linebreak: true,
            showbreak: 10,
            ..wrap(15)
        };
        let text = format!("a-{}", "b".repeat(40));
        assert_eq!(
            starts(&text, narrow)[..4],
            [(0, 0), (2, 10), (7, 10), (12, 10)]
        );
    }

    #[test]
    fn plain_lines_wrap_as_any_other_does() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut below = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % n as u64).unwrap()
        };
        // Some end in a character that isn't plain, which starts where a
        // plain one would: wider, longer, a control, or with an accent or
        // a joiner after it.
        let ends = [
            "", "", "字", "é", "\x01", "\u{301}", "\u{200d}", "e\u{301}", "\t",
        ];
        for _ in 0..4000 {
            let bytes = b"ab  -,.;x";
            let mut text: String = (0..below(80))
                .map(|_| char::from(bytes[below(bytes.len())]))
                .collect();
            let end = ends[below(ends.len())];
            text.push_str(end);
            let laid = Laid::new(text, 4);
            assert_eq!(laid.is_plain(), end.is_empty(), "{:?}", laid.text());
            let wrap = Wrap {
                width: 1 + below(20),
                linebreak: below(2) == 0,
                breakindent: below(2) == 0,
                showbreak: below(4),
            };
            let room_after = below(2) == 0;
            // With hints before some characters, and after the end.
            let mut inlays: Vec<(usize, usize)> = Vec::new();
            for col in 0..laid.cells() {
                if below(5) == 0 {
                    inlays.push((col, 1 + below(5)));
                    if below(4) == 0 {
                        inlays.push((col, 1 + below(3)));
                    }
                }
            }
            if below(2) == 0 {
                inlays.push((laid.cells(), 1 + below(5)));
            }
            let inlays = if below(2) == 0 { &inlays[..] } else { &[] };
            let Some(rows) = Rows::of(&laid, wrap, room_after, inlays) else {
                continue;
            };
            assert_eq!(
                rows.go_on(&laid, vec![ScreenRow::WHOLE], inlays),
                rows.go_on_cells(&laid, vec![ScreenRow::WHOLE], inlays),
                "{:?} with {inlays:?} in {wrap:?}, room after: {room_after}",
                laid.text(),
            );
        }
        // As found in review: a wide character doesn't split, and an
        // accent takes no row of its own.
        assert_eq!(starts("aaaaaaaaa字", wrap(5)), [(0, 0), (5, 0), (9, 0)]);
        assert_eq!(starts("aaaaaaé", wrap(5)), [(0, 0), (5, 0)]);
    }

    #[test]
    fn lines_with_hints_wrap_inside_the_width_with_text_on_every_row() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut below = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % n as u64).unwrap()
        };
        let pieces = ["a", "b", " ", ",", "字", "\t", "é"];
        for _ in 0..3000 {
            let text: String = (0..below(40))
                .map(|_| pieces[below(pieces.len())])
                .collect();
            let laid = Laid::new(text, 4);
            // Hints before some characters, and after the end.
            let mut inlays: Vec<(usize, usize)> = Vec::new();
            for (cell, _, _) in laid.pieces() {
                if below(4) == 0 {
                    inlays.push((cell, 1 + below(6)));
                }
            }
            if below(2) == 0 {
                inlays.push((laid.cells(), 1 + below(6)));
            }
            let wrap = Wrap {
                width: 1 + below(16),
                linebreak: below(2) == 0,
                breakindent: below(2) == 0,
                showbreak: below(3),
            };
            let rows = rows(&laid, wrap, false, &inlays);
            let context = format!("{:?} with {inlays:?} in {wrap:?}: {rows:?}", laid.text());
            assert_eq!(rows[0], ScreenRow::WHOLE, "{context}");
            for (index, row) in rows.iter().enumerate() {
                let next = rows.get(index + 1).map_or(usize::MAX, |next| next.start);
                assert!(row.start < next, "{context}");
                // A row of its own text, never of hints alone.
                assert!(row.start < laid.cells() || laid.cells() == 0, "{context}");
                // As wide as the row, but for a character and its hints
                // too wide for any row, which has one to itself.
                let on = |col: usize| row.start <= col && (col < next || next == usize::MAX);
                let hinted: usize = (inlays.iter())
                    .filter(|&&(col, _)| on(col))
                    .map(|&(_, cells)| cells)
                    .sum();
                let end = next.min(laid.cells());
                let room = wrap.width - if index == 0 { 0 } else { row.indent };
                let alone = laid.piece_at(row.start) == laid.piece_at(end.max(1) - 1);
                assert!(
                    end - row.start + hinted <= room || alone,
                    "row {index}: {context}"
                );
            }
        }
    }

    #[test]
    fn a_line_with_hints_wraps_again_from_where_it_changed_as_it_would_afresh() {
        use super::super::kept::tests::Random;
        const PIECES: [&str; 8] = ["a", "b", " ", ",", "字", "\t", "é", "-"];
        fn text(random: &mut Random, most: usize) -> String {
            (0..random.below(most))
                .map(|_| PIECES[random.below(PIECES.len())])
                .collect()
        }
        // Those of `keep` before cell `from`, and some of the line's own
        // from there on, after its end too.
        fn hints(
            random: &mut Random,
            laid: &Laid,
            keep: &[(usize, usize)],
            from: usize,
        ) -> Vec<(usize, usize)> {
            let mut inlays: Vec<(usize, usize)> = (keep.iter().copied())
                .filter(|&(col, _)| col < from)
                .collect();
            for (cell, _, _) in laid.pieces().filter(|&(cell, _, _)| cell >= from) {
                if random.below(3) == 0 {
                    inlays.push((cell, 1 + random.below(5)));
                }
            }
            if random.below(2) == 0 {
                inlays.push((laid.cells(), 1 + random.below(5)));
            }
            inlays
        }
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for _ in 0..3000 {
            let before = text(&mut random, 60);
            let cut = (0..=random.below(before.len() + 1))
                .rev()
                .find(|&at| before.is_char_boundary(at))
                .unwrap_or(0);
            let after = format!("{}{}", &before[..cut], text(&mut random, 20));
            let (old, new) = (Laid::new(before, 4), Laid::new(after, 4));
            // What's laid out again of the new text starts from the
            // character before the cut, which an accent may join.
            let from = old.col_before(old.col_of(cut)).unwrap_or(0);
            let old_inlays = hints(&mut random, &old, &[], 0);
            let new_inlays = hints(&mut random, &new, &old_inlays, from);
            let wrap = Wrap {
                width: 2 + random.below(14),
                linebreak: random.below(2) == 0,
                breakindent: random.below(2) == 0,
                showbreak: random.below(3),
            };
            let rows_before = rows(&old, wrap, false, &old_inlays);
            // As kept rows find it: where the text changed, or the first
            // hint that isn't as it was.
            let differ = (old_inlays.iter().zip(&new_inlays))
                .position(|(was, now)| was != now)
                .unwrap_or_else(|| old_inlays.len().min(new_inlays.len()));
            let changed = (old_inlays.get(differ).into_iter())
                .chain(new_inlays.get(differ))
                .map(|&(col, _)| col)
                .fold(from, usize::min);
            assert_eq!(
                rows_again(&new, wrap, false, &rows_before, (changed, &new_inlays)),
                rows(&new, wrap, false, &new_inlays),
                "{:?} to {:?}, {old_inlays:?} to {new_inlays:?}, in {wrap:?}",
                old.text(),
                new.text(),
            );
        }
    }

    #[test]
    fn the_cell_after_the_end_fits_while_its_typed_at() {
        let laid = Laid::new("abcd".to_owned(), 4);
        assert_eq!(rows(&laid, wrap(4), false, &[]).len(), 1);
        let rows = rows(&laid, wrap(4), true, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].start, 4);
    }
}
