//! One line of the grid.

use std::ops::Range;

use crate::cell::{Cell, CellFlags, StyleIndex};
use crate::style::Style;

/// The most codepoints a cell may hold beyond its first.
///
/// Real graphemes stay far below this (long emoji ZWJ sequences have about
/// ten). The cap keeps a stream of combining characters from growing one
/// cell without limit.
const MAX_GRAPHEME_EXTRA: usize = 32;

/// One line of cells, plus everything those cells refer to.
///
/// A row owns its own style palette and its graphemes (codepoints beyond a
/// cell's first). That keeps cells small, and it means anything a row
/// references moves with it when the screen scrolls. When the row is reused
/// or its palette compacted, the references go too, so nothing needs
/// collecting across the whole grid.
///
/// Rows keep a wide-character invariant: a [`CellFlags::WIDE`] cell is always
/// followed by a [`CellFlags::WIDE_SPACER`] cell, and a spacer always follows a
/// wide cell. Every mutation keeps it by blanking the other half of any wide
/// character it partly overwrites.
#[derive(Clone, Debug)]
pub struct Row {
    cells: Vec<Cell>,
    /// Non-default styles used by cells. [`StyleIndex`] `i` refers to
    /// `palette[i - 1]`, and index 0 is the default style.
    palette: Vec<Style>,
    /// Codepoints beyond the first, for cells flagged [`CellFlags::GRAPHEME`],
    /// keyed by column. Rare enough that a linear scan beats a map.
    graphemes: Vec<(usize, String)>,
    wrapped: bool,
}

impl Row {
    pub(crate) fn new(width: usize) -> Self {
        Self {
            cells: vec![Cell::BLANK; width],
            palette: Vec::new(),
            graphemes: Vec::new(),
            wrapped: false,
        }
    }

    /// The number of cells in the row.
    #[must_use]
    pub fn width(&self) -> usize {
        self.cells.len()
    }

    /// Whether this row's text continues on the next row because it reached
    /// the right edge, as opposed to ending with a newline.
    #[must_use]
    pub fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    /// The cells, left to right.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// The style of one of this row's cells.
    ///
    /// # Panics
    ///
    /// If `cell` came from a different row.
    #[must_use]
    pub fn style(&self, cell: Cell) -> &Style {
        match cell.style().0 {
            0 => &Style::DEFAULT,
            i => &self.palette[usize::from(i) - 1],
        }
    }

    /// Appends the text of the cell at `col` to `out`.
    ///
    /// That's the cell's character plus any combining codepoints. A
    /// [`CellFlags::WIDE_SPACER`] cell adds nothing, because its text belongs
    /// to the cell on its left.
    pub fn push_text(&self, col: usize, out: &mut String) {
        let cell = self.cells[col];
        if cell.is_wide_spacer() {
            return;
        }
        out.push(cell.ch());
        if cell.has_grapheme()
            && let Some(extra) = self.grapheme(col)
        {
            out.push_str(extra);
        }
    }

    /// The row's text, without trailing spaces.
    #[must_use]
    pub fn text(&self) -> String {
        let mut text = String::with_capacity(self.width());
        for col in 0..self.width() {
            self.push_text(col, &mut text);
        }
        text.truncate(text.trim_end_matches(' ').len());
        text
    }

    pub(crate) fn set_wrapped(&mut self, wrapped: bool) {
        self.wrapped = wrapped;
    }

    /// Writes `ch` at `col`, taking two cells if it is `wide`.
    pub(crate) fn write(&mut self, col: usize, ch: char, wide: bool, style: &Style) {
        let style = self.intern(style);
        if wide {
            debug_assert!(col + 1 < self.width(), "a wide character needs two cells");
            self.unlink_wide(col);
            self.unlink_wide(col + 1);
            self.set_cell(col, Cell::new(ch, style, CellFlags::WIDE));
            self.set_cell(col + 1, Cell::new(' ', style, CellFlags::WIDE_SPACER));
        } else {
            self.unlink_wide(col);
            self.set_cell(col, Cell::new(ch, style, CellFlags::empty()));
        }
    }

