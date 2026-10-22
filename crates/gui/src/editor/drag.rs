//! Selecting with the mouse, as Neovim does with `mouse=a`: dragging
//! selects characters in visual mode, from where the button went down to
//! where the mouse is, or with Alt held, a block of columns; a double
//! click selects a word, and dragging after it whole words; a triple click
//! selects the line, and dragging after it whole lines. Letting go leaves the selection for `y`, `d`, `c`, `>` and
//! the rest. A plain click puts the cursor there and ends a selection,
//! which `gv` selects again, as one made with keys: the desk notes it as
//! the button goes down and comes up (see [`super::visual`]).
//!
//! The mouse finds its character through the rows as they're drawn: on a
//! row a long line goes on over, the line's character there; on an inlay
//! hint, the character it's drawn before; past a row's end, its last.
//! Only the row under the mouse is laid out, and only once the mouse is
//! over another cell, and the window is drawn again only when the
//! selection changes, so moving the mouse costs next to nothing.
//!
//! Past the view's top or bottom, the view scrolls while the button is
//! held, faster the further past the mouse is, as a chat's does, and the
//! selection goes on to the rows that come into view. The window wakes
//! for that only while it goes on: once the mouse comes back, lets go, or
//! the view can go no further, nothing wakes it. While the button is held
//! the cursor stays under the mouse, the view kept still around it;
//! letting go keeps `scrolloff` rows around the cursor again, as a click
//! does.
//!
//! Where the button went down, and the word a double click took, are kept
//! as bytes of the text, moved with edits made meanwhile, as an agent's;
//! a key typed during the drag ends it, as does the text being replaced.
//!
//! A selection made from insert mode is Neovim's "(insert) VISUAL", as the
//! bar says: once it ends, by an operator, Esc, a click or a command done
//! to it some other way, the editor goes back to insert mode, at the
//! cursor; a macro goes back once it's done. Typing `:` on it, `gra`, or
//! leaving the pane leave it in normal mode.

use std::time::{Duration, Instant};

use clackatoo_protocol::Edit;
use clackatoo_text::Bias;

use super::text::Text;
use super::{Asking, BufferCopy, Editor, Mode, on_character, wrap};
use crate::motion::{Cursor, Point, last_row, rows_down, rows_up};
use crate::objects::Object;
use crate::vim::Selection;

/// How long a frame is: the view never scrolls by itself more often.
const FRAME: Duration = Duration::from_millis(16);

/// The most time one step makes up for, as after the window was held up:
/// the view never jumps.
const LONGEST_STEP: Duration = Duration::from_millis(100);

/// How many rows a second the view scrolls by itself with the mouse `past`
/// rows beyond its edge: a few just past it, and more the further it goes,
/// up to a few hundred, as a chat's (see `chat::paint::edge`).
fn speed(past: u64) -> f64 {
    let past = u32::try_from(past).unwrap_or(u32::MAX);
    (3.0 + 8.0 * f64::from(past)).min(300.0)
}

/// How long the view takes to scroll a row by itself with the mouse
/// `past` rows beyond its edge.
fn step(past: i64) -> Duration {
    Duration::from_secs_f64(1.0 / speed(past.unsigned_abs()))
}

/// The mouse selecting, while its button is held.
#[derive(Clone, Copy, Debug)]
pub(super) struct Drag {
    /// The byte of the character the button went down on.
    origin: usize,
    by: By,
    /// Whether anything is selected: a plain click selects nothing until
    /// the mouse moves off its character.
    started: bool,
    /// The cell of the pane the mouse was last over, as its row and
    /// column, which can be off the pane: it's followed only once it
    /// changes.
    cell: (i64, i64),
    /// While the mouse is past the view's top or bottom, by how many rows,
    /// as a negative number above it, and when the view last scrolled for
    /// that.
    edge: Option<(i64, Instant)>,
    /// In a drag by words, the word the mouse was last in, as the bytes
    /// of its first and last characters: while the mouse stays in it,
    /// it isn't looked for again, however long it is.
    word: Option<(usize, usize)>,
}

/// What a drag selects by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum By {
    Characters,
    /// A block of columns, with Alt held, from the character clicked to
    /// the one under the mouse.
    Block,
    /// Whole words, from the word clicked twice: the bytes of its first
    /// and last characters.
    Words(usize, usize),
    Lines,
}

