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
//! In an editor, `f`, `c` and `a` are pieces of code its grammar finds:
//! `af` is a function and `if` its body, `ac` a class and `ic` its body,
//! and `aa` an argument with its comma and `ia` the argument. Only the
//! daemon has the syntax tree, so the editor asks it for those too.

use clackatoo_term::Row;
use clackatoo_text::Construct;

use crate::motion::{Class, Lines, Point, Walk, class_at, next_cell, previous_cell, text_end};

/// How far a bracket object looks for each of its brackets, in cells.
const BRACKET_SEARCH: usize = 100_000;

/// A kind of text object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Object {
    /// `w`: letters, digits and `_`, or a run of other punctuation.
    Word,
    /// `W`: anything but blanks.
    BigWord,
    /// `"`, `'` or `` ` ``: a quoted string, on one line.
    Quote(char),
    /// `(`, `[`, `{` or `<` (or their closing halves): what's between a
    /// pair, which may span lines.
    Bracket { open: char, close: char },
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
            'b' => Some(Self::Block),
            'f' => Some(Self::Code(Construct::Function)),
            'c' => Some(Self::Code(Construct::Class)),
            'a' => Some(Self::Code(Construct::Argument)),
            _ => None,
        }
    }

    /// Where the object around `at` starts and ends, both included: just
    /// its insides if `inner`. None if there's no such object there, or
    /// it's empty. Blocks and pieces of code aren't found here (see the
    /// module docs).
    pub(crate) fn find(self, lines: &impl Lines, at: Point, inner: bool) -> Option<(Point, Point)> {
        match self {
            Self::Word => Some(word(lines.row(at.line).as_deref(), at, inner, |class| {
                class
            })),
            Self::BigWord => Some(word(
                lines.row(at.line).as_deref(),
                at,
                inner,
                |class| match class {
                    Class::Blank => Class::Blank,
                    Class::Word | Class::Punctuation => Class::Word,
                },
            )),
            Self::Quote(quote) => quoted(lines.row(at.line).as_deref(), at, quote, inner),
            Self::Bracket { open, close } => bracketed(lines, at, open, close, inner),
            Self::Block | Self::Code(_) => None,
        }
    }
}

/// `iw` and `aw`, or `iW` and `aW` with `classify` lumping punctuation in
/// with letters.
fn word(
    row: Option<&Row>,
    at: Point,
    inner: bool,
    classify: impl Fn(Class) -> Class,
) -> (Point, Point) {
    let class = |col| classify(class_at(row, col));
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
fn with_blanks(row: Option<&Row>, start: usize, last: usize, end: usize) -> (usize, usize) {
    let blank = |col| class_at(row, col) == Class::Blank;
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
fn quoted(row: Option<&Row>, at: Point, quote: char, inner: bool) -> Option<(Point, Point)> {
    let cells = row?.cells();
    let quotes: Vec<usize> = (0..cells.len())
        .filter(|&col| cells[col].ch() == quote && (col == 0 || cells[col - 1].ch() != '\\'))
        .collect();
    let (open, close) = quotes
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
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
        row.cells().get(point.col).map(|cell| cell.ch())
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
    let indented = (0..close.col).all(|col| class_at(row.as_deref(), col) == Class::Blank);
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
        object.find(text, from, inner)
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