    /// Appends a zero-width codepoint (a combining accent, a variation selector,
    /// a joiner…) to the cell at `col`.
    pub(crate) fn append_to_grapheme(&mut self, col: usize, ch: char) {
        debug_assert!(!self.cells[col].is_wide_spacer(), "spacers hold no text");
        if let Some((_, extra)) = self.graphemes.iter_mut().find(|(c, _)| *c == col) {
            if extra.chars().count() < MAX_GRAPHEME_EXTRA {
                extra.push(ch);
            }
        } else {
            self.graphemes.push((col, ch.to_string()));
            self.cells[col].add_flags(CellFlags::GRAPHEME);
        }
    }

    /// The last codepoint of the cell at `col`.
    pub(crate) fn last_char(&self, col: usize) -> char {
        self.grapheme(col)
            .and_then(|extra| extra.chars().next_back())
            .unwrap_or_else(|| self.cells[col].ch())
    }

    /// Blanks `cols` in `style`, clamped to the row.
    pub(crate) fn fill(&mut self, cols: Range<usize>, style: &Style) {
        let cols = cols.start..cols.end.min(self.width());
        if cols.is_empty() {
            return;
        }
        let blank = Cell::blank(self.intern(style));
        self.unlink_wide(cols.start);
        self.unlink_wide(cols.end - 1);
        self.graphemes.retain(|(col, _)| !cols.contains(col));
        self.cells[cols].fill(blank);
    }

    /// Blanks the whole row in `style` and forgets everything it referenced.
    pub(crate) fn reset(&mut self, style: &Style) {
        self.palette.clear();
        self.graphemes.clear();
        self.wrapped = false;
        let blank = Cell::blank(self.intern(style));
        self.cells.fill(blank);
    }

    /// Inserts `count` blank cells at `col`, shifting the rest of the row right.
    /// Cells pushed past the right edge are lost.
    pub(crate) fn insert_blanks(&mut self, col: usize, count: usize, style: &Style) {
        let width = self.width();
        if col >= width {
            return;
        }
        let count = count.min(width - col);
        let blank = Cell::blank(self.intern(style));

        // Inserting between the halves of a wide character splits it.
        if self.cells[col].is_wide_spacer() {
            self.blank_in_place(col - 1);
            self.blank_in_place(col);
        }
        // A wide character pushed against the right edge would lose its spacer.
        if let Some(last_kept) = (width - count).checked_sub(1)
            && last_kept >= col
            && self.cells[last_kept].is_wide()
        {
            self.blank_in_place(last_kept);
        }

        self.graphemes.retain_mut(|(c, _)| {
            if *c >= col {
                *c += count;
            }
            *c < width
        });
        self.cells.copy_within(col..width - count, col + count);
        self.cells[col..col + count].fill(blank);
    }

    /// Deletes `count` cells at `col`, shifting the rest of the row left and
    /// filling the right edge with blanks.
    pub(crate) fn delete_cells(&mut self, col: usize, count: usize, style: &Style) {
        let width = self.width();
        if col >= width {
            return;
        }
        let count = count.min(width - col);
        let blank = Cell::blank(self.intern(style));

        // Deleting a spacer leaves its wide character half-drawn.
        if self.cells[col].is_wide_spacer() {
            self.blank_in_place(col - 1);
        }
        // Deleting a wide character's first half would shift its orphaned
        // spacer into place.
        let after = col + count;
        if after < width && self.cells[after].is_wide_spacer() {
            self.blank_in_place(after);
        }

        self.graphemes.retain_mut(|(c, _)| {
            if *c < col {
                true
            } else if *c < after {
                false
            } else {
                *c -= count;
                true
            }
        });
        self.cells.copy_within(after..width, col);
        self.cells[width - count..].fill(blank);
    }

    /// Returns `style`'s palette index, adding it to the palette if needed.
    ///
    /// Compacts the palette first if it is full. Indices obtained before this
    /// call may be invalid afterwards, so callers intern *before* touching
    /// cells.
    fn intern(&mut self, style: &Style) -> StyleIndex {
        if *style == Style::DEFAULT {
            return StyleIndex::DEFAULT;
        }
        if let Some(i) = self.palette.iter().position(|s| s == style) {
            return palette_index(i);
        }
        if self.palette.len() >= self.palette_limit() {
            self.compact_palette();
        }
        self.palette.push(*style);
        palette_index(self.palette.len() - 1)
    }

