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
//! A line's end is a `\n` to them. All of `it` or `at`, every search and
//! every try again, takes no more than [`WORK`] steps between them:
//! past that it finds nothing, as Vim's does when there's no tag, so no
//! text, however many tags or `<`s it has, freezes the window.

use crate::motion::steps::{NUL, Step, Steps, white};
use crate::motion::{Lines, Point, Words};

/// How many steps through the text all of `it` or `at` takes, at most.
pub(super) const WORK: usize = 1_000_000;

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
    loop {
        if steps.inc(&mut pos).stuck() {
            return false;
        }
        let here = ch(steps, pos);
        if here == '>' {
            return before != '/' && (breaks || before != '\n');
        }
        before = here;
    }
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
    loop {
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
}

/// How many steps finding the last closing tags of all names takes, at
/// most: a budget of their own, so a tag near the top of a long file,
/// whose closing tag is the only one of its name, is still found.
const LAST_CLOSING_WORK: usize = 8 * WORK;

/// Where the last closing tag of each name is, as it's asked for: the
/// text's last `</name>`, found back from its end once a name, `None`
/// if it has none. A tag with no end, as `<br>` or `<img>`, is then known
/// to have none at once, each time its kind is tried again.
struct LastClosing<'a, L: Lines> {
    steps: Steps<'a, L>,
    known: std::cell::RefCell<std::collections::HashMap<Vec<char>, Option<Point>>>,
}

impl<'a, L: Lines> LastClosing<'a, L> {
    fn new(lines: &'a L) -> Self {
        Self {
            steps: Steps::budgeted(lines, LAST_CLOSING_WORK),
            known: std::cell::RefCell::default(),
        }
    }

    /// The text's last `</name>`, either case, found back from its end.
    fn last_of(&self, name: &[char]) -> Option<Point> {
        let steps = &self.steps;
        let last = steps.lines().bounds().1;
        let mut pos = Point {
            line: last,
            col: steps.end(last),
        };
        loop {
            if steps.dec(&mut pos).stuck() {
                return None;
            }
            if closes(steps, pos, name) {
                return Some(pos);
            }
        }
    }

    /// Whether the text has a `</name>`, either case, after `from`: `None`
    /// if that's not known, as looking for names has used its budget.
    fn any_after(&self, name: &[char], from: Point) -> Option<bool> {
        let key: Vec<char> = name.iter().flat_map(|ch| ch.to_lowercase()).collect();
        let cached = self.known.borrow().get(&key).copied();
        let found = if let Some(found) = cached {
            found
        } else {
            let found = self.last_of(name);
            if self.steps.ran_out() {
                return None;
            }
            self.known.borrow_mut().insert(key, found);
            found
        };
        Some(found.is_some_and(|last| last > from))
    }
}

