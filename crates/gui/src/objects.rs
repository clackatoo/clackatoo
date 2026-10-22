//! Text objects: the pieces of text `i` and `a` pick out after `y`, or in
//! visual mode, as in Vim. `iw` is the word under the cursor and `aw` the
//! word with the blanks after it, a count that many, as Vim's
//! `current_word()` takes them (see [`words`]); `i"` is what's between
//! the quotes around the cursor and `a"` the quotes too; `i(` is what's
//! between the parentheses around it, across lines, and `a(` the
//! parentheses too.
//!
//! In Vim `b` is another name for `(`. Here it is the shell's block, the
//! more useful thing in a terminal: `ib` is a command's output, `ab` its
//! prompt, command and output. Only the daemon knows where blocks are, so
//! the pane asks it for those.
//!
//! `ip` is the paragraph around the cursor, or the run of blank lines it's
//! in, and `ap` the paragraph with the blank lines after it, or before it
//! if there are none after: whole lines, as Vim's take, a count that many.
//!
//! In an editor, `f`, `c` and `a` are pieces of code its grammar finds:
//! `af` is a function and `if` its body, `ac` a class and `ic` its body,
//! and `aa` an argument with its comma and `ia` the argument. Only the
//! daemon has the syntax tree, so the editor asks it for those too.

use clackatoo_text::Construct;

use crate::vim::Selection;

use crate::motion::{
    Cells, Class, Lines, Point, Walk, Words, class_at, line_end, next_cell, previous_cell, text_end,
};

mod words;

/// How far a bracket object looks for each of its brackets, in cells.
const BRACKET_SEARCH: usize = 100_000;

/// A kind of text object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Object {
    /// `w`: letters, digits and `_`, with what the file's language adds
    /// to them, or a run of other punctuation.
    Word,
    /// `W`: anything but blanks.
    BigWord,
    /// `"`, `'` or `` ` ``: a quoted string, on one line.
    Quote(char),
    /// `(`, `[`, `{` or `<` (or their closing halves): what's between a
    /// pair, which may span lines.
    Bracket { open: char, close: char },
    /// `p`: a paragraph, whole lines.
    Paragraph,
    /// `b`: the shell's block.
    Block,
    /// `f`, `c` or `a`: a piece of code.
    Code(Construct),
}

impl Object {
    /// The object a key names.
    pub(crate) fn of_key(key: char) -> Option<Self> {
        let bracket = |open, close| Some(Self::Bracket { open, close });
        match key {
            'w' => Some(Self::Word),
            'W' => Some(Self::BigWord),
            '"' | '\'' | '`' => Some(Self::Quote(key)),
            '(' | ')' => bracket('(', ')'),
            '[' | ']' => bracket('[', ']'),
            '{' | '}' => bracket('{', '}'),
            '<' | '>' => bracket('<', '>'),
            'p' => Some(Self::Paragraph),
            'b' => Some(Self::Block),
            'f' => Some(Self::Code(Construct::Function)),
            'c' => Some(Self::Code(Construct::Class)),
            'a' => Some(Self::Code(Construct::Argument)),
            _ => None,
        }
    }

    /// Where the object around `at` starts and ends, both included: just
    /// its insides if `inner`, and `count` of them where that's a thing (a
    /// paragraph, words). An error if there's no such object there, or
    /// it's empty, with where Vim's leaves the cursor if it moves it
    /// failing, as counting words past the text's end does. Blocks and
    /// pieces of code aren't found here (see the module docs).
    pub(crate) fn find(
        self,
        lines: &impl Lines,
        at: Point,
        inner: bool,
        count: usize,
    ) -> Result<Found, Option<Point>> {
        let chars = |(start, last)| Found {
            start,
            last,
            lines: false,
            inclusive: true,
        };
        match self {
            Self::Word | Self::BigWord => {
                let big = self == Self::BigWord;
                let found = words::current(lines, at, None, (inner, big), count)
                    .map_err(|failed| Some(failed.end))?;
                Ok(Found {
                    start: found.start,
                    last: found.end,
                    lines: false,
                    inclusive: found.inclusive,
                })
            }
            Self::Quote(quote) => quoted(lines.row(at.line).as_deref(), at, quote, inner)
                .map(chars)
                .ok_or(None),
            Self::Bracket { open, close } => {
                let (start, last, inclusive) =
                    bracketed(lines, at, (open, close), inner, count).ok_or(None)?;
                Ok(Found {
                    start,
                    last,
                    lines: false,
                    inclusive,
                })
            }
            Self::Paragraph => {
                let (first, last) = paragraph(lines, at.line, inner, count.max(1)).ok_or(None)?;
                Ok(Found {
                    start: Point {
                        line: first,
                        col: 0,
                    },
                    last: Point { line: last, col: 0 },
                    lines: true,
                    inclusive: true,
                })
            }
            Self::Block | Self::Code(_) => Err(None),
        }
    }
}

