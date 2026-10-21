//! Block visual mode: Ctrl+V selects a block of columns, as Vim's does,
//! from one corner to the other, and what's done to it is done on each of
//! its lines.
//!
//! Columns are cells as drawn: a tab or a wide character a corner is on
//! is in the block whole, but one that an edge cuts on another line is
//! split as Vim splits it. What's taken of it is spaces, and what's left
//! of it, by `d`, `c` and `r`, is spaces too, so the text after stays in
//! its column. `$` takes every line to its end.
//!
//! What each command does, as Neovim does it (checked headless):
//! - `y`, `d`, `x` keep the block's rows in a register as a block, which
//!   `p` and `P` put back as one, a row on each line from the cursor's,
//!   padded with spaces to the block's width where text follows, and on
//!   new lines past the file's end. A line that ends before the block's
//!   left edge gives a row of spaces; one that ends inside it, what it has.
//! - `c`, `I` and `A` type on the first line; once insert mode ends, what
//!   was typed there is typed on the others: before the block for `I`
//!   and `c`, on lines that reach its left edge, and after it for `A`,
//!   padding lines that are too short, or at each line's end after `$`.
//!   Typing a line break, or leaving the line, types it on the first only.
//! - `r`, `~`, `u` and `U` change what's in the block, and `>` and `<` move
//!   the text from its left edge on, by `shiftwidth`, leaving the block as
//!   Vim's do, where `>` on other selections keeps them.
//! - `D` and `C` take each line from the block's left edge to its end.
//! - `.` makes the change again on a block as big from the cursor.
//!
//! Each change is one undo step, however many lines it makes.
//!
//! A folder's listing hides each line's mark in cells of none, so a block
//! never reaches one: what it does in a listing is done to names. Its `..`
//! line is left as it is, and a block put past the listing's end, which
//! would make lines with no marks, is refused.

use clackatoo_protocol::Edit;
use clackatoo_text::Bias;

use super::keys::Operator;
use super::registers::{Register, Registers, Wrote};
use super::text::{Laid, Text, tab_end};
use super::visual::Extent;
use super::{BufferCopy, Editor, FLASH, Mode, on_character};
use crate::motion::{Block, Cursor, Point};
use crate::vim::Selection;

/// What the bar says when a block would go past a listing's end.
pub(super) const PAST_LISTING: &str =
    "a block can't go past a listing's end: o makes a line for a new entry";

/// What the bar says when a block would be put on a listing's `..` line.
pub(super) const PUT_ON_PARENT: &str =
    "a block can't go on a listing's .. line, which names nothing";

/// What the bar says when someone else changed a block's lines while
/// its first was typed on.
pub(super) const SPOILED: &str = "not repeated on the other lines: the text changed meanwhile";

/// What the bar says when a block's typing would start on a listing's
/// `..` line.
pub(super) const ON_PARENT: &str = "a listing's .. line names nothing: start on an entry";

/// What of one line a block takes, as Vim cuts it: bytes are the line's,
/// from its start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Cut {
    /// The characters the block touches, from the first's first byte to
    /// just after the last's.
    pub(super) start: usize,
    pub(super) end: usize,
    /// Those wholly in it: all but a first and a last its edges cut.
    whole: (usize, usize),
    /// The columns of a character the left edge cuts that are before the
    /// block, and those in it.
    before: usize,
    lead: usize,
    /// The columns of a character the right edge cuts that are in the
    /// block, and those after it.
    trail: usize,
    after: usize,
    /// The columns of the line in the block.
    cols: usize,
    /// The line ends before the block's left edge.
    short: bool,
}

/// What `block` takes of the line `laid`: a character at a time, from the
/// one at its left edge, so as many as are in it.
pub(super) fn cut(laid: &Laid, block: Block) -> Cut {
    let cells = laid.cells();
    let len = laid.text().len();
    if cells <= block.left {
        return Cut {
            start: len,
            end: len,
            whole: (len, len),
            short: cells < block.left,
            ..Cut::default()
        };
    }
    let end_col = block.right.saturating_add(1);
    let start = laid.byte_at(block.left);
    let mut cut = Cut {
        start,
        end: start,
        whole: (start, start),
        ..Cut::default()
    };
    let mut byte = start;
    while byte < len {
        let col = laid.col_of(byte);
        if col >= end_col {
            break;
        }
        let next = laid.char_end(byte);
        let next_col = laid.col_of(next);
        let inside = next_col.min(end_col) - col.max(block.left);
        cut.cols += inside;
        if col < block.left {
            cut.before = block.left - col;
            cut.lead = inside;
            cut.whole = (next, next);
        } else if next_col <= end_col {
            cut.whole.1 = next;
        } else {
            cut.trail = inside;
        }
        if next_col > end_col {
            cut.after = next_col - end_col;
        }
        byte = next;
    }
    cut.end = byte;
    cut
}