    /// The palette size that triggers compaction.
    ///
    /// A row can show at most `width` styles at once, so compacting at twice
    /// that frees at least half the palette. Compaction therefore costs O(1)
    /// per new style on average, however often an app recolours a line.
    fn palette_limit(&self) -> usize {
        (2 * self.width()).max(16)
    }

    /// Drops palette entries no cell uses and renumbers the rest.
    fn compact_palette(&mut self) {
        let mut used = vec![false; self.palette.len()];
        for cell in &self.cells {
            if let Some(i) = usize::from(cell.style().0).checked_sub(1) {
                used[i] = true;
            }
        }

        let mut remap = vec![StyleIndex::DEFAULT; self.palette.len()];
        let mut kept = 0;
        for (i, &is_used) in used.iter().enumerate() {
            if is_used {
                self.palette[kept] = self.palette[i];
                remap[i] = palette_index(kept);
                kept += 1;
            }
        }
        self.palette.truncate(kept);

        for cell in &mut self.cells {
            if let Some(i) = usize::from(cell.style().0).checked_sub(1) {
                *cell = cell.with_style(remap[i]);
            }
        }
    }

    /// Before `col` is overwritten, blanks the other half of the wide
    /// character it belongs to, if any.
    fn unlink_wide(&mut self, col: usize) {
        let cell = self.cells[col];
        if cell.is_wide_spacer() {
            self.blank_in_place(col - 1);
        } else if cell.is_wide() {
            self.blank_in_place(col + 1);
        }
    }

    /// Blanks one cell, keeping its style.
    fn blank_in_place(&mut self, col: usize) {
        let style = self.cells[col].style();
        self.set_cell(col, Cell::blank(style));
    }

    fn set_cell(&mut self, col: usize, cell: Cell) {
        if self.cells[col].has_grapheme() {
            self.graphemes.retain(|(c, _)| *c != col);
        }
        self.cells[col] = cell;
    }

    fn grapheme(&self, col: usize) -> Option<&str> {
        self.graphemes
            .iter()
            .find(|(c, _)| *c == col)
            .map(|(_, extra)| extra.as_str())
    }

    /// Panics if any of the row's invariants are broken.
    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        for (col, cell) in self.cells.iter().enumerate() {
            if cell.is_wide() {
                assert!(
                    self.cells.get(col + 1).is_some_and(|c| c.is_wide_spacer()),
                    "wide cell at {col} is not followed by a spacer"
                );
            }
            if cell.is_wide_spacer() {
                assert!(
                    col > 0 && self.cells[col - 1].is_wide(),
                    "spacer at {col} does not follow a wide cell"
                );
            }
            assert_eq!(
                cell.has_grapheme(),
                self.grapheme(col).is_some(),
                "grapheme flag and table disagree at {col}"
            );
            assert!(
                usize::from(cell.style().0) <= self.palette.len(),
                "cell at {col} has a dangling style index"
            );
        }
        for (col, _) in &self.graphemes {
            assert!(*col < self.width(), "grapheme entry past the edge");
        }
    }
}