/// Where an object was found: from `start` to `last`, both included, and
/// whether it's whole lines, as a paragraph is, or characters. Unless
/// `inclusive`, `last` is a line's start just after it, which an operator
/// takes up to as it does an exclusive motion, as Vim's: the end of the
/// line before, or whole lines from the first's indent (see
/// [`taken`](Self::taken)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) start: Point,
    pub(crate) last: Point,
    pub(crate) lines: bool,
    pub(crate) inclusive: bool,
}

impl Found {
    /// What an operator takes of it, as Vim's `do_pending()` takes an
    /// exclusive end at a line's start: to the end of the line before, or
    /// from at or before the first line's first non-blank, whole lines.
    /// None if that's nothing. The editor's operators take it as they do
    /// any exclusive motion; this is for what only copies.
    pub(crate) fn taken(self, lines: &impl Lines) -> Option<Self> {
        if self.inclusive {
            return Some(self);
        }
        let Self { start, last, .. } = self;
        if last.line <= start.line {
            return None;
        }
        let line = last.line - 1;
        let row = lines.row(start.line);
        let indent = row.as_deref().map_or(0, |row| {
            (row.on_from(0))
                .find(|(_, spot)| !spot.blank())
                .map_or(usize::MAX, |(col, _)| col)
        });
        Some(Self {
            start,
            last: Point {
                line,
                col: lines.last_cell(line),
            },
            lines: start.col <= indent,
            inclusive: true,
        })
    }

    /// Where a selection of it ends: just past the end of the line before
    /// a line's start it's up to, so the line break's in it, as Vim's.
    fn selected_last(self, lines: &impl Lines) -> Point {
        if self.inclusive || self.last.line == 0 {
            return self.last;
        }
        let line = self.last.line - 1;
        Point {
            line,
            col: line_end(line, lines),
        }
    }
}

/// Whether line `line` has nothing but blanks, as Vim's `linewhite()`
/// says, which paragraphs as objects end at, where `{` and `}` stop only
/// at empty lines.
fn blank(lines: &impl Lines, line: u64) -> bool {
    text_end(lines.row(line).as_deref()) == 0
}

/// What's selected in visual mode: from `anchor` to `cursor`, whole lines
/// if `lines`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    pub(crate) anchor: Point,
    pub(crate) cursor: Point,
    pub(crate) lines: bool,
}

impl Object {
    /// The kind of selection selecting it in visual mode leaves, from
    /// `kind`: whole lines if `lines`, as a paragraph is, or characters,
    /// but a block stays one for words and quoted strings, as Vim's do; and
    /// failing, as it was.
    pub(crate) fn selection(self, kind: Selection, lines: bool, failed: bool) -> Selection {
        let keeps_block = matches!(self, Self::Word | Self::BigWord | Self::Quote(_));
        if failed || (kind == Selection::Block && keeps_block) {
            kind
        } else if lines {
            Selection::Lines
        } else {
            Selection::Characters
        }
    }

