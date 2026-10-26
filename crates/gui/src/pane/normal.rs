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
use winit::keyboard::Key;

use super::{CopyTo, Document, Lines, Mode, Pane, Pending, Selection};
use crate::cmdline::{Keyed, Line};
use crate::input::KeyPress;
use crate::motion::{Block, Cursor, Motion, Point, start_of_cell};
use crate::vim::{
    Action, Command, Dialect, Entry, Operator, Parsed, Read, Reader, Scroll, Stroke, Target,
    ViewAt, WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP, strokes,
};

/// Where a motion left the cursor on a line of history not fetched yet,
/// with the column it wanted, and where on the line it goes once the
/// line comes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Unplaced {
    cursor: Cursor,
    goes: Place,
}

/// Where on its line a cursor waiting for the line goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    /// The column it wants, as `j` goes to.
    Wanted,
    /// The first non-blank, as `gg` goes to with `startofline`.
    FirstNonBlank,
    /// This column, as far as the line goes, as `5zt` keeps and `|` goes
    /// to.
    Column(usize),
    /// The last non-blank, as `g_` goes to.
    LastNonBlank,
}

/// What waits for its line: the cursor's place, and a selection's other
/// end's, begun where the cursor was. Each is forgotten once its end has
/// moved on.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Unplacements {
    pub(super) cursor: Option<Unplaced>,
    pub(super) anchor: Option<Unplaced>,
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
/// takes, `f` and its kin with `;` and `,` too, with `y` its only
/// operator and `i` its only way into insert mode; a register only before
/// a yank, which keeps its text there; but nothing that changes text, as
/// it has none to change, and no mark, `*` or `#` yet. Esc, `gx`, `[[`
/// and `]]` are its own.
pub(super) struct TerminalKeys;

impl Dialect for TerminalKeys {
    type Own = TerminalAction;

