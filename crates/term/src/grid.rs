//! The rows of one screen, plus its scrollback.

use std::collections::VecDeque;
use std::ops::Range;

use crate::blocks::Mark;
use crate::damage::Damage;
use crate::row::Row;
use crate::scrollback::{Compaction, Scrollback};
use crate::snapshot::{Captured, Line, LineLinks, RestoreError};
use crate::style::Style;

/// A screen of rows and the history above it.
///
/// The screen is a ring of rows. A row that scrolls off the top goes into
/// [`Scrollback`], which hands back a row to blank and reuse as the new
/// bottom row, so scrolling doesn't allocate rows.
#[derive(Clone, Debug)]
pub struct Grid {
    /// Exactly `height` rows, top first.
    rows: VecDeque<Row>,
    scrollback: Scrollback,
    /// Lines changed since the damage was last taken.
    damage: Damage,
    width: usize,
    height: usize,
}

impl Grid {
    pub(crate) fn new(
        width: usize,
        height: usize,
        scrollback_limit: usize,
        compaction: Compaction,
    ) -> Self {
        Self {
            rows: (0..height).map(|_| Row::new(width)).collect(),
            scrollback: Scrollback::new(scrollback_limit, compaction),
            damage: Self::all_damaged(),
            width,
            height,
        }
    }

    /// The screen's rows and its history's, oldest first, for a snapshot,
    /// with the runs of them that are part of links.
    pub(crate) fn lines(&self) -> (Captured, Captured) {
        let mut screen = Captured::default();
        for row in &self.rows {
            screen.push_row(row);
        }
        (screen, self.scrollback.lines())
    }

