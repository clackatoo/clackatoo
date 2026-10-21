//! Normal and visual mode: clackatoo's own cursor, selection, copying and
//! search.
//!
//! Keys are read by the grammar every pane reads them with (see
//! [`crate::vim`]), as [`TerminalKeys`] says a terminal reads them. The
//! cursor moves with Vim's motions over the whole document, history then
//! screen, and the view follows it. `v` and `V` select from where the
//! cursor was to where it goes, by character or by line, and `y` copies;
//! `y` also takes a motion or a text object (see [`super::operator`]).
//! `/` and `?` search forward and back, and `n` and `N` search again. The
//! daemon does the copying and searching, since it holds all of history
//! and the window only the lines it has shown.

use clackatoo_protocol::{Match, Position, Request};
use clackatoo_term::{MOST_LINES, MOST_TEXT};
use winit::keyboard::{Key, NamedKey};

use super::{Copy, Document, Lines, Mode, Pane, Pending, Selection};
use crate::cmdline::{Keyed, Line};
use crate::input::KeyPress;
use crate::motion::{Block, Cursor, Motion, Point, start_of_cell};
use crate::vim::{
    Action, Command, Dialect, Entry, Operator, Parsed, Read, Reader, Scroll, Stroke, Target,
    WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP, strokes,
};

/// Where a motion left the cursor on a line of history not fetched yet,
/// and whether it goes to the line's first non-blank, as `gg` does with
/// `startofline`, or else to the wanted column, once the line comes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Unplaced {
    at: Point,
    start_of_line: bool,
}

/// A search being typed.
#[derive(Debug)]
pub(super) struct Prompt {
    pub(super) line: Line,
    pub(super) backward: bool,
}

/// A command of a terminal's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TerminalAction {
    /// Esc, with nothing typed before it but a count: leaves visual mode,
    /// or else forgets the match found.
    Escape,
    /// `gx`: opens the link under the cursor, as in Vim.
    OpenLink,
    /// `]]`, or `[[` if not `forward`: to the start of the shell's block
    /// after the cursor's line, or before it.
    Block { forward: bool },
}

/// How a terminal reads keys: every motion, as a move and as what `y`
/// takes, with `y` its only operator and `i` its only way into insert
/// mode, but nothing that changes text, as it has none to change, and no
/// register, mark, `f` and its kin, `*`, `#` or `zt` and its kin yet. Esc,
/// `gx`, `[[` and `]]` are its own.
pub(super) struct TerminalKeys;

impl Dialect for TerminalKeys {
    type Own = TerminalAction;

    fn own(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        match stroke {
            Stroke::Escape => Some(Read::Yes(TerminalAction::Escape)),
            // An editor moves with them; a terminal is typed into with
            // them, in insert mode.
            Stroke::Enter
            | Stroke::Backspace
            | Stroke::Char {
                ch: ' ',
                ctrl: false,
            } => Some(Read::No),
            _ => None,
        }
    }

    fn after_g(
        &self,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        (stroke.plain()? == 'x').then_some(Read::Yes(TerminalAction::OpenLink))
    }

    fn after_bracket(
        &self,
        backward: bool,
        stroke: Stroke,
        _keys: &mut Reader<'_>,
        _visual: bool,
    ) -> Option<Read<Self::Own>> {
        let again = if backward { '[' } else { ']' };
        (stroke.plain()? == again)
            .then_some(Read::Yes(TerminalAction::Block { forward: !backward }))
    }

    fn takes(&self, command: &Command<Self::Own>) -> bool {
        if command.register.is_some() {
            return false;
        }
        match command.action {
            Action::Move(motion) | Action::Operate(Operator::Yank, Target::Motion(motion)) => {
                !matches!(motion, Motion::Find { .. })
            }
            Action::Insert(entry) => entry == Entry::Before,
            Action::Operate(
                Operator::Yank,
                Target::Lines | Target::Object { .. } | Target::Selection,
            )
            | Action::Visual { .. }
            | Action::BlockVisual
            | Action::Reselect
            | Action::OtherEnd
            | Action::OtherCorner
            | Action::Select { .. }
            | Action::Search { .. }
            | Action::SearchAgain { .. }
            | Action::Scroll(_)
            | Action::Own(_) => true,
            Action::Operate(..)
            | Action::DeleteChars { .. }
            | Action::DeleteToEnd { .. }
            | Action::Substitute
            | Action::Put { .. }
            | Action::Join
            | Action::Replace(_)
            | Action::Undo
            | Action::Redo
            | Action::Repeat
            | Action::SearchWord { .. }
            | Action::FindAgain { .. }
            | Action::View(_)
            | Action::SetMark(_)
            | Action::Mark { .. }
            | Action::Tilde => false,
        }
    }
}

