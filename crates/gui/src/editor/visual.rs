//! What a selection leaves behind once visual mode ends: the selection
//! itself, which `gv` selects again, and for a change made to it, how
//! much it took, which `.` takes again from the cursor.
//!
//! Each file keeps its last selection, as Vim keeps one for each buffer,
//! with the marks (see [`Marks::select`]): `<` and `>` are its ends, so
//! `gv` and `:'<,'>` agree, and going to another file and back keeps it.
//! The selection is noted as each key in visual mode starts and ends, and
//! as the mouse makes it, and is the last however visual mode ends: by a
//! command, Esc, a click or the focus leaving. It's kept as bytes of the
//! text moved with every edit, anyone's, so `gv` finds the same
//! characters after lines are put in above them, text is typed in them or
//! their case is changed; deleted and put back, as by an undo, it's all
//! there again. Vim keeps a line and a column, which only lines put in or
//! taken out move. In visual mode, `gv` selects the selection before the
//! one being made, which becomes the last, so it swaps between the two,
//! as in Vim. A selection `.` makes from the cursor isn't one: it's
//! neither noted nor marked, as in Vim.
//!
//! `.` after a change made in visual mode makes it again on as much text
//! from the cursor, as Vim does: as many lines, for whole lines; for
//! characters on one line, as many columns, so a tab counts for the
//! columns it takes; for characters over several lines, as many lines, to
//! the same column of the last, or to its end if the selection was taken
//! there with `$`. A count before `.` changes nothing, as in Vim: the
//! change keeps its own, as `V3>` indents by three steps each time.
//!
//! Where clackatoo differs from Vim: as many columns as a selection took
//! take no line break past a shorter line's end, where Vim's join the
//! lines; to a line's end after `$`, they do, as Vim's. In visual
//! mode, `.` makes a change made in visual mode again on what's selected,
//! where Vim's does nothing, so `V>` then `.` indents the lines again, as
//! `>` keeps them selected. And `.` after `p` over a selection puts the
//! same text over as much again, where Vim's only deletes it.
//!
//! A block of columns, as Ctrl+V selects, is the same, but for what's
//! done to one, which is in [`super::block`].
//!
//! A folder's listing hides each line's mark, which says what entry it
//! is: `r` leaves the marks as they are, and a change that would take
//! one with the text before it, joining lines, is refused, as `J` is. Its
//! `..` line names nothing as it is: `r` leaves it as it is, and `J`
//! won't join it.

use std::rc::Rc;

use clackatoo_protocol::Edit;

use super::marks::{Marks, Selected, Shape};
use super::registers::Register;
use super::text::Text;
use super::{BufferCopy, Editor, Mode, on_character};
use crate::motion::{Cursor, Motion, Point};
use crate::vim::{Operator, Selection, Target};

/// What the bar says when a change would join a listing's lines.
pub(super) const JOINS_ENTRIES: &str = "a listing's lines can't be joined: act on whole lines";

/// What the bar says when `>` or `<` is pressed in a listing.
pub(super) const INDENTS_ENTRIES: &str =
    "a listing's lines can't be indented: a name may start with a space";

/// How much a change made in visual mode took, for `.` to take as much
/// from the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Extent {
    /// Whole lines, this many.
    Lines(u64),
    /// Characters on one line, across this many columns.
    Across(usize),
    /// Characters over `lines` lines, to column `to` of the last.
    Down { lines: u64, to: usize },
    /// Characters over `lines` lines, one or more, to just past the last's
    /// last character, as a selection taken there with `$` is, so its
    /// line break goes too.
    ToEnd { lines: u64 },
    /// A block of `lines` lines, `cols` columns wide, or to each line's end
    /// if `None` (see [`super::block`]).
    Block { lines: u64, cols: Option<usize> },
}