    /// What's selected once `count` of the object are, in visual mode, from
    /// `selected`: the object around the cursor, or for a paragraph, as
    /// Vim's `current_par()` goes, the selection taken on by `count`
    /// paragraphs if it's more than a line, by lines or not; for words, as
    /// Vim's `current_word()` goes, the same. An error if there's no such
    /// object there, with what's selected if Vim's leaves it changed
    /// failing, as counting words past the text's end does.
    pub(crate) fn select(
        self,
        lines: &impl Lines,
        selected: Selected,
        inner: bool,
        count: usize,
    ) -> Result<Selected, Option<Selected>> {
        if let Self::Word | Self::BigWord = self {
            let big = self == Self::BigWord;
            let anchor = Some(selected.anchor);
            // By characters, from lines too, as Vim's.
            let chars = |found: words::Worded| Selected {
                anchor: found.start,
                cursor: found.end,
                lines: false,
            };
            return words::current(lines, selected.cursor, anchor, (inner, big), count)
                .map(chars)
                .map_err(|failed| Some(chars(failed)));
        }
        if self != Self::Paragraph {
            // As Vim's: brackets are looked for from the selection's start,
            // and a quoted string can't be over lines.
            let from = match self {
                Self::Bracket { .. } => selected.anchor.min(selected.cursor),
                Self::Quote(_) if selected.anchor.line != selected.cursor.line => {
                    return Err(None);
                }
                _ => selected.cursor,
            };
            let found = self.find(lines, from, inner, count).map_err(|_| None)?;
            return Ok(Selected {
                anchor: found.start,
                cursor: found.selected_last(lines),
                lines: found.lines,
            });
        }
        let line = selected.cursor.line;
        if selected.anchor.line == line {
            let (start, end) = paragraph(lines, line, inner, count.max(1)).ok_or(None)?;
            // By lines already, on a line that starts one: on to the next.
            if !(selected.lines && start == line) {
                let at = |line| Point { line, col: 0 };
                // The anchor moves only to another line, as Vim's.
                let anchor = if selected.anchor.line == start {
                    selected.anchor
                } else {
                    at(start)
                };
                return Ok(Selected {
                    anchor,
                    cursor: at(end),
                    lines: true,
                });
            }
        }
        // Failing at the text's end, as far as it got, as Vim's.
        let (line, failed) = more_paragraphs(lines, selected, inner, count.max(1));
        let selected = Selected {
            cursor: Point { line, col: 0 },
            ..selected
        };
        if failed {
            Err(Some(selected))
        } else {
            Ok(selected)
        }
    }
}

/// Where the cursor of `selected`, more than a line, goes for `count`
/// more paragraphs: on away from the anchor, over a paragraph and, unless
/// `inner`, the blank lines after it, as Vim's `current_par()` extends a
/// selection, stopping at the text's end, and saying if it got there
/// with paragraphs still to go, where Vim's fails.
fn more_paragraphs(
    lines: &impl Lines,
    selected: Selected,
    inner: bool,
    count: usize,
) -> (u64, bool) {
    let (first, last) = lines.bounds();
    let back = selected.cursor.line < selected.anchor.line;
    let end = if back { first } else { last };
    let step = |line: u64| if back { line - 1 } else { line + 1 };
    let mut at = selected.cursor.line;
    for _ in 0..count {
        if at == end {
            return (at, true);
        }
        let mut was_white = None;
        for _ in 0..2 {
            let next = step(at);
            let white = blank(lines, next);
            if was_white == Some(white) {
                break;
            }
            at = next;
            while at != end && blank(lines, step(at)) == white {
                at = step(at);
            }
            if inner || at == end {
                break;
            }
            was_white = Some(white);
        }
    }
    (at, false)
}

/// `ip` and `ap`, `count` of them, around line `line`, as Vim's
/// `current_par()` finds them: the first and last lines. A run of blank
/// lines is a paragraph for `ip`; `ap` takes a paragraph's blank lines
/// after it, or if there are none, before it. None if there aren't
/// `count` of them.
fn paragraph(lines: &impl Lines, line: u64, inner: bool, count: usize) -> Option<(u64, u64)> {
    let include = !inner;
    let (first, last) = lines.bounds();
    // Line numbers that can be one before the first, as Vim's go.
    let (first, last) = (first.cast_signed(), last.cast_signed());
    let white = |line: i64| blank(lines, line.cast_unsigned());
    let white_in_front = white(line.cast_signed());
    // Back to the start of the paragraph, or of the blank lines.
    let mut start = line.cast_signed();
    while start > first && white(start - 1) == white_in_front {
        start -= 1;
    }
    // Past the blank lines it starts with, if any.
    let mut end = start;
    while end <= last && white(end) {
        end += 1;
    }
    end -= 1;
    let mut left = count - usize::from(!include && white_in_front);
    while left > 0 {
        left -= 1;
        if end == last {
            return None;
        }
        let blank_next = !include && white(end + 1);
        if include || !blank_next {
            end += 1;
            while end < last && !white(end + 1) {
                end += 1;
            }
        }
        if left == 0 && white_in_front && include {
            break;
        }
        if include || blank_next {
            while end < last && white(end + 1) {
                end += 1;
            }
        }
    }
    // With no blank lines after, `ap` takes those before.
    if !white_in_front && !white(end) && include {
        while start > first && white(start - 1) {
            start -= 1;
        }
    }
    Some((start.cast_unsigned(), end.cast_unsigned()))
}

