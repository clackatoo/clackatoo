//! One line of the grid.

mod builder;
mod graphemes;
mod lookup;

use std::ops::Range;

use crate::blocks::Mark;
use crate::cell::{Cell, CellFlags, StyleIndex};
use crate::links::LinkId;
use crate::size::Size;
use crate::style::Style;
pub use builder::RowBuilder;
use graphemes::Graphemes;
pub use graphemes::MAX_EXTRA;
use lookup::{LINEAR_SEARCH_MAX, StyleLookup};

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
///
/// A new row allocates no cells until something is written to it: most of a
/// fresh screen is never written, and until then it reads as blank cells
/// shared by every row.
#[derive(Clone, Debug)]
pub struct Row {
    /// Empty until the row is first written, then `width` cells.
    cells: Vec<Cell>,
    /// Non-default styles used by cells. [`StyleIndex`] `i` refers to
    /// `palette[i - 1]`, and index 0 is the default style.
    palette: Vec<Style>,
    /// What most rows never need, kept out of line so they don't pay for it.
    extras: Option<Box<Extras>>,
    /// The style [`intern`](Self::intern) returned last, checked before
    /// anything else: runs of text in one style intern it over and over.
    last_style: StyleIndex,
    /// Every cell from this column on is [`Cell::BLANK`]. An upper bound,
    /// raised by writes, so finding where the row's content ends needn't
    /// look at every blank after it.
    written: u16,
    width: u16,
    wrapped: bool,
    /// The row wrapped early, because a wide character didn't fit in its
    /// last column: that column is padding, not text.
    padded: bool,
}

// Scrollback is mostly rows, so their size matters. The last-style hint, the
// width and the two flags fit in what would otherwise be padding.
const _: () = assert!(size_of::<Row>() <= 64);

/// What a row reads as before it is first written. One copy serves every
/// row as wide as a terminal can be; a row wider than that (only ever
/// rebuilt from history) allocates its cells up front.
static BLANK_CELLS: [Cell; Size::MAX as usize] = [Cell::BLANK; Size::MAX as usize];

/// The rarely needed parts of a row.
#[derive(Clone, Debug, Default)]
struct Extras {
    /// Codepoints beyond the first, for cells flagged
    /// [`CellFlags::GRAPHEME`].
    graphemes: Graphemes,
    /// An index into the palette, built while it is too big to search one by
    /// one, and dropped when the row scrolls into scrollback.
    lookup: StyleLookup,
    /// Where a shell's block starts (see [`crate::blocks`]).
    mark: Option<Mark>,
}

impl Row {
    pub(crate) fn new(width: usize) -> Self {
        let cells = if width > BLANK_CELLS.len() {
            vec![Cell::BLANK; width]
        } else {
            Vec::new()
        };
        Self {
            cells,
            palette: Vec::new(),
            extras: None,
            last_style: StyleIndex::DEFAULT,
            written: 0,
            width: u16::try_from(width).expect("columns fit in u16"),
            wrapped: false,
            padded: false,
        }
    }

    /// The number of cells in the row.
    #[must_use]
    pub fn width(&self) -> usize {
        usize::from(self.width)
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
        if self.is_allocated() {
            &self.cells
        } else {
            &BLANK_CELLS[..self.width()]
        }
    }

    /// Whether the row has cells of its own, rather than being blank and
    /// never written.
    fn is_allocated(&self) -> bool {
        self.cells.len() == self.width()
    }

    /// Gives the row cells of its own before they're changed. Every method
    /// that changes cells calls this first, once per call.
    fn allocate(&mut self) {
        #[cold]
        #[inline(never)]
        fn allocate(row: &mut Row) {
            row.cells = vec![Cell::BLANK; row.width()];
        }
        if !self.is_allocated() {
            allocate(self);
        }
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

    /// The links the row's palette refers to: every link its cells are
    /// part of, and perhaps some an overwritten cell was, until the palette
    /// is next compacted. Cheap, as it looks at styles, not cells.
    pub fn links(&self) -> impl Iterator<Item = LinkId> + '_ {
        self.palette.iter().filter_map(|style| style.link)
    }

    /// The row's palette: the styles other than the default its cells
    /// have, or had until it's next compacted.
    pub(crate) fn styles(&self) -> &[Style] {
        &self.palette
    }

    /// Appends the text of the cell at `col` to `out`.
    ///
    /// That's the cell's character plus any combining codepoints. A
    /// [`CellFlags::WIDE_SPACER`] cell adds nothing, because its text belongs
    /// to the cell on its left.
    pub fn push_text(&self, col: usize, out: &mut String) {
        let cell = self.cells()[col];
        if cell.is_wide_spacer() {
            return;
        }
        out.push(cell.ch());
        if cell.has_grapheme()
            && let Some(extra) = self.grapheme(col)
        {
            out.extend(extra);
        }
    }