/// What `.` makes again of a change made in visual mode, besides its
/// command: how much it took, and what it put, if it put over the
/// selection, which `.` puts again.
#[derive(Clone, Debug)]
pub(super) struct Again {
    pub(super) extent: Extent,
    pub(super) put: Option<Rc<Register>>,
}

impl Editor {
    /// The selection, as the bytes of its first and last characters, and
    /// how it was made, while in visual mode.
    fn selection_now(&self, copy: &BufferCopy) -> Option<((usize, usize), Shape)> {
        let (Mode::Visual(kind), Some(anchor)) = (self.mode, self.anchor) else {
            return None;
        };
        let at = self.cursor.at;
        let (first, last) = if anchor <= at {
            (anchor, at)
        } else {
            (at, anchor)
        };
        let text = self.text(copy);
        let bytes = (text.byte_at(first), text.byte_at(last));
        let shape = Shape {
            kind,
            backward: at < anchor,
            to_end: self.cursor.want() == usize::MAX,
        };
        Some((bytes, shape))
    }

    /// Notes the selection as the file's last, in `marks`, while in visual
    /// mode: unless `.` made it, or a command is waiting with it. The
    /// first noted of a go of visual mode keeps the last before it, for
    /// `gv` to swap with.
    pub(crate) fn note_selection(&mut self, copy: &BufferCopy, marks: &mut Marks) {
        if self.repeat_selection || self.awaiting.is_some() {
            return;
        }
        let Some((bytes, shape)) = self.selection_now(copy) else {
            self.selecting = false;
            return;
        };
        marks.select(copy.file(), copy.text(), bytes, shape, !self.selecting);
        self.selecting = true;
    }

    /// `gv`: selects the file's last selection again, as it was made, with
    /// the cursor at the end it was at. In visual mode, it selects the one
    /// before what's selected, which becomes the last. Fails if there's
    /// none.
    pub(super) fn reselect(&mut self, copy: &BufferCopy, marks: &mut Marks) {
        let selected = if matches!(self.mode, Mode::Visual(_)) {
            marks.swap(copy.file())
        } else {
            marks.selected(copy.file()).cloned()
        };
        // None, or its anchor's line gone, fails, as Vim's `nv_gv_cmd()`
        // does when `vi_start` is past the last line.
        let anchor_gone = |selected: &Selected| {
            let [start, last] = selected.ends();
            let anchor = if selected.shape.backward { last } else { start };
            anchor.0 > self.text(copy).last_line()
        };
        let Some(selected) = selected.filter(|selected| !anchor_gone(selected)) else {
            self.fail();
            return;
        };
        self.select_kept(&selected, copy);
    }

    /// Selects `selected`, as `gv` does.
    fn select_kept(&mut self, selected: &Selected, copy: &BufferCopy) {
        let text = self.text(copy);
        let shape = selected.shape;
        // On a line there is, and just past its end, as Vim's selection can
        // be, if the column is there or further on.
        let point = |(line, col): (u64, usize)| {
            let line = line.min(text.last_line());
            let start = text.line_start(line);
            if start + col >= text.line_end(line) {
                return Point {
                    line,
                    col: text.line_cells(line),
                };
            }
            let rope = text.rope();
            let byte = rope.char_to_byte(rope.byte_to_char(start + col));
            on_character(text.point_of(byte), &text)
        };
        let [start, last] = selected.ends();
        let (first, last) = (point(start), point(last));
        let (anchor, head) = if shape.backward {
            (last, first)
        } else {
            (first, last)
        };
        drop(text);
        self.mode = Mode::Visual(shape.kind);
        self.anchor = Some(anchor);
        self.cursor = if shape.to_end {
            Cursor::wanting(head, usize::MAX)
        } else {
            Cursor::new(head)
        };
    }