/// `count` spaces.
fn spaces(count: usize) -> String {
    " ".repeat(count)
}

/// Builds one edit out of replacements made in order down the text.
struct Splice {
    edit: Edit,
    /// How far through the text the edit has gone.
    through: usize,
}

impl Splice {
    fn new() -> Self {
        Self {
            edit: Edit::new(),
            through: 0,
        }
    }

    /// Replaces `old`, at byte `at` of the text, with `new`.
    fn replace(&mut self, at: usize, old: &str, new: &str) {
        self.edit.retain(at - self.through).delete(old).insert(new);
        self.through = at + old.len();
    }

    /// The edit, for a text `len` bytes long, if it changes anything.
    fn finish(mut self, len: usize) -> Option<Edit> {
        self.edit.retain(len - self.through);
        (!self.edit.is_noop()).then_some(self.edit)
    }
}

/// How to type `text` at column `col` of the line `laid`, which starts at
/// byte `line_start` of the text: where, what it replaces and with what.
/// Past the line's end, spaces pad it out to the column. A tab the column
/// is inside is split into spaces either side of it, and a wide character
/// keeps its place after it, the column reached by spaces before it, as
/// Vim does both.
fn typed_at(laid: &Laid, line_start: usize, col: usize, text: &str) -> (usize, String, String) {
    let cells = laid.cells();
    let line = laid.text();
    if col >= cells {
        return (
            line_start + line.len(),
            String::new(),
            format!("{}{text}", spaces(col - cells)),
        );
    }
    let byte = laid.byte_at(col);
    let first = laid.col_of(byte);
    if first == col {
        return (line_start + byte, String::new(), text.to_owned());
    }
    let end = laid.char_end(byte);
    if line[byte..].starts_with('\t') {
        let after = laid.col_of(end) - col;
        let new = format!("{}{text}{}", spaces(col - first), spaces(after));
        return (line_start + byte, line[byte..end].to_owned(), new);
    }
    let new = format!("{}{text}", spaces(col - first));
    (line_start + byte, String::new(), new)
}

/// The columns `row` takes, typed at column `col`, with tabs stopping at
/// multiples of `tabstop`.
fn width_at(row: &str, col: usize, tabstop: usize) -> usize {
    Laid::new(format!("{}{row}", spaces(col)), tabstop).cells() - col
}

/// Blanks from column `col` reaching `width` columns on: tabs to each
/// stop they reach and spaces for the rest, if `tabs`, or all spaces.
fn blanks_from(col: usize, width: usize, tabs: bool, tabstop: usize) -> String {
    let end = col + width;
    let mut blanks = String::new();
    let mut at = col;
    while tabs && tab_end(at, tabstop) <= end && at < end {
        blanks.push('\t');
        at = tab_end(at, tabstop);
    }
    blanks.push_str(&spaces(end - at));
    blanks
}

/// Whether `ch` is a blank, as `>` and `<` move them.
fn blank(ch: char) -> bool {
    ch == ' ' || ch == '\t'
}

/// What an editor keeps of block visual mode.
#[derive(Debug, Default)]
pub(super) struct Blocks {
    /// The block `.` selected from the cursor, exactly as big as the one
    /// its change was made to, until that's made again.
    pub(super) repeat: Option<Block>,
    /// Where insert mode on a block's first line types, for what's typed
    /// to go on its other lines once it ends.
    pub(super) spread: Option<Spread>,
    /// The block selected, as last settled, for drawing.
    pub(super) shown: Option<Block>,
    /// How many cells the line on each row shown has, as last settled: a
    /// block is drawn only over a line's text.
    pub(super) cells: Vec<usize>,
}