impl Pane {
    /// A key pressed in normal or visual mode.
    pub(super) fn normal_key(&mut self, press: &KeyPress<'_>) -> bool {
        if self.prompt.is_some() {
            return self.prompt_key(press);
        }
        self.status = None;
        // Ctrl+C always reaches the program, and Ctrl+Shift+C copies what's
        // selected, as in other terminals, whatever's half typed.
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        if press.modifiers.ctrl && matches!(character, Some('c' | 'C')) {
            self.keys.clear();
            if character == Some('C') {
                return self.yank_selection();
            }
            self.send_keyed(vec![0x03]);
            return false;
        }
        let mut redraw = false;
        for stroke in strokes(press, false) {
            let visual = matches!(self.mode, Mode::Visual(_));
            redraw |= match self.keys.read(stroke, visual, &TerminalKeys) {
                // The keys typed are drawn in the bar while they wait, and
                // gone from it once they make nothing.
                Parsed::Incomplete | Parsed::Nothing => true,
                Parsed::Command(command) => self.execute(command),
            };
        }
        redraw
    }

    /// Does `command`, which [`TerminalKeys`] took.
    fn execute(&mut self, command: Command<TerminalAction>) -> bool {
        let count = command.count;
        let height = self.height();
        let half = i64::try_from((height / 2).max(1)).unwrap_or(1);
        let page = i64::try_from(height.max(1)).unwrap_or(1);
        let down = |down: bool, lines: i64| if down { lines } else { -lines };
        match command.action {
            Action::Move(motion) => self.move_cursor(motion, count),
            Action::Operate(Operator::Yank, Target::Motion(motion)) => {
                self.yank_motion(motion, count)
            }
            Action::Operate(Operator::Yank, Target::Lines) => self.yank_lines(command.times()),
            Action::Operate(Operator::Yank, Target::Selection) => self.yank_selection(),
            // Copied after `y`, selected in visual mode.
            Action::Operate(Operator::Yank, Target::Object { object, inner })
            | Action::Select { object, inner } => self.use_object(object, inner),
            Action::Insert(Entry::Before) => self.insert(),
            Action::Visual { lines: false } => self.toggle_visual(Selection::Characters),
            Action::Visual { lines: true } => self.toggle_visual(Selection::Lines),
            Action::BlockVisual => self.toggle_visual(Selection::Block),
            Action::Reselect => self.reselect(),
            Action::OtherCorner if self.mode == Mode::Visual(Selection::Block) => {
                self.other_corner()
            }
            Action::OtherEnd | Action::OtherCorner => self.other_end(),
            Action::Search { backward } => {
                self.prompt = Some(Prompt {
                    line: Line::default(),
                    backward,
                });
                true
            }
            Action::SearchAgain { reverse } => self.search_again(reverse),
            // Half and whole pages move the view and the cursor together.
            Action::Scroll(Scroll::HalfPage { down: is_down }) => self.page(down(is_down, half)),
            Action::Scroll(Scroll::Page { down: is_down }) => self.page(down(is_down, page)),
            // A line of view, keeping the cursor in it.
            Action::Scroll(Scroll::Line { down: is_down }) => self.scroll_view(down(!is_down, 1)),
            Action::Own(TerminalAction::Escape) => {
                if matches!(self.mode, Mode::Visual(_)) {
                    self.leave_visual();
                } else {
                    self.found = None;
                }
                true
            }
            Action::Own(TerminalAction::OpenLink) => self.open_link_at_cursor(),
            Action::Own(TerminalAction::Block { forward }) => self.jump_to_block(forward),
            Action::Operate(..)
            | Action::Insert(_)
            | Action::DeleteChars { .. }
            | Action::DeleteToEnd { .. }
            | Action::Substitute
            | Action::Put { .. }
            | Action::Join
            | Action::Replace(_)
            | Action::Undo
            | Action::Redo
            | Action::Repeat
            | Action::SearchWord { .. }
            | Action::FindAgain { .. }
            | Action::View(_)
            | Action::SetMark(_)
            | Action::Mark { .. }
            | Action::Tilde => {
                debug_assert!(false, "a terminal doesn't take {command:?}");
                false
            }
        }
    }

