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

    /// The text from `from` to `to`, both included, as it would be typed:
    /// lines end in newlines unless they wrapped onto the next, and blanks
    /// at the ends of lines are left out (blanks before more text on the
    /// line are kept). With `whole_lines`, every line is
    /// taken whole.
    #[must_use]
    pub fn text(&self, from: Position, to: Position, whole_lines: bool) -> String {
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        let (first, last) = self.document_bounds();
        let mut text = String::new();
        for line in from.line.max(first)..=to.line.min(last) {
            let Some(row) = self.document_line(line) else {
                continue;
            };
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
                if end >= text_end(&row) {
                    text.truncate(at + text[at..].trim_end_matches(' ').len());
                }
                if line != to.line.min(last) {
                    text.push('\n');
                }
            }
        }
        text
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
        if query.is_empty() {
            return None;
        }
        let (first, last) = self.document_bounds();
        let from_line = from.line.clamp(first, last);
        let lines = last - first + 1;
        // Every line once, starting from `from`'s and going round, then
        // `from`'s line again for matches on the far side of `from`.
        for step in 0..=lines {
            let line = if backward {
                first + (from_line - first + lines - step % lines) % lines
            } else {
                first + (from_line - first + step) % lines
            };
            let Some(row) = self.document_line(line) else {
                continue;
            };
            let matches = matches_in(&row, &query, exact);
            let found = match (step, backward) {
                (0, false) => matches.into_iter().find(|&(col, _)| col > from.col),
                (0, true) => matches.into_iter().rev().find(|&(col, _)| col < from.col),
                (_, false) => matches.into_iter().next(),
                (_, true) => matches.into_iter().next_back(),
            };
            if let Some((col, cells)) = found {
                return Some(Match {
                    start: Position { line, col },
                    cells,
                });
            }
        }
        None
    }
}

/// Where the row's text ends: one past its last character that isn't a
/// blank.
fn text_end(row: &Row) -> usize {
    row.cells()
        .iter()
        .rposition(|cell| cell.ch() != ' ' || cell.is_wide_spacer())
        .map_or(0, |last| last + 1)
}

/// Every place `query` starts in `row`, as a column and the cells it
/// covers.
fn matches_in(row: &Row, query: &[char], exact: bool) -> Vec<(usize, usize)> {
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

#[cfg(test)]
mod tests {
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
    fn matches_count_wide_characters_as_two_cells() {
        let t = terminal("a字b");
        let found = t.search("字b", at(0, 0), false, false).unwrap();
        assert_eq!((found.start.col, found.cells), (1, 3));
    }
}
