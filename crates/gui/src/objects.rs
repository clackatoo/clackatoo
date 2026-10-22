//! Text objects: the pieces of text `i` and `a` pick out after `y`, or in
//! visual mode, as in Vim. `iw` is the word under the cursor and `aw` the
//! word with the blanks after it; `i"` is what's between the quotes around
//! the cursor and `a"` the quotes too; `i(` is what's between the
//! parentheses around it, across lines, and `a(` the parentheses too.
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

use crate::motion::{
    Cells, Class, Lines, Point, Walk, Words, class_at, next_cell, previous_cell, text_end,
};

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
    /// paragraph). None if there's no such object there, or it's empty.
    /// Blocks and pieces of code aren't found here (see the module docs).
    pub(crate) fn find(
        self,
        lines: &impl Lines,
        at: Point,
        inner: bool,
        count: usize,
    ) -> Option<Found> {
        let chars = |(start, last)| Found {
            start,
            last,
            lines: false,
        };
        match self {
            Self::Word | Self::BigWord => {
                let row = lines.row(at.line);
                let big = self == Self::BigWord;
                Some(chars(word(row.as_deref(), at, inner, big, lines.words())))
            }
            Self::Quote(quote) => {
                quoted(lines.row(at.line).as_deref(), at, quote, inner).map(chars)
            }
            Self::Bracket { open, close } => bracketed(lines, at, open, close, inner).map(chars),
            Self::Paragraph => {
                let (first, last) = paragraph(lines, at.line, inner, count.max(1))?;
                Some(Found {
                    start: Point {
                        line: first,
                        col: 0,
                    },
                    last: Point { line: last, col: 0 },
                    lines: true,
                })
            }
            Self::Block | Self::Code(_) => None,
        }
    }
}

/// Where an object was found: from `start` to `last`, both included, and
/// whether it's whole lines, as a paragraph is, or characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) start: Point,
    pub(crate) last: Point,
    pub(crate) lines: bool,
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
    /// What's selected once `count` of the object are, in visual mode, from
    /// `selected`: the object around the cursor, or for a paragraph, as
    /// Vim's `current_par()` goes, the selection taken on by `count`
    /// paragraphs if it's more than a line, by lines or not. None if
    /// there's no such object there.
    pub(crate) fn select(
        self,
        lines: &impl Lines,
        selected: Selected,
        inner: bool,
        count: usize,
    ) -> Option<Selected> {
        if self != Self::Paragraph {
            let found = self.find(lines, selected.cursor, inner, count)?;
            return Some(Selected {
                anchor: found.start,
                cursor: found.last,
                lines: found.lines,
            });
        }
        let line = selected.cursor.line;
        if selected.anchor.line == line {
            let (start, end) = paragraph(lines, line, inner, count.max(1))?;
            // By lines already, on a line that starts one: on to the next.
            if !(selected.lines && start == line) {
                let at = |line| Point { line, col: 0 };
                return Some(Selected {
                    anchor: at(start),
                    cursor: at(end),
                    lines: true,
                });
            }
        }
        let line = more_paragraphs(lines, selected, inner, count.max(1));
        Some(Selected {
            cursor: Point { line, col: 0 },
            ..selected
        })
    }
}

/// Where the cursor of `selected`, more than a line, goes for `count`
/// more paragraphs: on away from the anchor, over a paragraph and, unless
/// `inner`, the blank lines after it, as Vim's `current_par()` extends a
/// selection, stopping at the text's end.
fn more_paragraphs(lines: &impl Lines, selected: Selected, inner: bool, count: usize) -> u64 {
    let (first, last) = lines.bounds();
    let back = selected.cursor.line < selected.anchor.line;
    let end = if back { first } else { last };
    let step = |line: u64| if back { line - 1 } else { line + 1 };
    let mut at = selected.cursor.line;
    for _ in 0..count {
        if at == end {
            break;
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
    at
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

/// `iw` and `aw`, with `words` part of words; or if `big`, `iW` and
/// `aW`, lumping punctuation in with letters.
fn word(
    row: Option<&impl Cells>,
    at: Point,
    inner: bool,
    big: bool,
    words: Words,
) -> (Point, Point) {
    let class = |col| match class_at(row, col, words) {
        Class::Punctuation if big => Class::Word,
        class => class,
    };
    let end = text_end(row).max(at.col + 1);
    let run = |col: usize| {
        let wanted = class(col);
        let mut start = col;
        while let Some(previous) = previous_cell(row, start).filter(|&p| class(p) == wanted) {
            start = previous;
        }
        let mut last = col;
        while let Some(next) = next_cell(row, last).filter(|&n| n < end && class(n) == wanted) {
            last = next;
        }
        (start, last)
    };
    let (mut start, mut last) = run(at.col);
    if !inner {
        if class(at.col) == Class::Blank {
            // The blanks, and the word after them.
            if let Some(next) = next_cell(row, last).filter(|&n| n < end) {
                last = run(next).1;
            }
        } else {
            (start, last) = with_blanks(row, start, last, end);
        }
    }
    let point = |col| Point { line: at.line, col };
    (point(start), point(last))
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

/// `i(` and `a(`, and the like: the brackets around the cursor, or that it
/// is on.
fn bracketed(
    lines: &impl Lines,
    at: Point,
    open: char,
    close: char,
    inner: bool,
) -> Option<(Point, Point)> {
    let ch = |point: Point| {
        let row = lines.row(point.line)?;
        row.cell(point.col).map(|spot| spot.ch)
    };
    // The opening bracket: the cursor's, or the first unmatched one before
    // it.
    let mut walk = Walk::new(lines, at);
    if ch(at) != Some(open) {
        let mut depth = 0usize;
        for steps in 0.. {
            if steps == BRACKET_SEARCH || !walk.back() {
                return None;
            }
            match ch(walk.at()) {
                Some(c) if c == close => depth += 1,
                Some(c) if c == open => match depth.checked_sub(1) {
                    Some(fewer) => depth = fewer,
                    None => break,
                },
                _ => {}
            }
        }
    }
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
        return Some((start, end));
    }
    inside(lines, start, end)
}

/// What's strictly between the brackets at `open` and `close`, leaving
/// out the line break after an opening bracket that ends its line, and the
/// indent before a closing one that starts its line. None if empty.
fn inside(lines: &impl Lines, open: Point, close: Point) -> Option<(Point, Point)> {
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
    let last = if indented && close.line > first.line {
        let line = close.line - 1;
        Point {
            line,
            col: text_end(lines.row(line).as_deref()).saturating_sub(1),
        }
    } else {
        let mut walk = Walk::new(lines, close);
        walk.back();
        walk.at()
    };
    (first <= last && first != close).then_some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::tests::{Text, at};

    fn found(text: &Text, from: Point, keys: &str) -> Option<(Point, Point)> {
        let mut keys = keys.chars();
        let inner = keys.next() == Some('i');
        let object = Object::of_key(keys.next().unwrap()).unwrap();
        let found = object.find(text, from, inner, 1)?;
        assert!(!found.lines);
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
        // Over lines, without the break after `(` or the indent before `)`.
        assert_eq!(found(&text, at(2, 2), "i("), Some((at(2, 0), at(2, 3))));
        assert_eq!(found(&text, at(2, 2), "a("), Some((at(1, 1), at(3, 0))));
        assert_eq!(found(&text, at(0, 0), "i("), None);
        assert_eq!(found(&Text::new(&["()"]), at(0, 0), "i("), None);
    }
}