    fn describe(&self, action: TerminalAction) -> String {
        match action {
            TerminalAction::Escape => "Cancel command or selection",
            TerminalAction::OpenLink => "Open link under cursor",
            TerminalAction::Block { forward: true } => "Next shell block",
            TerminalAction::Block { forward: false } => "Previous shell block",
        }
        .into()
    }

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
        // A register is for a yank, which keeps its text there, as an
        // editor's does; and Esc after one still does what Esc does, as in
        // Vim.
        let yank = matches!(command.action, Action::Operate(Operator::Yank, _));
        if command.register.is_some()
            && !yank
            && command.action != Action::Own(TerminalAction::Escape)
        {
            return false;
        }
        match command.action {
            Action::Insert(entry) => entry == Entry::Before,
            Action::Move(_)
            | Action::Operate(
                Operator::Yank,
                Target::Motion(_)
                | Target::Lines
                | Target::Object { .. }
                | Target::Selection
                | Target::SelectedLines,
            )
            | Action::FindAgain { .. }
            | Action::Visual { .. }
            | Action::BlockVisual
            | Action::Reselect
            | Action::OtherEnd
            | Action::OtherCorner
            | Action::Select { .. }
            | Action::Search { .. }
            | Action::SearchAgain { .. }
            | Action::Scroll(_)
            | Action::View(_)
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
        if let Some((copy, _)) = terminal_control(press) {
            self.keys.clear();
            if copy {
                return self.yank_selection(CopyTo::Clipboard);
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
        let to = CopyTo::Register(command.register);
        match command.action {
            Action::Move(motion) => {
                let motion = self.finds.remember(motion);
                self.move_cursor(motion, count)
            }
            Action::Operate(Operator::Yank, Target::Motion(motion)) => {
                let motion = self.finds.remember(motion);
                self.yank_motion(motion, count, to)
            }
            Action::FindAgain { reverse } => match self.finds.again(reverse) {
                Some(motion) => self.move_cursor(motion, count),
                None => false,
            },
            Action::Operate(Operator::Yank, Target::Lines) => self.yank_lines(command.times(), to),
            Action::Operate(Operator::Yank, Target::Selection) => self.yank_selection(to),
            // `Y` in visual mode: the lines selected, a block as it is.
            Action::Operate(Operator::Yank, Target::SelectedLines) => {
                if matches!(self.mode, Mode::Visual(kind) if kind != Selection::Block) {
                    self.mode = Mode::Visual(Selection::Lines);
                }
                self.yank_selection(to)
            }
            // Copied after `y`, selected in visual mode.
            Action::Operate(Operator::Yank, Target::Object { object, inner })
            | Action::Select { object, inner } => {
                self.use_object(object, inner, command.times(), to)
            }
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
            Action::Scroll(scroll) => self.scroll(scroll, count),
            Action::View(at) => self.view_at(at, count),
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
            let unplaced = &mut self.unplaced;
            std::mem::swap(&mut unplaced.cursor, &mut unplaced.anchor);
            self.follow_cursor();
        }
        true
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
        match crate::vim::prompt_strokes(press)
            .first()
            .map(|stroke| crate::cmdline::controls::Control::read(*stroke).0)
        {
            Some(crate::cmdline::controls::Control::Accept) => {
                let mut prompt = self.prompt.take().expect("a prompt is open");
                if !prompt.line.is_empty() {
                    self.search = Some((prompt.line.take(), prompt.backward));
                    self.search_again(false);
                }
            }
            Some(crate::cmdline::controls::Control::Cancel) => self.prompt = None,
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
                terminal: self.terminal(),
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
                self.unplaced.anchor = self.unplaced.cursor;
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
        self.unplaced.anchor = None;
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

    /// A key in normal mode while text selected with the mouse shows, which
    /// is read as Vim reads a key in visual mode: a count or a register
    /// waits with the selection, and `y` after them, or alone, yanks it,
    /// into the register named. `None` for any other key, which is the
    /// pane's, and forgets the selection.
    pub(super) fn mouse_selection_key(&mut self, press: &KeyPress<'_>) -> Option<bool> {
        // An operator typed before, as `y` waiting for its motion, has the
        // key, as it did before the drag; and a search being typed has it.
        if self.mode != Mode::Normal
            || self.prompt.is_some()
            || self.mouse_selected().is_none()
            || !only_count_and_register(self.keys.so_far())
        {
            return None;
        }
        let [stroke] = strokes(press, false)[..] else {
            return None;
        };
        let mut typed = self.keys.so_far().to_vec();
        typed.push(stroke);
        match crate::vim::parse(&typed, true, &TerminalKeys) {
            Parsed::Command(Command {
                action: Action::Operate(Operator::Yank, Target::Selection),
                register,
                ..
            }) => {
                self.keys.clear();
                self.status = None;
                self.copy_mouse_selection(CopyTo::Register(register));
                self.mouse_selection = None;
                Some(true)
            }
            Parsed::Incomplete if only_count_and_register(&typed) => {
                self.keys.read(stroke, false, &TerminalKeys);
                Some(true)
            }
            _ => None,
        }
    }

    /// Copies the selection and returns to normal mode, with the cursor at
    /// the selection's start.
    fn yank_selection(&mut self, to: CopyTo) -> bool {
        let Some((start, end, kind)) = self.selection() else {
            return false;
        };
        self.copy(start, end, kind, to);
        self.leave_visual();
        self.cursor = Some(Cursor::new(start));
        self.follow_cursor();
        true
    }

    /// `yy` and `Y`: copies the cursor's line, and with a count, as many
    /// lines from it as there are.
    fn yank_lines(&mut self, lines: usize, to: CopyTo) -> bool {
        let Some(cursor) = self.cursor else {
            return false;
        };
        let mut last = cursor;
        last.repeat(Motion::Down(1), lines.saturating_sub(1), &Document(self));
        self.copy(cursor.at, last.at, Selection::Lines, to);
        true
    }

    /// Ctrl+D, Ctrl+U, Ctrl+F and their kin, with `count` typed, as Vim's
    /// do (see [`crate::vim::Paging`]): the half pages move the view and
    /// the cursor together, the pages the view and the cursor to its top
    /// or bottom line, and Ctrl+E and Ctrl+Y the view, keeping the cursor
    /// in it.
    fn scroll(&mut self, scroll: Scroll, count: Option<usize>) -> bool {
        let rows = self.paging.rows(scroll, count, self.height().max(1));
        let rows = i64::try_from(rows).unwrap_or(i64::MAX);
        let (top, at) = (self.scroll, self.cursor.map(|cursor| cursor.at));
        match scroll {
            Scroll::HalfPage { down } => self.page(if down { rows } else { -rows }),
            Scroll::Page { down } => self.whole_page(if down { rows } else { -rows }),
            Scroll::Line { down } => return self.scroll_view(if down { -rows } else { rows }),
        };
        // The pages, but not Ctrl+E and Ctrl+Y, as Vim's; and not when
        // neither the view nor the cursor could move.
        let moved = self.scroll != top || self.cursor.map(|cursor| cursor.at) != at;
        if self.startofline && moved {
            self.onto_start_of_line();
        }
        true
    }

    /// Puts the cursor at its line's first non-blank, as `startofline`
    /// has paging do: once the line comes, if it hasn't yet.
    fn onto_start_of_line(&mut self) {
        let Some(mut cursor) = self.cursor else {
            return;
        };
        cursor.onto_line(cursor.at.line, true, &Document(self));
        self.note_unplaced(cursor, Place::FirstNonBlank);
        self.cursor = Some(cursor);
    }

    /// Notes where the cursor is, `cursor`, if its line hasn't come yet,
    /// to put it where it `goes` on the line once it has; or forgets what
    /// was noted.
    fn note_unplaced(&mut self, cursor: Cursor, goes: Place) {
        let fetched = Document(self).row(cursor.at.line).is_some();
        self.unplaced.cursor = (!fetched).then_some(Unplaced { cursor, goes });
    }

    /// Puts the cursor on line `count` from history's oldest, as `G`
    /// counts, as `5zt` does: in the column it's in, as far as the line
    /// goes, still wanting the one it wanted, as Vim's. One still waiting
    /// for its line, as after a page with `startofline`, goes on to wait to
    /// go where it would have gone.
    fn onto_counted_line(&mut self, count: usize) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let (first, last) = Document(self).bounds();
        let line = first.saturating_add(count as u64 - 1).min(last);
        let goes = match self.unplaced.cursor {
            Some(waiting) if waiting.cursor.at == cursor.at => waiting.goes,
            _ => Place::Column(cursor.at.col),
        };
        let moved = Unplaced {
            cursor: cursor.moved(Point {
                line,
                col: cursor.at.col,
            }),
            goes,
        };
        self.cursor = Some(self.placed(moved).unwrap_or(moved.cursor));
        self.note_unplaced(moved.cursor, goes);
    }

    /// `zt`, `zz` and `zb`: scrolls the view to put the cursor's line at
    /// its top, middle or bottom, as far as history goes back and no
    /// further than the live screen, the cursor staying where it is; with
    /// a count, line `count` from history's oldest, as `G` counts, the
    /// cursor going there in its column, as Vim's.
    fn view_at(&mut self, at: ViewAt, count: Option<usize>) -> bool {
        if let Some(count) = count {
            self.onto_counted_line(count);
        }
        let Some(cursor) = self.cursor else {
            return false;
        };
        let height = self.height().max(1);
        let line = cursor.at.line;
        let top = match at {
            ViewAt::Top => line,
            ViewAt::Middle => line.saturating_sub((height - 1) / 2),
            ViewAt::Bottom => line.saturating_sub(height - 1),
        };
        let (first, _) = Document(self).bounds();
        self.scroll = (self.history_end())
            .saturating_sub(top.max(first))
            .min(self.max_scroll());
        self.fetch_history();
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
        let goes = match motion {
            Motion::First
            | Motion::Last
            | Motion::ViewTop
            | Motion::ViewMiddle
            | Motion::ViewBottom
                if self.startofline =>
            {
                Some(Place::FirstNonBlank)
            }
            // `50%` to a line, as `gg` goes.
            Motion::Match if count.is_some() && self.startofline => Some(Place::FirstNonBlank),
            Motion::Match if count.is_some() => Some(Place::Wanted),
            Motion::Up(_)
            | Motion::Down(_)
            | Motion::RowUp(_)
            | Motion::RowDown(_)
            | Motion::TextEnd
            | Motion::First
            | Motion::Last
            | Motion::ViewTop
            | Motion::ViewMiddle
            | Motion::ViewBottom => Some(Place::Wanted),
            Motion::NextLine | Motion::PreviousLine | Motion::LineFirstNonBlank => {
                Some(Place::FirstNonBlank)
            }
            Motion::LastNonBlank => Some(Place::LastNonBlank),
            Motion::Column => Some(Place::Column(cursor.want())),
            // `2$` to another line's end.
            Motion::LineEnd if count.is_some_and(|count| count > 1) => Some(Place::Wanted),
            _ => None,
        };
        match goes {
            Some(goes) => self.note_unplaced(cursor, goes),
            None => self.unplaced.cursor = None,
        }
        self.cursor = Some(cursor);
        self.follow_cursor();
        true
    }

    /// Puts the cursor where it goes on its line, and a selection's other
    /// end where it goes on its, now their lines have come, if motions
    /// left them there before they had and they're still there.
    pub(super) fn place_unplaced(&mut self) {
        if let Some(unplaced) = self.unplaced.cursor {
            if self
                .cursor
                .is_none_or(|cursor| cursor.at != unplaced.cursor.at)
            {
                self.unplaced.cursor = None;
            } else if let Some(placed) = self.placed(unplaced) {
                self.cursor = Some(placed);
                self.unplaced.cursor = None;
            }
        }
        if let Some(unplaced) = self.unplaced.anchor {
            if self.anchor != Some(unplaced.cursor.at) {
                self.unplaced.anchor = None;
            } else if let Some(placed) = self.placed(unplaced) {
                self.anchor = Some(placed.at);
                self.unplaced.anchor = None;
            }
        }
    }

    /// Where `unplaced` goes on its line, if the line has come.
    fn placed(&self, unplaced: Unplaced) -> Option<Cursor> {
        let document = Document(self);
        let line = unplaced.cursor.at.line;
        document.row(line)?;
        let mut cursor = unplaced.cursor;
        match unplaced.goes {
            Place::Column(col) => {
                let col = col.min(document.last_cell(line));
                cursor.at.col = start_of_cell(document.row(line), col);
            }
            Place::LastNonBlank => cursor.go(Motion::LastNonBlank, None, &document, false),
            goes => cursor.onto_line(line, goes == Place::FirstNonBlank, &document),
        }
        Some(cursor)
    }

    /// Forgets what waits for its line, as history is numbered afresh.
    pub(super) fn forget_unplaced(&mut self) {
        self.unplaced = Unplacements::default();
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

    /// Ctrl+F, Page Down and Page Up: scrolls the view `lines` down, or up
    /// if negative, and once it's moved, puts the cursor on its top line
    /// going down, or its bottom line going up, as Vim's do. If the view
    /// can't move, at the live screen or history's start, nothing does.
    fn whole_page(&mut self, lines: i64) -> bool {
        if !self.scroll_by(-lines) {
            return true;
        }
        let top = self.view_top();
        let line = if lines > 0 {
            top
        } else {
            top + self.height().saturating_sub(1)
        };
        self.move_to_line(line)
    }

    /// Ctrl+D and Ctrl+U: moves the view and the cursor `lines` down (or
    /// up, if negative).
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

/// Whether `typed`, which the grammar reads as incomplete, is only counts
/// and registers, as `3`, `"a` or `"a2`, and not the start of a command.
/// (`0` alone is a motion, so not incomplete.)
fn only_count_and_register(typed: &[Stroke]) -> bool {
    let mut rest = typed;
    loop {
        rest = match rest {
            [] => return true,
            [
                Stroke::Char {
                    ch: '"',
                    ctrl: false,
                },
            ] => &[],
            [
                Stroke::Char {
                    ch: '"',
                    ctrl: false,
                },
                _,
                more @ ..,
            ]
            | [
                Stroke::Char {
                    ch: '0'..='9',
                    ctrl: false,
                },
                more @ ..,
            ] => more,
            _ => return false,
        };
    }
}

pub(super) fn terminal_control(
    press: &KeyPress<'_>,
) -> Option<(bool, crate::shortcuts::bindings::Hint)> {
    let character = match press.key {
        Key::Character(text) => text.chars().next(),
        _ => None,
    };
    if !press.modifiers.ctrl || !matches!(character, Some('c' | 'C')) {
        return None;
    }
    let copy = character == Some('C');
    Some((
        copy,
        crate::shortcuts::bindings::Hint::Action(if copy {
            "Copy selection"
        } else {
            "Send interrupt to terminal program"
        }),
    ))
}
