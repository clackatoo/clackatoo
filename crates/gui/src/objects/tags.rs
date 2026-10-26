//! `it` and `at`: the HTML or XML tags around the cursor and what's
//! between them, a port of Vim's `current_tagblock()`, read through
//! [`Steps`] as the C reads `pos_T`s. Vim finds tags with `searchpair()`
//! and patterns; here they're matched by hand, as the patterns match:
//!
//! - any opening tag going back, `<[^ \t>/!]\+\%(\_s\_[^>]\{-}[^/]>\|$\|\_s\=>\)`,
//!   paired with any closing tag, `</[^>]*>`;
//! - the opening tag's own name going on, `<name\>\%(\_s\_[^>]\{-}\_[^/]>\|\_s\?>\)`,
//!   paired with `</name>`, either case.
//!
//! A line's end is a `\n` to them. Each search looks at no more than
//! [`SEARCH`] places, so a file of no tags costs a bounded look.

use crate::motion::steps::{NUL, Step, Steps, white};
use crate::motion::{Lines, Point, Words};

/// How many places a search for a tag looks at, at most.
const SEARCH: usize = 1_000_000;

/// The character at `at` as the patterns read it: a line's end a `\n`.
fn ch<L: Lines>(steps: &Steps<'_, L>, at: Point) -> char {
    match steps.char(at) {
        NUL => '\n',
        other => other,
    }
}

/// Whether `ch` is whitespace or a line's end, as `\_s` matches.
fn space(ch: char) -> bool {
    white(ch) || ch == '\n'
}

/// After what's matched up to `at`, the rest of an opening tag: `\_s`,
/// then up to the first `>`, with something before it that's no `/`, nor
/// a line's end unless `breaks`; or with `bare`, `\_s\=>` or `\_s\?>`, or
/// a line's end, `$`, with `eol`.
fn opening_rest<L: Lines>(steps: &Steps<'_, L>, at: Point, breaks: bool, eol: bool) -> bool {
    let first = ch(steps, at);
    if first == '>' || (eol && first == '\n') {
        return true;
    }
    if !space(first) {
        return false;
    }
    let mut pos = at;
    if steps.inc(&mut pos).stuck() {
        return false;
    }
    if ch(steps, pos) == '>' {
        return true;
    }
    // On to the first `>`, at least one character after the blank.
    let mut before = ch(steps, pos);
    for _ in 0..SEARCH {
        if steps.inc(&mut pos).stuck() {
            return false;
        }
        let here = ch(steps, pos);
        if here == '>' {
            return before != '/' && (breaks || before != '\n');
        }
        before = here;
    }
    false
}

/// Whether an opening tag of any name starts at `at`.
fn opens_any<L: Lines>(steps: &Steps<'_, L>, at: Point) -> bool {
    if ch(steps, at) != '<' {
        return false;
    }
    let mut pos = at;
    let mut named = false;
    loop {
        if steps.inc(&mut pos) != Step::Along {
            // A line's end, or the text's, right after the name: `$`.
            return named && ch(steps, pos) == '\n';
        }
        let here = ch(steps, pos);
        if matches!(here, ' ' | '\t' | '>' | '/' | '!' | '\n') {
            break;
        }
        named = true;
    }
    named && opening_rest(steps, pos, false, true)
}

/// Whether a closing tag of any name starts at `at`: `</`, then up to a
/// `>` on the line.
fn closes_any<L: Lines>(steps: &Steps<'_, L>, at: Point) -> bool {
    let mut pos = at;
    if ch(steps, pos) != '<' || steps.inc(&mut pos) != Step::Along || ch(steps, pos) != '/' {
        return false;
    }
    while steps.inc(&mut pos) == Step::Along {
        if ch(steps, pos) == '>' {
            return true;
        }
    }
    false
}

/// Whether `name` follows `at`, either case, and returns where after it.
fn named<L: Lines>(steps: &Steps<'_, L>, at: Point, name: &[char]) -> Option<Point> {
    let mut pos = at;
    for &wanted in name {
        if steps.inc(&mut pos) != Step::Along {
            return None;
        }
        let here = ch(steps, pos);
        if !here.to_lowercase().eq(wanted.to_lowercase()) {
            return None;
        }
    }
    steps.inc(&mut pos);
    Some(pos)
}

