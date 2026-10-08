//! A file's text as the Vim engine and the screen see it: each line laid
//! out in cells, as a terminal's rows are, with where in the text each
//! cell's character is.
//!
//! Lines end at `\n` only, as Vim's do, and the line break that ends the
//! last line isn't the start of another. A tab takes the cells to the next
//! multiple of [`TAB`], drawn blank; a control character is drawn as `^`
//! and a letter, as Vim draws it.

use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

use clackatoo_term::{Row, RowBuilder, Style};
use clackatoo_text::Rope;

use crate::motion::{Lines, Point};

/// Columns a tab stops at a multiple of.
pub(crate) const TAB: usize = 4;

/// One line, laid out in cells.
#[derive(Debug)]
pub(crate) struct Laid {
    /// The line's text, without its line break.
    text: String,
    row: Row,
    /// Each character's first cell, and its byte in the line, in order.
    /// Characters drawn in the cell before, such as accents, have none.
    starts: Vec<(usize, usize)>,
    /// The cells the line fills.
    cells: usize,
}

impl Laid {
    pub(crate) fn new(text: String) -> Self {
        let room = (text.chars())
            .map(|ch| if ch == '\t' { TAB } else { 2 })
            .sum();
        let mut builder = RowBuilder::new(room);
        let mut starts = Vec::new();
        for (byte, ch) in text.char_indices() {
            let before = builder.filled();
            push(&mut builder, ch, before);
            if builder.filled() > before {
                starts.push((before, byte));
            }
        }
        let cells = builder.filled();
        Self {
            text,
            row: builder.finish(false),
            starts,
            cells,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The cells the line fills.
    pub(crate) fn cells(&self) -> usize {
        self.cells
    }

    /// The byte, in the line, of the character at cell `col`, or the
    /// line's end past its last.
    pub(crate) fn byte_at(&self, col: usize) -> usize {
        if col >= self.cells {
            return self.text.len();
        }
        match self.starts.partition_point(|&(cell, _)| cell <= col) {
            0 => 0,
            after => self.starts[after - 1].1,
        }
    }

    /// The first cell of the character at `byte`, or past the line's last,
    /// the cell after it.
    pub(crate) fn col_of(&self, byte: usize) -> usize {
        if byte >= self.text.len() {
            return self.cells;
        }
        match self.starts.partition_point(|&(_, start)| start <= byte) {
            0 => 0,
            after => self.starts[after - 1].0,
        }
    }

    /// The byte after the character at `byte`, with any drawn in its cell.
    pub(crate) fn char_end(&self, byte: usize) -> usize {
        let after = self.starts.partition_point(|&(_, start)| start <= byte);
        self.starts
            .get(after)
            .map_or(self.text.len(), |&(_, end)| end)
    }

    /// The line as drawn in `width` cells from cell `left`.
    pub(crate) fn shown(&self, left: usize, width: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        for (index, &(cell, byte)) in self.starts.iter().enumerate() {
            let (next_cell, next_byte) =
                (self.starts.get(index + 1).copied()).unwrap_or((self.cells, self.text.len()));
            if next_cell <= left {
                continue;
            }
            if cell < left {
                // Half off the left edge: the rest of it, blank.
                builder.push(&" ".repeat(next_cell - left), &Style::DEFAULT, true);
                continue;
            }
            if cell >= left + width {
                break;
            }
            let piece = &self.text[byte..next_byte];
            match piece.chars().next() {
                Some('\t') => builder.push(&" ".repeat(next_cell - cell), &Style::DEFAULT, true),
                Some(ch) if ch.is_control() => push(&mut builder, ch, cell),
                _ => builder.push(piece, &Style::DEFAULT, false),
            }
        }
        builder.finish(false)
    }
}

impl Deref for Laid {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.row
    }
}

/// Lays `ch` out at cell `col`.
fn push(builder: &mut RowBuilder, ch: char, col: usize) {
    match ch {
        '\t' => {
            let next = (col / TAB + 1) * TAB;
            builder.push(&" ".repeat(next - col), &Style::DEFAULT, true);
        }
        '\0'..='\x1f' | '\x7f' => {
            let letter = char::from(u8::try_from(ch).unwrap_or(b'?') ^ 0x40);
            builder.push(&format!("^{letter}"), &Style::DEFAULT, true);
        }
        ch if ch.is_control() => builder.push("\u{fffd}", &Style::DEFAULT, true),
        ch => {
            let mut bytes = [0; 4];
            builder.push(ch.encode_utf8(&mut bytes), &Style::DEFAULT, false);
        }
    }
}

/// A line of a [`Text`], as the Vim engine asks for it.
pub(crate) struct LaidRow(Rc<Laid>);

impl Deref for LaidRow {
    type Target = Row;

