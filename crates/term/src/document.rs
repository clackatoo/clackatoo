//! The terminal as one document of lines: its history, then its screen.
//!
//! History lines are numbered from 0 as they scroll off the screen, and the
//! screen's rows are numbered on from the history's end, so a line keeps
//! its number as it scrolls into history. On the alternate screen, which
//! keeps no history, the document is the screen alone.

use std::borrow::Cow;

use crate::row::Row;
use crate::terminal::Terminal;

/// A place in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Position {
    /// The line's number.
    pub line: u64,
    /// The column, from 0.
    pub col: usize,
}

/// The most text one copy gives, in bytes: more is cut to this, back to
/// where a character starts, so a copy of a huge history neither holds
/// the daemon up nor makes a message too big to send.
pub const MOST_TEXT: usize = 4 << 20;

/// The most lines one copy reads, for the same reason: a history of blank
/// lines makes little text but takes as long to read.
pub const MOST_LINES: u64 = 100_000;

/// Where a search found its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Match {
    /// The first cell of the match.
    pub start: Position,
    /// How many cells it covers.
    pub cells: usize,
}

impl Terminal {
    /// The first and last lines of the document, inclusive.
    #[must_use]
    pub fn document_bounds(&self) -> (u64, u64) {
        let primary = self.primary();
        let end = primary.scrollback_end();
        let height = self.grid().height() as u64;
        let first = if self.is_alternate_screen() {
            end
        } else {
            end - primary.scrollback_len() as u64
        };
        (first, end + height - 1)
    }

