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

/// The screen rows `laid` goes on over, wrapped as `wrap` says. If
/// `room_after`, the cell after the line's last has to fit too, for the
/// cursor typing there.
pub(crate) fn rows(laid: &Laid, wrap: Wrap, room_after: bool) -> Vec<ScreenRow> {
    let width = wrap.width.max(1);
    let mut rows = vec![ScreenRow::WHOLE];
    if laid.cells() + usize::from(room_after) <= width {
        return rows;
    }
    let indent = if wrap.breakindent {
        let blanks = (laid.pieces())
            .find(|&(_, _, ch)| ch != ' ' && ch != '\t')
            .map_or(laid.cells(), |(cell, _, _)| cell);
        blanks.min(width.saturating_sub(LEAST_TEXT))
    } else {
        0
    };
    // At least a cell for the text, however much is before it.
    let indent = (indent + wrap.showbreak).min(width - 1);
    let end = laid.cells();
    let after = room_after.then_some((end, end + 1, 'x'));
    // Where the row starts, the cells it has, and with `linebreak`, the
    // cell after the last character on it that a row can end after: not
    // in the blanks it starts with, which would leave it empty.
    let (mut start, mut room, mut break_at) = (0, width, None);
    let mut words = false;
    for (cell, next, ch) in laid.pieces().chain(after) {
        while next > start + room {
            let edge = start + room;
            let to = if ch == '\t' && cell < edge {
                edge
            } else if let Some(at) = break_at.filter(|&at| at > start && at <= cell) {
                at
            } else if cell > start {
                cell
            } else {
                // Wider than a whole row: it has one to itself.
                break;
            };
            rows.push(ScreenRow { start: to, indent });
            // What's carried onto the row, if anything, is a word.
            (start, room, break_at, words) = (to, width - indent, None, to < cell);
        }
        words |= ch != ' ' && ch != '\t';
        if wrap.linebreak && words && BREAK_AFTER.contains(&ch) {
            break_at = Some(next);
        }
    }
    rows
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
}

impl Shown {
    /// The cell after the last of the line on it: past the line's end on
    /// its last row.
    pub(crate) fn end(&self) -> usize {
        self.next.unwrap_or(usize::MAX)
    }

    /// Where cell `cell` of the line is drawn, counting from the gutter,
    /// if it's on this row and in the `width` cells right of the gutter.
    pub(crate) fn col_of(&self, cell: usize, width: usize) -> Option<usize> {
        let col = self.indent + cell.checked_sub(self.start)?;
        (cell < self.end() && col < width).then_some(col)
    }
}

/// The rows of `text` in a view `height` rows high whose top row is
/// `top`, as far as line `end`. When lines don't wrap, `left` is the
/// first cell shown.
pub(crate) fn view(
    text: &Text<'_>,
    top: RowAt,
    height: usize,
    end: u64,
    left: usize,
) -> Vec<Shown> {
    let mut shown = Vec::with_capacity(height);
    let mut line = top.line;
    while shown.len() < height && line <= end {
        let rows = text.screen_rows(line);
        let first = if line == top.line { top.row } else { 0 };
        for (row, screen) in rows.iter().enumerate().skip(first) {
            if shown.len() == height {
                break;
            }
            shown.push(Shown {
                line,
                row,
                start: if text.wraps() { screen.start } else { left },
                indent: screen.indent,
                next: rows.get(row + 1).map(|next| next.start),
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
        (rows(&laid, wrap, false).iter())
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
        let rows = rows(&laid, wrap(5), false);
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
    fn the_cell_after_the_end_fits_while_its_typed_at() {
        let laid = Laid::new("abcd".to_owned(), 4);
        assert_eq!(rows(&laid, wrap(4), false).len(), 1);
        let rows = rows(&laid, wrap(4), true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].start, 4);
    }
}