    /// Where the row's text ends: one past its last cell that isn't a
    /// blank. What's copied of it stops there, and so does what's lit of
    /// a selection going on past it.
    #[must_use]
    pub fn text_end(&self) -> usize {
        self.cells()
            .iter()
            .rposition(|cell| cell.ch() != ' ' || cell.is_wide_spacer())
            .map_or(0, |last| last + 1)
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
        self.padded = false;
    }

    /// Marks the row as wrapped early: a wide character didn't fit in its
    /// last column, which is left as padding.
    pub(crate) fn set_wrapped_early(&mut self) {
        self.wrapped = true;
        self.padded = true;
    }

    /// Whether the row wrapped early, leaving padding in its last column.
    /// Writing there since makes the column text again.
    pub(crate) fn is_padded(&self) -> bool {
        self.padded && self.cells().last() == Some(&Cell::BLANK)
    }

    /// Writes `ch` at `col`, taking two cells if it is `wide`.
    pub(crate) fn write(&mut self, col: usize, ch: char, wide: bool, style: &Style) {
        let style = self.intern(style);
        self.write_cell(col, ch, wide, style);
    }

    /// Like [`write`](Self::write), with a style already interned in this
    /// row by [`intern`](Self::intern).
    pub(crate) fn write_cell(&mut self, col: usize, ch: char, wide: bool, style: StyleIndex) {
        self.allocate();
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

    /// Writes printable ASCII from `col` onwards, one character per cell, in
    /// a style interned in this row by [`intern`](Self::intern). The text
    /// must fit in the row.
    pub(crate) fn write_ascii(&mut self, col: usize, text: &[u8], style: StyleIndex) {
        if text.is_empty() {
            return;
        }
        self.allocate();
        let cols = col..col + text.len();
        // Wide characters entirely inside the range are overwritten whole;
        // only those straddling its edges need their other half blanked.
        self.unlink_wide(cols.start);
        self.unlink_wide(cols.end - 1);
        self.remove_graphemes(&cols);
        self.touch(cols.end);
        for (cell, &byte) in self.cells[cols].iter_mut().zip(text) {
            *cell = Cell::new(char::from(byte), style, CellFlags::empty());
        }
    }

    /// How far the row has been written: every cell from here on is
    /// [`Cell::BLANK`].
    pub(crate) fn written(&self) -> usize {
        usize::from(self.written)
    }

    /// Notes that cells before `end` may no longer be blank.
    fn touch(&mut self, end: usize) {
        let end = u16::try_from(end).expect("columns fit in u16");
        self.written = self.written.max(end);
    }

    /// Appends a zero-width codepoint (a combining accent, a variation selector,
    /// a joiner…) to the cell at `col`.
    pub(crate) fn append_to_grapheme(&mut self, col: usize, ch: char) {
        self.allocate();
        debug_assert!(!self.cells[col].is_wide_spacer(), "spacers hold no text");
        if self.extras_mut().graphemes.append(col, ch) {
            self.cells[col].add_flags(CellFlags::GRAPHEME);
            self.touch(col + 1);
        }
    }

    /// The last codepoint of the cell at `col`.
    pub(crate) fn last_char(&self, col: usize) -> char {
        self.grapheme(col)
            .and_then(|extra| extra.last().copied())
            .unwrap_or_else(|| self.cells()[col].ch())
    }

    /// Blanks `cols` in `style`, clamped to the row.
    pub(crate) fn fill(&mut self, cols: Range<usize>, style: &Style) {
        let cols = cols.start..cols.end.min(self.width());
        if cols.is_empty() {
            return;
        }
        let blank = Cell::blank(self.intern(style));
        if blank == Cell::BLANK && !self.is_allocated() {
            return;
        }
        self.allocate();
        self.unlink_wide(cols.start);
        self.unlink_wide(cols.end - 1);
        self.remove_graphemes(&cols);
        if blank != Cell::BLANK {
            self.touch(cols.end);
        }
        self.cells[cols].fill(blank);
    }

    /// Blanks the whole row in `style` and forgets everything it referenced.
    pub(crate) fn reset(&mut self, style: &Style) {
        self.palette.clear();
        self.last_style = StyleIndex::DEFAULT;
        if let Some(extras) = &mut self.extras {
            // Keep the allocations: a recycled row is about to be written.
            extras.graphemes.clear();
            extras.lookup.invalidate();
            extras.mark = None;
        }
        self.wrapped = false;
        self.padded = false;
        let blank = Cell::blank(self.intern(style));
        if blank == Cell::BLANK {
            // Cells past the written mark are blank already, and so is a row
            // that has never been written. One that has keeps its cells: it
            // is about to be written again.
            let written = self.written();
            self.cells[..written].fill(blank);
            self.written = 0;
        } else {
            self.allocate();
            self.cells.fill(blank);
            self.written = self.width;
        }
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
        if blank == Cell::BLANK && !self.is_allocated() {
            return;
        }
        self.allocate();

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

        if let Some(extras) = &mut self.extras {
            extras.graphemes.shift_right(col, count, width);
        }
        self.touch((self.written() + count).min(width));
        if blank != Cell::BLANK {
            self.touch(col + count);
        }
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
        if blank == Cell::BLANK && !self.is_allocated() {
            return;
        }
        self.allocate();

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

        if let Some(extras) = &mut self.extras {
            extras.graphemes.shift_left(col, count);
        }
        if blank != Cell::BLANK {
            self.touch(width);
        }
        self.cells.copy_within(after..width, col);
        self.cells[width - count..].fill(blank);
    }

    /// Returns `style`'s palette index, adding it to the palette if needed.
    ///
    /// Compacts the palette first if it is full. Indices obtained before this
    /// call may be invalid afterwards, so callers intern *before* touching
    /// cells. Nothing else changes an index, so one stays valid until this
    /// row's next `intern`.
    pub(crate) fn intern(&mut self, style: &Style) -> StyleIndex {
        if *style == Style::DEFAULT {
            return StyleIndex::DEFAULT;
        }
        let last = self.last_style;
        if last != StyleIndex::DEFAULT && self.palette[usize::from(last.0) - 1] == *style {
            return last;
        }
        if let Some(index) = self.find_style(style) {
            self.last_style = index;
            return index;
        }
        if self.palette.len() >= self.palette_limit() {
            self.compact_palette();
        }
        self.palette.push(*style);
        let index = palette_index(self.palette.len() - 1);
        let limit = self.palette_limit();
        match &mut self.extras {
            Some(extras) if extras.lookup.is_built() => extras.lookup.insert(style, index),
            extras if self.palette.len() > LINEAR_SEARCH_MAX => extras
                .get_or_insert_default()
                .lookup
                .build(&self.palette, limit),
            _ => {}
        }
        self.last_style = index;
        index
    }

    fn find_style(&self, style: &Style) -> Option<StyleIndex> {
        match &self.extras {
            Some(extras) if extras.lookup.is_built() => extras.lookup.find(&self.palette, style),
            _ => self
                .palette
                .iter()
                .position(|s| s == style)
                .map(palette_index),
        }
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

        // Every index moved, so the lookup starts again once it's needed.
        self.last_style = StyleIndex::DEFAULT;
        if let Some(extras) = &mut self.extras {
            extras.lookup.invalidate();
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
        if cell != Cell::BLANK {
            self.touch(col + 1);
        }
        if self.cells[col].has_grapheme()
            && let Some(extras) = &mut self.extras
        {
            extras.graphemes.remove(col);
        }
        self.cells[col] = cell;
    }

    /// The codepoints the cell at `col` holds beyond its first, if any.
    pub(crate) fn grapheme(&self, col: usize) -> Option<&[char]> {
        self.extras.as_ref()?.graphemes.get(col)
    }

    fn remove_graphemes(&mut self, cols: &Range<usize>) {
        if let Some(extras) = &mut self.extras
            && !extras.graphemes.is_empty()
        {
            extras.graphemes.remove_range(cols);
        }
    }

    /// The number of the block whose prompt starts on this row, if one
    /// does (see [`crate::blocks`]): where a window marks how the block's
    /// command ended. Rows in history keep no marks: see
    /// [`Grid::history_prompts`](crate::Grid::history_prompts).
    #[must_use]
    pub fn prompt_block(&self) -> Option<u32> {
        self.mark()
            .filter(|mark| mark.prompt)
            .map(|mark| mark.block)
    }

    /// The shell's mark on this row, if any.
    pub(crate) fn mark(&self) -> Option<Mark> {
        self.extras.as_ref()?.mark
    }

    pub(crate) fn set_mark(&mut self, mark: Option<Mark>) {
        if mark.is_some() || self.extras.is_some() {
            self.extras_mut().mark = mark;
        }
    }

    pub(crate) fn take_mark(&mut self) -> Option<Mark> {
        self.extras.as_mut()?.mark.take()
    }

    fn extras_mut(&mut self) -> &mut Extras {
        self.extras.get_or_insert_default()
    }

    /// Roughly the heap memory the row holds: its cells, palette and
    /// extras, but not what the extras hold in turn.
    #[cfg(test)]
    pub(crate) fn heap_bytes(&self) -> usize {
        self.cells.capacity() * size_of::<Cell>()
            + self.palette.capacity() * size_of::<Style>()
            + self.extras.as_ref().map_or(0, |_| size_of::<Extras>())
    }

    /// Panics if any of the row's invariants are broken.
    #[cfg(test)]
    pub(crate) fn assert_invariants(&self) {
        assert!(
            self.is_allocated() || self.cells.is_empty(),
            "a row has cells but not one per column"
        );
        assert!(
            self.cells()[self.written()..]
                .iter()
                .all(|&cell| cell == Cell::BLANK),
            "a cell after the written mark isn't blank"
        );
        for (col, cell) in self.cells().iter().enumerate() {
            if cell.is_wide() {
                assert!(
                    self.cells()
                        .get(col + 1)
                        .is_some_and(|c| c.is_wide_spacer()),
                    "wide cell at {col} is not followed by a spacer"
                );
            }
            if cell.is_wide_spacer() {
                assert!(
                    col > 0 && self.cells()[col - 1].is_wide(),
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
        if let Some(extras) = &self.extras {
            let cols: Vec<usize> = extras.graphemes.cols().collect();
            assert!(cols.is_sorted(), "graphemes out of order: {cols:?}");
            assert!(
                cols.last().is_none_or(|&col| col < self.width()),
                "grapheme entry past the edge"
            );
            if extras.lookup.is_built() {
                for (position, style) in self.palette.iter().enumerate() {
                    assert_eq!(
                        extras.lookup.find(&self.palette, style),
                        Some(palette_index(position)),
                        "the style lookup disagrees with the palette"
                    );
                }
            }
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
    fn text_ends_after_the_last_cell_that_isnt_blank() {
        assert_eq!(row_with("a字b", 8).text_end(), 4);
        assert_eq!(row_with("ab 字", 8).text_end(), 5, "a wide last character");
        assert_eq!(row_with("", 8).text_end(), 0);
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
        assert_eq!(row.text().chars().count(), 1 + graphemes::MAX_EXTRA);
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
    fn big_palettes_are_found_through_the_lookup() {
        let mut row = Row::new(40);
        let style = |i: u8| Style {
            fg: Color::Indexed(i),
            bg: Color::Rgb(i, i, 0),
            ..Style::DEFAULT
        };
        // Enough styles to build the lookup, then to compact it away and
        // build it again.
        for i in 0..=255 {
            row.write(usize::from(i % 40), 'x', false, &style(i));
            row.assert_invariants();
        }
        assert!(row.extras.as_ref().is_some_and(|e| e.lookup.is_built()));
        assert_eq!(*row.style(row.cells()[255 % 40]), style(255));
    }

    #[test]
    fn a_row_allocates_its_cells_only_once_written() {
        let mut row = Row::new(80);
        assert_eq!(row.heap_bytes(), 0);
        assert_eq!(row.cells().len(), 80);
        // Blanking what's blank already changes nothing.
        row.fill(0..80, &Style::DEFAULT);
        row.insert_blanks(3, 2, &Style::DEFAULT);
        row.delete_cells(3, 2, &Style::DEFAULT);
        row.reset(&Style::DEFAULT);
        assert_eq!(row.heap_bytes(), 0);
        row.assert_invariants();

        row.write(5, 'x', false, &Style::DEFAULT);
        assert_eq!(row.heap_bytes(), 80 * size_of::<Cell>());
        assert_eq!(row.text(), "     x");
        row.assert_invariants();
    }

    #[test]
    fn blanking_in_a_colour_allocates() {
        let mut row = Row::new(8);
        row.fill(2..4, &red());
        assert_eq!(*row.style(row.cells()[3]), red());
        assert_eq!(*row.style(row.cells()[4]), Style::DEFAULT);
        row.assert_invariants();

        let mut row = Row::new(8);
        row.reset(&red());
        assert!(row.cells().iter().all(|&cell| *row.style(cell) == red()));
        row.assert_invariants();
    }

    #[test]
    fn rows_wider_than_any_terminal_allocate_up_front() {
        let row = Row::new(usize::from(Size::MAX) + 1);
        assert_eq!(row.cells().len(), usize::from(Size::MAX) + 1);
        row.assert_invariants();
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