/// Where insert mode on a block's first line types, for what's typed to
/// be typed on its other lines once insert mode ends (see [`Editor::spread`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct Spread {
    /// The byte typing started at...
    start: usize,
    /// ...and the first line's end, both moved with every edit, the end
    /// after what's typed at it.
    end: usize,
    /// How many bytes the first line had after where typing started.
    tail: usize,
    /// The block's lines, from the first's start to the last's end, moved
    /// with every edit...
    span: (usize, usize),
    /// ...and how many lines after the first it has.
    lines: u64,
    /// Where on them.
    at: At,
    /// Where the cursor goes once it's done, as Vim's goes to the block's
    /// top left after `I` and `A`: after `c`, it stays.
    back_to: Option<Point>,
    /// Someone else changed the block's lines meanwhile, as an agent may:
    /// what was typed is typed on the first line only.
    spoiled: bool,
}

/// Where on a block's lines what was typed goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At {
    /// Before column this, on lines that reach it, as `I` and `c` type.
    Before(usize),
    /// After the block, at this column, padding lines too short, as `A`.
    After(usize),
    /// At each line's end, as `A` after `$`.
    End,
}

impl Spread {
    /// Moves its places with `edit`, made here, as typing is.
    pub(super) fn made(&mut self, edit: &Edit) {
        self.start = edit.map(self.start, Bias::Before);
        self.end = edit.map(self.end, Bias::After);
        self.span = (
            edit.map(self.span.0, Bias::Before),
            edit.map(self.span.1, Bias::After),
        );
    }

    /// Moves its places with `edit`, which someone else made: one that
    /// touches the block's lines spoils it.
    pub(super) fn follow(&mut self, edit: &Edit) {
        match edit.map_untouched(self.span.0, self.span.1) {
            Some(span) => {
                self.span = span;
                self.start = edit.map(self.start, Bias::Before);
                self.end = edit.map(self.end, Bias::After);
            }
            None => self.spoiled = true,
        }
    }
}

impl Editor {
    /// The block selected, or the one `.` takes from the cursor.
    pub(super) fn selected_block(&self, copy: &BufferCopy) -> Option<Block> {
        let (Mode::Visual(Selection::Block), Some(anchor)) = (self.mode, self.anchor) else {
            return None;
        };
        if let Some(block) = self.blocks.repeat {
            return Some(block);
        }
        let to_end = self.cursor.want() == usize::MAX;
        Some(Block::between(
            anchor,
            self.cursor.at,
            to_end,
            &self.text(copy),
        ))
    }

    /// Ctrl+V: selects a block, starting at the cursor; switches to one
    /// from another kind of selection; or, in a block already, ends it.
    pub(super) fn block_visual(&mut self) {
        match self.mode {
            Mode::Visual(Selection::Block) => self.leave_visual(),
            Mode::Visual(_) => self.mode = Mode::Visual(Selection::Block),
            _ => {
                self.mode = Mode::Visual(Selection::Block);
                self.anchor = Some(self.cursor.at);
            }
        }
    }

    /// `O`: in a block, the cursor goes to the other corner on its line,
    /// and the selection's start to the other on its, as Vim's do; in any
    /// other selection, as `o`.
    ///
    /// As in Vim, a corner may go just past a short line's end, so the
    /// block keeps its width; and after `$` the cursor counts as just past
    /// its line's end, so the block becomes one that wide.
    pub(super) fn other_corner(&mut self, copy: &BufferCopy) {
        let (Some(anchor), Some(_)) = (self.anchor, self.selected_block(copy)) else {
            return self.other_end();
        };
        let text = self.text(copy);
        let cells = |line| text.laid(line).map_or(0, |laid| laid.cells());
        let mut was = self.cursor.at;
        if self.cursor.want() == usize::MAX {
            was.col = cells(was.line);
        }
        let block = Block::between(anchor, was, false, &text);
        let at = |line, col| {
            if col >= cells(line) {
                Point {
                    line,
                    col: cells(line),
                }
            } else {
                on_character(Point { line, col }, &text)
            }
        };
        let mut head = at(was.line, block.right);
        let mut tail = at(anchor.line, block.left);
        let mut want = block.right;
        if head.col == was.col {
            head = at(was.line, block.left);
            tail = at(anchor.line, block.right);
            want = block.left;
        }
        self.anchor = Some(tail);
        self.cursor = Cursor::wanting(head, want);
    }