impl Editor {
    /// As [`press_with`](Self::press_with), with no block.
    #[cfg(test)]
    pub(crate) fn press(&mut self, at: (usize, usize), clicks: u8, copy: &mut BufferCopy) {
        self.press_with(at, clicks, false, copy);
    }

    /// The mouse button went down at row `line` and column `col` of the
    /// pane, the `clicks`th time in a row: once puts the cursor there,
    /// ending a selection, and a drag from it selects characters, or a
    /// block if `block`, as with Alt held; twice selects the word there and
    /// three times the line. A drag follows (see
    /// [`drag_to`](Self::drag_to)) until [`release`](Self::release).
    pub(crate) fn press_with(
        &mut self,
        (line, col): (usize, usize),
        clicks: u8,
        block: bool,
        copy: &mut BufferCopy,
    ) {
        self.panel = None;
        self.actions = None;
        self.close_completion();
        self.drag = None;
        self.forget_stale_insert();
        // A double or triple click is on the character the first was on,
        // though letting go of that may have scrolled the view.
        let first = (self.clicked)
            .filter(|_| clicks > 1)
            .map(|byte| self.text(copy).point_of(byte));
        let first = first.map(|at| (at, at.col));
        let Some((at, want)) = first.or_else(|| self.point_under(line, col, copy)) else {
            return;
        };
        // A click ends a selection, and one the mouse made from insert
        // mode goes back to it. What a double click selects is a new go of
        // visual mode, so `gv` swaps it with the one ended, as after a
        // drag; a triple click goes on with the double click's.
        self.leave_visual();
        if clicks < 3 {
            self.selecting = false;
        }
        self.back_to_insert();
        let text = self.text(copy);
        let on = on_character(at, &text);
        let (origin, at_byte) = (text.byte_at(on), text.byte_at(at));
        let word = (clicks == 2).then(|| word_around(&text, on));
        drop(text);
        let by = match (clicks, word) {
            (1, _) => {
                self.place_click(at, want, copy);
                self.clicked = Some(at_byte);
                if block { By::Block } else { By::Characters }
            }
            (_, Some((first, last))) => {
                self.select_between(first, last, Selection::Characters, copy);
                let text = self.text(copy);
                By::Words(text.byte_at(first), text.byte_at(last))
            }
            _ => {
                self.select_between(on, on, Selection::Lines, copy);
                By::Lines
            }
        };
        self.drag = Some(Drag {
            origin,
            by,
            started: !matches!(by, By::Characters | By::Block),
            cell: (to_i64(line), to_i64(col)),
            edge: None,
            word: None,
        });
    }

    /// The mouse moved to row `line` and column `col` of the pane, which
    /// may be off it, with the button held, at `now`: the selection goes
    /// to the character there, or the view starts scrolling towards it.
    /// Returns whether the window must be redrawn.
    pub(crate) fn drag_to(
        &mut self,
        (line, col): (i64, i64),
        now: Instant,
        copy: &mut BufferCopy,
    ) -> bool {
        let Some(mut drag) = self.drag else {
            return false;
        };
        if drag.cell == (line, col) {
            return false;
        }
        drag.cell = (line, col);
        let height = to_i64(self.height.max(1));
        let past = if line < 0 {
            line
        } else {
            (line - height + 1).max(0)
        };
        let mut scrolled = false;
        drag.edge = match drag.edge {
            _ if past == 0 => None,
            // Further or nearer, it goes on from when it last scrolled.
            Some((_, since)) => Some((past, since)),
            // Just past the edge, it scrolls a row at once.
            None => {
                scrolled = self.scroll_rows(past.signum(), copy);
                scrolled.then_some((past, now))
            }
        };
        self.drag = Some(drag);
        self.extend_drag(copy) | scrolled
    }

    /// The mouse button came up: the drag ends, leaving what it selected
    /// selected, and the view keeps `scrolloff` rows around the cursor
    /// again. Returns the text selected, for the primary selection, if
    /// `wanted` and anything is.
    pub(crate) fn release(&mut self, wanted: bool, copy: &BufferCopy) -> Option<String> {
        let drag = self.drag.take()?;
        self.follow(copy);
        if !drag.started || !wanted {
            return None;
        }
        self.selected_text(copy).filter(|text| !text.is_empty())
    }

