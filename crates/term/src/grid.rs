//! The rows of one screen, plus its scrollback.

use std::collections::VecDeque;
use std::ops::Range;

use crate::row::Row;
use crate::scrollback::Scrollback;
use crate::style::Style;

/// A screen of rows and the history above it.
///
/// The screen is a ring of rows. A row that scrolls off the top is stored
/// in [`Scrollback`], compactly, and then blanked and reused as the new
/// bottom row, so scrolling never allocates rows.
#[derive(Clone, Debug)]
pub struct Grid {
    /// Exactly `height` rows, top first.
    rows: VecDeque<Row>,
    scrollback: Scrollback,
    width: usize,
    height: usize,
}

impl Grid {
    pub(crate) fn new(width: usize, height: usize, scrollback_limit: usize) -> Self {
        Self {
            rows: (0..height).map(|_| Row::new(width)).collect(),
            scrollback: Scrollback::new(scrollback_limit),
            width,
            height,
        }
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
    /// scrolled off. History is stored compactly, so each is rebuilt.
    #[must_use]
    pub fn scrollback(&self) -> impl ExactSizeIterator<Item = Row> + '_ {
        self.scrollback.rows()
    }

    pub(crate) fn row_mut(&mut self, line: usize) -> &mut Row {
        &mut self.rows[line]
    }

    /// Scrolls the whole screen up by `count` lines. The top lines move into
    /// scrollback, and blank lines in `blank` appear at the bottom.
    pub(crate) fn scroll_up_into_scrollback(&mut self, count: usize, blank: &Style) {
        for _ in 0..count.min(self.height) {
            let mut row = self.rows.pop_front().expect("a grid always has rows");
            // A row that wrapped early, because a wide character didn't fit in
            // its last column, ends with padding rather than text.
            let padded = row.is_wrapped()
                && row
                    .cells()
                    .last()
                    .is_some_and(|&cell| cell == crate::Cell::BLANK)
                && self
                    .rows
                    .front()
                    .is_some_and(|next| next.cells().first().is_some_and(|cell| cell.is_wide()));
            self.scrollback.push(&row, padded);
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
        self.rotate_left(rows.clone(), rows.len() - count);
        for index in rows.start..rows.start + count {
            self.rows[index].reset(blank);
        }
    }

    /// Blanks every screen row in `blank`.
    pub(crate) fn clear(&mut self, blank: &Style) {
        for row in &mut self.rows {
            row.reset(blank);
        }
    }

    pub(crate) fn clear_scrollback(&mut self) {
        self.scrollback.clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(lines: &[&str], scrollback_limit: usize) -> Grid {
        let mut grid = Grid::new(4, lines.len(), scrollback_limit);
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
    fn clear_scrollback_keeps_the_screen() {
        let mut grid = grid(&["a", "b"], 10);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        grid.clear_scrollback();
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(screen(&grid), ["b", ""]);
    }
}