    /// The document's line `line`, if it exists.
    #[must_use]
    pub fn document_line(&self, line: u64) -> Option<Cow<'_, Row>> {
        let (first, last) = self.document_bounds();
        if !(first..=last).contains(&line) {
            return None;
        }
        let end = self.primary().scrollback_end();
        if line < end {
            let (_, row) = self.primary().scrollback_lines(line..line + 1).next()?;
            Some(Cow::Owned(row))
        } else {
            let row = usize::try_from(line - end).ok()?;
            Some(Cow::Borrowed(self.grid().row(row)))
        }
    }

    /// The document's lines from `first` to `last`, both included, that it
    /// has, in order, with their numbers, but no more than [`MOST_LINES`]:
    /// history's read in one pass, as reading it a line at a time would go
    /// through its pages again for each.
    fn lines_between(&self, first: u64, last: u64) -> impl Iterator<Item = (u64, Cow<'_, Row>)> {
        let (lowest, highest) = self.document_bounds();
        let (first, last) = (first.max(lowest), last.min(highest));
        let end = self.primary().scrollback_end();
        let history = (first < end && first <= last)
            .then(|| {
                self.primary()
                    .scrollback_lines(first..last.min(end - 1) + 1)
            })
            .into_iter()
            .flatten()
            .map(|(line, row)| (line, Cow::Owned(row)));
        let screen = (first.max(end)..=last).filter_map(move |line| {
            let row = usize::try_from(line - end).ok()?;
            Some((line, Cow::Borrowed(self.grid().row(row))))
        });
        history
            .chain(screen)
            .take(usize::try_from(MOST_LINES).unwrap_or(usize::MAX))
    }

    /// The text from `from` to `to`, both included, as it would be typed:
    /// lines end in newlines unless they wrapped onto the next, and blanks
    /// at the ends of lines are left out (blanks before more text on the
    /// line are kept). With `whole_lines`, every line is
    /// taken whole. No more than [`MOST_TEXT`] and [`MOST_LINES`].
    #[must_use]
    pub fn text(&self, from: Position, to: Position, whole_lines: bool) -> String {
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        let last = to.line.min(self.document_bounds().1);
        let mut text = String::new();
        for (line, row) in self.lines_between(from.line, to.line) {
            if text.len() > MOST_TEXT {
                break;
            }
            let start = if whole_lines || line != from.line {
                0
            } else {
                from.col
            };
            let end = if whole_lines || line != to.line {
                row.width()
            } else {
                to.col.saturating_add(1).min(row.width())
            };
            let at = text.len();
            for col in start..end {
                row.push_text(col, &mut text);
            }
            let wrapped = row.is_wrapped() && end == row.width();
            if !wrapped {
                // Blanks up to the end of the line aren't text.
                if end >= row.text_end() {
                    text.truncate(at + text[at..].trim_end_matches(' ').len());
                }
                if line != last {
                    text.push('\n');
                }
            }
        }
        cut_to_most(text)
    }

    /// The text of the block of columns with corners at `from` and `to`, as
    /// block visual mode copies it: on each line between, the cells from
    /// the lesser column to the greater, both included, or to the line's
    /// end if the greater is `usize::MAX`. A line for each, joined with
    /// newlines whether or not they wrapped, and blanks up to the end of a
    /// line's text left out, as [`text`](Self::text) leaves them. Half a
    /// wide character an edge cuts is a space, as an editor's block takes
    /// it; a client takes one at a corner in whole. No more than
    /// [`MOST_TEXT`] and [`MOST_LINES`].
    #[must_use]
    pub fn block_text(&self, from: Position, to: Position) -> String {
        let (left, right) = (from.col.min(to.col), from.col.max(to.col));
        let mut text = String::new();
        let lines = self.lines_between(from.line.min(to.line), from.line.max(to.line));
        for (index, (_, row)) in lines.enumerate() {
            if text.len() > MOST_TEXT {
                break;
            }
            if index > 0 {
                text.push('\n');
            }
            let at = text.len();
            let end = right.saturating_add(1).min(row.width());
            let start = left.min(end);
            for (col, cell) in (start..end).zip(&row.cells()[start..end]) {
                let cut_left = col == left && cell.is_wide_spacer();
                let cut_right = col + 1 == end && cell.is_wide() && end < row.width();
                if cut_left || cut_right {
                    text.push(' ');
                } else {
                    row.push_text(col, &mut text);
                }
            }
            // Blanks up to the end of the line aren't text.
            if end >= row.text_end() {
                text.truncate(at + text[at..].trim_end_matches(' ').len());
            }
        }
        cut_to_most(text)
    }

    /// The next place `query` appears after `from`, or before it if
    /// `backward`, wrapping around the document's ends. Case is ignored
    /// unless `exact`. Matches don't cross lines.
    #[must_use]
    pub fn search(
        &self,
        query: &str,
        from: Position,
        backward: bool,
        exact: bool,
    ) -> Option<Match> {
        let query: Vec<char> = query.chars().collect();
        self.round(&query, from, backward, exact)
            .next()
            .map(|(found, _)| found)
    }

    /// The `count`th place `query` appears after `from`, or before it if
    /// `backward`, as [`search`](Self::search) finds the next, and whether
    /// it went round the document's ends to get there. A count past the
    /// matches there are goes round as often as it says, which ends where
    /// what's left over a time round does: so the document is gone
    /// through twice at most, however big the count.
    #[must_use]
    pub fn search_nth(
        &self,
        query: &str,
        from: Position,
        backward: bool,
        exact: bool,
        count: usize,
    ) -> Option<(Match, bool)> {
        let query: Vec<char> = query.chars().collect();
        let count = count.max(1);
        let mut seen = 0;
        for found in self.round(&query, from, backward, exact) {
            seen += 1;
            if seen == count {
                return Some(found);
            }
        }
        // Fewer than the count: round as often as it takes.
        let nth = (count - 1) % seen.max(1);
        let (found, _) = self.round(&query, from, backward, exact).nth(nth)?;
        Some((found, true))
    }

    /// Every match of `query` once, in the order a search from `from`
    /// meets them, round the ends to `from` itself, each with whether it
    /// went round to it.
    fn round<'a>(
        &'a self,
        query: &'a [char],
        from: Position,
        backward: bool,
        exact: bool,
    ) -> impl Iterator<Item = (Match, bool)> + 'a {
        let (first, last) = self.document_bounds();
        let from_line = from.line.clamp(first, last);
        let lines = last - first + 1;
        let steps = if query.is_empty() { 0 } else { lines + 1 };
        // Every line once, starting from `from`'s and going round, then
        // `from`'s line again for matches on the far side of `from`.
        (0..steps).flat_map(move |step| {
            let along = from_line - first;
            let (line, round) = if backward {
                (first + (along + lines - step % lines) % lines, step > along)
            } else {
                (first + (along + step) % lines, along + step >= lines)
            };
            let matches = self
                .document_line(line)
                .map(|row| matches_in(&row, query, exact))
                .unwrap_or_default();
            let wanted = move |col: usize| match step {
                0 if backward => col < from.col,
                0 => col > from.col,
                _ if step < lines => true,
                _ if backward => col >= from.col,
                _ => col <= from.col,
            };
            let mut found: Vec<(Match, bool)> = (matches.into_iter())
                .filter(|&(col, _)| wanted(col))
                .map(|(col, cells)| {
                    let start = Position { line, col };
                    (Match { start, cells }, round)
                })
                .collect();
            if backward {
                found.reverse();
            }
            found
        })
    }
}