    /// Moves where the drag started, and the character clicked, with
    /// `edit`, made by anyone, as the cursor moves.
    pub(super) fn drag_follows(&mut self, edit: &Edit) {
        let map = |byte: &mut usize| *byte = edit.map(*byte, Bias::Before);
        if let Some(clicked) = &mut self.clicked {
            map(clicked);
        }
        if let Some(drag) = &mut self.drag {
            map(&mut drag.origin);
            if let By::Words(first, last) = &mut drag.by {
                map(first);
                map(last);
            }
            drag.word = None;
        }
    }

    /// Whether visual mode came from insert mode, by the mouse, and goes
    /// back there once it ends: the bar says `(insert) VISUAL`.
    pub(crate) fn visual_from_insert(&self) -> bool {
        self.insert_after && matches!(self.mode, Mode::Visual(_))
    }

    /// Says whether the keys coming are a binding's or a macro's, being
    /// typed: a selection made from insert mode that they end goes back
    /// there only once they're all done.
    pub(crate) fn typing_framed(&mut self, framed: bool) {
        self.framed = framed;
    }

    /// Forgets that a selection came from insert mode, if it's ended in a
    /// way that didn't go back there: a key, or a click, then finds the
    /// editor in normal mode, as it was left.
    pub(super) fn forget_stale_insert(&mut self) {
        if !matches!(self.mode, Mode::Visual(_)) {
            self.insert_after = false;
        }
    }

    /// The window lost the focus mid-drag, so the button's release may
    /// never come: the drag ends, leaving what it selected, and the view
    /// stops scrolling by itself.
    pub(crate) fn drag_lost(&mut self) {
        self.drag = None;
    }

    /// Scrolls the view on, as far as the time since it last did says, as
    /// of `now`, while a drag is past its top or bottom. Returns whether
    /// it scrolled.
    pub(super) fn tick_drag(&mut self, now: Instant, copy: &mut BufferCopy) -> bool {
        let Some(Drag {
            edge: Some((past, since)),
            ..
        }) = self.drag
        else {
            return false;
        };
        // A row for each step the time since makes up.
        let elapsed = now.saturating_duration_since(since).min(LONGEST_STEP);
        let rows = elapsed.as_nanos() / step(past).as_nanos().max(1);
        let Ok(rows) = i64::try_from(rows) else {
            return false;
        };
        if rows == 0 {
            return false;
        }
        let scrolled = self.scroll_rows(rows * past.signum(), copy);
        if let Some(drag) = &mut self.drag {
            // Once it can go no further, nothing wakes the window for it.
            drag.edge = scrolled.then_some((past, now));
        }
        if scrolled {
            self.extend_drag(copy);
        }
        scrolled
    }

    /// When the view next scrolls by itself, for a drag past its edge.
    pub(super) fn drag_wakes(&self) -> Option<Instant> {
        let (past, since) = self.drag?.edge?;
        Some(since + step(past).max(FRAME))
    }

    /// Goes back to insert mode once a selection the mouse made from it
    /// has ended, as Neovim does, unless the command that ended it went
    /// to insert mode itself, as `c` does, or is still being typed, or
    /// asked for a menu of code actions, which is picked from in normal
    /// mode, as `gra` does.
    pub(crate) fn back_to_insert(&mut self) {
        if !self.insert_after || matches!(self.mode, Mode::Visual(_)) {
            return;
        }
        self.insert_after = false;
        let menu = self.actions.is_some() || matches!(self.asked, Some((_, Asking::Actions(_))));
        if self.mode == Mode::Normal && self.prompt.is_none() && !self.keys.pending() && !menu {
            self.new_step = true;
            self.enter_insert(1);
        }
    }

