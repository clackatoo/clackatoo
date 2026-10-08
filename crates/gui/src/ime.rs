//! Typing through an input method (IME), as Chinese, Japanese and Korean
//! are typed: keys build up text the input method shows choices for, and
//! the text chosen is committed, then typed into the pane as if keyed.
//!
//! Until then the text being composed, the preedit, is drawn underlined
//! over the cells at the cursor, without touching the terminal.

use clackatoo_term::{Row, RowBuilder, Style, Underline};
use unicode_width::UnicodeWidthChar;

/// Text the input method is composing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preedit {
    pub(crate) text: String,
    /// Where in `text` the input method's cursor is, in bytes, if it shows
    /// one.
    pub(crate) cursor: Option<usize>,
}

impl Preedit {
    /// `row` with this text over its cells from `col`, underlined, and the
    /// column the cursor goes in. Whatever the text covers is hidden, and a
    /// wide character it covers half of is blanked.
    pub(crate) fn over(&self, row: &Row, col: usize) -> (Row, usize) {
        let width = row.width();
        let mut builder = RowBuilder::new(width);
        let mut text = String::new();
        let mut filled = 0;
        let mut push_cells = |builder: &mut RowBuilder, from: usize, to: usize| {
            let mut col = from;
            while col < to.min(width) {
                let cell = row.cells()[col];
                let cells = if cell.is_wide() { 2 } else { 1 };
                text.clear();
                if cell.is_wide_spacer() || col + cells > to {
                    text.push(' ');
                    builder.push(&text, &Style::DEFAULT, true);
                    col += 1;
                    continue;
                }
                row.push_text(col, &mut text);
                builder.push(&text, row.style(cell), true);
                col += cells;
            }
        };
        let start = col.min(width);
        push_cells(&mut builder, 0, start);
        let underlined = Style {
            underline: Underline::Single,
            ..Style::DEFAULT
        };
        let mut cursor = None;
        for (index, ch) in self.text.char_indices() {
            if self.cursor == Some(index) {
                cursor = Some(start + filled);
            }
            let cells = ch.width().unwrap_or(0);
            if start + filled + cells > width {
                break;
            }
            let mut buffer = [0; 4];
            builder.push(ch.encode_utf8(&mut buffer), &underlined, false);
            filled += cells;
        }
        let end = start + filled;
        // The cell the text ends in may have been half of a wide one.
        let resume = if end < width && row.cells()[end].is_wide_spacer() {
            builder.push(" ", &Style::DEFAULT, true);
            end + 1
        } else {
            end
        };
        push_cells(&mut builder, resume, width);
        let cursor = cursor.unwrap_or(end).min(width.saturating_sub(1));
        (builder.finish(row.is_wrapped()), cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        let mut builder = RowBuilder::new(10);
        builder.push(text, &Style::DEFAULT, false);
        builder.finish(false)
    }

    fn preedit(text: &str, cursor: Option<usize>) -> Preedit {
        Preedit {
            text: text.to_owned(),
            cursor,
        }
    }

    #[test]
    fn the_text_covers_the_cells_at_the_cursor() {
        let (over, cursor) = preedit("ni", None).over(&row("$ abcdef"), 2);
        assert_eq!(over.text().trim_end(), "$ nicdef");
        assert_eq!(cursor, 4, "after the text, without a cursor of its own");
        let underlined = |col| over.style(over.cells()[col]).underline;
        assert_eq!(underlined(1), Underline::None);
        assert_eq!(underlined(2), Underline::Single);
        assert_eq!(underlined(4), Underline::None);
    }

    #[test]
    fn wide_text_takes_two_cells_and_half_covered_ones_are_blanked() {
        // 你 is cols 2-3 and 好 is 4-5; two cells from col 3 cover half of
        // each.
        let (over, cursor) = preedit("ab", Some(1)).over(&row("$ 你好x"), 3);
        assert_eq!(over.text().trim_end(), "$  ab x");
        assert_eq!(cursor, 4);
        let (over, _) = preedit("日本", None).over(&row("$ abcdef"), 2);
        assert_eq!(over.text().trim_end(), "$ 日本ef");
    }

    #[test]
    fn text_past_the_edge_is_cut_off() {
        let (over, cursor) = preedit("abcdef", None).over(&row("$"), 7);
        assert_eq!(over.text().trim_end(), "$      abc");
        assert_eq!(cursor, 9);
    }
}