    /// The text of each of `block`'s lines, as `y` keeps it: what's in it,
    /// with spaces for the part of a character an edge cuts, and a row of
    /// spaces as wide as the block for a line that ends before it.
    fn block_rows(&self, block: Block, copy: &BufferCopy) -> Vec<String> {
        let text = self.text(copy);
        (block.first..=block.last.min(text.last_line()))
            .map(|line| {
                let Some(laid) = text.laid(line) else {
                    return String::new();
                };
                let cut = cut(&laid, block);
                if cut.short {
                    return block.width().map_or_else(String::new, spaces);
                }
                let (start, end) = cut.whole;
                format!(
                    "{}{}{}",
                    spaces(cut.lead),
                    &laid.text()[start..end],
                    spaces(cut.trail)
                )
            })
            .collect()
    }

    /// The block's rows as one text, a line for each, as the mouse copies
    /// it to the primary selection.
    pub(super) fn block_text(&self, block: Block, copy: &BufferCopy) -> String {
        self.block_rows(block, copy).join("\n")
    }

    /// `operator` on `block`, `times` over for `>` and `<`, keeping what's
    /// taken in `registers` under `name`.
    pub(super) fn operate_block(
        &mut self,
        operator: Operator,
        block: Block,
        times: usize,
        copy: &mut BufferCopy,
        (registers, name): (&mut Registers, Option<char>),
    ) {
        if let Operator::Indent | Operator::Dedent = operator {
            // Not in a listing: a name may start with a space.
            if self.hide.is_some() {
                self.refuse(super::visual::INDENTS_ENTRIES);
                return;
            }
            self.shift_block(block, times, operator == Operator::Dedent, copy);
            return;
        }
        if let Operator::Recase(recase) = operator {
            self.edit_block(block, copy, |laid, cut| {
                let (start, end) = cut.whole;
                let old = &laid.text()[start..end];
                (start, old.to_owned(), recase.text(old))
            });
            self.end_block(block, copy);
            return;
        }
        if operator == Operator::Change && self.starts_on_kept(block, copy) {
            self.refuse(ON_PARENT);
            return;
        }
        let rows = self.block_rows(block, copy);
        let width = block.width().unwrap_or_else(|| {
            let widths = rows
                .iter()
                .map(|row| unicode_width::UnicodeWidthStr::width(&**row));
            widths.max().unwrap_or(0)
        });
        let small = block.first == block.last;
        let wrote = if operator == Operator::Yank {
            Wrote::Yank
        } else {
            Wrote::Delete {
                small,
                history: !small,
            }
        };
        registers.wrote(name, Register::block(&rows, width), wrote);
        match operator {
            Operator::Yank => {
                let last = Point {
                    line: block.last,
                    col: block.right,
                };
                let first = Point {
                    line: block.first,
                    col: block.left,
                };
                self.flash = Some((
                    first,
                    last,
                    Selection::Block,
                    std::time::Instant::now() + FLASH,
                ));
                self.end_block(block, copy);
            }
            Operator::Delete => {
                self.delete_block(block, copy);
                self.end_block(block, copy);
            }
            Operator::Change => {
                self.delete_block(block, copy);
                self.type_on_block(block, At::Before(block.left), (1, false), copy);
            }
            Operator::Indent | Operator::Dedent | Operator::Recase(_) | Operator::Reindent => {}
        }
    }

    /// Takes `block` out of each of its lines, leaving spaces for the part
    /// of a character an edge cuts that's outside it.
    fn delete_block(&mut self, block: Block, copy: &mut BufferCopy) {
        self.edit_block(block, copy, |laid, cut| {
            let old = &laid.text()[cut.start..cut.end];
            (cut.start, old.to_owned(), spaces(cut.before + cut.after))
        });
    }

    /// `r` in a block: puts `ch` in each column of it, as Vim's does, or
    /// half as many of a wide character, and a space for an odd column.
    pub(super) fn replace_block(&mut self, ch: char, block: Block, copy: &mut BufferCopy) {
        let wide = clackatoo_term::char_width(ch) == Some(2);
        self.edit_block(block, copy, |laid, cut| {
            let old = &laid.text()[cut.start..cut.end];
            let with = if wide {
                format!(
                    "{}{}",
                    ch.to_string().repeat(cut.cols / 2),
                    spaces(cut.cols % 2)
                )
            } else {
                ch.to_string().repeat(cut.cols)
            };
            let new = format!("{}{with}{}", spaces(cut.before), spaces(cut.after));
            (cut.start, old.to_owned(), new)
        });
        self.end_block(block, copy);
    }