    /// The character at row `line` and column `col` of the pane, as a
    /// click finds it: on a line's number, the line's start, and beside a
    /// row the line goes on over, the row's; past a row's end, its last
    /// character, or in insert mode the cell after it; on an inlay hint,
    /// the character it's drawn before. Below the text, on its last row.
    /// Only that row is laid out. With it, the column of the line clicked,
    /// past its end or in the middle of a tab or a wide character as it
    /// may be, which the cursor goes on aiming for, as Vim's
    /// `jump_to_mouse()` sets `curswant`.
    fn point_under(
        &mut self,
        line: usize,
        col: usize,
        copy: &BufferCopy,
    ) -> Option<(Point, usize)> {
        self.place_gutter(copy);
        let text = self.text(copy);
        self.clamp_top(&text);
        let end = self.view_end(&text);
        let line = line.min(self.height.max(1) - 1) as u64;
        let row = rows_down(self.top.row(&text), line, end, &text);
        let mut placed = Vec::new();
        let shown = wrap::view(&text, (row, 1), end, self.left, &mut placed);
        let row = shown.first()?;
        let (col, want) = match col.checked_sub(self.gutter) {
            // Past the row's end, its last character; on an inlay hint,
            // the character it's drawn before.
            Some(col) => {
                let cell = row.cell_at(col, &placed);
                (cell.min(row.end().saturating_sub(1)).max(row.start), cell)
            }
            None if row.row == 0 => (0, 0),
            None => (row.start, row.start),
        };
        let at = Point {
            line: row.line.min(text.last_line()),
            col,
        };
        Some((at, want))
    }

    /// Puts the cursor at `at`, as a click does, aiming for column `want`,
    /// the one clicked: in insert mode, where typing goes on from, which it
    /// moved, as the arrow keys move it.
    fn place_click(&mut self, at: Point, want: usize, copy: &BufferCopy) {
        self.cursor = Cursor::new(at);
        if self.mode == Mode::Insert {
            let byte = self.insert_byte(copy);
            self.cursor = Cursor::new(self.text(copy).point_of(byte));
            self.moved_while_inserting(copy);
        } else {
            self.normal_cursor(copy);
        }
        // Aiming for the column clicked, as Vim's `jump_to_mouse()` sets
        // `curswant` to it, before the wheel moves the cursor.
        self.cursor = Cursor::wanting(self.cursor.at, want);
    }

    /// Selects from `anchor` to `head`, by `kind`, in visual mode: from
    /// insert mode, until the selection ends.
    fn select_between(
        &mut self,
        anchor: Point,
        head: Point,
        kind: Selection,
        copy: &mut BufferCopy,
    ) {
        if self.mode == Mode::Insert {
            self.leave_insert(copy);
            self.insert_after = true;
        }
        self.mode = Mode::Visual(kind);
        self.anchor = Some(anchor);
        self.cursor = Cursor::new(head);
        self.settle_want(copy);
    }

    /// Takes the selection on to the character under the mouse, held at
    /// the cell the drag last saw, or at the view's edge row if that's
    /// past it. Returns whether the selection changed.
    fn extend_drag(&mut self, copy: &mut BufferCopy) -> bool {
        let Some(mut drag) = self.drag else {
            return false;
        };
        let (line, col) = drag.cell;
        let line = usize::try_from(line.max(0)).unwrap_or(usize::MAX);
        let col = usize::try_from(col.max(0)).unwrap_or(usize::MAX);
        let Some((at, _)) = self.point_under(line, col, copy) else {
            return false;
        };
        let text = self.text(copy);
        let at = on_character(at, &text);
        let point = |byte| on_character(text.point_of(byte), &text);
        let origin = point(drag.origin);
        let (anchor, head, kind) = match drag.by {
            // Until the mouse leaves the character clicked, nothing is
            // selected.
            By::Characters | By::Block if !drag.started && at == origin => return false,
            By::Characters => (origin, at, Selection::Characters),
            By::Block => (origin, at, Selection::Block),
            // Forwards from the word clicked, to the end of the word the
            // mouse is in; backwards, from its end to that word's start.
            By::Words(first, last) => {
                let byte = text.byte_at(at);
                let (start, end) = match drag.word {
                    Some((start, end)) if start <= byte && byte <= end => (start, end),
                    _ => {
                        let (start, end) = word_around(&text, at);
                        (text.byte_at(start), text.byte_at(end))
                    }
                };
                drag.word = Some((start, end));
                if byte >= first {
                    (point(first), point(end.max(last)), Selection::Characters)
                } else {
                    (point(last), point(start), Selection::Characters)
                }
            }
            By::Lines => (origin, at, Selection::Lines),
        };
        drop(text);
        drag.started = true;
        self.drag = Some(drag);
        let was = (self.mode, self.anchor, self.cursor.at);
        self.select_between(anchor, head, kind, copy);
        (self.mode, self.anchor, self.cursor.at) != was
    }