    fn deref(&self) -> &Row {
        &self.0.row
    }
}

/// A buffer's text, laid out a line at a time as it's asked for. The last
/// few lines asked for are kept, since motions ask for one line many
/// times.
pub(crate) struct Text<'a> {
    rope: &'a Rope,
    kept: RefCell<Vec<(u64, Rc<Laid>)>>,
}

/// How many laid out lines a [`Text`] keeps.
const KEPT: usize = 4;

impl<'a> Text<'a> {
    pub(crate) fn new(rope: &'a Rope) -> Self {
        Self {
            rope,
            kept: RefCell::new(Vec::new()),
        }
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
        let mut kept = self.kept.borrow_mut();
        if let Some((_, laid)) = kept.iter().find(|(kept, _)| *kept == line) {
            return Some(Rc::clone(laid));
        }
        let laid = Rc::new(Laid::new(self.line_text(line)));
        if kept.len() == KEPT {
            kept.remove(0);
        }
        kept.push((line, Rc::clone(&laid)));
        Some(laid)
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
        self.line_start(line) + self.line_text(line).len()
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
    type Row<'r>
        = LaidRow
    where
        Self: 'r;

    fn row(&self, line: u64) -> Option<LaidRow> {
        self.laid(line).map(LaidRow)
    }

    fn bounds(&self) -> (u64, u64) {
        (0, self.last_line())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_end_at_line_breaks_and_the_last_break_ends_the_last_line() {
        for (text, lines) in [("", 1), ("a", 1), ("a\n", 1), ("a\nb", 2), ("a\n\n", 2)] {
            let rope = Rope::from_str(text);
            assert_eq!(Text::new(&rope).lines(), lines, "{text:?}");
        }
        let rope = Rope::from_str("one\ntwo\n");
        let text = Text::new(&rope);
        assert_eq!(text.line_text(1), "two");
        assert_eq!((text.line_start(1), text.line_end(1)), (4, 7));
    }

    #[test]
    fn tabs_wide_characters_and_controls_take_their_cells() {
        let laid = Laid::new("\tx字\x01é".to_owned());
        assert_eq!(laid.cells(), TAB + 1 + 2 + 2 + 1);
        // Every cell of the tab is the tab.
        assert_eq!((laid.byte_at(0), laid.byte_at(3)), (0, 0));
        assert_eq!(laid.byte_at(TAB), 1);
        assert_eq!((laid.byte_at(5), laid.byte_at(6)), (2, 2));
        assert_eq!(laid.byte_at(7), 5, "^A");
        assert_eq!(laid.col_of(6), 9, "é");
        assert_eq!(laid.col_of(99), laid.cells());
        assert_eq!(laid.char_end(2), 5);
        assert_eq!(laid.row.text().trim_start(), "x字^Aé");
    }

    #[test]
    fn a_line_is_drawn_from_any_column() {
        let laid = Laid::new("ab字cd".to_owned());
        assert_eq!(laid.shown(0, 4).text(), "ab字");
        assert_eq!(laid.shown(1, 3).text(), "b字");
        // Half of 字 off the left edge.
        assert_eq!(laid.shown(3, 3).text(), " cd");
    }

    #[test]
    fn places_go_between_points_and_bytes() {
        let rope = Rope::from_str("a\tb\n字x\n");
        let text = Text::new(&rope);
        let point = Point { line: 1, col: 2 };
        assert_eq!(text.byte_at(point), 7);
        assert_eq!(text.point_of(7), point);
        assert_eq!(text.char_end(Point { line: 1, col: 0 }), 7);
        assert_eq!(text.point_of(2), Point { line: 0, col: TAB });
        // After the last line break: the line after, to type on.
        assert_eq!(text.point_of(9), Point { line: 2, col: 0 });
        assert_eq!(text.byte_at(Point { line: 2, col: 0 }), 9);
    }
}