    /// How much what's selected takes, for `.` to take as much again from
    /// the cursor, as Vim measures it: to the last column the last
    /// character takes, so a selection ending on a tab takes all of it.
    pub(super) fn extent(&self, copy: &BufferCopy) -> Option<Extent> {
        let (Mode::Visual(kind), Some(anchor)) = (self.mode, self.anchor) else {
            return None;
        };
        let at = self.cursor.at;
        let (start, end) = if anchor <= at {
            (anchor, at)
        } else {
            (at, anchor)
        };
        let lines = end.line - start.line + 1;
        match kind {
            Selection::Lines => return Some(Extent::Lines(lines)),
            Selection::Block => return self.selected_block(copy).map(Editor::block_extent),
            Selection::Characters => {}
        }
        if self.cursor.want() == usize::MAX {
            return Some(Extent::ToEnd { lines });
        }
        let last = last_col(&self.text(copy), end);
        Some(match lines {
            1 => Extent::Across(last + 1 - start.col.min(last)),
            _ => Extent::Down { lines, to: last },
        })
    }

    /// Selects as much as `extent` from the cursor, for `.` to make a
    /// change made in visual mode again on it. Across a line, it goes from
    /// the column the cursor is drawn at, a tab's last, as in Vim. The
    /// selection goes once the change is done.
    pub(super) fn select_extent(&mut self, extent: Extent, copy: &BufferCopy) {
        if let Extent::Block { lines, cols } = extent {
            return self.select_block_extent(lines, cols, copy);
        }
        let text = self.text(copy);
        let at = self.cursor.at;
        let down =
            |lines: u64| (at.line.saturating_add(lines.saturating_sub(1))).min(text.last_line());
        let (kind, head) = match extent {
            Extent::Lines(lines) => (
                Selection::Lines,
                Point {
                    line: down(lines),
                    col: at.col,
                },
            ),
            Extent::Across(cols) => {
                let from = drawn_col(&text, at);
                let col = from.saturating_add(cols.saturating_sub(1));
                (Selection::Characters, Point { line: at.line, col })
            }
            Extent::Down { lines, to } => (
                Selection::Characters,
                Point {
                    line: down(lines),
                    col: to,
                },
            ),
            // Just past the line's last character, so its line break goes
            // too, as Vim's.
            Extent::ToEnd { lines } => {
                let line = down(lines);
                let col = text.line_cells(line);
                (Selection::Characters, Point { line, col })
            }
            Extent::Block { .. } => return,
        };
        let head = if matches!(extent, Extent::ToEnd { .. }) {
            head
        } else {
            on_character(head, &text)
        };
        drop(text);
        self.mode = Mode::Visual(kind);
        self.anchor = Some(at);
        self.cursor = Cursor::new(head);
        self.repeat_selection = true;
    }

    /// `o`: the cursor goes to the selection's other end.
    pub(super) fn other_end(&mut self) {
        if let Some(anchor) = self.anchor {
            self.anchor = Some(self.cursor.at);
            self.cursor = Cursor::new(anchor);
        }
    }

    /// Ends the selection `.` made from the cursor, once its change is
    /// done, if the change didn't: `>` and `<` keep the lines selected, and
    /// `.` leaves the cursor at the first's first character that isn't
    /// blank, as Vim's `>` does.
    pub(super) fn end_repeat_selection(&mut self, copy: &BufferCopy) {
        if !std::mem::take(&mut self.repeat_selection) || !matches!(self.mode, Mode::Visual(_)) {
            return;
        }
        let first = self
            .anchor
            .map_or(self.cursor.at, |anchor| anchor.min(self.cursor.at));
        self.leave_visual();
        self.cursor = Cursor::new(Point {
            line: first.line,
            col: 0,
        });
        self.cursor.apply(Motion::FirstNonBlank, &self.text(copy));
    }

