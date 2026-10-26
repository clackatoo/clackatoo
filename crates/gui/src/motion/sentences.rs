//! Sentences, as Vim's `(` and `)` go between them and `is` and `as` take
//! them: ports of `findsent()` and its helpers in Vim's `textobject.c`,
//! read through [`Steps`] as the C reads `pos_T`s.
//!
//! A sentence ends at a `.`, `!` or `?`, with any of `)`, `]`, `"` and
//! `'` after it, before a line's end, a space or a tab; a paragraph's
//! start and end, an empty line, are sentences' ends too (see Vim's
//! `:help sentence`). As Vim's with `cpoptions` without `J`, one space
//! after the `.` is enough. Paragraphs start only at empty lines and form
//! feeds, not nroff's macros.

use super::steps::{NUL, Step, Steps, white};
use super::{Lines, Point};

/// Whether line `line` starts a paragraph or a section, as Vim's
/// `startPS()` with no macros: an empty line, or a form feed's.
fn starts_paragraph<L: Lines>(steps: &Steps<'_, L>, line: u64) -> bool {
    let first = steps.char(Point { line, col: 0 });
    first == NUL || first == '\x0c'
}

/// Whether `ch` ends a sentence.
fn ends(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?')
}

/// Whether `ch` may follow what ends a sentence, in it.
fn closes(ch: char) -> bool {
    matches!(ch, ')' | ']' | '"' | '\'')
}

/// Where the start of the `count`th sentence on from `at` is, or back
/// from it if not `forward`, as Vim's `findsent()` finds it: `None` where
/// Vim's fails, going nowhere.
pub(crate) fn find<L: Lines>(
    steps: &Steps<'_, L>,
    at: Point,
    forward: bool,
    count: usize,
) -> Option<Point> {
    let step = |pos: &mut Point| {
        if forward {
            steps.incl(pos)
        } else {
            steps.decl(pos)
        }
    };
    let last = steps.lines().bounds().1;
    let mut pos = at;
    let mut noskip = false;
    let mut count = count.max(1);
    while count > 0 {
        count -= 1;
        let prev = pos;
        'found: {
            if steps.char(pos) == NUL {
                // On an empty line: on to one that isn't.
                loop {
                    if step(&mut pos).stuck() || steps.char(pos) != NUL {
                        break;
                    }
                }
                if forward {
                    break 'found;
                }
            } else if forward && pos.col == 0 && starts_paragraph(steps, pos.line) {
                // At a paragraph's start going on: the next line.
                if pos.line == last {
                    return None;
                }
                pos = Point {
                    line: pos.line + 1,
                    col: 0,
                };
                break 'found;
            } else if !forward {
                steps.decl(&mut pos);
            }
            back_over_end(steps, &mut pos, forward);
            // On to the sentence's end.
            let start_line = pos.line;
            loop {
                let ch = steps.char(pos);
                if ch == NUL || (pos.col == 0 && starts_paragraph(steps, pos.line)) {
                    if !forward && pos.line != start_line {
                        pos.line += 1;
                    }
                    break;
                }
                if let Some(after) = ending(steps, pos) {
                    pos = after;
                    break;
                }
                if step(&mut pos).stuck() {
                    if count > 0 {
                        return None;
                    }
                    noskip = true;
                    break;
                }
            }
        }
        // Over blanks.
        while !noskip && white(steps.char(pos)) {
            if steps.incl(&mut pos).stuck() {
                break;
            }
        }
        if pos == prev {
            // Nowhere yet: a character on, and again.
            if step(&mut pos).stuck() {
                if count > 0 {
                    return None;
                }
                break;
            }
            count += 1;
        }
    }
    Some(pos)
}