/// Whether an opening tag named `name` starts at `at`, `name` a whole
/// word, as `\>` has it, by the words of `words`.
fn opens<L: Lines>(steps: &Steps<'_, L>, at: Point, name: &[char], words: Words) -> bool {
    if ch(steps, at) != '<' {
        return false;
    }
    let Some(after) = named(steps, at, name) else {
        return false;
    };
    // The end of a word: a word's character before, and none after.
    let ends_word =
        name.last().is_some_and(|&last| words.is_word(last)) && !words.is_word(ch(steps, after));
    ends_word && opening_rest(steps, after, true, false)
}

/// Whether `</name>` starts at `at`, either case.
fn closes<L: Lines>(steps: &Steps<'_, L>, at: Point, name: &[char]) -> bool {
    let mut pos = at;
    if ch(steps, pos) != '<' || steps.inc(&mut pos) != Step::Along || ch(steps, pos) != '/' {
        return false;
    }
    named(steps, pos, name).is_some_and(|after| ch(steps, after) == '>')
}

/// The opening tag before `from` that no closing tag between closes, as
/// Vim's `searchpair()` back with any tag's patterns finds it.
fn opening_before<L: Lines>(steps: &Steps<'_, L>, from: Point) -> Option<Point> {
    let mut pos = from;
    let mut nest = 0usize;
    for _ in 0..SEARCH {
        if steps.dec(&mut pos).stuck() {
            return None;
        }
        if opens_any(steps, pos) {
            match nest.checked_sub(1) {
                Some(fewer) => nest = fewer,
                None => return Some(pos),
            }
        } else if closes_any(steps, pos) {
            nest += 1;
        }
    }
    None
}

/// The closing tag named `name` after `from` that pairs with the opening
/// one before it, as `searchpair()` on finds it.
fn closing_after<L: Lines>(steps: &Steps<'_, L>, from: Point, name: &[char]) -> Option<Point> {
    let words = steps.lines().words();
    let mut pos = from;
    let mut nest = 0usize;
    for _ in 0..SEARCH {
        if steps.inc(&mut pos).stuck() {
            return None;
        }
        if opens(steps, pos, name, words) {
            nest += 1;
        } else if closes(steps, pos, name) {
            match nest.checked_sub(1) {
                Some(fewer) => nest = fewer,
                None => return Some(pos),
            }
        }
    }
    None
}

/// Whether the cursor at `at` is in a tag, as Vim's `in_html_tag()`: a
/// closing one if `closing`, or else an opening one that doesn't close
/// itself, as `<br/>` does.
fn in_tag<L: Lines>(steps: &Steps<'_, L>, at: Point, closing: bool) -> bool {
    // Back to a `<` under or before the cursor, stopping at a `>`.
    let mut p = at;
    while p.col > 0 {
        if steps.char(p) == '<' {
            break;
        }
        let mut back = p;
        steps.dec(&mut back);
        p = back;
        if steps.char(p) == '>' {
            break;
        }
    }
    if steps.char(p) != '<' {
        return false;
    }
    let mut after = p;
    steps.inc(&mut after);
    let slash = after.line == p.line && steps.char(after) == '/';
    if closing {
        return slash;
    }
    if slash {
        return false;
    }
    let mut pos = p;
    let mut last = NUL;
    for _ in 0..SEARCH {
        if steps.inc(&mut pos).stuck() {
            return false;
        }
        let here = steps.char(pos);
        if here == '>' {
            return last != '/';
        }
        last = here;
    }
    false
}

/// Whether `at` is in its line's indent, as Vim's `inindent(1)`.
fn in_indent<L: Lines>(steps: &Steps<'_, L>, at: Point) -> bool {
    let mut col = Point { col: 0, ..at };
    while col.col <= at.col && white(steps.char(col)) {
        if col.col == at.col {
            return true;
        }
        if steps.inc(&mut col) != Step::Along {
            return false;
        }
    }
    false
}

/// With the cursor at `cursor` on a tag, past its line's indent, to where
/// its block is found from, as `current_tagblock()` starts: an opening
/// tag's `>`, or just before a closing tag, which returns true, as that's
/// the end found so far too.
fn on_tag<L: Lines>(steps: &Steps<'_, L>, cursor: &mut Point) -> bool {
    while in_indent(steps, *cursor) {
        if steps.inc(cursor) != Step::Along {
            break;
        }
    }
    if in_tag(steps, *cursor, false) {
        while steps.char(*cursor) != '>' {
            if steps.inc(cursor).stuck() {
                break;
            }
        }
        return false;
    }
    if !in_tag(steps, *cursor, true) {
        return false;
    }
    while steps.char(*cursor) != '<' {
        if steps.dec(cursor).stuck() {
            break;
        }
    }
    steps.dec(cursor);
    true
}