    /// The styles the screen's rows and its history may have, with the
    /// links they're part of.
    pub(crate) fn styles(&self) -> impl Iterator<Item = &Style> + '_ {
        let screen = self.rows.iter().flat_map(Row::styles);
        screen.chain(self.scrollback.styles())
    }

    /// A grid showing `screen`, `width` × `screen.len()`, over `history`,
    /// with the links `links` gives the screen's lines.
    pub(crate) fn restored(
        width: usize,
        screen: &[Line],
        mut links: LineLinks<'_>,
        history: Scrollback,
    ) -> Result<Self, RestoreError> {
        let rows = screen
            .iter()
            .enumerate()
            .map(|(number, line)| {
                if usize::from(line.width) == width {
                    line.to_linked_row(&links.of(number)?)
                } else {
                    Err(RestoreError("a screen line isn't as wide as the screen"))
                }
            })
            .collect::<Result<VecDeque<_>, _>>()?;
        links.finish()?;
        Ok(Self {
            height: rows.len(),
            rows,
            scrollback: history,
            damage: Self::all_damaged(),
            width,
        })
    }

    /// Damage covering the whole screen: a new screen hasn't been seen.
    fn all_damaged() -> Damage {
        let mut damage = Damage::new();
        damage.mark_all();
        damage
    }

    /// Columns per row.
    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// Rows on screen.
    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    /// Rows currently held in scrollback.
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    /// The screen row `line`, counting from the top of the screen.
    ///
    /// # Panics
    ///
    /// If `line` is not on screen.
    #[must_use]
    pub fn row(&self, line: usize) -> &Row {
        &self.rows[line]
    }

    /// The screen's rows, top to bottom.
    #[must_use]
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &Row> {
        self.rows.iter()
    }

    /// The scrollback's rows, oldest first, each at the width it had when it
    /// scrolled off. History is mostly stored compactly, so each is rebuilt.
    #[must_use]
    pub fn scrollback(&self) -> impl ExactSizeIterator<Item = Row> + '_ {
        self.scrollback.rows()
    }

    /// The number the next line scrolled into history will get. Lines are
    /// numbered from 0 as they arrive, so numbers stay put while new lines
    /// push old ones out. A resize can take lines back, though, which makes
    /// this smaller (see [`history_generation`](Self::history_generation)).
    #[must_use]
    pub fn scrollback_end(&self) -> u64 {
        self.scrollback.end()
    }

    /// A number that changes whenever history's lines are renumbered:
    /// when it is cleared, or a resize takes back the start of the line the
    /// screen's top row continues and puts it back rewrapped. Line numbers
    /// read before then may name other lines now, or none.
    #[must_use]
    pub fn history_generation(&self) -> u64 {
        self.scrollback.generation()
    }

    /// Carries on from a grid whose history generation was `before`: this
    /// one's history is numbered afresh.
    pub(crate) fn follow_history(&mut self, before: u64) {
        self.scrollback.follow(before);
    }

    /// The history lines numbered within `numbers` that are still held,
    /// oldest first, each with its number and at the width it had.
    pub fn scrollback_lines(
        &self,
        numbers: std::ops::Range<u64>,
    ) -> impl Iterator<Item = (u64, Row)> + '_ {
        self.scrollback.numbered_rows(numbers)
    }

    /// The history lines numbered within `numbers` that a block's prompt
    /// starts on, oldest first, each with its number and the block's
    /// (see [`Row::prompt_block`], which says it for the screen's rows).
    pub fn history_prompts(
        &self,
        numbers: std::ops::Range<u64>,
    ) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.scrollback
            .marks_from(numbers.start)
            .take_while(move |&(line, _)| line < numbers.end)
            .filter(|(_, mark)| mark.prompt)
            .map(|(line, mark)| (line, mark.block))
    }

    /// Every line a shell marked (see [`crate::blocks`]), in history then
    /// on screen, numbered as in [`crate::document`].
    pub(crate) fn marks(&self) -> impl Iterator<Item = (u64, Mark)> + '_ {
        let end = self.scrollback.end();
        let screen = (end..)
            .zip(&self.rows)
            .filter_map(|(line, row)| Some((line, row.mark()?)));
        self.scrollback.marks().chain(screen)
    }

    /// Marks the lines in `marks`, numbered as [`marks`](Self::marks)
    /// numbers them. Lines that aren't held are left out.
    pub(crate) fn set_marks(&mut self, marks: &[(u64, Mark)]) {
        let end = self.scrollback.end();
        self.scrollback
            .set_marks(marks.iter().copied().filter(|&(line, _)| line < end));
        for row in &mut self.rows {
            row.set_mark(None);
        }
        for &(line, mark) in marks {
            if let Some(row) = line
                .checked_sub(end)
                .and_then(|line| usize::try_from(line).ok())
                .and_then(|line| self.rows.get_mut(line))
            {
                row.set_mark(Some(mark));
            }
        }
    }

    /// Screen row `line`, for changing. Every change to the screen goes
    /// through this or another method of the grid, so each can record what
    /// it damaged.
    pub(crate) fn row_mut(&mut self, line: usize) -> &mut Row {
        self.damage.mark(line);
        &mut self.rows[line]
    }

    /// Screen row `line`, for erasing or editing rather than printing.
    /// Doing that to the top row ends the line in history it continued:
    /// what's there is no longer known to be more of that line.
    pub(crate) fn edit_row(&mut self, line: usize) -> &mut Row {
        if line == 0 {
            self.end_history_line();
        }
        self.row_mut(line)
    }

    /// Adds the lines changed since the last call to `into`.
    pub(crate) fn take_damage(&mut self, into: &mut Damage) {
        into.merge(&self.damage);
        self.damage.clear();
    }

    /// Records that every line changed.
    pub(crate) fn damage_all(&mut self) {
        self.damage.mark_all();
    }

    /// Scrolls the whole screen up by `count` lines. The top lines move into
    /// scrollback, and blank lines in `blank` appear at the bottom.
    pub(crate) fn scroll_up_into_scrollback(&mut self, count: usize, blank: &Style) {
        if count > 0 {
            self.damage.mark_all();
        }
        for _ in 0..count.min(self.height) {
            let row = self.rows.pop_front().expect("a grid always has rows");
            let mut row = match self.scrollback.push(row) {
                Some(spare) if spare.width() == self.width => spare,
                _ => Row::new(self.width),
            };
            row.reset(blank);
            self.rows.push_back(row);
        }
    }

    /// Scrolls the screen lines in `region` up by `count`, discarding the lines
    /// that leave the top of the region and blanking those that enter at the
    /// bottom.
    pub(crate) fn scroll_up(&mut self, region: Range<usize>, count: usize, blank: &Style) {
        let count = count.min(region.len());
        let rows = self.absolute(region);
        if count > 0 {
            self.damage.mark_range(rows.clone());
            self.top_replaced(&rows);
        }
        self.rotate_left(rows.clone(), count);
        for index in rows.end - count..rows.end {
            self.rows[index].reset(blank);
        }
    }

    /// Scrolls the screen lines in `region` down by `count`, discarding the
    /// lines that leave the bottom of the region and blanking those that enter
    /// at the top.
    pub(crate) fn scroll_down(&mut self, region: Range<usize>, count: usize, blank: &Style) {
        let count = count.min(region.len());
        let rows = self.absolute(region);
        if count > 0 {
            self.damage.mark_range(rows.clone());
            self.top_replaced(&rows);
        }
        self.rotate_left(rows.clone(), rows.len() - count);
        for index in rows.start..rows.start + count {
            self.rows[index].reset(blank);
        }
    }

    /// Ends the line in history the top row continued, if scrolling `rows`
    /// within the screen replaces the top row.
    fn top_replaced(&mut self, rows: &Range<usize>) {
        if rows.start == 0 {
            self.end_history_line();
        }
    }

    /// Resizes the screen, rewrapping its lines to the new width, and returns
    /// where the cell at `cursor` ended up. If the cursor is `after` that
    /// cell, waiting to wrap, it stays after it: the returned flag says it
    /// is waiting to wrap again, at the end of a row. With `autowrap`, a
    /// cursor just past the last line's text where a row now ends waits
    /// there too. Rows that no longer fit above the cursor go into
    /// scrollback; blank rows below it go first.
    ///
    /// A line that started in scrollback is rewrapped whole, as if it had
    /// been printed at the new width: its part in scrollback is taken back
    /// and rewrapped with the rest. The rows before the one the screen's
    /// top row now starts on go back into scrollback, so the screen shows
    /// the same lines as before. History older than that line is left as
    /// it is, since it can be wrapped to any width when it is read. So is
    /// the start of a line longer than [`TAKE_BACK_SCREENS`] screens: only
    /// its end is rewrapped, so resizing stays quick, and the line still
    /// reads whole.
    pub(crate) fn reflow(
        &mut self,
        width: usize,
        height: usize,
        cursor: (usize, usize),
        after: bool,
        autowrap: bool,
    ) -> (usize, usize, bool) {
        let cells = (TAKE_BACK_SCREENS * self.width * self.height).max(TAKE_BACK_CELLS_MIN);
        let mut old = self.scrollback.take_continued(cells);
        let screen_start = old.len();
        old.extend(self.rows.drain(..));
        let cursor = (cursor.0 + screen_start, cursor.1);
        // The last row with text: a cursor past it has no line after it.
        let last_text = old
            .iter()
            .rposition(|row| row.is_wrapped() || content_end(row) > 0)
            .unwrap_or(0);
        let mut rows = Vec::with_capacity(old.len());
        let mut moved = (0, 0);
        // The row the screen's old top row starts on.
        let mut screen_top = 0;
        let mut out = Row::new(width);
        let mut out_col = 0;
        for (line, row) in old.iter().enumerate() {
            let starts_line = line == 0 || !old[line - 1].is_wrapped();
            if starts_line && line > 0 {
                rows.push(std::mem::replace(&mut out, Row::new(width)));
                out_col = 0;
            }
            // A mark goes to the row its row's first character lands on, and
            // so does the top of the screen.
            let mut mark = row.mark();
            let mut top = line == screen_start;
            let text_end = if row.is_wrapped() {
                row.width() - usize::from(row.is_padded())
            } else {
                content_end(row)
            };
            // Keep the cursor's cell, even in the blank tail of its line.
            let end = if line == cursor.0 {
                text_end.max(cursor.1 + 1).min(row.width())
            } else {
                text_end
            };
            for col in 0..end {
                let cell = row.cells()[col];
                if cell.is_wide_spacer() {
                    continue;
                }
                let cells = if cell.is_wide() { 2 } else { 1 };
                if cells > width {
                    continue;
                }
                // A cursor past the last line's text that would start a row
                // waits at the end of the row before to wrap, as it does
                // after printing up to the end of a row. With a line after
                // it, wrapping would join the two, and without autowrap
                // it couldn't wait.
                if line == cursor.0
                    && col == cursor.1
                    && col >= text_end
                    && out_col == width
                    && !row.is_wrapped()
                    && !after
                    && autowrap
                    && line >= last_text
                {
                    moved = (rows.len(), width);
                    break;
                }
                if out_col + cells > width {
                    if out_col < width {
                        out.set_wrapped_early();
                    } else {
                        out.set_wrapped(true);
                    }
                    rows.push(std::mem::replace(&mut out, Row::new(width)));
                    out_col = 0;
                }
                if let Some(mark) = mark.take() {
                    out.set_mark(Some(mark));
                }
                if std::mem::take(&mut top) {
                    screen_top = rows.len();
                }
                if line == cursor.0 && (col == cursor.1 || col + 1 == cursor.1 && cells == 2) {
                    moved = if after {
                        (rows.len(), out_col + cells)
                    } else {
                        (rows.len(), (out_col + cursor.1 - col).min(width - 1))
                    };
                }
                copy_cell(&mut out, out_col, row, col);
                out_col += cells;
            }
            if let Some(mark) = mark {
                out.set_mark(Some(mark));
            }
            if top {
                screen_top = rows.len();
            }
        }
        rows.push(out);
        let overflow = self.fit(rows, width, height, screen_top, moved.0);
        (moved.0 - overflow, moved.1.min(width - 1), moved.1 == width)
    }

    /// Makes rewrapped `rows`, `width` wide, the screen, `height` tall,
    /// and returns how many went into scrollback. The rows before
    /// `screen_top`, where the old screen's top row starts, go back into
    /// scrollback. Blank rows below the cursor's, `cursor`, are just
    /// space, so they go before anything else scrolls away. Rows above the
    /// cursor go into scrollback; if the cursor is too high for that, rows
    /// below it are lost instead.
    fn fit(
        &mut self,
        mut rows: Vec<Row>,
        width: usize,
        height: usize,
        screen_top: usize,
        cursor: usize,
    ) -> usize {
        while rows.len() > screen_top + height
            && rows.len() > cursor + 1
            && content_end(&rows[rows.len() - 1]) == 0
        {
            rows.pop();
        }
        let overflow = rows
            .len()
            .saturating_sub(height)
            .max(screen_top)
            .min(cursor);
        for row in rows.drain(..overflow) {
            self.scrollback.push(row);
        }
        rows.truncate(height);
        rows.resize_with(height, || Row::new(width));
        self.rows = rows.into();
        self.width = width;
        self.height = height;
        self.damage.mark_all();
        overflow
    }

    /// Resizes the screen without rewrapping: rows are cut off or extended
    /// on the right, and lost or added at the bottom. Full-screen programs,
    /// which use the alternate screen, redraw after a resize anyway.
    pub(crate) fn crop(&mut self, width: usize, height: usize) {
        let old: Vec<Row> = self.rows.drain(..).collect();
        let mut rows: Vec<Row> = old
            .iter()
            .take(height)
            .map(|row| {
                let mut cropped = Row::new(width);
                for col in 0..row.width().min(width) {
                    let cell = row.cells()[col];
                    // Half a wide character can't be kept.
                    if cell.is_wide_spacer() || (cell.is_wide() && col + 1 >= width) {
                        continue;
                    }
                    if cell != crate::Cell::BLANK {
                        copy_cell(&mut cropped, col, row, col);
                    }
                }
                cropped
            })
            .collect();
        rows.resize_with(height, || Row::new(width));
        self.rows = rows.into();
        self.width = width;
        self.height = height;
        self.damage.mark_all();
    }

    /// Blanks every screen row in `blank`.
    pub(crate) fn clear(&mut self, blank: &Style) {
        self.damage.mark_all();
        for row in &mut self.rows {
            row.reset(blank);
        }
        self.end_history_line();
    }

    /// Ends the newest line in scrollback, which the screen's top row no
    /// longer continues: everything above the cursor was erased. Erasing
    /// rows ends their lines on screen the same way, so a resize won't
    /// join the screen's first line to the history before it.
    pub(crate) fn end_history_line(&mut self) {
        self.scrollback.end_newest_line();
    }

    pub(crate) fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    pub(crate) fn scrollback_needs_compaction(&self) -> bool {
        self.scrollback.needs_compaction()
    }

    pub(crate) fn compact_scrollback(&mut self) {
        self.scrollback.compact();
    }

    /// Roughly the heap memory the screen's rows hold.
    #[cfg(test)]
    pub(crate) fn screen_heap_bytes(&self) -> usize {
        self.rows.capacity() * size_of::<Row>()
            + self.rows.iter().map(Row::heap_bytes).sum::<usize>()
    }

    #[cfg(test)]
    pub(crate) fn assert_scrollback_invariants(&self) {
        self.scrollback.assert_invariants();
    }

    fn absolute(&self, lines: Range<usize>) -> Range<usize> {
        assert!(lines.end <= self.height, "region {lines:?} is off screen");
        lines
    }

    /// Rotates `rows` left by `count` in place, without making the deque
    /// contiguous, which would move every row.
    fn rotate_left(&mut self, rows: Range<usize>, count: usize) {
        // The rows are almost always in one of the deque's two slices, where
        // the standard library rotates them with a few block moves.
        let (front, back) = self.rows.as_mut_slices();
        let split = front.len();
        if rows.end <= split {
            front[rows].rotate_left(count);
            return;
        }
        if rows.start >= split {
            back[rows.start - split..rows.end - split].rotate_left(count);
            return;
        }
        // They straddle the deque's wrap point: three reversals, by swaps.
        self.reverse(rows.start..rows.start + count);
        self.reverse(rows.start + count..rows.end);
        self.reverse(rows);
    }

    fn reverse(&mut self, rows: Range<usize>) {
        let (mut low, mut high) = (rows.start, rows.end);
        while low + 1 < high {
            high -= 1;
            self.rows.swap(low, high);
            low += 1;
        }
    }
}