/// Back from `pos` over blanks and what ends a sentence to the last
/// character that's neither, as `findsent()` goes before it looks for
/// the sentence's end: over one `.`, `!` or `?` at most, and not into an
/// empty line going on.
fn back_over_end<L: Lines>(steps: &Steps<'_, L>, pos: &mut Point, forward: bool) {
    let mut found_dot = false;
    loop {
        let ch = steps.char(*pos);
        if !(white(ch) || ends(ch) || closes(ch)) {
            return;
        }
        let mut before = *pos;
        if steps.decl(&mut before).stuck() || (steps.empty(before.line) && forward) {
            return;
        }
        if found_dot {
            return;
        }
        if ends(ch) {
            found_dot = true;
        }
        let before_ch = steps.char(before);
        if closes(ch) && !(ends(before_ch) || closes(before_ch)) {
            return;
        }
        steps.decl(pos);
    }
}

/// Where what follows a sentence ending at `pos` starts, if one ends
/// there: a `.`, `!` or `?`, any of `)` `]` `"` `'`, then a blank, a
/// line's end or the text's; past a line's end, the next line's start.
fn ending<L: Lines>(steps: &Steps<'_, L>, pos: Point) -> Option<Point> {
    if !ends(steps.char(pos)) {
        return None;
    }
    let mut after = pos;
    let next = loop {
        if steps.inc(&mut after).stuck() {
            break None;
        }
        let next = steps.char(after);
        if !closes(next) {
            break Some(next);
        }
    };
    if !next.is_none_or(|next| white(next) || next == NUL) {
        return None;
    }
    if steps.char(after) == NUL {
        steps.inc(&mut after);
    }
    Some(after)
}

/// Back from `at` over blanks to the first of them, as Vim's
/// `find_first_blank()`: where it is if it's on none.
pub(crate) fn first_blank<L: Lines>(steps: &Steps<'_, L>, at: &mut Point) {
    while steps.decl(at) != Step::Stuck {
        if !white(steps.char(*at)) {
            steps.incl(at);
            break;
        }
    }
}

/// `count` sentences and as many blanks between them, halved, as Vim's
/// `findsent_forward()` goes, from `at`, at a sentence's start if
/// `at_start`.
pub(crate) fn forward_by<L: Lines>(
    steps: &Steps<'_, L>,
    at: &mut Point,
    count: usize,
    mut at_start: bool,
) {
    let mut left = count;
    let mut seen: [Option<(Point, bool)>; 2] = [None, None];
    while left > 0 {
        // Gone round to where it was two steps ago, as at the text's end,
        // it'll only go round again: what's left over is what matters, so
        // a count as big as there is costs no more than the text.
        let state = (*at, at_start);
        if seen[1] == Some(state) && left > 2 {
            left = 2 + left % 2;
        }
        seen = [Some(state), seen[0]];
        left -= 1;
        if let Some(found) = find(steps, *at, true, 1) {
            *at = found;
        }
        if at_start {
            first_blank(steps, at);
        }
        if left == 0 || at_start {
            steps.decl(at);
        }
        at_start = !at_start;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::motion::tests::Text;

    /// `Text` that counts the rows asked for.
    struct Counted(Text, Cell<usize>);

    impl Lines for Counted {
        type Line = clackatoo_term::Row;
        type Row<'a> = &'a clackatoo_term::Row;

        fn row(&self, line: u64) -> Option<&clackatoo_term::Row> {
            self.1.set(self.1.get() + 1);
            self.0.row(line)
        }

        fn bounds(&self) -> (u64, u64) {
            self.0.bounds()
        }
    }

    #[test]
    fn a_sentence_over_many_lines_asks_for_each_line_once() {
        let lines = vec!["no end on this line"; 1000];
        let text = Counted(Text::new(&lines), Cell::new(0));
        let steps = Steps::new(&text);
        let found = find(&steps, Point { line: 0, col: 0 }, true, 1);
        assert_eq!(found.map(|at| at.line), Some(999), "to the text's end");
        assert!(text.1.get() <= 1000 + 2, "{} rows asked for", text.1.get());
    }
}