    /// Makes one edit of what `each` does to each line of `block` that
    /// reaches into it, but a listing's `..`: where in the line, what it
    /// replaces, and with what.
    fn edit_block(
        &mut self,
        block: Block,
        copy: &mut BufferCopy,
        mut each: impl FnMut(&Laid, Cut) -> (usize, String, String),
    ) {
        let text = self.text(copy);
        let mut splice = Splice::new();
        for line in block.first..=block.last.min(text.last_line()) {
            let Some(laid) = text.laid(line) else {
                continue;
            };
            let cut = cut(&laid, block);
            if cut.cols == 0 || self.kept_line(&text, line) {
                continue;
            }
            let (at, old, new) = each(&laid, cut);
            splice.replace(text.line_start(line) + at, &old, &new);
        }
        let edit = splice.finish(text.rope().len_bytes());
        drop(text);
        if let Some(edit) = edit {
            self.make(edit, copy);
        }
    }

    /// Why a listing refuses a block of `rows` rows put from line `first`,
    /// if it does: one would go past its end, making a line with no mark,
    /// or on its `..`, which would then name something.
    fn listing_refuses(&self, first: u64, rows: usize, text: &Text<'_>) -> Option<&'static str> {
        self.hide?;
        let last = first.saturating_add(rows as u64).saturating_sub(1);
        if last > text.last_line() {
            return Some(PAST_LISTING);
        }
        (first..=last)
            .any(|line| self.kept_line(text, line))
            .then_some(PUT_ON_PARENT)
    }

    /// Whether `line` is one a listing keeps as it is, as its `..`.
    pub(super) fn kept_line(&self, text: &Text<'_>, line: u64) -> bool {
        self.keep.is_some_and(|keep| keep(&text.line_text(line)))
    }

    /// Whether `block` starts on a line a listing keeps as it is, where
    /// typing would go.
    fn starts_on_kept(&self, block: Block, copy: &BufferCopy) -> bool {
        self.kept_line(&self.text(copy), block.first)
    }

    /// Leaves the block, with the cursor at its top left, as Vim's commands
    /// leave it.
    fn end_block(&mut self, block: Block, copy: &BufferCopy) {
        self.leave_visual();
        let point = Point {
            line: block.first,
            col: block.left,
        };
        self.cursor = Cursor::new(on_character(point, &self.text(copy)));
    }

    /// `>`, or `<` if `out`, in a block: on each line long enough, the
    /// blanks at its left edge, and just before it, made `times` steps of
    /// `shiftwidth` longer; or as many columns of blanks from its left edge
    /// taken away, as there are.
    fn shift_block(&mut self, block: Block, times: usize, out: bool, copy: &mut BufferCopy) {
        let by = self
            .indent
            .step
            .saturating_mul(times)
            .min(super::MAX_INDENT);
        let (tabs, tabstop) = (!self.indent.spaces, self.indent.tabstop);
        let text = self.text(copy);
        let mut splice = Splice::new();
        for line in block.first..=block.last.min(text.last_line()) {
            let Some(laid) = text.laid(line) else {
                continue;
            };
            if laid.cells() <= block.left {
                continue;
            }
            let line_text = laid.text();
            let at = laid.byte_at(block.left);
            // The blanks from the left edge on.
            let end = line_text.len() - line_text[at..].trim_start_matches(blank).len();
            let (start, new) = if out {
                if end == at {
                    continue;
                }
                // To this column, keeping the blanks that still fit before
                // it as they are, a tab too, and spaces for the rest.
                let to = laid.col_of(end) - by.min(laid.col_of(end) - block.left);
                // A tab the edge cuts is spaces.
                let first = laid.col_of(at);
                let mut new = spaces(block.left - first);
                let (mut byte, mut col) = (at, block.left);
                while first == block.left && byte < end && laid.col_of(laid.char_end(byte)) <= to {
                    let next = laid.char_end(byte);
                    new.push_str(&line_text[byte..next]);
                    (byte, col) = (next, laid.col_of(next));
                }
                new.push_str(&spaces(to - col));
                (at, new)
            } else {
                // With those just before it.
                let start = line_text[..at].trim_end_matches(blank).len();
                let from = laid.col_of(start);
                let width = laid.col_of(end) - from + by;
                (start, blanks_from(from, width, tabs, tabstop))
            };
            let line_start = text.line_start(line);
            splice.replace(line_start + start, &line_text[start..end], &new);
        }
        let edit = splice.finish(text.rope().len_bytes());
        drop(text);
        if let Some(edit) = edit {
            self.make(edit, copy);
        }
        self.end_block(block, copy);
    }

    /// `I`, or `A` if `after`, in a block: insert mode on its first line,
    /// before it or after it, or at the line's end after `$`; what's typed
    /// goes on the others once insert mode ends.
    pub(super) fn insert_block(
        &mut self,
        after: bool,
        times: usize,
        block: Block,
        copy: &mut BufferCopy,
    ) {
        if self.starts_on_kept(block, copy) {
            self.refuse(ON_PARENT);
            return;
        }
        let at = match (after, block.to_end()) {
            (false, _) => At::Before(block.left),
            (true, false) => At::After(block.right + 1),
            (true, true) => At::End,
        };
        self.type_on_block(block, at, (times, true), copy);
    }

    /// Starts insert mode on `block`'s first line, `at` its place there,
    /// for what's typed, `times` over, to go on the others too. A tab there
    /// is split, or a short line padded, first. After `I` and `A`
    /// (`top_left`), the cursor goes back to the block's top left once
    /// it's done.
    fn type_on_block(
        &mut self,
        block: Block,
        at: At,
        (times, top_left): (usize, bool),
        copy: &mut BufferCopy,
    ) {
        let text = self.text(copy);
        let line = block.first;
        let line_start = text.line_start(line);
        let start = match (at, text.laid(line)) {
            (_, None) => line_start,
            (At::End, Some(laid)) => line_start + laid.text().len(),
            (At::Before(col) | At::After(col), Some(laid)) => {
                let (byte, old, new) = typed_at(&laid, line_start, col, "");
                drop(text);
                if old != new {
                    let len = copy.text().len_bytes();
                    self.make(Edit::replace(len, byte, &old, &new), copy);
                }
                let text = self.text(copy);
                let laid = text.laid(line);
                line_start + laid.map_or(0, |laid| laid.byte_at(col))
            }
        };
        let text = self.text(copy);
        let last = block.last.min(text.last_line());
        let end = text.line_end(line);
        let spread = Spread {
            start,
            end,
            tail: end - start,
            span: (text.line_start(line), text.line_end(last)),
            lines: last - line,
            at,
            back_to: top_left.then_some(Point {
                line,
                col: block.left,
            }),
            spoiled: false,
        };
        self.cursor = Cursor::new(text.point_of(start));
        drop(text);
        self.leave_visual();
        self.enter_insert(times);
        self.blocks.spread = Some(spread);
    }

    /// Insert mode on a block's first line is ending: types what was typed
    /// there on its other lines, as part of the same change, unless a line
    /// break was typed, or the line lost text. Returns where the cursor
    /// goes, if somewhere other than where insert mode leaves it: after
    /// `I` and `A` typed on every line, the block's top left.
    pub(super) fn spread(&mut self, copy: &mut BufferCopy) -> Option<Point> {
        let spread = self.blocks.spread.take()?;
        if spread.spoiled {
            self.status = Some(SPOILED.into());
            return None;
        }
        let text = self.text(copy);
        let len = text.rope().len_bytes();
        if spread.span.1 > len || spread.end > len {
            return None;
        }
        let (first, last) = (
            text.point_of(spread.span.0).line,
            text.point_of(spread.span.1).line,
        );
        // A line break typed: on the first line only, as in Vim.
        if last - first != spread.lines || text.point_of(spread.end).line != first {
            return None;
        }
        let grown =
            (spread.end.checked_sub(spread.start)).and_then(|now| now.checked_sub(spread.tail));
        let typed = match grown {
            Some(len) if len > 0 && spread.start >= text.line_start(first) => {
                Some(text.slice(spread.start, spread.start + len))
            }
            _ => None,
        };
        // With nothing typed, the cursor steps back as after any insert.
        let typed = typed?;
        let mut splice = Splice::new();
        for line in first + 1..=last {
            let Some(laid) = text.laid(line) else {
                continue;
            };
            if self.kept_line(&text, line) {
                continue;
            }
            let line_start = text.line_start(line);
            let (at, old, new) = match spread.at {
                At::Before(col) if laid.cells() < col => continue,
                At::Before(col) | At::After(col) => typed_at(&laid, line_start, col, &typed),
                At::End => (line_start + laid.text().len(), String::new(), typed.clone()),
            };
            splice.replace(at, &old, &new);
        }
        let edit = splice.finish(text.rope().len_bytes());
        drop(text);
        if let Some(edit) = edit {
            self.make(edit, copy);
        }
        spread.back_to
    }

    /// Puts `register`, a block, `times` over, after the cursor, or before
    /// it if `before`: a row on each line from the cursor's, at the same
    /// column, padded to the block's width where text follows, and on new
    /// lines past the file's end. The cursor goes to its first character.
    pub(super) fn put_block(
        &mut self,
        before: bool,
        times: usize,
        register: &Register,
        copy: &mut BufferCopy,
    ) {
        let width = register.block.unwrap_or(0);
        let rows: Vec<&str> = register.text.split('\n').collect();
        let text = self.text(copy);
        let at = self.cursor.at;
        let laid = text.laid(at.line);
        let col = match &laid {
            Some(laid) if !before && laid.cells() > 0 => {
                let byte = laid.byte_at(at.col);
                laid.col_of(laid.char_end(byte))
            }
            _ => at.col,
        };
        drop(laid);
        if let Some(why) = self.listing_refuses(at.line, rows.len(), &text) {
            drop(text);
            self.refuse(why);
            return;
        }
        let tabstop = self.indent.tabstop;
        let piece = |row: &str, short: bool| {
            let pad = spaces(width.saturating_sub(width_at(row, col, tabstop)));
            let mut piece = String::new();
            for time in 0..times {
                piece.push_str(row);
                if time + 1 < times || !short {
                    piece.push_str(&pad);
                }
            }
            piece
        };
        let mut splice = Splice::new();
        let len = text.rope().len_bytes();
        let mut added = String::new();
        for (index, row) in rows.iter().enumerate() {
            let line = at.line + index as u64;
            if let Some(laid) = text.laid(line) {
                let short = col >= laid.cells();
                let line_start = text.line_start(line);
                let (byte, old, new) = typed_at(&laid, line_start, col, &piece(row, short));
                splice.replace(byte, &old, &new);
            } else {
                added.push('\n');
                added.push_str(&spaces(col));
                added.push_str(&piece(row, true));
            }
        }
        if !added.is_empty() {
            // After a last line break, the new lines go before it.
            let ends_with_break = len > 0 && text.rope().byte(len - 1) == b'\n';
            if ends_with_break {
                added.remove(0);
                added.push('\n');
                splice.replace(len, "", &added);
            } else {
                splice.replace(len, "", &added);
            }
        }
        let edit = splice.finish(len);
        drop(text);
        if let Some(edit) = edit {
            self.make(edit, copy);
        }
        let text = self.text(copy);
        self.cursor = Cursor::new(on_character(Point { line: at.line, col }, &text));
    }

    /// `p` or `P` over a block: takes it out, keeping it after `p`, as a
    /// delete, then puts what `register` holds: a block, at its top left;
    /// characters on one line, before it on each of its lines; anything
    /// else, as it would be put at the cursor.
    pub(super) fn put_over_block(
        &mut self,
        before: bool,
        times: usize,
        block: Block,
        register: &Register,
        copy: &mut BufferCopy,
        registers: &mut Registers,
    ) {
        // Refused before anything's taken, rather than half done.
        let rows_put = register.text.split('\n').count();
        if register.block.is_some()
            && let Some(why) = self.listing_refuses(block.first, rows_put, &self.text(copy))
        {
            self.refuse(why);
            return;
        }
        let rows = self.block_rows(block, copy);
        let width = block.width().unwrap_or_else(|| {
            let widths = rows
                .iter()
                .map(|row| unicode_width::UnicodeWidthStr::width(&**row));
            widths.max().unwrap_or(0)
        });
        self.delete_block(block, copy);
        if !before {
            let small = block.first == block.last;
            let wrote = Wrote::Delete {
                small,
                history: !small,
            };
            registers.wrote(None, Register::block(&rows, width), wrote);
        }
        self.end_block(block, copy);
        if register.block.is_some() {
            self.put_block(true, times, register, copy);
        } else if !register.lines && !register.text.contains('\n') {
            let text = self.text(copy);
            let mut splice = Splice::new();
            let typed = register.text.repeat(times);
            for line in block.first..=block.last.min(text.last_line()) {
                let Some(laid) = text.laid(line) else {
                    continue;
                };
                if laid.cells() < block.left || self.kept_line(&text, line) {
                    continue;
                }
                let line_start = text.line_start(line);
                let (at, old, new) = typed_at(&laid, line_start, block.left, &typed);
                splice.replace(at, &old, &new);
            }
            let edit = splice.finish(text.rope().len_bytes());
            drop(text);
            if let Some(edit) = edit {
                self.make(edit, copy);
            }
        } else if register.lines {
            self.cursor = Cursor::new(Point {
                line: block.last,
                col: 0,
            });
            self.put_register(false, times, copy, register);
        } else {
            self.put_register(true, times, copy, register);
        }
    }

    /// How much `block` takes, for `.` to take as much again from the
    /// cursor: as many lines, and as many columns, or to each line's end.
    pub(super) fn block_extent(block: Block) -> Extent {
        Extent::Block {
            lines: block.last - block.first + 1,
            cols: block.width(),
        }
    }

    /// Selects a block as big as `lines` lines by `cols` columns, or to
    /// each line's end, from the cursor's character, for `.` to make a
    /// change made to a block again on it.
    pub(super) fn select_block_extent(
        &mut self,
        lines: u64,
        cols: Option<usize>,
        copy: &BufferCopy,
    ) {
        let text = self.text(copy);
        let at = on_character(self.cursor.at, &text);
        let last = at
            .line
            .saturating_add(lines.saturating_sub(1))
            .min(text.last_line());
        let block = Block {
            first: at.line,
            last,
            left: at.col,
            right: cols.map_or(Block::TO_END, |cols| at.col + cols.max(1) - 1),
        };
        drop(text);
        self.mode = Mode::Visual(Selection::Block);
        self.anchor = Some(at);
        self.cursor = Cursor::new(at);
        self.blocks.repeat = Some(block);
        self.repeat_selection = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(left: usize, right: usize) -> Block {
        Block {
            first: 0,
            last: 0,
            left,
            right,
        }
    }

    fn laid(text: &str) -> Laid {
        Laid::new(text.into(), 4)
    }

    #[test]
    fn a_cut_takes_whole_characters_and_spaces_for_those_its_edges_cut() {
        // A tab from 2 to 3, cut by the left edge at 3.
        let cut = cut(&laid("ab\tcd"), block(3, 4));
        assert_eq!((cut.start, cut.end, cut.whole), (2, 4, (3, 4)));
        assert_eq!((cut.before, cut.lead, cut.trail, cut.after), (1, 1, 0, 0));
        // A tab from 1 to 3, cut by the right edge at 1.
        let cut = super::cut(&laid("a\tbc"), block(0, 1));
        assert_eq!((cut.start, cut.end, cut.whole), (0, 2, (0, 1)));
        assert_eq!((cut.trail, cut.after, cut.cols), (1, 2, 2));
        // One tab across the whole block.
        let cut = super::cut(&laid("\tab"), block(1, 2));
        assert_eq!((cut.start, cut.end), (0, 1));
        assert_eq!((cut.before, cut.lead, cut.after, cut.cols), (1, 2, 1, 2));
        // A wide character cut by the left edge.
        let cut = super::cut(&laid("a中b"), block(2, 3));
        assert_eq!((cut.before, cut.lead, cut.whole), (1, 1, (4, 5)));
    }

    #[test]
    fn a_line_ending_before_the_block_is_short_and_one_ending_at_it_is_not() {
        assert!(cut(&laid("a"), block(2, 3)).short);
        let at_edge = cut(&laid("ab"), block(2, 3));
        assert!(!at_edge.short);
        assert_eq!(at_edge.cols, 0);
        let inside = cut(&laid("abc"), block(2, 3));
        assert_eq!((inside.whole, inside.cols), ((2, 3), 1));
    }

    #[test]
    fn typing_inside_a_tab_splits_it_and_inside_a_wide_character_pads_before_it() {
        assert_eq!(
            typed_at(&laid("\tab"), 10, 2, "X"),
            (10, "\t".into(), "  X  ".into())
        );
        assert_eq!(
            typed_at(&laid("a中b"), 0, 2, "X"),
            (1, String::new(), " X".into())
        );
        assert_eq!(
            typed_at(&laid("a"), 0, 4, "X"),
            (1, String::new(), "   X".into())
        );
        assert_eq!(
            typed_at(&laid("abc"), 0, 1, "X"),
            (1, String::new(), "X".into())
        );
    }

    #[test]
    fn blanks_reach_their_width_with_tabs_to_each_stop_they_pass() {
        assert_eq!(blanks_from(2, 4, true, 4), "\t  ");
        assert_eq!(blanks_from(2, 4, false, 4), "    ");
        assert_eq!(blanks_from(0, 8, true, 4), "\t\t");
        assert_eq!(blanks_from(3, 0, true, 4), "");
    }
}