/// `start` to `last`, with the blanks after it, or if there are none, the
/// blanks before it, as `aw` and `a"` take.
fn with_blanks(row: Option<&impl Cells>, start: usize, last: usize, end: usize) -> (usize, usize) {
    let blank = |col| class_at(row, col, Words::NONE) == Class::Blank;
    let mut after = last;
    while let Some(next) = next_cell(row, after).filter(|&n| n < end && blank(n)) {
        after = next;
    }
    if after != last {
        return (start, after);
    }
    let mut before = start;
    while let Some(previous) = previous_cell(row, before).filter(|&p| blank(p)) {
        before = previous;
    }
    (before, last)
}

/// `i"` and `a"`: the quoted string the cursor is in or on, or else the
/// next one on the line. Quotes pair up from the line's start, and one
/// after a backslash doesn't count.
fn quoted(row: Option<&impl Cells>, at: Point, quote: char, inner: bool) -> Option<(Point, Point)> {
    // The quotes, each with whether the cell before is a backslash.
    let mut before = ' ';
    let mut quotes = row?.on_from(0).filter_map(|(col, spot)| {
        let escaped = before == '\\';
        before = spot.ch;
        (spot.ch == quote && !escaped).then_some(col)
    });
    let (open, close) = std::iter::from_fn(|| Some((quotes.next()?, quotes.next()?)))
        .find(|&(_, close)| at.col <= close)?;
    let point = |col| Point { line: at.line, col };
    if inner {
        if open + 1 == close {
            return None;
        }
        Some((point(open + 1), point(previous_cell(row, close)?)))
    } else {
        let (start, last) = with_blanks(row, open, close, text_end(row));
        Some((point(start), point(last)))
    }
}

/// `i(` and `a(`, and the like, as Vim's `current_block()` finds them:
/// the brackets around the cursor, or that it's on, or `count` levels out;
/// or if there are none around it, as Neovim's, the next after it, and
/// with a count, the next after that, in it.
fn bracketed(
    lines: &impl Lines,
    at: Point,
    (open, close): (char, char),
    inner: bool,
    count: usize,
) -> Option<(Point, Point, bool)> {
    let ch = |point: Point| {
        let row = lines.row(point.line)?;
        row.cell(point.col).map(|spot| spot.ch)
    };
    // The first opening bracket back from `from`, or on if `forward`, that
    // isn't matched by a closing one between.
    let unmatched = |from: Point, forward: bool| {
        let mut walk = Walk::new(lines, from);
        let mut depth = 0usize;
        for _ in 0..BRACKET_SEARCH {
            if !(if forward { walk.forward() } else { walk.back() }) {
                return None;
            }
            match ch(walk.at()) {
                Some(c) if c == close => depth += 1,
                Some(c) if c == open => match depth.checked_sub(1) {
                    Some(fewer) => depth = fewer,
                    None => return Some(walk.at()),
                },
                _ => {}
            }
        }
        None
    };
    // From just after an opening bracket the cursor's on, so it's the
    // first, as Vim's.
    let from = if ch(at) == Some(open) {
        let mut walk = Walk::new(lines, at);
        walk.forward();
        walk.at()
    } else {
        at
    };
    let forward = unmatched(from, false).is_none();
    let mut start = from;
    for _ in 0..count.max(1) {
        start = unmatched(start, forward)?;
    }
    let mut walk = Walk::new(lines, start);
    let start = walk.at();
    // Its partner.
    let mut depth = 0usize;
    for _ in 0..BRACKET_SEARCH {
        if !walk.forward() {
            return None;
        }
        match ch(walk.at()) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close => match depth.checked_sub(1) {
                Some(fewer) => depth = fewer,
                None => break,
            },
            _ => {}
        }
    }
    let end = walk.at();
    if ch(end) != Some(close) || end <= start {
        return None;
    }
    if !inner {
        return Some((start, end, true));
    }
    inside(lines, start, end)
}

