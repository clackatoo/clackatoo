//! The rows of one screen, plus its scrollback.

use std::collections::VecDeque;
use std::ops::Range;

use crate::row::Row;
use crate::style::Style;

/// A screen of rows and the scrollback above it.
///
/// Rows are kept in one deque: scrollback first, oldest at the front,
/// followed by the screen's rows. Scrolling into scrollback moves the
/// boundary instead of copying rows. Once scrollback is full, the oldest row
/// is reused as the new blank line, so steady-state scrolling doesn't allocate.
#[derive(Clone, Debug)]
pub struct Grid {
    rows: VecDeque<Row>,
    width: usize,
    height: usize,
    scrollback_limit: usize,
}

impl Grid {
    pub(crate) fn new(width: usize, height: usize, scrollback_limit: usize) -> Self {
        Self {
            rows: (0..height).map(|_| Row::new(width)).collect(),
            width,
            height,
            scrollback_limit,
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
        self.rows.len() - self.height
    }

    /// The screen row `line`, counting from the top of the screen.
    ///
    /// # Panics
    ///
    /// If `line` is not on screen.
    #[must_use]
    pub fn row(&self, line: usize) -> &Row {
        assert!(line < self.height, "row {line} is off screen");
        &self.rows[self.scrollback_len() + line]
    }

    /// The screen's rows, top to bottom.
    #[must_use]
    pub fn rows(&self) -> impl ExactSizeIterator<Item = &Row> {
        self.rows.range(self.scrollback_len()..)
    }

    /// The scrollback's rows, oldest first.
    #[must_use]
    pub fn scrollback(&self) -> impl ExactSizeIterator<Item = &Row> {
        self.rows.range(..self.scrollback_len())
    }

    pub(crate) fn row_mut(&mut self, line: usize) -> &mut Row {
        assert!(line < self.height, "row {line} is off screen");
        let index = self.scrollback_len() + line;
        &mut self.rows[index]
    }

    /// Scrolls the whole screen up by `count` lines. The top lines move into
    /// scrollback, and blank lines in `blank` appear at the bottom.
    pub(crate) fn scroll_up_into_scrollback(&mut self, count: usize, blank: &Style) {
        for _ in 0..count.min(self.height) {
            // When scrollback is full, the front row is the one to discard:
            // the oldest scrollback row, or, with no scrollback at all, the
            // top screen row, which has nowhere to go. Either way, reuse it.
            let mut row = if self.scrollback_len() >= self.scrollback_limit {
                self.rows.pop_front().expect("a grid always has rows")
            } else {
                Row::new(self.width)
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
        let start = self.scrollback_len();
        for row in self.rows.range_mut(start..) {
            row.reset(blank);
        }
    }

    pub(crate) fn clear_scrollback(&mut self) {
        let scrollback = self.scrollback_len();
        self.rows.drain(..scrollback);
    }

    fn absolute(&self, lines: Range<usize>) -> Range<usize> {
        assert!(lines.end <= self.height, "region {lines:?} is off screen");
        let offset = self.scrollback_len();
        lines.start + offset..lines.end + offset
    }

    /// Rotates `rows` left by `count` in place, using three reversals. This
    /// avoids allocating, and avoids making the deque contiguous, which could
    /// move all of scrollback.
    fn rotate_left(&mut self, rows: Range<usize>, count: usize) {
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
        grid.scrollback().map(Row::text).collect()
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
    fn clear_scrollback_keeps_the_screen() {
        let mut grid = grid(&["a", "b"], 10);
        grid.scroll_up_into_scrollback(1, &Style::DEFAULT);
        grid.clear_scrollback();
        assert_eq!(grid.scrollback_len(), 0);
        assert_eq!(screen(&grid), ["b", ""]);
    }
}
