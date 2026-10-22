//! `i"` and `a"`, and the other quotes, as Vim's `current_quote()` finds
//! them, ported as it is: on one line, which it reads character by
//! character, a backslash escaping the character after it, as Vim's
//! `quoteescape` has it. It works out which quotes pair up from the line's
//! start only on a quote or with a selection; off one, the quote before
//! the cursor is the opening one, whichever it is, as in Vim.

use crate::motion::Cells;

/// What `quoteescape` holds, as Vim's has it.
const ESCAPE: char = '\\';

/// What [`current`] found, in cells: from `start` to `end`, which is in it
/// if `inclusive` or else the cell just after it; in visual mode, where
/// the selection's other end goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Quoted {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) inclusive: bool,
    pub(super) anchor: Option<usize>,
}

/// A line's characters, as `current_quote()` indexes its bytes: its
/// characters' first cells, in order, and the cells it takes.
struct Line {
    chars: Vec<(usize, char)>,
    cells: usize,
}

impl Line {
    /// The characters of `row` before cell `end`, where its line ends.
    fn of(row: Option<&impl Cells>, end: usize) -> Self {
        let chars = row.map_or_else(Vec::new, |row| {
            (row.on_from(0))
                .take_while(|&(col, _)| col < end)
                .filter(|(_, spot)| !spot.spacer)
                .map(|(col, spot)| (col, spot.ch))
                .collect()
        });
        Self { chars, cells: end }
    }

    /// The character at index `i`, or `None` past the end, Vim's NUL.
    fn at(&self, i: isize) -> Option<char> {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.chars.get(i))
            .map(|&(_, ch)| ch)
    }

    /// The index of the character in cell `col`, or the end past it.
    fn index(&self, col: usize) -> isize {
        let i = if col >= self.cells {
            self.chars.len()
        } else {
            self.chars
                .partition_point(|&(start, _)| start <= col)
                .saturating_sub(1)
        };
        i.cast_signed()
    }

    /// The first cell of the character at index `i`, or past the end.
    fn col(&self, i: isize) -> usize {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.chars.get(i))
            .map_or(self.cells, |&(col, _)| col)
    }

    /// Vim's `find_next_quote()`: the first `quote` from `i` on, or `None`.
    fn next_quote(&self, mut i: isize, quote: char, escape: bool) -> Option<isize> {
        loop {
            let c = self.at(i)?;
            if escape && c == ESCAPE {
                i += 1;
                self.at(i)?;
            } else if c == quote {
                return Some(i);
            }
            i += 1;
        }
    }

    /// Vim's `find_prev_quote()`: the last `quote` before `i`, or the
    /// line's start.
    fn previous_quote(&self, mut i: isize, quote: char, escape: bool) -> isize {
        while i > 0 {
            i -= 1;
            let mut n = 0;
            if escape {
                while i - n > 0 && self.at(i - n - 1) == Some(ESCAPE) {
                    n += 1;
                }
            }
            if n % 2 == 1 {
                i -= n;
            } else if self.at(i) == Some(quote) {
                break;
            }
        }
        i
    }
}

/// Whether a selection from `anchor` to `cursor` on `line`, more than a
/// character, is just what's inside quotes, as after `vi"`, and whether a
/// quote is in it.
fn selection(
    line: &Line,
    (cursor, anchor): (isize, isize),
    vis_before: bool,
    quote: char,
) -> (bool, bool) {
    let q = Some(quote);
    let (from, to) = if vis_before {
        (anchor, cursor)
    } else {
        (cursor, anchor)
    };
    let inside =
        from > 0 && line.at(from - 1) == q && line.at(to).is_some() && line.at(to + 1) == q;
    let selected = (from..=to).map_while(|i| line.at(i)).any(|c| c == quote);
    (inside, selected)
}