fn palette_index(position: usize) -> StyleIndex {
    StyleIndex(u16::try_from(position + 1).expect("palette size is bounded by twice the row width"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    fn red() -> Style {
        Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        }
    }

    fn row_with(text: &str, width: usize) -> Row {
        let mut row = Row::new(width);
        let mut col = 0;
        for ch in text.chars() {
            let wide = ch == '字';
            row.write(col, ch, wide, &Style::DEFAULT);
            col += if wide { 2 } else { 1 };
        }
        row
    }

    #[test]
    fn text_skips_spacers_and_trailing_blanks() {
        let row = row_with("a字b", 8);
        assert_eq!(row.text(), "a字b");
        row.assert_invariants();
    }

    #[test]
    fn overwriting_either_half_of_a_wide_char_blanks_the_other() {
        let mut row = row_with("字", 4);
        row.write(1, 'x', false, &Style::DEFAULT);
        assert_eq!(row.text(), " x");
        row.assert_invariants();

        let mut row = row_with("字", 4);
        row.write(0, 'x', false, &Style::DEFAULT);
        assert_eq!(row.text(), "x");
        row.assert_invariants();
    }

    #[test]
    fn wide_char_written_over_the_middle_of_two_others() {
        let mut row = row_with("字字", 6);
        row.write(1, '字', true, &Style::DEFAULT);
        assert_eq!(row.text(), " 字");
        row.assert_invariants();
    }

    #[test]
    fn graphemes_are_kept_and_cleared_with_their_cell() {
        let mut row = row_with("e", 4);
        row.append_to_grapheme(0, '\u{301}');
        assert_eq!(row.text(), "e\u{301}");
        assert_eq!(row.last_char(0), '\u{301}');
        row.write(0, 'x', false, &Style::DEFAULT);
        assert_eq!(row.text(), "x");
        row.assert_invariants();
    }

    #[test]
    fn grapheme_growth_is_capped() {
        let mut row = Row::new(2);
        for _ in 0..1000 {
            row.append_to_grapheme(0, '\u{301}');
        }
        assert_eq!(row.text().chars().count(), 1 + MAX_GRAPHEME_EXTRA);
    }

    #[test]
    fn insert_shifts_cells_and_graphemes() {
        let mut row = row_with("abc", 5);
        row.append_to_grapheme(1, '\u{301}');
        row.insert_blanks(1, 2, &Style::DEFAULT);
        assert_eq!(row.text(), "a  b\u{301}c");
        row.assert_invariants();

        row.insert_blanks(0, 4, &Style::DEFAULT);
        assert_eq!(row.text(), "    a");
        row.assert_invariants();
    }

    #[test]
    fn insert_drops_a_wide_char_pushed_half_off_the_edge() {
        let mut row = row_with("ab字", 4);
        row.insert_blanks(0, 1, &Style::DEFAULT);
        assert_eq!(row.text(), " ab");
        row.assert_invariants();
    }

    #[test]
    fn insert_inside_a_wide_char_splits_it() {
        let mut row = row_with("字", 4);
        row.insert_blanks(1, 1, &Style::DEFAULT);
        assert_eq!(row.text(), "");
        row.assert_invariants();
    }

    #[test]
    fn delete_shifts_cells_and_graphemes() {
        let mut row = row_with("abcd", 4);
        row.append_to_grapheme(3, '\u{301}');
        row.delete_cells(1, 2, &Style::DEFAULT);
        assert_eq!(row.text(), "ad\u{301}");
        row.assert_invariants();
    }

    #[test]
    fn delete_never_leaves_half_a_wide_char() {
        let mut row = row_with("a字b", 5);
        row.delete_cells(1, 1, &Style::DEFAULT);
        assert_eq!(row.text(), "a b");
        row.assert_invariants();

        let mut row = row_with("a字b", 5);
        row.delete_cells(2, 1, &Style::DEFAULT);
        assert_eq!(row.text(), "a b");
        row.assert_invariants();
    }

    #[test]
    fn fill_blanks_the_outer_halves_of_wide_chars_it_cuts() {
        let mut row = row_with("字字字", 6);
        row.fill(1..5, &red());
        assert_eq!(row.text(), "");
        assert_eq!(*row.style(row.cells()[2]), red());
        row.assert_invariants();
    }

    #[test]
    fn palette_compacts_instead_of_growing_forever() {
        let mut row = Row::new(4);
        for i in 0..=255 {
            let style = Style {
                fg: Color::Indexed(i),
                ..Style::DEFAULT
            };
            row.write(usize::from(i % 4), 'x', false, &style);
            row.assert_invariants();
        }
        assert!(row.palette.len() <= row.palette_limit());
        assert_eq!(
            *row.style(row.cells()[3]),
            Style {
                fg: Color::Indexed(255),
                ..Style::DEFAULT
            }
        );
    }

    #[test]
    fn reset_forgets_everything() {
        let mut row = row_with("a字", 4);
        row.append_to_grapheme(0, '\u{301}');
        row.set_wrapped(true);
        row.reset(&red());
        assert_eq!(row.text(), "");
        assert!(!row.is_wrapped());
        assert_eq!(row.palette, vec![red()]);
        row.assert_invariants();
    }
}