/// What `it` or `at` (`include`) finds, `count` tags out, from the cursor
/// at `cursor`, in visual mode from `visual` if it's on, as Vim's
/// `current_tagblock()`: where it starts and ends, and whether the end is
/// in it; `None` where Vim's fails.
fn tag_block<L: Lines>(
    steps: &Steps<'_, L>,
    mut cursor: Point,
    visual: Option<Point>,
    include: bool,
    count: usize,
) -> Option<(Point, Point, bool)> {
    let mut old_end = cursor;
    let mut old_start = cursor;
    if visual.is_none() {
        steps.decl(&mut old_end);
    }
    match visual {
        Some(anchor) if anchor < cursor => {
            old_start = anchor;
            cursor = anchor;
        }
        Some(anchor) if anchor > cursor => old_end = anchor,
        _ => {
            if on_tag(steps, &mut cursor) {
                old_end = cursor;
            }
        }
    }
    let mut include = include;
    let count_arg = count.max(1);
    let mut count = count_arg;
    loop {
        // Back to the unclosed opening tag, `count` out.
        for _ in 0..count {
            cursor = opening_before(steps, cursor)?;
        }
        let mut start = cursor;
        // On to the closing tag of its name.
        steps.inc(&mut cursor);
        let mut name = Vec::new();
        let mut at = cursor;
        loop {
            let here = steps.char(at);
            if here == NUL || here == '>' || white(here) {
                break;
            }
            name.push(here);
            if steps.inc(&mut at) != Step::Along {
                break;
            }
        }
        if name.is_empty() {
            return None;
        }
        if let Some(end) = closing_after(steps, cursor, &name).filter(|&end| end >= old_end) {
            cursor = end;
        } else {
            // A tag with no end, or one that ends before the last found:
            // another opening tag before it.
            count = 1;
            cursor = start;
            continue;
        }
        let mut inclusive = true;
        if include {
            // Up to the `>`.
            while steps.char(cursor) != '>' {
                if steps.inc(&mut cursor).stuck() {
                    break;
                }
            }
        } else if steps.char(cursor) == '<' {
            // Not the closing tag's `<`; on a line of its own, the line
            // break before it, an exclusive end.
            if visual.is_none() && cursor.col == 0 {
                inclusive = false;
            } else {
                steps.dec(&mut cursor);
            }
        }
        let end = cursor;
        if !include {
            // Not the opening tag, past a `>` in quotes.
            let mut quoted = false;
            cursor = start;
            while !steps.inc(&mut cursor).stuck() {
                match steps.char(cursor) {
                    '>' if !quoted => {
                        steps.inc(&mut cursor);
                        start = cursor;
                        break;
                    }
                    '"' | '\'' => quoted = !quoted,
                    _ => {}
                }
            }
            // The same as selected already: the tags too, afresh.
            if visual.is_some() && start == old_start && end == old_end {
                include = true;
                cursor = old_start;
                count = count_arg;
                continue;
            }
        }
        return Some((start, end, inclusive));
    }
}

/// `count` tag blocks out from `at`, as an operator takes `it` or `at`
/// (`include`): where it starts and ends, and whether the end is in it.
/// Between tags with nothing between, an empty stretch at the start.
pub(super) fn current<L: Lines>(
    lines: &L,
    at: Point,
    include: bool,
    count: usize,
) -> Option<(Point, Point, bool)> {
    let steps = Steps::new(lines);
    let (start, end, inclusive) = tag_block(&steps, at, None, include, count)?;
    if end < start {
        return Some((start, start, false));
    }
    // An end at a line's start isn't moved to the line before's end, as
    // Vim's `nv_object()` keeps it for tags: the line break before is
    // taken, as an inclusive end on it.
    if !inclusive && end.col == 0 && end.line > start.line {
        let line = end.line - 1;
        return Some((
            start,
            Point {
                line,
                col: steps.end(line),
            },
            true,
        ));
    }
    Some((start, end, inclusive))
}

/// The same, in visual mode, from `anchor` to `cursor`: what's selected,
/// the anchor then the cursor.
pub(super) fn select<L: Lines>(
    lines: &L,
    anchor: Point,
    cursor: Point,
    include: bool,
    count: usize,
) -> Option<(Point, Point)> {
    let steps = Steps::new(lines);
    let (start, end, _) = tag_block(&steps, cursor, Some(anchor), include, count)?;
    Some((start, if end < start { start } else { end }))
}