/// Where the quoted string starts and ends on `line`, its quotes, as
/// `current_quote()` finds them from the cursor: on a quote with a
/// selection (not `vis_empty`), from the anchor on if `vis_before`, the next
/// string; on one or with a selection, the string the quotes paired from
/// the line's start make; or else from the quote before the cursor.
fn string(
    line: &Line,
    cursor: isize,
    (vis_empty, vis_before): (bool, bool),
    quote: char,
) -> Option<(isize, isize)> {
    let q = Some(quote);
    let mut col_start = cursor;
    let mut col_end;
    if !vis_empty && line.at(col_start) == q {
        // Already selecting, on a quote: the next quoted string.
        if vis_before {
            col_start = line.next_quote(col_start + 1, quote, false)?;
            if let Some(end) = line.next_quote(col_start + 1, quote, true) {
                col_end = end;
            } else {
                col_end = col_start;
                col_start = cursor;
            }
        } else {
            col_end = line.previous_quote(col_start, quote, false);
            if line.at(col_end) != q {
                return None;
            }
            col_start = line.previous_quote(col_end, quote, true);
            if line.at(col_start) != q {
                col_start = col_end;
                col_end = cursor;
            }
        }
    } else if line.at(col_start) == q || !vis_empty {
        let first = if vis_empty {
            col_start
        } else if vis_before {
            line.next_quote(col_start, quote, false).unwrap_or(-1)
        } else {
            line.previous_quote(col_start, quote, false)
        };
        // Which quotes pair up, from the line's start.
        col_start = 0;
        loop {
            col_start = line.next_quote(col_start, quote, false)?;
            if col_start > first {
                return None;
            }
            col_end = line.next_quote(col_start + 1, quote, true)?;
            if col_start <= first && first <= col_end {
                break;
            }
            col_start = col_end + 1;
        }
    } else {
        // The quote before the cursor opens it, or else the next.
        col_start = line.previous_quote(col_start, quote, true);
        if line.at(col_start) != q {
            col_start = line.next_quote(col_start, quote, false)?;
        }
        col_end = line.next_quote(col_start + 1, quote, true)?;
    }
    Some((col_start, col_end))
}

/// Vim's `current_quote()` on the line `row`, which ends at cell `end`
/// (see [`line_end`](crate::motion::line_end)), the cursor in cell `cursor`,
/// and in visual mode the selection's other end in cell `anchor`, on the
/// same line: the quoted string, just its insides unless `include`, with
/// the blanks after it, or before, if `include`; a count of 2 or more
/// takes the quotes of `i"` too. None where Vim's fails.
pub(super) fn current(
    (row, end): (Option<&impl Cells>, usize),
    cursor: usize,
    anchor: Option<usize>,
    (include, count): (bool, usize),
    quote: char,
) -> Option<Quoted> {
    let line = Line::of(row, end);
    let q = Some(quote);
    let cursor = line.index(cursor);
    let visual = anchor.map(|anchor| line.index(anchor));
    let vis_empty = visual.is_none_or(|anchor| anchor == cursor);
    let vis_before = visual.is_some_and(|anchor| anchor < cursor);
    let anchor = visual.unwrap_or(cursor);
    let (inside_quotes, selected_quote) = if vis_empty {
        (false, false)
    } else {
        selection(&line, (cursor, anchor), vis_before, quote)
    };
    let (mut col_start, mut col_end) = string(&line, cursor, (vis_empty, vis_before), quote)?;
    let white = |i: isize| matches!(line.at(i), Some(' ' | '\t'));
    if include {
        if white(col_end + 1) {
            while white(col_end + 1) {
                col_end += 1;
            }
        } else {
            while col_start > 0 && white(col_start - 1) {
                col_start -= 1;
            }
        }
    }
    // After vi" another i" takes the quotes, as v2i" does.
    if !include && count < 2 && (vis_empty || !inside_quotes) {
        col_start += 1;
    }
    let mut new_anchor = anchor;
    if visual.is_some() {
        let starts_at_quote = line.at(anchor) == q || (anchor > 0 && line.at(anchor - 1) == q);
        if vis_empty || (vis_before && !selected_quote && (inside_quotes || !starts_at_quote)) {
            new_anchor = col_start;
        }
    }
    // Its end: past the closing quote, and the blanks, as Vim's
    // `inc_cursor()` steps, in it only at the line's end.
    let mut last = col_end;
    let mut inclusive = false;
    if include || count > 1 || (!vis_empty && inside_quotes) {
        if line.at(last).is_some() {
            last += 1;
        }
        inclusive = line.at(last).is_none();
    }
    if visual.is_none() {
        return Some(Quoted {
            start: line.col(col_start),
            end: line.col(last),
            inclusive,
            anchor: None,
        });
    }
    let mut cursor_at = last;
    if vis_empty || vis_before {
        cursor_at -= 1;
    } else {
        let ends_at_quote =
            line.at(anchor) == q || (line.at(anchor).is_some() && line.at(anchor + 1) == q);
        if inside_quotes || (!selected_quote && !ends_at_quote) {
            new_anchor = cursor_at - 1;
        }
        cursor_at = col_start;
    }
    Some(Quoted {
        start: line.col(new_anchor),
        end: line.col(cursor_at),
        inclusive: true,
        anchor: Some(line.col(new_anchor)),
    })
}