    /// `o` and `O` in visual mode: the cursor goes to the other end of the
    /// selection, and the selection's start to where the cursor was.
    fn other_end(&mut self) -> bool {
        if let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) {
            self.anchor = Some(cursor.at);
            self.cursor = Some(Cursor::new(anchor));
            self.follow_cursor();
        }
        true
    }

    /// Takes in what the daemon answered a copy or a search with.
    /// What `asked` lines asked for was cut short by the daemon, as it cuts
    /// any copy at [`MOST_TEXT`] and [`MOST_LINES`], it's said, wherever
    /// it went.
    pub(super) fn copied(&mut self, text: String, to: Copy, asked: u64) {
        let lines = text.lines().count().max(1);
        let cut = asked > MOST_LINES || text.len() + 4 > MOST_TEXT;
        // Selecting with the mouse copies to the primary selection all the
        // time, which isn't worth a message.
        if cut {
            self.status = Some(format!(
                "copied {lines} lines, cut short: a copy stops at {MOST_LINES} lines or 4 MiB"
            ));
        } else if to == Copy::Clipboard {
            self.status = Some(if lines == 1 {
                "copied".into()
            } else {
                format!("copied {lines} lines")
            });
        }
        self.yanked = Some((text, to));
    }

    /// Takes in where a search from `from` found its text, if anywhere,
    /// going backward if `backward`. One that went round the end says so,
    /// as Vim does.
    pub(super) fn searched(&mut self, found: Option<Match>, (from, backward): (Point, bool)) {
        let Some(found) = found else {
            let query = self.search.as_ref().map_or("", |(query, _)| query);
            self.status = Some(format!("not found: {query}"));
            return;
        };
        let at = Point {
            line: found.start.line,
            col: found.start.col,
        };
        // A match back where it started went all the way round.
        if !backward && at <= from {
            self.status = Some(WRAPPED_TO_TOP.into());
        } else if backward && at >= from {
            self.status = Some(WRAPPED_TO_BOTTOM.into());
        }
        self.found = Some(found);
        self.cursor = Some(Cursor::new(Point {
            line: found.start.line,
            col: found.start.col,
        }));
        self.follow_cursor();
    }

    /// The characters of a visual selection on each line, as the start and
    /// end of the selected range, ends included: for a block, its top left
    /// and bottom right corners, the last at `usize::MAX` after `$`.
    pub(super) fn selection(&self) -> Option<(Point, Point, Selection)> {
        let Mode::Visual(kind) = self.mode else {
            return None;
        };
        let (anchor, cursor) = (self.anchor?, self.cursor?);
        if kind == Selection::Block {
            let to_end = cursor.want() == usize::MAX;
            let block = Block::between(anchor, cursor.at, to_end, &Document(self));
            let (start, end) = super::corners(block);
            return Some((start, end, kind));
        }
        let cursor = cursor.at;
        let (start, end) = if anchor <= cursor {
            (anchor, cursor)
        } else {
            (cursor, anchor)
        };
        Some((start, end, kind))
    }

    /// A key typed into the search prompt, edited as the command line is
    /// (see [`crate::cmdline`]); the desk has Up, Down and a register's
    /// name after Ctrl+R first.
    fn prompt_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(prompt) = &mut self.prompt else {
            return false;
        };
        match press.key {
            Key::Named(NamedKey::Enter) => {
                let mut prompt = self.prompt.take().expect("a prompt is open");
                if !prompt.line.is_empty() {
                    self.search = Some((prompt.line.take(), prompt.backward));
                    self.search_again(false);
                }
            }
            Key::Named(NamedKey::Escape) => self.prompt = None,
            _ => {
                if prompt.line.key(press) == Keyed::Emptied {
                    self.prompt = None;
                }
            }
        }
        true
    }

    /// The search being typed, for the desk to put a register in, or a
    /// search typed before.
    pub(crate) fn search_line(&mut self) -> Option<&mut Line> {
        self.prompt.as_mut().map(|prompt| &mut prompt.line)
    }

    /// Searches for the last query again, the same way or, if `reverse`,
    /// the other way.
    fn search_again(&mut self, reverse: bool) -> bool {
        let (Some((query, backward)), Some(cursor)) = (self.search.clone(), self.cursor) else {
            return false;
        };
        let backward = backward != reverse;
        self.request(
            Request::Search {
                terminal: self.terminal,
                exact: self.case.exact(&query),
                query,
                from: position(cursor.at),
                backward,
            },
            Pending::Search(cursor.at, backward),
        );
        false
    }

    fn toggle_visual(&mut self, kind: Selection) -> bool {
        match self.mode {
            Mode::Visual(current) if current == kind => self.leave_visual(),
            Mode::Visual(_) => self.mode = Mode::Visual(kind),
            _ => {
                self.mode = Mode::Visual(kind);
                self.anchor = self.cursor.map(|cursor| cursor.at);
            }
        }
        true
    }

    /// `O` in a block: the cursor goes to the other corner on its line,
    /// and the selection's start to the other on its, as in Vim.
    fn other_corner(&mut self) -> bool {
        let (Some(anchor), Some(cursor)) = (self.anchor, self.cursor) else {
            return false;
        };
        let document = Document(self);
        let block = Block::between(anchor, cursor.at, false, &document);
        let at = |line, col| Point {
            line,
            col: start_of_cell(document.row(line), col),
        };
        let (mut head, mut tail) = (at(cursor.at.line, block.right), at(anchor.line, block.left));
        if head.col == cursor.at.col {
            (head, tail) = (at(cursor.at.line, block.left), at(anchor.line, block.right));
        }
        self.anchor = Some(tail);
        self.cursor = Some(Cursor::new(head));
        self.follow_cursor();
        true
    }

    /// Ends a visual selection, back in normal mode, keeping it for `gv`.
    pub(super) fn leave_visual(&mut self) {
        if let (Mode::Visual(kind), Some(anchor), Some(cursor)) =
            (self.mode, self.anchor, self.cursor)
        {
            self.last_visual = Some((anchor, cursor.at, kind));
        }
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.anchor = None;
    }

    /// `gv`: selects the last visual selection again, by what it was, with
    /// the cursor where it was, kept within the lines there are now; in
    /// visual mode, what's selected becomes the last, so it swaps between
    /// the two, as in Vim.
    fn reselect(&mut self) -> bool {
        let (Some((anchor, head, kind)), Some(cursor)) = (self.last_visual, self.cursor) else {
            return false;
        };
        if let (Mode::Visual(now), Some(was)) = (self.mode, self.anchor) {
            self.last_visual = Some((was, cursor.at, now));
        }
        let document = Document(self);
        let (first, last) = document.bounds();
        let within = |point: Point| {
            let line = point.line.clamp(first, last);
            Point {
                line,
                col: point.col.min(document.last_cell(line)),
            }
        };
        let (anchor, head) = (within(anchor), within(head));
        self.mode = Mode::Visual(kind);
        self.anchor = Some(anchor);
        self.cursor = Some(Cursor::new(head));
        self.follow_cursor();
        true
    }

    /// Copies the selection and returns to normal mode, with the cursor at
    /// the selection's start.
    fn yank_selection(&mut self) -> bool {
        let Some((start, end, kind)) = self.selection() else {
            return false;
        };
        self.copy(start, end, kind, Copy::Clipboard);
        self.leave_visual();
        self.cursor = Some(Cursor::new(start));
        self.follow_cursor();
        true
    }

    /// `yy` and `Y`: copies the cursor's line, and with a count, as many
    /// lines from it as there are.
    fn yank_lines(&mut self, lines: usize) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let mut last = cursor;
        last.repeat(Motion::Down(1), lines.saturating_sub(1), &Document(self));
        self.copy(cursor.at, last.at, Selection::Lines, Copy::Clipboard);
        true
    }

    /// Ctrl+E and Ctrl+Y: moves the view `lines` up, keeping the cursor in
    /// it.
    fn scroll_view(&mut self, lines: i64) -> bool {
        self.scroll_by(lines);
        self.keep_cursor_in_view();
        true
    }

    pub(super) fn enter_normal(&mut self) {
        self.mode = Mode::Normal;
        let Some(screen) = &self.screen else {
            return;
        };
        let program = screen.state.cursor;
        let top = self.view_top();
        let bottom = top + self.height().saturating_sub(1);
        let line = self.history_end() + u64::from(program.line);
        let at = if (top..=bottom).contains(&line) {
            Point {
                line,
                col: usize::from(program.col),
            }
        } else {
            Point {
                line: bottom,
                col: 0,
            }
        };
        self.cursor = Some(Cursor::new(at));
    }

    /// Moves the normal-mode cursor by `motion`, as `count` says (see
    /// [`Cursor::go`]), and the view to keep it in sight. Returns whether
    /// there's a cursor to move.
    pub(super) fn move_cursor(&mut self, motion: Motion, count: Option<usize>) -> bool {
        let Some(mut cursor) = self.cursor.take() else {
            return false;
        };
        cursor.go(motion, count, &Document(self), self.startofline);
        // On a line of history not fetched yet, its column is found once
        // it comes.
        let vertical = matches!(
            motion,
            Motion::Up(_)
                | Motion::Down(_)
                | Motion::RowUp(_)
                | Motion::RowDown(_)
                | Motion::TextEnd
        );
        let to_line = matches!(
            motion,
            Motion::First
                | Motion::Last
                | Motion::ViewTop
                | Motion::ViewMiddle
                | Motion::ViewBottom
        );
        let fetched = Document(self).row(cursor.at.line).is_some();
        self.unplaced = (!fetched && (vertical || to_line)).then_some(Unplaced {
            at: cursor.at,
            start_of_line: to_line && self.startofline,
        });
        self.cursor = Some(cursor);
        self.follow_cursor();
        true
    }

    /// Puts the cursor where it goes on its line, now the line has come,
    /// if a motion left it there before it had.
    pub(super) fn place_unplaced(&mut self) {
        let Some(unplaced) = self.unplaced else {
            return;
        };
        let document = Document(self);
        let Some(mut cursor) = self.cursor.filter(|cursor| cursor.at == unplaced.at) else {
            self.unplaced = None;
            return;
        };
        if document.row(unplaced.at.line).is_none() {
            return;
        }
        cursor.onto_line(unplaced.at.line, unplaced.start_of_line, &document);
        self.cursor = Some(cursor);
        self.unplaced = None;
    }

    /// Moves the cursor up or down to `line`.
    fn move_to_line(&mut self, line: u64) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let from = cursor.at.line;
        if line < from {
            self.move_cursor(Motion::Up(from - line), None);
        } else if line > from {
            self.move_cursor(Motion::Down(line - from), None);
        }
        true
    }

    /// Ctrl+D, Ctrl+U and their kin: moves the view and the cursor `lines`
    /// down (or up, if negative).
    fn page(&mut self, lines: i64) -> bool {
        self.scroll_by(-lines);
        let motion = if lines > 0 {
            Motion::Down(lines.unsigned_abs())
        } else {
            Motion::Up(lines.unsigned_abs())
        };
        self.move_cursor(motion, None);
        true
    }

    /// Scrolls the view so the cursor is in it.
    pub(super) fn follow_cursor(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let end = self.history_end();
        let top = self.view_top();
        let height = self.height().max(1);
        let line = cursor.at.line;
        let new_top = if line < top {
            line
        } else if line >= top + height {
            line + 1 - height
        } else {
            return;
        };
        self.scroll = end.saturating_sub(new_top).min(self.max_scroll());
        self.fetch_history();
    }

    /// Moves the cursor into the view, after the view moved without it.
    pub(super) fn keep_cursor_in_view(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let top = self.view_top();
        let bottom = top + self.height().saturating_sub(1);
        let line = cursor.at.line.clamp(top, bottom);
        if line != cursor.at.line {
            self.move_to_line(line);
        }
    }

    /// Moves the cursor back within the document, after it shrank: a reset,
    /// or a smaller screen.
    pub(super) fn keep_cursor_in_bounds(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let (first, last) = Document(self).bounds();
        let line = cursor.at.line.clamp(first, last);
        if line != cursor.at.line {
            self.move_to_line(line);
        }
    }
}

pub(super) fn position(point: Point) -> Position {
    Position {
        line: point.line,
        col: point.col,
    }
}
