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

use crate::motion::{Cells, Lines, Point, Walk, line_end, previous_cell, text_end};

mod found;
mod quotes;
mod words;

use words::{Pos, Step};

pub(crate) use found::Found;
#[cfg(test)]
pub(crate) use words::LINES_LOOKED_UP;
pub(crate) use words::back_fails;

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

    /// Where the object around `at` starts and ends (see [`Found`]): just
    /// its insides if `inner`, and `count` of them where that's a thing (a
    /// paragraph, words). An error if there's no such object there, with
    /// where Vim's leaves the cursor if it moves it failing, as counting
    /// words past the text's end does. Blocks and pieces of code aren't
    /// found here (see the module docs).
    pub(crate) fn find(
        self,
        lines: &impl Lines,
        at: Point,
        inner: bool,
        count: usize,
    ) -> Result<Found, Option<Point>> {
        match self {
            Self::Word | Self::BigWord => {
                let big = self == Self::BigWord;
                let found = words::current(lines, at, None, (inner, big), count)
                    .map_err(|failed| Some(failed.end))?;
                Ok(Found::new((found.start, found.end), false, found.inclusive))
            }
            Self::Quote(quote) => {
                let row = lines.row(at.line);
                let line = (row.as_deref(), line_end(at.line, lines));
                let found =
                    quotes::current(line, at.col, None, (!inner, count), quote).ok_or(None)?;
                let point = |col| Point { line: at.line, col };
                let ends = (point(found.start), point(found.end));
                Ok(Found::new(ends, false, found.inclusive))
            }
            Self::Bracket { open, close } => {
                let (start, last, inclusive) =
                    bracketed(lines, at, (open, close), inner, count).ok_or(None)?;
                Ok(Found::new((start, last), false, inclusive))
            }
            Self::Paragraph => {
                let (first, last) = paragraph(lines, at.line, inner, count.max(1)).ok_or(None)?;
                let at = |line| Point { line, col: 0 };
                Ok(Found::new((at(first), at(last)), true, true))
            }
            Self::Block | Self::Code(_) => Err(None),
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
    /// failing, as it was. Paragraphs taken on from a selection of more
    /// than a line, which isn't by lines, keep its kind, as Vim's
    /// `current_par()` does.
    pub(crate) fn selection(self, kind: Selection, lines: bool, failed: bool) -> Selection {
        let keeps_block = matches!(self, Self::Word | Self::BigWord | Self::Quote(_));
        let taken_on = self == Self::Paragraph && !lines;
        if failed || taken_on || (kind == Selection::Block && keeps_block) {
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
        if let Self::Bracket { open, close } = self {
            return bracket_selected(lines, selected, (open, close), inner, count).ok_or(None);
        }
        // As Vim's `current_quote()`, on one line.
        if let Self::Quote(quote) = self {
            let at = selected.cursor;
            if selected.anchor.line != at.line {
                return Err(None);
            }
            let row = lines.row(at.line);
            let line = (row.as_deref(), line_end(at.line, lines));
            let anchor = Some(selected.anchor.col);
            let found =
                quotes::current(line, at.col, anchor, (!inner, count), quote).ok_or(None)?;
            let point = |col| Point { line: at.line, col };
            return Ok(Selected {
                anchor: point(found.anchor.unwrap_or(found.start)),
                cursor: point(found.end),
                lines: false,
            });
        }
        if self != Self::Paragraph {
            let found = self
                .find(lines, selected.cursor, inner, count)
                .map_err(|_| None)?;
            return Ok(Selected {
                anchor: found.start(),
                cursor: found.selected_last(lines),
                lines: found.lines(),
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

/// `i(` and `a(`, and the like, as Vim's `current_block()` finds them:
/// the brackets around the cursor, or that it's on, or `count` levels out;
/// or if there are none around it, as Neovim's, the next after it, and
/// with a count, the next after that, in it.
fn bracketed(
    lines: &impl Lines,
    at: Point,
    pair: (char, char),
    inner: bool,
    count: usize,
) -> Option<(Point, Point, bool)> {
    let (start, end) = brackets(lines, at, pair, count, true)?;
    Some(if inner {
        inside(lines, start, end)
    } else {
        (start, end, true)
    })
}

/// The character in cell `at` of `lines`.
fn char_at(lines: &impl Lines, at: Point) -> Option<char> {
    let row = lines.row(at.line)?;
    row.cell(at.col).map(|spot| spot.ch)
}

/// The first opening bracket of `pair` back from `from`, or on if
/// `forward`, not `from`'s own, that isn't matched by a closing one
/// between, as Vim's `findmatchlimit()` looks.
fn unmatched(
    lines: &impl Lines,
    from: Point,
    (open, close): (char, char),
    forward: bool,
) -> Option<Point> {
    let mut walk = Walk::new(lines, from);
    let mut depth = 0usize;
    for _ in 0..BRACKET_SEARCH {
        if !(if forward { walk.forward() } else { walk.back() }) {
            return None;
        }
        match char_at(lines, walk.at()) {
            Some(c) if c == close => depth += 1,
            Some(c) if c == open => match depth.checked_sub(1) {
                Some(fewer) => depth = fewer,
                None => return Some(walk.at()),
            },
            _ => {}
        }
    }
    None
}

/// The character before the one at `at`, as Vim's `decl()` steps: from a
/// line's start, the line before's last.
fn char_before(lines: &impl Lines, at: Point) -> Point {
    if at.col > 0 {
        let row = lines.row(at.line);
        let col = previous_cell(row.as_deref(), at.col).unwrap_or(0);
        return Point { col, ..at };
    }
    match at.line.checked_sub(1) {
        Some(line) if line >= lines.bounds().0 => Point {
            line,
            col: lines.last_cell(line),
        },
        _ => at,
    }
}

/// The closing bracket that pairs with the opening one at `start`.
fn partner(lines: &impl Lines, start: Point, (open, close): (char, char)) -> Option<Point> {
    let mut walk = Walk::new(lines, start);
    let mut depth = 0usize;
    for _ in 0..BRACKET_SEARCH {
        if !walk.forward() {
            return None;
        }
        match char_at(lines, walk.at()) {
            Some(c) if c == open => depth += 1,
            Some(c) if c == close => match depth.checked_sub(1) {
                Some(fewer) => depth = fewer,
                None => break,
            },
            _ => {}
        }
    }
    let end = walk.at();
    (char_at(lines, end) == Some(close) && end > start).then_some(end)
}

/// The brackets of `pair` around `at`, `count` levels out, or the next
/// after it (see [`bracketed`]). On an opening bracket, it's the first if
/// `on_open`, as Vim's goes from just after one the cursor's on, but not
/// from the start of a selection of more than a character.
fn brackets(
    lines: &impl Lines,
    at: Point,
    pair: (char, char),
    count: usize,
    on_open: bool,
) -> Option<(Point, Point)> {
    // In a line's indent, `{` looks from past it, as Vim's.
    let mut at = at;
    if on_open && pair.0 == '{' {
        let mut pos = Pos::new(lines, at);
        while in_indent(lines, pos.at) && pos.inc() == Step::Along {}
        at = pos.at;
    }
    let from = if on_open && char_at(lines, at) == Some(pair.0) {
        let mut walk = Walk::new(lines, at);
        walk.forward();
        walk.at()
    } else {
        at
    };
    let forward = unmatched(lines, from, pair, false).is_none();
    let mut start = from;
    for _ in 0..count.max(1) {
        start = unmatched(lines, start, pair, forward)?;
    }
    Some((start, partner(lines, start, pair)?))
}

/// `i(` and `a(` and the like in visual mode, from `selected`, as Vim's
/// `current_block()` goes: from a selection of more than a character, the
/// brackets around its start; and inside them, if that's no more than is
/// selected, the brackets out from those, until it's more. None if there
/// are none.
fn bracket_selected(
    lines: &impl Lines,
    selected: Selected,
    pair: (char, char),
    inner: bool,
    count: usize,
) -> Option<Selected> {
    let single = selected.anchor == selected.cursor;
    let low = selected.anchor.min(selected.cursor);
    let high = selected.anchor.max(selected.cursor);
    let (mut start, mut end) = brackets(lines, low, pair, count, single)?;
    let found = loop {
        if !inner {
            break (start, end, true);
        }
        let (first, last, inclusive) = inside(lines, start, end);
        // Nothing inside, as Vim's fails in visual mode.
        if first == end {
            return None;
        }
        // Its last character, before the line `)` starts.
        let last_char = if inclusive {
            last
        } else {
            char_before(lines, last)
        };
        let grows = first < low || high < last_char;
        if grows || first == last_char {
            break (first, last, inclusive);
        }
        // No more than is selected: the brackets out from these, from
        // before the selection's start.
        start = unmatched(lines, char_before(lines, low), pair, false)?;
        end = partner(lines, start, pair)?;
    };
    let (start, last, inclusive) = found;
    let found = Found::new((start, last), false, inclusive);
    Some(Selected {
        anchor: start,
        cursor: found.selected_last(lines),
        lines: false,
    })
}

/// What's between the brackets at `open` and `close`, as Vim's
/// `current_block()` leaves it without them: from the character after
/// `open`, or the next line's start if `open` ends its line, to the one
/// before `close`, back over the indent before it and any lines of blanks
/// before that (Vim's `inindent()`). Back over any of those, it ends at the
/// next line's start after it, exclusive, Vim's `sol`, which an operator
/// takes up to as an exclusive motion: whole lines from the start's
/// indent. With nothing between, it's empty, at the start.
fn inside(lines: &impl Lines, open: Point, close: Point) -> (Point, Point, bool) {
    let mut start = Pos::new(lines, open);
    start.incl();
    let start = start.at;
    let mut end = Pos::new(lines, close);
    let mut sol = close.col == 0;
    end.decl();
    while in_indent(lines, end.at) {
        sol = true;
        if end.decl() != Step::Along {
            break;
        }
    }
    if sol {
        end.incl();
        (start, end.at, false)
    } else if start <= end.at {
        (start, end.at, true)
    } else {
        (start, start, false)
    }
}

/// Vim's `inindent(1)`: whether the character at `at` is blank, and all
/// before it on its line.
fn in_indent(lines: &impl Lines, at: Point) -> bool {
    let row = lines.row(at.line);
    at.col < line_end(at.line, lines)
        && row.as_deref().is_some_and(|row| {
            (row.on_from(0))
                .take_while(|&(col, _)| col <= at.col)
                .all(|(_, spot)| spot.blank())
        })
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
        Some((found.start(), found.last()))
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
        // On an empty last line, back over the line break before: the
        // ends in order, as a terminal's copy is asked for them.
        let text = Text::new(&["ab", ""]);
        assert_eq!(found(&text, at(1, 0), "iw"), Some((at(0, 1), at(1, 0))));
    }

    #[test]
    fn quotes_pair_from_the_start_of_the_line() {
        let text = Text::new(&[r#"a "b c" d "e\"f" "#]);
        assert_eq!(cols(found(&text, at(0, 4), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 2), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 2), "a\"")), Some((2, 7)));
        // Before the first pair: the next one. An escaped quote is text.
        assert_eq!(cols(found(&text, at(0, 0), "i\"")), Some((3, 5)));
        assert_eq!(cols(found(&text, at(0, 11), "i\"")), Some((11, 14)));
        // Between two, from the quote before, whichever it is, as Vim's.
        assert_eq!(cols(found(&text, at(0, 8), "i\"")), Some((7, 9)));
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
                .is_some_and(Found::lines)
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
            found.ok().map(|found| (found.start(), found.last()))
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
