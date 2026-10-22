//! Where `zt`, `zz` and `zb` put the view, worked out as Vim's
//! `scroll_cursor_top()`, `scroll_cursor_halfway()` and
//! `scroll_cursor_bot()` do without `smoothscroll`: the view always starts
//! at a line's start, so a line that doesn't fit above the cursor's is left
//! out, and `scrolloff` counts whole lines.

use crate::vim::ViewAt;

/// The lines a view is worked out over: how many screen rows each takes,
/// and the first and last there are.
pub(crate) struct Lines<F> {
    pub(crate) rows: F,
    pub(crate) first: u64,
    pub(crate) last: u64,
}

impl<F: Fn(u64) -> u64> Lines<F> {
    fn of(&self, line: u64) -> u64 {
        (self.rows)(line)
    }
}

/// The line the view starts at for `at` with the cursor on line `cursor`,
/// in a view `height` rows tall now starting at line `top`, keeping
/// `scrolloff` rows of lines by the cursor where they fit. The cursor's
/// line is taken to fit in the view.
pub(crate) fn view_top<F: Fn(u64) -> u64>(
    at: ViewAt,
    cursor: u64,
    top: u64,
    height: u64,
    scrolloff: u64,
    lines: &Lines<F>,
) -> u64 {
    match at {
        ViewAt::Top => cursor_top(cursor, top, height, scrolloff, lines),
        ViewAt::Middle => halfway(cursor, height, true, false, lines),
        ViewAt::Bottom => cursor_bottom(cursor, height, scrolloff, lines),
    }
}

/// `zt`: the cursor's line at the top, with `scrolloff` rows of lines
/// above it, and room for as many below; or, if they don't fit, halfway.
fn cursor_top<F: Fn(u64) -> u64>(
    cursor: u64,
    top: u64,
    height: u64,
    scrolloff: u64,
    lines: &Lines<F>,
) -> u64 {
    let mut used = lines.of(cursor);
    let mut scrolled = if cursor < top { used } else { 0 };
    let mut new_top = cursor;
    let mut extra = 0;
    let mut below = cursor + 1;
    while new_top > lines.first {
        let above = new_top - 1;
        let rows = lines.of(above);
        if above < top {
            scrolled += rows;
        }
        if (new_top >= top || scrolled > 0) && extra >= scrolloff {
            break;
        }
        used += rows;
        if extra + rows <= scrolloff && below < lines.last {
            used += lines.of(below);
        }
        if used > height {
            break;
        }
        extra += rows;
        new_top = above;
        below += 1;
    }
    if used > height {
        halfway(cursor, height, false, false, lines)
    } else {
        new_top.min(cursor)
    }
}

/// `zz`, and where the others' lines don't fit: the cursor's line in the
/// middle, lines added below and above it in turn, below first unless
/// `prefer_above`, until the next doesn't fit. With `at_end`, the rows
/// past the text's end count as lines below.
fn halfway<F: Fn(u64) -> u64>(
    cursor: u64,
    height: u64,
    at_end: bool,
    prefer_above: bool,
    lines: &Lines<F>,
) -> u64 {
    let mut used = lines.of(cursor);
    let (mut top, mut down) = (cursor, cursor);
    'adding: while top > lines.first {
        let (mut above, mut below) = (0, 0);
        for round in 1..=2 {
            let add_below = if prefer_above {
                round == 2 && below < above
            } else {
                round == 1 && below <= above
            };
            if add_below {
                if down < lines.last {
                    down += 1;
                    used += lines.of(down);
                    if used > height {
                        break 'adding;
                    }
                    below += lines.of(down);
                } else {
                    below += 1;
                    if at_end {
                        used += 1;
                    }
                }
            }
            let add_above = round == 1
                && if prefer_above {
                    below >= above
                } else {
                    below > above
                };
            if add_above {
                used += lines.of(top - 1);
                if used > height {
                    break 'adding;
                }
                above += lines.of(top - 1);
                top -= 1;
            }
        }
    }
    top
}