/// Every place `query` starts in `row`, as a column and the cells it
/// covers, as [`Terminal::search`] finds it, case ignored unless `exact`:
/// so a window lighting the matches it draws lights what a search goes
/// to.
#[must_use]
pub fn matches_in(row: &Row, query: &[char], exact: bool) -> Vec<(usize, usize)> {
    // Each character with the column of the cell it's in.
    let mut chars: Vec<(usize, char)> = Vec::new();
    let mut cell_text = String::new();
    for col in 0..row.width() {
        cell_text.clear();
        row.push_text(col, &mut cell_text);
        chars.extend(cell_text.chars().map(|ch| (col, ch)));
    }
    let same = |a: char, b: char| {
        if exact {
            a == b
        } else {
            a.to_lowercase().eq(b.to_lowercase())
        }
    };
    let mut found = Vec::new();
    if chars.len() < query.len() {
        return found;
    }
    for start in 0..=chars.len() - query.len() {
        let window = &chars[start..start + query.len()];
        if window.iter().zip(query).all(|(&(_, a), &b)| same(a, b)) {
            let first_col = window[0].0;
            let last = window[window.len() - 1].0;
            let last_cells = if row.cells()[last].is_wide() { 2 } else { 1 };
            found.push((first_col, last + last_cells - first_col));
        }
    }
    found
}

