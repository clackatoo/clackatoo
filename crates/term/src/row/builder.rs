//! Rebuilding a row from its text.

use super::Row;
use crate::style::Style;
use crate::terminal::ZWJ;
use crate::width;

/// Builds a [`Row`] from the text it shows, filling cells by the rules the
/// emulator prints by: a wide character takes two cells, and zero-width
/// characters (combining accents, emoji joined with a ZWJ…) join the cell
/// before them.
///
/// Scrollback and clients both rebuild rows this way, so a row rebuilt from
/// its text always matches the original. The builder never panics on bad
/// input: text that doesn't fit is dropped.
#[derive(Debug)]
pub struct RowBuilder {
    row: Row,
    col: usize,
    /// Whether the next character joins the previous cell, because that
    /// one ends in a ZWJ.
    joins: bool,
}

impl RowBuilder {
    /// A builder for a blank row of `width` cells.
    #[must_use]
    pub fn new(width: usize) -> Self {
        Self {
            row: Row::new(width),
            col: 0,
            joins: false,
        }
    }

    /// Appends `text` in `style`. If `separate`, its first character starts
    /// a new cell even if the previous cell ends in a ZWJ, which happens
    /// when cursor movement put two unrelated characters side by side.
    pub fn push(&mut self, text: &str, style: &Style, separate: bool) {
        self.joins &= !separate;
        for ch in text.chars() {
            match width::of(ch) {
                None => {}
                Some(cells) if cells == 0 || self.joins => {
                    let Some(previous) = self.previous_cell() else {
                        continue;
                    };
                    self.row.append_to_grapheme(previous, ch);
                    self.joins = self.row.last_char(previous) == ZWJ;
                }
                Some(cells) => {
                    if self.col + cells > self.row.width() {
                        continue;
                    }
                    self.row.write(self.col, ch, cells == 2, style);
                    self.col += cells;
                    self.joins = false;
                }
            }
        }
    }

    /// How many columns the text so far fills.
    #[must_use]
    pub fn filled(&self) -> usize {
        self.col
    }

    /// The finished row, marked as continuing on the next one if `wrapped`.
    /// A row that continues before its last column wrapped early, so what
    /// is left of it is padding.
    #[must_use]
    pub fn finish(mut self, wrapped: bool) -> Row {
        if wrapped && self.col < self.row.width() {
            self.row.set_wrapped_early();
        } else {
            self.row.set_wrapped(wrapped);
        }
        self.row
    }

    /// The cell a zero-width character joins: the last one written, or the
    /// wide character a spacer belongs to.
    fn previous_cell(&self) -> Option<usize> {
        let last = self.col.checked_sub(1)?;
        if self.row.cells()[last].is_wide_spacer() {
            last.checked_sub(1)
        } else {
            Some(last)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_characters_and_graphemes_fill_cells_as_printed() {
        let mut builder = RowBuilder::new(6);
        builder.push("字e\u{301}👩\u{200D}💻", &Style::DEFAULT, false);
        let row = builder.finish(true);
        assert_eq!(row.text(), "字e\u{301}👩\u{200D}💻");
        assert!(row.cells()[0].is_wide());
        assert!(row.cells()[3].is_wide());
        assert!(row.is_wrapped());
    }

    #[test]
    fn separate_text_does_not_join_a_trailing_zwj() {
        let mut builder = RowBuilder::new(4);
        builder.push("a\u{200D}", &Style::DEFAULT, false);
        builder.push("b", &Style::DEFAULT, true);
        let row = builder.finish(false);
        assert_eq!(row.cells()[1].ch(), 'b');
    }

    #[test]
    fn text_that_does_not_fit_is_dropped() {
        let mut builder = RowBuilder::new(3);
        builder.push("\u{301}ab字c", &Style::DEFAULT, false);
        assert_eq!(builder.finish(false).text(), "abc");
    }
}