/// `zb`: the cursor's line at the bottom, then scrolled on by whole lines
/// until there are `scrolloff` rows of lines below it; or, if they don't
/// fit, halfway, lines above first.
fn cursor_bottom<F: Fn(u64) -> u64>(
    cursor: u64,
    height: u64,
    scrolloff: u64,
    lines: &Lines<F>,
) -> u64 {
    // As many whole lines as fit, down to the cursor's.
    let mut top = cursor;
    let mut used = 0;
    let mut line = cursor;
    loop {
        let rows = lines.of(line);
        if used + rows > height {
            break;
        }
        top = line;
        used += rows;
        if line == lines.first {
            break;
        }
        line -= 1;
    }
    let empty = height - used;
    let past = cursor + 1;
    // How far on that leaves `scrolloff` rows below.
    let mut used = lines.of(cursor);
    // Less the empty rows past the text's end, so it can be fewer than
    // none.
    let mut scrolled: i128 = 0;
    let (mut up, mut down) = (cursor, cursor);
    let mut extra = 0;
    while up > lines.first && extra < scrolloff && down < lines.last {
        up -= 1;
        used += lines.of(up);
        if used > height {
            break;
        }
        down += 1;
        used += lines.of(down);
        if used > height {
            break;
        }
        extra += lines.of(down);
        scrolled += i128::from(lines.of(down));
        if down == past {
            scrolled -= i128::from(empty);
        }
    }
    let by = if scrolled <= 0 {
        0
    } else if used > height {
        used
    } else {
        // Whole lines from the top, enough to scroll that many rows.
        let (mut rows, mut by, mut line) = (0, 0, top);
        while rows < scrolled && line < past {
            rows += i128::from(lines.of(line));
            by += 1;
            line += 1;
        }
        if rows < scrolled { u64::MAX } else { by }
    };
    if by >= height {
        halfway(cursor, height, false, true, lines)
    } else {
        top + by
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sixty lines from 0, the even ones (odd counting from 1) `long` rows.
    fn lines(long: u64) -> Lines<impl Fn(u64) -> u64> {
        Lines {
            rows: move |line| if line % 2 == 0 { long } else { 1 },
            first: 0,
            last: 59,
        }
    }

    // Each checked against Neovim 0.11 (`crates/gui/tests/nvim/check.lua`),
    // lines counted from 1 there.

    #[test]
    fn lines_a_row_each_go_where_they_always_have() {
        let one = lines(1);
        assert_eq!(view_top(ViewAt::Top, 29, 10, 10, 0, &one), 29);
        assert_eq!(view_top(ViewAt::Middle, 29, 10, 10, 0, &one), 25);
        assert_eq!(view_top(ViewAt::Bottom, 29, 10, 10, 0, &one), 20);
        // `scrolloff` lines above zt, below zb, even at the text's end.
        assert_eq!(view_top(ViewAt::Top, 29, 10, 10, 3, &one), 26);
        assert_eq!(view_top(ViewAt::Top, 59, 10, 10, 3, &one), 56);
        assert_eq!(view_top(ViewAt::Bottom, 29, 10, 10, 3, &one), 23);
    }

    #[test]
    fn where_lines_wrap_the_view_starts_at_a_lines_start() {
        let two = lines(2);
        // `zz` on line 14 of ten rows: line 12, as 11 doesn't fit.
        assert_eq!(view_top(ViewAt::Middle, 13, 10, 10, 0, &two), 11);
        // In nine rows from line 4: line 2.
        assert_eq!(view_top(ViewAt::Middle, 3, 0, 9, 0, &two), 1);
        // `zb` on line 57 of three rows, in nine: line 54.
        let three = lines(3);
        assert_eq!(view_top(ViewAt::Bottom, 56, 56, 9, 0, &three), 53);
    }
}