/// What's strictly between the brackets at `open` and `close`, leaving
/// out the line break after an opening bracket that ends its line, and the
/// indent before a closing one that starts its line. None if empty.
fn inside(lines: &impl Lines, open: Point, close: Point) -> Option<(Point, Point, bool)> {
    let mut walk = Walk::new(lines, open);
    walk.forward();
    let mut first = walk.at();
    if first.line < close.line && first.col >= text_end(lines.row(first.line).as_deref()) {
        first = Point {
            line: first.line + 1,
            col: 0,
        };
    }
    let row = lines.row(close.line);
    let indented = row.as_deref().is_none_or(|row| {
        (row.on_from(0))
            .take_while(|&(col, _)| col < close.col)
            .all(|(_, spot)| spot.blank())
    });
    // Up to the line the closing bracket starts, not in it, as Vim's
    // `current_block()` with `sol`: to an operator, the lines between
    // whole from their indent.
    if indented && close.line > first.line {
        let last = Point {
            line: close.line,
            col: 0,
        };
        return (first < last).then_some((first, last, false));
    }
    let mut walk = Walk::new(lines, close);
    walk.back();
    let last = walk.at();
    (first <= last && first != close).then_some((first, last, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::tests::{Text, at};

    fn found(text: &Text, from: Point, keys: &str) -> Option<(Point, Point)> {
        let mut keys = keys.chars();
        let inner = keys.next() == Some('i');
        let object = Object::of_key(keys.next().unwrap()).unwrap();
        let found = object.find(text, from, inner, 1).ok()?.taken(text)?;
        Some((found.start, found.last))
    }

    fn cols(range: Option<(Point, Point)>) -> Option<(usize, usize)> {
        range.map(|(start, last)| (start.col, last.col))
    }

    #[test]
    fn words_with_and_without_their_blanks() {
        let text = Text::new(&["ls  ~/src/a.rs x"]);
        assert_eq!(cols(found(&text, at(0, 1), "iw")), Some((0, 1)));
        assert_eq!(cols(found(&text, at(0, 1), "aw")), Some((0, 3)));
        assert_eq!(cols(found(&text, at(0, 6), "iw")), Some((6, 8)));
        assert_eq!(cols(found(&text, at(0, 6), "iW")), Some((4, 13)));
        // On blanks: the blanks, or the blanks and the word after.
        assert_eq!(cols(found(&text, at(0, 2), "iw")), Some((2, 3)));
        assert_eq!(cols(found(&text, at(0, 2), "aw")), Some((2, 5)));
        // The last word has no blanks after, so takes those before.
        assert_eq!(cols(found(&text, at(0, 15), "aw")), Some((14, 15)));
    }

    #[test]
    fn quotes_pair_from_the_start_of_the_line() {
        let text = Text::new(&[r#"a "b c" d "e\"f" "#]);
        assert_eq!(cols(found(&text, at(0, 4), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 2), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 2), "a\"")), Some((2, 7)));
        // Before the first pair: the next one. An escaped quote is text.
        assert_eq!(cols(found(&text, at(0, 0), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 8), "i\"")), Some((11, 14)));
        assert_eq!(found(&Text::new(&["\"\""]), at(0, 0), "i\""), None);
    }

    #[test]
    fn brackets_nest_and_span_lines() {
        let text = Text::new(&["f(a, (b), c)", "g(", "  x,", ")"]);
        assert_eq!(found(&text, at(0, 3), "i("), Some((at(0, 2), at(0, 10))));
        assert_eq!(found(&text, at(0, 6), "i)"), Some((at(0, 6), at(0, 6))));
        assert_eq!(found(&text, at(0, 5), "a("), Some((at(0, 5), at(0, 7))));
        assert_eq!(found(&text, at(0, 1), "a("), Some((at(0, 1), at(0, 11))));
        // Over lines, without the break after `(` or the indent before `)`:
        // the lines between whole, as Vim's.
        assert_eq!(found(&text, at(2, 2), "i("), Some((at(2, 0), at(2, 3))));
        let inner = Object::of_key('(').unwrap().find(&text, at(2, 2), true, 1);
        assert!(
            inner
                .ok()
                .and_then(|found| found.taken(&text))
                .is_some_and(|found| found.lines)
        );
        assert_eq!(found(&text, at(2, 2), "a("), Some((at(1, 1), at(3, 0))));
        // With none around it, the next, as Neovim's.
        assert_eq!(found(&text, at(0, 0), "i("), Some((at(0, 2), at(0, 10))));
        assert_eq!(found(&Text::new(&["()"]), at(0, 0), "i("), None);
    }

    #[test]
    fn a_count_of_brackets_goes_out_or_on_into_the_next() {
        let counted = |text: &[&str], from, count| {
            let found = Object::of_key('(')
                .unwrap()
                .find(&Text::new(text), from, true, count);
            found.ok().map(|found| (found.start, found.last))
        };
        // Out a level, as Vim's.
        let text = ["f(a, (b), c)"];
        assert_eq!(counted(&text, at(0, 6), 2), Some((at(0, 2), at(0, 10))));
        assert_eq!(counted(&text, at(0, 6), 3), None);
        // With none around the cursor, on into the next, as Neovim's.
        assert_eq!(
            counted(&["x ((y)) z"], at(0, 0), 2),
            Some((at(0, 4), at(0, 4)))
        );
        assert_eq!(counted(&["x ((y)) z"], at(0, 0), 3), None);
    }
}