    /// Scrolls the view `rows` screen rows down, or up if negative, no
    /// further than the file's start, or than shows its last row at the
    /// view's bottom. Returns whether it moved.
    fn scroll_rows(&mut self, rows: i64, copy: &BufferCopy) -> bool {
        let text = self.text(copy);
        self.clamp_top(&text);
        let top = self.top.row(&text);
        let to = if rows < 0 {
            rows_up(top, rows.unsigned_abs(), 0, &text)
        } else {
            let last = text.last_line();
            let lowest = rows_up(
                last_row(&text, last),
                self.height.max(1) as u64 - 1,
                0,
                &text,
            );
            rows_down(top, rows.unsigned_abs(), last, &text)
                .min(lowest)
                .max(top)
        };
        if to == top {
            return false;
        }
        self.top = wrap::Top::of(to, &text);
        true
    }

    /// The text selected in visual mode, whole lines with their line
    /// breaks, as `y` would copy it.
    fn selected_text(&self, copy: &BufferCopy) -> Option<String> {
        let (Mode::Visual(kind), Some(anchor)) = (self.mode, self.anchor) else {
            return None;
        };
        let at = self.cursor.at;
        let (first, last) = if anchor <= at {
            (anchor, at)
        } else {
            (at, anchor)
        };
        if kind == Selection::Block {
            return Some(self.block_text(self.selected_block(copy)?, copy));
        }
        let text = self.text(copy);
        let (start, end) = match kind {
            Selection::Lines => (text.line_start(first.line), text.line_start(last.line + 1)),
            Selection::Characters | Selection::Block => (text.byte_at(first), text.char_end(last)),
        };
        Some(text.slice(start, end))
    }
}

/// The word around `at`, as `iw` finds it, by the text's word
/// characters: its first and last characters.
fn word_around(text: &Text<'_>, at: Point) -> (Point, Point) {
    (Object::Word.find(text, at, true, 1)).map_or((at, at), |found| (found.start(), found.last()))
}

/// `value` as a row or column that can be off the pane.
fn to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use clackatoo_protocol::{BufferId, Size};

    use super::{BufferCopy, By, Editor, speed};
    use crate::options::Options;

    #[test]
    fn the_further_past_the_edge_the_faster_it_scrolls() {
        assert!(speed(1) > 0.0);
        assert!(speed(1) < speed(4));
        assert!(speed(4) < speed(20));
        // Not without end.
        assert!((speed(u64::MAX) - speed(1000)).abs() < f64::EPSILON);
    }

    #[test]
    fn a_word_drag_looks_for_the_word_under_the_mouse_once_while_its_in_it() {
        let text = format!("start {} end\n", "x".repeat(100_000));
        let mut copy = BufferCopy::scratch(BufferId(1), "/a.txt", &text);
        let options = Options {
            number: false,
            relativenumber: false,
            gitsigns: false,
            wrap: false,
            ..Options::default()
        };
        let mut editor = Editor::new(BufferId(1), &options);
        editor.resize(Size::new(40, 10).unwrap());
        editor.settle(&copy);
        let now = Instant::now();
        editor.press((0, 1), 2, &mut copy);
        editor.drag_to((0, 8), now, &mut copy);
        let drag = editor.drag.unwrap();
        assert_eq!(drag.by, By::Words(0, 4));
        assert_eq!(drag.word, Some((6, 100_005)), "the long word, found");
        // Kept as the mouse goes on in it: a word said to end sooner is
        // taken as the word, so it wasn't looked for again.
        editor.drag.as_mut().unwrap().word = Some((6, 9));
        editor.drag_to((0, 9), now, &mut copy);
        assert_eq!(editor.cursor.at.col, 9);
        // Out of it, the next is looked for.
        editor.drag_to((0, 0), now, &mut copy);
        assert_eq!(editor.drag.unwrap().word, Some((0, 4)));
    }
}