/// A resize takes back at most this many screens' worth of cells of the
/// line the screen's top row continues from history.
const TAKE_BACK_SCREENS: usize = 4;

/// Small screens take back this many cells, so ordinary long lines are
/// still rewrapped whole.
const TAKE_BACK_CELLS_MIN: usize = 16 * 1024;

/// Where a row's content ends: everything after is blank.
fn content_end(row: &Row) -> usize {
    row.cells()[..row.written()]
        .iter()
        .rposition(|&cell| cell != crate::Cell::BLANK)
        .map_or(0, |last| last + 1)
}

/// Copies the cell at `src_col` of `src`, with its style and grapheme, to
/// `dst_col` of `dst`.
fn copy_cell(dst: &mut Row, dst_col: usize, src: &Row, src_col: usize) {
    let cell = src.cells()[src_col];
    dst.write(dst_col, cell.ch(), cell.is_wide(), src.style(cell));
    if let Some(extra) = src.grapheme(src_col) {
        for &ch in extra {
            dst.append_to_grapheme(dst_col, ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(lines: &[&str], scrollback_limit: usize) -> Grid {
        let mut grid = Grid::new(4, lines.len(), scrollback_limit, Compaction::default());
        for (line, text) in lines.iter().enumerate() {
            for (col, ch) in text.chars().enumerate() {
                grid.row_mut(line).write(col, ch, false, &Style::DEFAULT);
            }
        }
        grid
    }

    fn screen(grid: &Grid) -> Vec<String> {
        grid.rows().map(Row::text).collect()
    }

    fn scrollback(grid: &Grid) -> Vec<String> {
        grid.scrollback().map(|row| row.text()).collect()
    }

    #[test]
    fn scrolling_into_scrollback_keeps_the_newest_lines() {
        let mut grid = grid(&["a", "b", "c"], 2);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["b", "c", ""]);
        assert_eq!(scrollback(&grid), ["a"]);

        grid.scroll_up_into_scrollback(2, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["", "", ""]);
        assert_eq!(scrollback(&grid), ["b", "c"]);
    }

    #[test]
    fn scrolling_without_scrollback_discards_the_top_line() {
        let mut grid = grid(&["a", "b", "c"], 0);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["b", "c", ""]);
        assert_eq!(grid.scrollback_len(), 0);
    }

    #[test]
    fn scroll_count_is_clamped_to_the_screen() {
        let mut grid = grid(&["a", "b"], 10);
        grid.scroll_up_into_scrollback(usize::MAX, &Style::DEFAULT);
        assert_eq!(scrollback(&grid), ["a", "b"]);
    }

    #[test]
    fn region_scrolls_leave_lines_outside_the_region_alone() {
        let mut grid = grid(&["a", "b", "c", "d", "e"], 10);
        grid.scroll_up(1..4, 1, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["a", "c", "d", "", "e"]);
        assert_eq!(grid.scrollback_len(), 0);

        grid.scroll_down(1..4, 2, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["a", "", "", "c", "e"]);

        grid.scroll_down(0..5, 99, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["", "", "", "", ""]);
    }

    #[test]
    fn region_scrolls_work_below_scrollback() {
        let mut grid = grid(&["a", "b", "c"], 10);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        grid.row_mut(2).write(0, 'd', false, &Style::DEFAULT);
        grid.scroll_up(0..3, 1, &Style::DEFAULT);
        assert_eq!(screen(&grid), ["c", "d", ""]);
        assert_eq!(scrollback(&grid), ["a"]);
    }

    #[test]
    fn region_scrolls_work_wherever_the_ring_wraps() {
        // Scrolling with full scrollback walks the deque's start around its
        // buffer, so some of these offsets put the region across the point
        // where the buffer wraps.
        for offset in 0..12 {
            let mut grid = grid(&["", "", "", ""], 3);
            for _ in 0..offset {
                grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
            }
            for (line, ch) in "abcd".chars().enumerate() {
                grid.row_mut(line).write(0, ch, false, &Style::DEFAULT);
            }
            grid.scroll_up(0..3, 1, &Style::DEFAULT);
            assert_eq!(screen(&grid), ["b", "c", "", "d"], "offset {offset}");
            grid.scroll_down(1..4, 2, &Style::DEFAULT);
            assert_eq!(screen(&grid), ["b", "", "", "c"], "offset {offset}");
        }
    }

    #[test]
    fn a_new_screen_allocates_no_cells() {
        let grid = Grid::new(120, 40, 100, Compaction::default());
        // The rows themselves, and nothing for their cells: a few KiB
        // rather than the 40 KiB of cells.
        assert_eq!(grid.screen_heap_bytes(), 40 * size_of::<Row>());
    }

    #[test]
    fn clear_scrollback_keeps_the_screen() {
        let mut grid = grid(&["a", "b"], 10);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        grid.clear_scrollback();
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(screen(&grid), ["b", ""]);
    }
}