/// `text`, cut to [`MOST_TEXT`] bytes, back to where a character starts,
/// if it's longer.
fn cut_to_most(mut text: String) -> String {
    if text.len() > MOST_TEXT {
        let at = (0..=MOST_TEXT)
            .rev()
            .find(|&at| text.is_char_boundary(at))
            .unwrap_or(0);
        text.truncate(at);
    }
    text
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use crate::{Config, Size, Terminal};

    use super::*;

    /// A 10 × 3 terminal after `output`, keeping history.
    fn terminal(output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(10, 3).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn at(line: u64, col: usize) -> Position {
        Position { line, col }
    }

    #[test]
    fn lines_are_numbered_through_history_and_screen() {
        let t = terminal("one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert_eq!(t.document_bounds(), (0, 4));
        assert_eq!(t.document_line(0).unwrap().text(), "one");
        assert_eq!(t.document_line(4).unwrap().text(), "five");
        assert!(t.document_line(5).is_none());
    }

    #[test]
    fn text_spans_lines_and_joins_wrapped_ones() {
        let t = terminal("hello there\r\nsecond\r\nthird");
        // "hello ther" wrapped onto "e".
        assert_eq!(t.text(at(0, 6), at(1, 0), false), "there");
        assert_eq!(t.text(at(0, 0), at(2, 2), false), "hello there\nsec");
        assert_eq!(t.text(at(2, 1), at(1, 2), true), "e\nsecond");
        // Backwards ranges are the same as forwards.
        assert_eq!(t.text(at(2, 2), at(2, 0), false), "sec");
        // Blanks are text, unless only blanks follow them.
        assert_eq!(t.text(at(0, 0), at(0, 5), false), "hello ");
        assert_eq!(t.text(at(2, 0), at(2, 8), false), "second");
    }

    #[test]
    fn a_block_takes_the_same_columns_of_each_line_without_their_end_blanks() {
        let t = terminal("abcdef\r\nabc\r\nabc  defg");
        assert_eq!(t.block_text(at(0, 1), at(1, 4)), "bcde\nbc");
        // Either corner first, and backwards across.
        assert_eq!(t.block_text(at(1, 4), at(0, 1)), "bcde\nbc");
        assert_eq!(t.block_text(at(0, 4), at(1, 1)), "bcde\nbc");
        // Blanks before more text on the line are kept.
        assert_eq!(t.block_text(at(2, 1), at(2, 3)), "bc ");
        assert_eq!(t.block_text(at(2, 2), at(2, 6)), "c  de");
        // To each line's end, and a line wrapped onto the next stays apart.
        assert_eq!(
            t.block_text(at(0, 2), at(2, usize::MAX)),
            "cdef\nc\nc  defg"
        );
        let t = terminal("hello there\r\nx");
        assert_eq!(t.block_text(at(0, 0), at(2, 1)), "he\ne\nx");
    }

    #[test]
    fn a_block_puts_a_space_for_half_a_wide_character_an_edge_cuts() {
        let t = terminal("a字b\r\nabcd");
        assert_eq!(t.block_text(at(0, 2), at(1, 3)), " b\ncd");
        assert_eq!(t.block_text(at(0, 0), at(1, 1)), "a \nab");
        assert_eq!(t.block_text(at(0, 1), at(1, 2)), "字\nbc");
    }

    #[test]
    fn a_copy_of_a_huge_history_reads_it_once_and_stops_at_the_most() {
        let config = Config {
            scrollback_lines: 300_000,
            ..Config::default()
        };
        let mut t = Terminal::new(Size::new(10, 3).unwrap(), &config);
        let lines = MOST_LINES + 1000;
        let mut output = String::new();
        for line in 0..lines {
            let _ = write!(output, "{line}\r\n");
        }
        t.advance(output.as_bytes());
        let far = at(u64::MAX, usize::MAX);
        let started = std::time::Instant::now();
        let text = t.text(at(0, 0), far, true);
        let block = t.block_text(at(0, 0), far);
        // Once through history: a line at a time took seconds.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        for copied in [&text, &block] {
            let got: Vec<&str> = copied.lines().collect();
            assert_eq!(got.len() as u64, MOST_LINES, "no more lines than the most");
            assert_eq!((got[0], got[got.len() - 1]), ("0", "99999"));
        }
        // From some way in, the same lines as reading them one by one.
        assert_eq!(t.text(at(5000, 0), at(5002, 9), false), "5000\n5001\n5002");
        assert_eq!(t.block_text(at(5000, 2), at(5002, 3)), "00\n01\n02");
    }

    #[test]
    fn a_copy_is_cut_to_the_most_text_where_a_character_starts() {
        let long = "é".repeat(MOST_TEXT);
        let cut = cut_to_most(long);
        assert!(cut.len() <= MOST_TEXT && cut.len() > MOST_TEXT - 4);
        assert!(cut.chars().all(|ch| ch == 'é'));
        assert_eq!(cut_to_most("short".into()), "short");
    }

    #[test]
    fn places_past_the_ends_are_taken_as_the_ends() {
        // As a client may send them.
        let t = terminal("hello there\r\nsecond\r\nthird");
        let far = at(u64::MAX, usize::MAX);
        assert_eq!(t.text(at(2, 0), at(2, usize::MAX), false), "second");
        assert_eq!(t.text(at(2, usize::MAX), at(2, usize::MAX), false), "");
        assert_eq!(t.text(at(0, 0), far, false), "hello there\nsecond\nthird");
        assert_eq!(t.text(far, far, true), "");
        assert_eq!(
            t.search("e", far, true, false).map(|m| m.start),
            Some(at(2, 1))
        );
        assert_eq!(
            t.search("t", far, false, false).map(|m| m.start),
            Some(at(0, 6))
        );
    }

    #[test]
    fn search_finds_forward_backward_and_wraps() {
        let t = terminal("cat\r\ndog cat\r\nbird\r\nCat");
        let found = |query, from, backward| {
            t.search(query, from, backward, false)
                .map(|m| (m.start.line, m.start.col, m.cells))
        };
        assert_eq!(found("cat", at(0, 0), false), Some((1, 4, 3)));
        assert_eq!(found("cat", at(1, 4), false), Some((3, 0, 3)));
        // Past the end, round to the start.
        assert_eq!(found("cat", at(3, 0), false), Some((0, 0, 3)));
        assert_eq!(found("cat", at(1, 4), true), Some((0, 0, 3)));
        assert_eq!(found("cat", at(0, 0), true), Some((3, 0, 3)));
        assert_eq!(found("CAT", at(1, 4), false), Some((3, 0, 3)));
        assert_eq!(found("fish", at(0, 0), false), None);
        // An exact search matches case.
        let exact = |query| t.search(query, at(0, 0), false, true).map(|m| m.start.line);
        assert_eq!(exact("Cat"), Some(3));
        assert_eq!(exact("cat"), Some(1));
        assert_eq!(exact("CAT"), None);
    }

    #[test]
    fn a_count_finds_the_nth_going_round_at_most_once() {
        // Matches at (0,0), (1,4), (3,0).
        let t = terminal("cat\r\ndog cat\r\nbird\r\nCat");
        let nth = |from, backward, count| {
            t.search_nth("cat", from, backward, false, count)
                .map(|(m, wrapped)| (m.start.line, m.start.col, wrapped))
        };
        assert_eq!(nth(at(0, 0), false, 1), Some((1, 4, false)));
        assert_eq!(nth(at(0, 0), false, 2), Some((3, 0, false)));
        assert_eq!(nth(at(0, 0), false, 3), Some((0, 0, true)));
        // Past the matches there are: round as often, but said to wrap.
        assert_eq!(nth(at(0, 0), false, 4), Some((1, 4, true)));
        assert_eq!(nth(at(0, 0), false, 3_000_000_001), Some((1, 4, true)));
        assert_eq!(nth(at(1, 4), true, 1), Some((0, 0, false)));
        assert_eq!(nth(at(1, 4), true, 2), Some((3, 0, true)));
        assert_eq!(nth(at(1, 4), true, 5), Some((3, 0, true)));
        // From the middle of a line, its matches either side.
        assert_eq!(nth(at(1, 2), false, 1), Some((1, 4, false)));
        assert_eq!(nth(at(1, 6), true, 1), Some((1, 4, false)));
        assert_eq!(t.search_nth("fish", at(0, 0), false, false, 3), None);
        assert_eq!(t.search_nth("", at(0, 0), false, false, 3), None);
        // A count of one is the next, as `search` finds it.
        for from in [at(0, 0), at(1, 4), at(3, 0), at(2, 1)] {
            for backward in [false, true] {
                assert_eq!(
                    t.search_nth("cat", from, backward, false, 1)
                        .map(|(m, _)| m),
                    t.search("cat", from, backward, false)
                );
            }
        }
    }

    #[test]
    fn matches_count_wide_characters_as_two_cells() {
        let t = terminal("a字b");
        let found = t.search("字b", at(0, 0), false, false).unwrap();
        assert_eq!((found.start.col, found.cells), (1, 3));
    }
}