    /// `J` in visual mode: joins the lines selected, or with one, it and
    /// the next, ignoring a count, as Vim's does.
    pub(super) fn join_selection(&mut self, copy: &mut BufferCopy) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let at = self.cursor.at;
        let (first, last) = (anchor.line.min(at.line), anchor.line.max(at.line));
        self.leave_visual();
        self.cursor = Cursor::new(Point {
            line: first,
            col: 0,
        });
        let joins = usize::try_from(last - first).unwrap_or(usize::MAX).max(1);
        self.join(joins, copy);
    }

    /// `r` in visual mode: puts `ch` in place of every character selected,
    /// a tab or a wide character included, but the line breaks, what's
    /// hidden of each line and the lines kept as they are, leaving visual
    /// mode with the cursor at the selection's start, as Vim's does. Only
    /// what's replaced changes, so marks between stay put.
    pub(super) fn replace_selection(&mut self, ch: char, copy: &mut BufferCopy) {
        if let Some(block) = self.selected_block(copy) {
            return self.replace_block(ch, block, copy);
        }
        let Some(span) = self.span(Operator::Delete, Target::Selection, None, copy) else {
            return;
        };
        let text = self.text(copy);
        let (start, end) = match span.lines {
            Some((first, last)) => (text.line_start(first), text.line_end(last)),
            None => (span.start(), span.end()),
        };
        let first = text.point_of(start).line;
        let last = text.point_of(end.saturating_sub(1).max(start)).line;
        let mut edit = Edit::new();
        // How far through the text the edit has gone.
        let mut through = 0;
        for line in first..=last {
            if self.keep.is_some_and(|keep| keep(&text.line_text(line))) {
                continue;
            }
            let from = start.max(text.shown_start(line));
            let to = end.min(text.line_end(line));
            if from >= to {
                continue;
            }
            let old = text.slice(from, to);
            let new: String = (old.chars())
                .map(|old| if old == '\r' { old } else { ch })
                .collect();
            edit.retain(from - through).delete(&old).insert(&new);
            through = to;
        }
        edit.retain(text.rope().len_bytes() - through);
        let at = on_character(text.point_of(start), &text);
        drop(text);
        self.make(edit, copy);
        self.leave_visual();
        self.cursor = Cursor::new(at);
    }

    /// Whether bytes `start` to `end` of the text, characters rather than
    /// whole lines, take any of what's hidden of a line, as a folder's
    /// listing hides each line's mark: only a line after the first can
    /// have it taken, as the cursor never goes there. Nothing is looked at
    /// in a file, which hides nothing.
    pub(super) fn takes_hidden(&self, (start, end): (usize, usize), copy: &BufferCopy) -> bool {
        if self.hide.is_none() {
            return false;
        }
        let text = self.text(copy);
        let mut line = text.point_of(start).line + 1;
        while line <= text.last_line() && text.line_start(line) < end {
            if text.shown_start(line) > text.line_start(line) {
                return true;
            }
            line += 1;
        }
        false
    }

    /// Refuses a change that would join a listing's lines, saying why.
    pub(super) fn refuse_join(&mut self) {
        self.refuse(JOINS_ENTRIES);
    }

    /// Refuses a change, saying `why`: it isn't one for `.` to make again,
    /// and a selection goes.
    pub(super) fn refuse(&mut self, why: &str) {
        self.status = Some(why.to_owned());
        self.changing = None;
        self.leave_visual();
        self.fail();
    }
}

/// The last column the character at `at` takes: its first, or for a tab
/// or a wide character, the one it ends at.
fn last_col(text: &Text<'_>, at: Point) -> usize {
    let Some(laid) = text.laid(at.line) else {
        return at.col;
    };
    let next = laid.col_of(laid.char_end(laid.byte_at(at.col)));
    next.saturating_sub(1).max(at.col)
}

/// The column the normal-mode cursor at `at` is drawn at, as Vim counts
/// from it: a tab's last, and any other character's first.
fn drawn_col(text: &Text<'_>, at: Point) -> usize {
    let tab = text.laid(at.line).is_some_and(|laid| {
        let byte = laid.byte_at(at.col);
        laid.text()[byte..].starts_with('\t')
    });
    if tab { last_col(text, at) } else { at.col }
}