/// The closing tag named `name` after `from` that pairs with the opening
/// one before it, as `searchpair()` on finds it: none at once if the
/// text's last of its name is before `from`.
fn closing_after<L: Lines>(
    steps: &Steps<'_, L>,
    from: Point,
    name: &[char],
    known: &LastClosing<'_, L>,
) -> Option<Point> {
    if known.any_after(name, from) == Some(false) {
        return None;
    }
    let words = steps.lines().words();
    let mut pos = from;
    let mut nest = 0usize;
    loop {
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
        if steps.dec(&mut p).stuck() {
            break;
        }
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
    loop {
        if steps.inc(&mut pos).stuck() {
            return false;
        }
        let here = steps.char(pos);
        if here == '>' {
            return last != '/';
        }
        last = here;
    }
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
    let known = LastClosing::new(steps.lines());
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
        if let Some(end) = closing_after(steps, cursor, &name, &known).filter(|&end| end >= old_end)
        {
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
    current_in(&Steps::budgeted(lines, WORK), at, include, count)
}

/// [`current`], stepping with `steps`.
fn current_in<L: Lines>(
    steps: &Steps<'_, L>,
    at: Point,
    include: bool,
    count: usize,
) -> Option<(Point, Point, bool)> {
    let found = tag_block(steps, at, None, include, count);
    // Stopped short of looking, what it found is nothing to go by.
    let (start, end, inclusive) = found.filter(|_| !steps.ran_out())?;
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
    let steps = Steps::budgeted(lines, WORK);
    let found = tag_block(&steps, cursor, Some(anchor), include, count);
    let (start, end, _) = found.filter(|_| !steps.ran_out())?;
    Some((start, if end < start { start } else { end }))
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{Row, RowBuilder, Style};

    use super::*;

    /// Lines of text, each a row as wide as it is.
    struct Plain(Vec<Row>);

    impl Plain {
        fn new(text: &str) -> Self {
            Self(
                text.split('\n')
                    .map(|line| {
                        let mut builder = RowBuilder::new(line.len().max(1));
                        builder.push(line, &Style::DEFAULT, false);
                        builder.finish(false)
                    })
                    .collect(),
            )
        }
    }

    impl Lines for Plain {
        type Line = Row;
        type Row<'a> = &'a Row;

        fn row(&self, line: u64) -> Option<&Row> {
            self.0.get(usize::try_from(line).ok()?)
        }

        fn bounds(&self) -> (u64, u64) {
            (0, self.0.len() as u64 - 1)
        }
    }

    /// The steps `it` takes from the last line of `text`, and what it
    /// finds.
    fn work(text: &str) -> (usize, Option<(Point, Point, bool)>) {
        let lines = Plain::new(text);
        let steps = Steps::budgeted(&lines, WORK);
        let last = lines.bounds().1;
        let found = current_in(&steps, Point { line: last, col: 1 }, false, 1);
        (steps.asked(), found)
    }

    /// What `it`, or `at` with `include`, finds from `at` in `text`
    /// within a budget of `budget` steps.
    fn within(text: &str, at: Point, include: bool, budget: usize) -> Option<(Point, Point, bool)> {
        let lines = Plain::new(text);
        current_in(&Steps::budgeted(&lines, budget), at, include, 1)
    }

    #[test]
    fn a_budget_run_out_part_way_finds_nothing_rather_than_a_wrong_block() {
        let cases = [
            ("<div class=\"x\">hello</div>", Point { line: 0, col: 17 }),
            (
                "<div>\n  <p>hi</p>\nhello\n</div>",
                Point { line: 2, col: 2 },
            ),
        ];
        for (text, at) in cases {
            for include in [false, true] {
                let whole = within(text, at, include, WORK);
                assert!(whole.is_some(), "{text:?}");
                for budget in 0..2000 {
                    let found = within(text, at, include, budget);
                    assert!(
                        found.is_none() || found == whole,
                        "{text:?} at {budget}: {found:?}, not {whole:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_long_attribute_is_found_through_or_not_at_all() {
        let data = vec!["x".repeat(50_000); 6].join("\n");
        let text = format!("<div data=\"{data}\">\nhello\n</div>");
        let found = within(&text, Point { line: 7, col: 2 }, false, WORK);
        let inside = (
            Point {
                line: 6,
                col: 50_002,
            },
            Point { line: 7, col: 5 },
            true,
        );
        assert!(found.is_none() || found == Some(inside), "{found:?}");
    }

    #[test]
    fn tags_with_no_end_cost_a_look_each_kind_not_each_time() {
        let mut text = String::from("<div>\n");
        text.push_str(&"<img src=\"a.png\"><br>\n".repeat(5));
        text.push_str("hello\n");
        // About 83 KB after it, each tag with no end a look to the end.
        text.push_str(&"<span class=\"a long attribute, as pages have\">\n".repeat(2000));
        text.push_str("</div>");
        let lines = Plain::new(&text);
        let steps = Steps::budgeted(&lines, WORK);
        let found = current_in(&steps, Point { line: 6, col: 2 }, true, 1);
        assert_eq!(
            found.map(|(start, _, _)| start),
            Some(Point { line: 0, col: 0 })
        );
        assert!(steps.asked() < WORK / 2, "{}", steps.asked());
    }

    #[test]
    fn the_way_in_is_budgeted() {
        // Every `<` an opening tag named for the rest of the line.
        let text = format!("{}\nx", "<".repeat(5000));
        let lines = Plain::new(&text);
        let before = crate::motion::steps::STEPS_ASKED.with(std::cell::Cell::get);
        assert_eq!(current(&lines, Point { line: 1, col: 0 }, false, 1), None);
        let at = Point { line: 0, col: 4999 };
        assert_eq!(select(&lines, at, at, false, 1), None);
        let asked = crate::motion::steps::STEPS_ASKED.with(std::cell::Cell::get) - before;
        assert!(asked <= 2 * (WORK + LAST_CLOSING_WORK) + 200, "{asked}");
    }

    #[test]
    fn looking_back_for_a_tag_stops_when_the_budget_does() {
        let lines = Plain::new("abcdefgh");
        let steps = Steps::budgeted(&lines, 2);
        assert!(!in_tag(&steps, Point { line: 0, col: 5 }, false));
    }

    #[test]
    fn no_text_takes_more_than_the_budget_however_many_tags_or_angles() {
        // Each `<br>` an opening tag with no end, tried again and again.
        let (asked, found) = work(&"<br>\n".repeat(2000));
        assert_eq!(found, None, "nothing found, as Vim's");
        assert!(asked <= WORK + 100, "{asked}");
        let (asked, found) = work(&format!("{}\nx", "<".repeat(1000)));
        assert_eq!(found, None);
        assert!(asked <= WORK + 100, "{asked}");
        // A tag found takes what finding it does.
        let (asked, found) = work("<p>\na\n</p>");
        assert!(found.is_some());
        assert!(asked < 100, "{asked}");
    }
}
