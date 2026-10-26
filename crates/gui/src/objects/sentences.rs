//! `is` and `as`: the sentences around the cursor, a port of Vim's
//! `current_sent()`, read through [`Steps`] as the C reads `pos_T`s (see
//! [`crate::motion::sentences`] for what a sentence is).

use crate::motion::sentences::{find, first_blank, forward_by};
use crate::motion::steps::{NUL, Steps, white};
use crate::motion::{Lines, Point};

/// `count` sentences at `at`, and with `include`, the blanks after them,
/// or before them if there are none after, as Vim's `current_sent()`
/// takes them outside visual mode: where they start and end, and whether
/// the end is in them.
pub(super) fn current<L: Lines>(
    lines: &L,
    at: Point,
    include: bool,
    count: usize,
) -> (Point, Point, bool) {
    let steps = Steps::new(lines);
    let (start, mut cursor) = around(&steps, at, include, count);
    // A line break after it, if there is one, as Vim's: an end just past
    // it, or with none, its last character in it.
    let inclusive = steps.incl(&mut cursor).stuck();
    if inclusive && cursor.col > 0 && steps.char(cursor) == NUL {
        steps.dec(&mut cursor);
    }
    (start, cursor, inclusive)
}

/// What's selected once `count` sentences are, in visual mode, from
/// `anchor` to `cursor`: the sentences around the cursor if only a
/// character's selected, or else the selection taken on by `count`, as
/// Vim's `current_sent()` goes. The anchor, the cursor, and whether the
/// selection was taken on, keeping its kind, rather than made afresh, by
/// characters.
pub(super) fn select<L: Lines>(
    lines: &L,
    anchor: Point,
    cursor: Point,
    include: bool,
    count: usize,
) -> (Point, Point, bool) {
    let steps = Steps::new(lines);
    if anchor != cursor {
        let next = find(&steps, cursor, true, 1).unwrap_or(cursor);
        let cursor = extend(&steps, (anchor, cursor, cursor), next, include, count);
        return (anchor, cursor, true);
    }
    let (start, end) = around(&steps, cursor, include, count);
    if start == end {
        // As Vim's, rather than be stuck with `is` on a blank before a
        // sentence: taken on from there.
        let pos = past_blanks(&steps, cursor);
        let cursor = extend(&steps, (anchor, start, pos), end, include, count);
        return (anchor, cursor, true);
    }
    (start, end, false)
}

/// Where the blanks from `at` end, as Vim's `while (ascii_iswhite(c))
/// incl(&pos)`.
fn past_blanks<L: Lines>(steps: &Steps<'_, L>, at: Point) -> Point {
    let mut pos = at;
    while white(steps.char(pos)) {
        if steps.incl(&mut pos).stuck() {
            break;
        }
    }
    pos
}

/// The start and last character of `count` sentences at `at`, and with
/// `include` the blanks after or before, as `current_sent()` finds them
/// before it either selects them or has an operator take them.
fn around<L: Lines>(
    steps: &Steps<'_, L>,
    at: Point,
    include: bool,
    count: usize,
) -> (Point, Point) {
    let mut cursor = find(steps, at, true, 1).unwrap_or(at);
    // On a blank just before the next sentence, or in a sentence.
    let start_blank = past_blanks(steps, at) == cursor;
    let mut start = at;
    if start_blank {
        first_blank(steps, &mut start);
    } else {
        cursor = find(steps, cursor, false, 1).unwrap_or(cursor);
        start = cursor;
    }
    let ncount = if include {
        count.saturating_mul(2)
    } else {
        count - usize::from(start_blank)
    };
    if ncount > 0 {
        forward_by(steps, &mut cursor, ncount, true);
    } else {
        steps.decl(&mut cursor);
    }
    if include {
        // With the blanks before it, not those at its end; with none at
        // its end, those before it.
        if start_blank {
            first_blank(steps, &mut cursor);
            if white(steps.char(cursor)) {
                steps.decl(&mut cursor);
            }
        } else if !white(steps.char(cursor)) {
            first_blank(steps, &mut start);
        }
    }
    (start, cursor)
}

/// The cursor of a selection from `anchor` to the cursor, `start` where
/// it was and `pos` as far as Vim's had read, taken on by `count`
/// sentences, or with `include` and their blanks, as `current_sent()`
/// extends one; `cursor` is where the cursor is now, at the next
/// sentence's start.
fn extend<L: Lines>(
    steps: &Steps<'_, L>,
    (anchor, start, mut pos): (Point, Point, Point),
    mut cursor: Point,
    include: bool,
    count: usize,
) -> Point {
    let count = if include {
        count.saturating_mul(2)
    } else {
        count
    };
    if start < anchor {
        // The cursor at the selection's start: in the blanks before a
        // sentence, in one or just after it, or at its start.
        let mut at_start = true;
        steps.decl(&mut pos);
        while pos < cursor {
            if !white(steps.char(pos)) {
                at_start = false;
                break;
            }
            if steps.incl(&mut pos).stuck() {
                break;
            }
        }
        if !at_start {
            cursor = find(steps, cursor, false, 1).unwrap_or(cursor);
            if cursor == start {
                at_start = true;
            } else {
                cursor = find(steps, cursor, true, 1).unwrap_or(cursor);
            }
        }
        back_by(steps, &mut cursor, count, (at_start, include));
        return cursor;
    }
    // The cursor at its end: just before a sentence, before or in the
    // blanks before one, or in one.
    steps.incl(&mut pos);
    let mut at_start = true;
    if pos != cursor {
        at_start = false;
        while pos < cursor {
            if !white(steps.char(pos)) {
                at_start = true;
                break;
            }
            if steps.incl(&mut pos).stuck() {
                break;
            }
        }
        if at_start {
            cursor = find(steps, cursor, false, 1).unwrap_or(cursor);
        } else {
            cursor = start;
        }
    }
    forward_by(steps, &mut cursor, count, at_start);
    cursor
}

/// Back `count` sentences and blanks, halved, from `cursor`, at a
/// sentence's start if `at_start`, blanks taken only with `include`, as
/// `current_sent()` takes a selection on backwards.
fn back_by<L: Lines>(
    steps: &Steps<'_, L>,
    cursor: &mut Point,
    count: usize,
    (mut at_start, include): (bool, bool),
) {
    let mut left = count;
    let mut seen: [Option<(Point, bool)>; 2] = [None, None];
    while left > 0 {
        // Gone round to where it was two steps ago, it'll only go round
        // again: what's left over is what matters.
        let state = (*cursor, at_start);
        if seen[1] == Some(state) && left > 2 {
            left = 2 + left % 2;
        }
        seen = [Some(state), seen[0]];
        left -= 1;
        if at_start {
            first_blank(steps, cursor);
        }
        if !at_start || (!include && !white(steps.char(*cursor))) {
            *cursor = find(steps, *cursor, false, 1).unwrap_or(*cursor);
        }
        at_start = !at_start;
    }
}
