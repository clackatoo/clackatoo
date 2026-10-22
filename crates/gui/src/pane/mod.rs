//! A pane showing a terminal: what it displays, its mode, and how keys and
//! scrolling drive it.
//!
//! A pane is in insert mode, where keys go to the program, or normal mode,
//! where they drive clackatoo. Ctrl+;, Shift+Esc or Ctrl+\ switches to
//! normal mode, and `i` back; Ctrl+\ pressed twice sends a real Ctrl+\ to
//! the program. Ctrl+; is the quickest, and costs programs nothing: a
//! terminal has no code to send them for it.
//!
//! Normal mode has a cursor of its own, moved with Vim's motions over the
//! whole document: history, then the screen. The view follows it, and
//! stays put while output arrives, so the text being read doesn't move.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::Instant;

use clackatoo_protocol::{
    ChatAction, ClientMessage, CursorShape, Match, Request, Response, Size, TerminalId, ViewUpdate,
};
use clackatoo_term::{Row, RowBuilder, Underline};
use winit::keyboard::{Key, NamedKey};

use crate::chat::{ChatView, TextPaint};
use crate::fonts::Fonts;
use crate::history::History;
use crate::input::{self, KeyPress};
use crate::motion::{Block, Cursor, Lines, Motion, Point, RowAt, View};
use crate::options::{BlockMarks, Case, Options};
use crate::render::{CursorStyle, Highlight, HighlightKind, SceneCursor};
use crate::screen::Screen;
use crate::vim::{self, Mode, Selection, enters_normal_mode, is_ctrl_backslash};

mod links;
mod marks;
mod mouse;
mod normal;
mod operator;
mod predict;

pub(crate) use mouse::{Copy, PaneMouse};
use predict::Predictor;

/// Text just copied, highlighted until `until`.
#[derive(Clone, Copy, Debug)]
struct Flash {
    start: Point,
    end: Point,
    kind: Selection,
    until: Instant,
}

/// The fewest rows a pane shows the pill saying it's scrolled back in:
/// fewer, and it would cover too much of what's being read.
const PILL_ROWS: u16 = 4;

/// A request sent to the daemon and not yet answered. Answers arrive in
/// the order requests were sent.
#[derive(Debug)]
enum Pending {
    /// History lines, the range asked for and the history's generation
    /// then.
    History(Range<u64>, u64),
    /// Text to copy, where to, and how many lines were asked for.
    Text(Copy, u64),
    /// A search, from where, and whether it went backward.
    Search(Point, bool),
    /// A block, and what for.
    Block(operator::BlockUse),
    /// Input for the program, numbered by the [`Predictor`].
    Input(u64),
    /// Anything answered with just "done" or an error.
    Other,
}

/// The guesses at the echo of what's typed, drawn over a line of the
/// screen.
#[derive(Debug)]
struct Overlay {
    /// The guesses.
    shown: predict::Shown,
    /// Their line as drawn: the screen's, with the guesses over it.
    row: Row,
}

/// A terminal shown in the window.
///
/// The pane does no I/O: messages for the daemon wait in its outbox, which
/// the window sends.
#[derive(Debug)]
pub(crate) struct Pane {
    terminal: TerminalId,
    /// Messages for the daemon, oldest first.
    outbox: Vec<ClientMessage>,
    /// The size last asked of the daemon.
    requested_size: Option<Size>,
    screen: Option<Screen>,
    /// For a chat with an agent, which has no screen, its view.
    chat: Option<Box<ChatView>>,
    history: History,
    /// How many lines up from the live screen the view is: 0 shows the
    /// screen as the program draws it.
    scroll: u64,
    pub(crate) mode: Mode,
    /// The normal-mode cursor, while in normal or visual mode.
    cursor: Option<Cursor>,
    /// Where a visual selection started.
    anchor: Option<Point>,
    /// The last visual selection, for `gv`: where it started, where the
    /// cursor was, and by what.
    last_visual: Option<(Point, Point, Selection)>,
    /// A search being typed.
    prompt: Option<normal::Prompt>,
    /// The last search, and whether it went backward.
    search: Option<(String, bool)>,
    /// How searches treat case, as the options say.
    case: Case,
    /// `gg`, `G`, `H`, `M` and `L` go to a line's first non-blank
    /// (`startofline`).
    startofline: bool,
    /// How far Ctrl+D and its kin go.
    paging: vim::Paging,
    /// The last `f`, `t`, `F` or `T`, for `;` and `,`.
    finds: vim::Finds,
    /// Where motions left the cursor, or a selection's other end, on
    /// lines not fetched yet.
    unplaced: normal::Unplacements,
    /// Which finished commands are marked at the end of their prompt's
    /// line, as the options say.
    blockmarks: BlockMarks,
    /// About the most of the conversation, in tokens, the chat it shows
    /// tells an agent it moves to, as the options say.
    carryover: u32,
    /// The last match found, highlighted until Esc.
    found: Option<Match>,
    /// A short message for the label, until the next key.
    status: Option<String>,
    /// Text copied, and where the window is to put it.
    yanked: Option<(String, Copy)>,
    /// Text the chat it shows copied, and where to put it, oldest first.
    chat_copies: Vec<(String, Copy)>,
    /// Text selected with the mouse.
    mouse_selection: Option<mouse::MouseSelection>,
    /// Text just copied to the clipboard.
    flash: Option<Flash>,
    pending: VecDeque<Pending>,
    /// A view update was applied but not yet drawn and acknowledged.
    unacknowledged: bool,
    /// The keys of a command being typed in normal or visual mode.
    keys: vim::Keys,
    /// Ctrl+\ just left insert mode, and pressing it again sends it to
    /// the program. Any other key forgets it.
    after_ctrl_backslash: bool,
    /// Drawn for history lines still on their way.
    blank: Row,
    /// Where the mouse is over the pane, for the link under it.
    hovered: Option<links::Hover>,
    /// A link clicked or `gx`ed, for the desk to open.
    opening: Option<crate::links::Target>,
    /// How many links the screen may know of before those no row shows
    /// are forgotten.
    links_kept: usize,
    /// Guesses at the echo of what's typed (see [`predict`]).
    predictor: Predictor,
    /// The guesses to draw, if any.
    overlay: Option<Overlay>,
    /// When a key typed in this window last reached the program, as
    /// `exit` and Enter, or Ctrl+D, would.
    keyed: Option<Instant>,
}

impl Pane {
    pub(crate) fn new(terminal: TerminalId) -> Self {
        Self {
            terminal,
            outbox: Vec::new(),
            requested_size: None,
            screen: None,
            chat: None,
            history: History::default(),
            scroll: 0,
            mode: Mode::Insert,
            cursor: None,
            anchor: None,
            last_visual: None,
            prompt: None,
            search: None,
            case: Case::default(),
            startofline: Options::default().startofline,
            paging: vim::Paging::default(),
            finds: vim::Finds::default(),
            unplaced: normal::Unplacements::default(),
            blockmarks: Options::default().blockmarks,
            carryover: Options::default().carryover,
            found: None,
            status: None,
            yanked: None,
            chat_copies: Vec::new(),
            mouse_selection: None,
            flash: None,
            pending: VecDeque::new(),
            unacknowledged: false,
            keys: vim::Keys::default(),
            after_ctrl_backslash: false,
            blank: RowBuilder::new(0).finish(false),
            hovered: None,
            opening: None,
            links_kept: links::KEPT_LINKS,
            predictor: Predictor::new(),
            overlay: None,
            keyed: None,
        }
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options) {
        self.case = Case::of(options);
        self.startofline = options.startofline;
        self.blockmarks = options.blockmarks;
        self.carryover = options.carryover;
        self.predictor.set_choice(options.predictecho);
        self.refresh_overlay();
    }

    /// Takes in whether the daemon is on another machine, where the echo
    /// of what's typed is guessed at.
    pub(crate) fn set_remote(&mut self, remote: bool) {
        self.predictor.set_remote(remote);
    }

    /// Settles the guesses at the echo of what's typed whose time is up
    /// at `now`. Returns whether the pane must be redrawn.
    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        self.predictor.tick(now, self.screen.as_ref());
        self.refresh_overlay()
    }

    /// When a guess at the echo of what's typed is next due to be
    /// settled, for [`tick`](Self::tick).
    pub(crate) fn wakes(&self) -> Option<Instant> {
        self.predictor.wakes()
    }

    /// Works out the guesses to draw again, as they or the screen have
    /// changed. Returns whether what's drawn changed.
    fn refresh_overlay(&mut self) -> bool {
        let overlay = self.screen.as_ref().and_then(|screen| {
            let shown = self.predictor.shown(screen)?;
            let row = usize::try_from(shown.line.checked_sub(screen.state.history_end)?).ok()?;
            Some(Overlay {
                row: overlaid(screen.rows.get(row)?, &shown.cells),
                shown,
            })
        });
        let shown =
            |overlay: &Option<Overlay>| overlay.as_ref().map(|overlay| overlay.shown.clone());
        let changed = shown(&self.overlay) != shown(&overlay);
        self.overlay = overlay;
        changed
    }

    pub(crate) fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }

    /// The line after the document's last, its history then its screen, as
    /// a request numbers lines, once there's a screen.
    pub(crate) fn document_end(&self) -> Option<u64> {
        self.screen.as_ref()?;
        Some(self.history_end() + self.height())
    }

    /// Whether the pane shows a chat, rather than a terminal's screen.
    pub(crate) fn is_chat(&self) -> bool {
        self.chat.is_some()
    }

    /// The text the chat the pane shows drew last, if it shows one (see
    /// [`ChatView::shown_text`]).
    pub(crate) fn chat_text(&self) -> Option<String> {
        self.chat.as_ref().map(|chat| chat.shown_text())
    }

    /// Tells the chat the pane shows, if any, that the daemon has gone out
    /// of reach, if `offline`, or come back.
    pub(crate) fn chat_offline(&mut self, offline: bool) {
        if let Some(chat) = &mut self.chat {
            chat.set_offline(offline);
        }
    }

    /// Whether the pane shows a chat whose agent is at work.
    pub(crate) fn chat_working(&self) -> bool {
        self.chat.as_ref().is_some_and(|chat| chat.working())
    }

    /// Asks the daemon to undo what turn `turn` of the chat the pane shows
    /// changed in files, or with `redo` to make it again: or, with no
    /// turn, the last that can be undone, or the first that can be
    /// redone, as `u` and Ctrl+R do. `force` does what can be done though
    /// some files can't be. Says why not, if it can't be asked.
    pub(crate) fn undo_turn(
        &mut self,
        turn: Option<usize>,
        redo: bool,
        force: bool,
    ) -> Result<(), String> {
        let chat = self
            .chat
            .as_ref()
            .ok_or("that's for a chat with an agent")?;
        let undo = if redo { "redo" } else { "undo" };
        // Asked for by its number, which doesn't count where the chat
        // moved to another agent.
        let turn = match turn {
            Some(turn) => clackatoo_protocol::turn_numbered(&chat.node.turns, turn + 1)
                .ok_or(format!("there's no turn {}", turn + 1))?,
            None => (chat.turn_to_undo(redo)).ok_or(format!("no turn has files to {undo}"))?,
        };
        let turn = u32::try_from(turn).unwrap_or(u32::MAX);
        let action = if redo {
            ChatAction::Redo { turn, force }
        } else {
            ChatAction::Undo { turn, force }
        };
        let terminal = self.terminal;
        self.request(Request::Chat { terminal, action }, Pending::Other);
        Ok(())
    }

    /// The title its program set, or its chat has.
    pub(crate) fn title(&self) -> Option<&str> {
        match (&self.screen, &self.chat) {
            (Some(screen), _) => Some(&screen.title),
            (None, Some(chat)) => Some(&chat.title),
            (None, None) => None,
        }
    }

    /// What a tab showing it is named: the title its program set, or its
    /// chat has, or else the name of the folder its shell last said it's
    /// in, `~` for `home`.
    pub(crate) fn name(&self, home: Option<&str>) -> Option<&str> {
        let title = self.title()?;
        if !title.trim().is_empty() || self.chat.is_some() {
            return Some(title);
        }
        let Some(cwd) = self
            .screen
            .as_ref()
            .and_then(|screen| screen.cwd.as_deref())
        else {
            return Some(title);
        };
        Some(folder_name(cwd, home))
    }

    /// Lays out and draws the chat it shows, if it shows one, in a pane
    /// `size` pixels with its top left at `origin`. `caret` draws the
    /// cursor in the message being typed.
    pub(crate) fn paint_chat(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        caret: bool,
    ) -> Option<TextPaint> {
        self.chat.as_ref()?;
        self.settle_chat(fonts);
        let reading = self.mode != Mode::Insert;
        let chat = self.chat.as_mut()?;
        chat.reading = reading;
        Some(chat.paint(fonts, origin, size, caret))
    }

    /// Carries out what was typed or clicked in the chat it shows, if it
    /// shows one: for that, the fonts are needed. Returns whether the pane
    /// must be redrawn.
    pub(crate) fn settle_chat(&mut self, fonts: &mut Fonts) -> bool {
        let Some(chat) = self.chat.as_mut().filter(|chat| chat.unsettled()) else {
            return false;
        };
        let settled = chat.settle(fonts);
        for mut action in settled.actions {
            if let ChatAction::Switch { budget, .. } = &mut action {
                *budget = self.carryover;
            }
            self.request(
                Request::Chat {
                    terminal: self.terminal,
                    action,
                },
                Pending::Other,
            );
        }
        self.chat_copies.extend(settled.copied);
        settled.redraw
    }

    /// Takes what the chat it shows copied, oldest first.
    pub(crate) fn take_chat_copies(&mut self) -> Vec<(String, Copy)> {
        std::mem::take(&mut self.chat_copies)
    }

    /// When the chat it shows must be drawn again, for a turn still going
    /// to count on a second.
    pub(crate) fn chat_live(&self) -> Option<Instant> {
        self.chat
            .as_ref()?
            .live()
            .filter(|&live| live > Instant::now())
    }

    /// How the mouse pointer looks at `at`, from the pane's top left, if it
    /// shows a chat.
    pub(crate) fn chat_pointer(&self, at: [f32; 2]) -> Option<crate::chat::Pointer> {
        Some(self.chat.as_ref()?.pointer(at))
    }

    /// Shows what an input method is composing in the chat's box, if it
    /// shows a chat being typed in. Returns whether it does.
    pub(crate) fn compose_in_chat(
        &mut self,
        composing: Option<(String, Option<(usize, usize)>)>,
    ) -> bool {
        match &mut self.chat {
            Some(chat) if self.mode == Mode::Insert => {
                chat.compose(composing);
                true
            }
            _ => false,
        }
    }

    /// A key in a chat: in insert mode, it edits the message, and in
    /// normal or visual mode, it reads the conversation (see
    /// [`ChatView::read_key`]), where an answer's number, pressed twice
    /// or with Enter, answers what the agent asks. Esc goes to normal
    /// mode, as there's no program to need it, unless the agent is asking
    /// something.
    fn chat_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(chat) = self.chat.as_mut() else {
            return false;
        };
        let escape = *press.key == Key::Named(NamedKey::Escape);
        if self.mode == Mode::Insert {
            if enters_normal_mode(press) || (escape && chat.node.question.is_none()) {
                self.mode = Mode::Normal;
                return true;
            }
            chat.key(press);
            return true;
        }
        let (redraw, insert) = chat.read_key(press);
        let mode = if insert {
            Mode::Insert
        } else {
            chat.visual().map_or(Mode::Normal, Mode::Visual)
        };
        let changed = mode != self.mode;
        self.mode = mode;
        redraw || changed
    }

    /// Whether a search is being typed, which takes every key.
    pub(crate) fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// Whether a two-key command is half typed, such as `y` waiting for
    /// its motion, or a count for its command, which the next key is for.
    /// Ctrl+\ waiting to be pressed again isn't: any other key does what
    /// it would.
    pub(crate) fn pending(&self) -> bool {
        self.keys.pending() || self.chat.as_ref().is_some_and(|chat| chat.pending())
    }

    /// The count typed, if that's all that's waiting: a key the pane
    /// doesn't take after it is the window's, as `:` is. A chat's `g` or
    /// `z` half typed counts as a count of one, so `g:` is `:` too.
    pub(crate) fn counting(&self) -> Option<usize> {
        if let Some(chat) = &self.chat {
            return chat.pending().then_some(1);
        }
        self.keys.counting()
    }

    /// Forgets a count typed, or a chat's two-key command half typed, as a
    /// binding of the window's took the key after it.
    pub(crate) fn forget_count(&mut self) {
        self.keys.clear();
        if let Some(chat) = &mut self.chat {
            chat.forget_prefix();
        }
    }

    /// Switches to normal mode, if in insert mode, as when the focus
    /// arrives while the window is in normal mode.
    pub(crate) fn enter_normal_mode(&mut self) {
        if self.mode == Mode::Insert {
            self.enter_normal();
        }
    }

    /// The focus has arrived at the pane: a chat asking something waits
    /// a moment before keys answer it (see [`ChatView::arrived`]).
    pub(crate) fn arrived(&mut self) {
        if let Some(chat) = &mut self.chat {
            chat.arrived();
        }
    }

    /// Drops the answer its chat armed, if one did, as the window losing
    /// the focus does.
    pub(crate) fn disarm(&mut self) {
        if let Some(chat) = &mut self.chat {
            chat.disarm();
        }
    }

    /// The chat it shows, if it shows one.
    pub(crate) fn chat_mut(&mut self) -> Option<&mut ChatView> {
        self.chat.as_deref_mut()
    }

    /// Whether `press` is one of the keys that answer the question its
    /// chat asks, if it asks one: a number, Enter or Esc. Held down, such
    /// a key repeats, and its repeats are dropped, so one held can't wait
    /// out the pause after the question comes, or arm and give an answer.
    pub(crate) fn answers_question(&self, press: &KeyPress<'_>) -> bool {
        let answer_key = match press.key {
            Key::Named(NamedKey::Enter | NamedKey::Escape) => true,
            Key::Character(text) => {
                text.len() == 1
                    && text.as_str() != "0"
                    && text.chars().all(|ch| ch.is_ascii_digit())
            }
            _ => false,
        };
        answer_key && self.asking()
    }

    /// Whether it shows a chat whose agent is asking something, which
    /// keys in insert mode would answer.
    pub(crate) fn asking(&self) -> bool {
        (self.chat.as_ref()).is_some_and(|chat| chat.node.question.is_some())
    }

    /// Switches to insert mode, if it's not in it, as when the focus
    /// arrives while the window is in insert mode: a terminal goes back to
    /// the live screen, as `i` does, and a chat to the message being
    /// typed. A terminal in insert mode already stays where the wheel
    /// scrolled it, until a key is typed.
    pub(crate) fn enter_insert_mode(&mut self) {
        if self.mode == Mode::Insert {
            return;
        }
        if self.chat.is_some() {
            self.mode = Mode::Insert;
            return;
        }
        self.insert();
    }

    /// Ends what was half done here as the focus leaves: a visual
    /// selection, which leaves normal mode, a two-key command half typed,
    /// or a search being typed. Where the view and the cursor are stays.
    pub(crate) fn leave(&mut self) {
        if let Some(chat) = &mut self.chat {
            chat.leave();
        }
        self.leave_visual();
        self.keys.clear();
        self.after_ctrl_backslash = false;
        self.prompt = None;
    }

    /// Starts over on a new connection to the daemon, which sends a fresh
    /// snapshot. Whatever was sent on the old one is forgotten, unanswered.
    pub(crate) fn reconnected(&mut self) {
        self.outbox.clear();
        self.pending.clear();
        self.history.forget_fetch();
        self.requested_size = None;
        self.unacknowledged = false;
        self.predictor.reconnected();
        self.overlay = None;
    }

    /// Asks the daemon to make the terminal `size`, unless it was asked
    /// already and is that size.
    pub(crate) fn resize(&mut self, size: Size) {
        let current = self.screen.as_ref().map(|screen| screen.size);
        if self.requested_size == Some(size) && current == Some(size) {
            return;
        }
        // Only a new height forgets the half page a count set, as Vim's
        // `scroll` is set afresh when the window's height changes.
        if self.requested_size.or(current).map(Size::rows) != Some(size.rows()) {
            self.paging.resized();
        }
        self.requested_size = Some(size);
        self.request(
            Request::Resize {
                terminal: self.terminal,
                size,
            },
            Pending::Other,
        );
    }

    /// Applies a view update. Returns whether the pane must be redrawn.
    pub(crate) fn view_changed(&mut self, update: &ViewUpdate) -> bool {
        if let ViewUpdate::Snapshot(view) = update
            && let Some(node) = view.chat()
            && self.chat.is_none()
        {
            self.chat = Some(Box::new(ChatView::of_view(view, node)));
            self.unacknowledged = true;
            return true;
        }
        if let Some(chat) = &mut self.chat {
            chat.apply(update);
            self.unacknowledged = true;
            return true;
        }
        let old_end = self.screen.as_ref().map(|screen| screen.state.history_end);
        let snapshot = matches!(update, ViewUpdate::Snapshot(_));
        match (&mut self.screen, update) {
            (Some(screen), _) => screen.apply(update),
            (None, ViewUpdate::Snapshot(view)) => self.screen = Screen::new(view),
            (None, ViewUpdate::Patch(_)) => return false,
        }
        let end = self.history_end();
        if let Some(screen) = &self.screen {
            if snapshot || old_end.is_some_and(|old_end| end < old_end) {
                self.predictor.screen_replaced();
            }
            self.predictor.screen_changed(screen);
        }
        self.refresh_overlay();
        // Stay on the lines being read, and in normal mode, on the lines
        // around the cursor.
        let follow = self.scroll > 0 || self.mode != Mode::Insert;
        match old_end {
            // History may have been renumbered, by clearing it or by a
            // resize rewrapping the line the screen's top row continues.
            // It can have fewer lines than before.
            Some(old_end) if snapshot => {
                self.history.clear();
                self.forget_unplaced();
                if follow {
                    self.scroll = (self.scroll + end).saturating_sub(old_end);
                }
            }
            // The terminal was reset, and numbering started again.
            Some(old_end) if end < old_end => {
                self.history.clear();
                self.forget_unplaced();
                self.scroll = 0;
            }
            // New lines scrolled into history.
            Some(old_end) if follow => self.scroll += end - old_end,
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll());
        // Normal mode began before there was a screen to put its cursor on.
        if self.mode == Mode::Normal && self.cursor.is_none() {
            self.enter_normal();
        }
        self.keep_cursor_in_bounds();
        self.forget_unseen_links();
        self.unacknowledged = true;
        self.fetch_history();
        true
    }

    /// Takes the daemon's answer to the oldest request. Returns whether the
    /// pane must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        let pending = self.pending.pop_front();
        if let Some(Pending::Input(number)) = pending {
            if let Response::Error(err) = response {
                log::warn!("the daemon refused input: {err}");
            }
            self.predictor.answered(number, Instant::now());
            return self.refresh_overlay();
        }
        match (pending, response) {
            (_, Response::Error(err)) => {
                log::warn!("the daemon refused a request: {err}");
                false
            }
            (Some(Pending::History(asked, generation)), Response::History(lines)) => {
                let shown = self.shown_history();
                self.history.receive(&lines, &asked, generation, &shown);
                if generation == self.history.generation() {
                    self.learn_links(&lines.links);
                    if let Some(screen) = &mut self.screen {
                        screen.exits.learn(&lines.marks.exits);
                    }
                }
                self.scroll = self.scroll.min(self.max_scroll());
                self.place_unplaced();
                self.fetch_history();
                true
            }
            (Some(Pending::Text(to, lines)), Response::Text(text)) => {
                self.copied(text, to, lines);
                true
            }
            (Some(Pending::Search(from, backward)), Response::Found(found)) => {
                self.searched(found, (from, backward));
                true
            }
            (Some(Pending::Block(use_)), Response::Block(block)) => {
                self.found_block(block, use_);
                true
            }
            _ => false,
        }
    }

    /// Takes text copied, for the window to put on the clipboard or the
    /// primary selection.
    pub(crate) fn take_yanked(&mut self) -> Option<(String, Copy)> {
        self.yanked.take()
    }

    /// Tells the daemon the last update is on screen, so it sends the next.
    pub(crate) fn drawn(&mut self) {
        if std::mem::take(&mut self.unacknowledged) {
            self.send(ClientMessage::FrameDone {
                terminal: self.terminal,
            });
        }
    }

    /// Handles a key press. Returns whether the pane must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        if self.chat.is_some() {
            return self.chat_key(press);
        }
        let modifiers = press.modifiers;
        let ctrl_backslash = is_ctrl_backslash(press);
        let to_normal = enters_normal_mode(press);
        let after_ctrl_backslash = std::mem::take(&mut self.after_ctrl_backslash);
        // Text selected with the mouse is copied by Ctrl+Shift+C, or `y` in
        // normal mode with nothing typed, and any other key forgets it.
        let ctrl_shift_c =
            modifiers.ctrl && matches!(press.key, Key::Character(text) if text == "C");
        let y = self.mode == Mode::Normal
            && !modifiers.ctrl
            && !modifiers.alt
            && !self.keys.pending()
            && matches!(press.key, Key::Character(text) if text == "y");
        if (ctrl_shift_c || y) && self.copy_mouse_selection(Copy::Clipboard) {
            self.mouse_selection = None;
            return true;
        }
        let forgot_selection = self.mouse_selection.take().is_some();
        if ctrl_shift_c && self.mode == Mode::Insert {
            return forgot_selection;
        }
        let redraw = match self.mode {
            Mode::Insert if to_normal => {
                self.enter_normal();
                self.after_ctrl_backslash = ctrl_backslash;
                true
            }
            // Typing returns to the live screen, where the typing shows, and
            // a guess at its echo may show at once.
            Mode::Insert => self
                .type_key(press)
                .is_some_and(|guessed| std::mem::replace(&mut self.scroll, 0) > 0 || guessed),
            _ if ctrl_backslash && after_ctrl_backslash => {
                self.send_keyed(vec![0x1c]);
                self.insert()
            }
            // From anywhere else, too: a selection or a search being typed
            // is left, and the cursor stays where it is.
            _ if to_normal => {
                self.leave_visual();
                self.mode = Mode::Normal;
                self.prompt = None;
                self.keys.clear();
                true
            }
            _ => self.normal_key(press),
        };
        redraw || forgot_selection
    }

    /// Sends pasted text, marked as a paste if the program asked for that.
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(chat) = &mut self.chat {
            chat.paste(text);
            return;
        }
        let Some(screen) = &self.screen else {
            return;
        };
        // Bracketed, it can't end itself early; otherwise its newlines
        // are Enter.
        self.send_keyed(clackatoo_term::paste(text, screen.state.modes));
        self.scroll = 0;
    }

    /// Scrolls `lines` up (or down, if negative), as from the mouse wheel
    /// turned with the pointer at `at`, a cell of the view, and a pixel
    /// from the pane's top left. A program that asked for mouse reports
    /// gets the wheel instead (see [`mouse`](self::mouse)). On the
    /// alternate screen, which has no history, other programs get arrow
    /// keys in insert mode, as other terminals send them; in normal mode,
    /// which keeps input from the program, normal mode's cursor moves
    /// instead, as `k` and `j` move it. In a chat, with Shift over a table
    /// too wide for it, the table scrolls sideways. Returns whether the
    /// pane must be redrawn.
    pub(crate) fn wheel(
        &mut self,
        lines: i64,
        (at, pixel): ([i64; 2], [f32; 2]),
        modifiers: input::Modifiers,
    ) -> bool {
        if let Some(chat) = &mut self.chat {
            // The wheel drops an answer armed, as a key does.
            let disarmed = chat.disarm();
            #[allow(clippy::cast_precision_loss)]
            let lines = lines as f32 * 3.0;
            if modifiers.shift
                && let Some(moved) = chat.wheel_sideways(pixel, lines)
            {
                return moved || disarmed;
            }
            return chat.scroll_lines(lines) || disarmed;
        }
        if self.reports_mouse(modifiers.shift) {
            let button = if lines > 0 {
                clackatoo_term::mouse::MouseButton::WheelUp
            } else {
                clackatoo_term::mouse::MouseButton::WheelDown
            };
            for _ in 0..lines.unsigned_abs() {
                self.mouse(&PaneMouse {
                    action: clackatoo_term::mouse::MouseAction::Press(button),
                    line: at[0],
                    col: at[1],
                    pixel: [0.0; 2],
                    clicks: 1,
                    modifiers,
                    block: false,
                });
            }
            return false;
        }
        let Some(screen) = &self.screen else {
            return false;
        };
        if screen.state.alternate {
            if self.mode != Mode::Insert {
                let motion = if lines > 0 {
                    Motion::Up(lines.unsigned_abs())
                } else {
                    Motion::Down(lines.unsigned_abs())
                };
                return self.move_cursor(motion, None);
            }
            let key = Key::Named(if lines > 0 {
                NamedKey::ArrowUp
            } else {
                NamedKey::ArrowDown
            });
            let press = KeyPress {
                key: &key,
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: input::Modifiers::default(),
            };
            let mut bytes = Vec::new();
            for _ in 0..lines.unsigned_abs() {
                input::encode(&press, screen.state.modes, &mut bytes);
            }
            self.send_input(bytes);
            return false;
        }
        let scrolled = self.scroll_by(lines);
        self.keep_cursor_in_view();
        scrolled
    }

    /// The rows to show `height` lines tall, top first, and the cursor if
    /// it's in view.
    pub(crate) fn scene(&self, height: usize) -> (Vec<&Row>, Option<SceneCursor>) {
        let Some(screen) = &self.screen else {
            return (Vec::new(), None);
        };
        let end = self.history_end();
        let top = self.view_top();
        let document = Document(self);
        // Guesses at the echo of what's typed show only in insert mode,
        // where keys go to the program.
        let overlay = self.overlay.as_ref().filter(|_| self.mode == Mode::Insert);
        let rows = (top..top + height as u64)
            .map(|line| match overlay {
                Some(overlay) if overlay.shown.line == line => &overlay.row,
                _ => document.line(line).unwrap_or(&self.blank),
            })
            .collect();
        // In normal mode, only clackatoo's cursor shows.
        let (at, style) = if let Some(cursor) = self.cursor {
            (Some(cursor.at), CursorStyle::Block)
        } else {
            let cursor = screen.state.cursor;
            let style = match cursor.shape {
                CursorShape::Block => CursorStyle::Block,
                CursorShape::Underline => CursorStyle::Underline,
                CursorShape::Default | CursorShape::Bar => CursorStyle::Bar,
            };
            let at = match overlay {
                Some(overlay) => Some(Point {
                    line: overlay.shown.line,
                    col: overlay.shown.cursor,
                }),
                None => cursor.visible.then_some(Point {
                    line: end + u64::from(cursor.line),
                    col: usize::from(cursor.col),
                }),
            };
            (at, style)
        };
        let cursor = at
            .filter(|at| at.line >= top && at.line - top < height as u64)
            .map(|at| SceneCursor {
                line: usize::try_from(at.line - top).expect("within the height"),
                col: at.col,
                style,
            });
        (rows, cursor)
    }

    /// When the highlight on text just copied goes, if it's showing: the
    /// window must be drawn again then.
    pub(crate) fn flash_ends(&self) -> Option<Instant> {
        self.flash
            .map(|flash| flash.until)
            .filter(|&until| until > Instant::now())
    }

    /// What to highlight in a view `height` lines tall: the visual
    /// selection, text just copied, and the last match found.
    pub(crate) fn highlights(&self, height: usize) -> Vec<Highlight> {
        let mut highlights = Vec::new();
        if let Some((start, end, kind)) = self.selection().or_else(|| self.mouse_selected()) {
            self.highlight(
                &mut highlights,
                height,
                (start, end, kind),
                HighlightKind::Selection,
            );
        }
        if let Some(flash) = self.flash
            && flash.until > Instant::now()
        {
            let range = (flash.start, flash.end, flash.kind);
            self.highlight(&mut highlights, height, range, HighlightKind::Copied);
        }
        let top = self.view_top();
        let bottom = top + height as u64;
        if let Some(found) = self.found
            && (top..bottom).contains(&found.start.line)
        {
            highlights.push(Highlight {
                line: usize::try_from(found.start.line - top).expect("within the height"),
                cols: found.start.col..found.start.col + found.cells,
                kind: HighlightKind::Match,
            });
        }
        if let Some(link) = self.hovered_link() {
            let shown = link
                .cells
                .into_iter()
                .filter(|(line, _)| (top..bottom).contains(line));
            highlights.extend(shown.map(|(line, cols)| Highlight {
                line: usize::try_from(line - top).expect("within the height"),
                cols,
                kind: HighlightKind::Link,
            }));
        }
        highlights
    }

    /// Highlights the text from `start` to `end`, characters or whole
    /// lines, where it's in a view `height` lines tall. Whole lines go the
    /// pane's width; characters go on past a row's text by one cell, for
    /// its line break, as the editor's do, unless the row goes on over the
    /// next, so the selection shows what `y` copies.
    fn highlight(
        &self,
        highlights: &mut Vec<Highlight>,
        height: usize,
        (start, end, kind): (Point, Point, Selection),
        highlight: HighlightKind,
    ) {
        let top = self.view_top();
        let bottom = top + height as u64;
        let document = Document(self);
        let in_view = |line: u64| usize::try_from(line - top).expect("within the height");
        for line in start.line.max(top)..end.line.saturating_add(1).min(bottom) {
            let row = document.line(line);
            let width = row.map_or(0, Row::width);
            let cols = match kind {
                Selection::Lines => 0..width,
                // From the top left corner to the bottom right, over each
                // line's text.
                Selection::Block => {
                    let block = Block {
                        first: start.line,
                        last: end.line,
                        left: start.col,
                        right: end.col,
                    };
                    match block.on(row.map_or(0, Row::text_end)) {
                        Some(cols) => cols,
                        None => continue,
                    }
                }
                Selection::Characters => {
                    let from = if line == start.line { start.col } else { 0 };
                    let to = if line == end.line {
                        let wide = row
                            .and_then(|row| row.cells().get(end.col))
                            .is_some_and(|cell| cell.is_wide());
                        (end.col + if wide { 2 } else { 1 }).min(width.max(end.col + 1))
                    } else {
                        row.map_or(from + 1, |row| past_text(row).max(from + 1))
                    };
                    from..to
                }
            };
            highlights.push(Highlight {
                line: in_view(line),
                cols,
                kind: highlight,
            });
        }
    }

    /// The note for the bottom bar: a search being typed, or a message,
    /// with how far up the view is scrolled.
    pub(crate) fn label(&self) -> Option<String> {
        if let Some(chat) = &self.chat {
            return chat.label();
        }
        if let Some(prompt) = &self.prompt {
            let slash = if prompt.backward { '?' } else { '/' };
            return Some(format!("{slash}{}", prompt.line.shown()));
        }
        let mut label = self.status.clone();
        // The pill over the pane says it, where it fits.
        if self.scroll > 0 && self.pill(true).is_none() {
            let up = format!("↑{}", self.scroll);
            label = Some(match label {
                Some(label) => format!("{label}  {up}"),
                None => up,
            });
        }
        // A command half typed shows first, as Vim's `showcmd` shows it.
        if let Some(keys) = self.keys.shown() {
            label = Some(match label {
                Some(label) => format!("{keys}  {label}"),
                None => keys,
            });
        }
        label
    }

    /// What the pill at the pane's bottom right says while the view is
    /// scrolled back from the live screen: how far, and, if the pane is
    /// `focused`, the key that goes back. In insert mode typing does, as
    /// keys go to the program; a pane without the focus takes the
    /// window's mode when it gets it, so says only how far. `None` at the
    /// live screen, or in a pane too small for it, where the bar says how
    /// far instead.
    pub(crate) fn pill(&self, focused: bool) -> Option<String> {
        let screen = self.screen.as_ref().filter(|_| self.scroll > 0)?;
        let lines = if self.scroll == 1 { "line" } else { "lines" };
        let mut pill = format!("↓ {} {lines}", self.scroll);
        if focused {
            let back = if self.mode == Mode::Insert {
                "type"
            } else {
                "G"
            };
            pill = format!("{pill} · {back} to return");
        }
        // A cell of room either side of its text, and a cell from the
        // pane's edge, over its last row, with rows above to read.
        let pane = self.shown_size(screen);
        let fits = pane.rows() >= PILL_ROWS
            && usize::from(pane.cols()) >= crate::render::mono_width(&pill) + 4;
        fits.then_some(pill)
    }

    /// How many cells the pane shows `screen` in: the size asked of the
    /// daemon for it, which the terminal is unless another window decides
    /// that, or else the terminal's.
    fn shown_size(&self, screen: &Screen) -> Size {
        self.requested_size.unwrap_or(screen.size)
    }

    /// Back to insert mode, at the live screen.
    fn insert(&mut self) -> bool {
        self.leave_visual();
        self.mode = Mode::Insert;
        self.cursor = None;
        self.prompt = None;
        self.keys.clear();
        self.status = None;
        self.scroll = 0;
        true
    }

    fn view_top(&self) -> u64 {
        self.history_end().saturating_sub(self.scroll)
    }

    fn height(&self) -> u64 {
        self.screen
            .as_ref()
            .map_or(0, |screen| u64::from(screen.size.rows()))
    }

    /// Sends a key to the program. Returns `None` if it sent nothing, or
    /// else whether the guesses drawn at the echo of what's typed changed.
    fn type_key(&mut self, press: &KeyPress<'_>) -> Option<bool> {
        let screen = self.screen.as_ref()?;
        let mut bytes = Vec::new();
        input::encode(press, screen.state.modes, &mut bytes);
        if bytes.is_empty() {
            return None;
        }
        self.keyed = Some(Instant::now());
        Some(self.send_typed(bytes, predict::Input::of(press)))
    }

    /// Sends `bytes`, typed or pasted in this window, to the program.
    fn send_keyed(&mut self, bytes: Vec<u8>) {
        self.keyed = Some(Instant::now());
        self.send_input(bytes);
    }

    /// Whether a key typed or pasted in this window reached the program
    /// at `since` or later.
    pub(crate) fn keyed_since(&self, since: Instant) -> bool {
        self.keyed.is_some_and(|keyed| keyed >= since)
    }

    fn scroll_by(&mut self, lines: i64) -> bool {
        if self
            .screen
            .as_ref()
            .is_none_or(|screen| screen.state.alternate)
        {
            return false;
        }
        let scroll = self
            .scroll
            .saturating_add_signed(lines)
            .min(self.max_scroll());
        if scroll == self.scroll {
            return false;
        }
        self.scroll = scroll;
        self.fetch_history();
        true
    }

    /// The furthest the view can scroll: to the oldest line held, once
    /// known, or as far as numbering goes until then.
    fn max_scroll(&self) -> u64 {
        self.history_end()
            .saturating_sub(self.history.start.unwrap_or(0))
    }

    fn history_end(&self) -> u64 {
        self.screen
            .as_ref()
            .map_or(0, |screen| screen.state.history_end)
    }

    /// The history lines in view.
    fn shown_history(&self) -> Range<u64> {
        let end = self.history_end();
        let height = self
            .screen
            .as_ref()
            .map_or(0, |screen| u64::from(screen.size.rows()));
        let top = end.saturating_sub(self.scroll);
        top..(top + height).min(end)
    }

    /// Asks for the history lines in view, if any are missing.
    fn fetch_history(&mut self) {
        if self.scroll == 0 {
            return;
        }
        let shown = self.shown_history();
        if let Some(wanted) = self.history.fetch(shown, self.history_end()) {
            let count = u16::try_from(wanted.end - wanted.start).unwrap_or(u16::MAX);
            self.request(
                Request::History {
                    terminal: self.terminal,
                    end: wanted.end,
                    count,
                },
                Pending::History(wanted, self.history.generation()),
            );
        }
    }

    /// Sends `bytes` to the program as though typed.
    pub(crate) fn send_bytes(&mut self, bytes: Vec<u8>) {
        self.send_input(bytes);
    }

    fn send_input(&mut self, bytes: Vec<u8>) {
        // Guesses already drawn stay, and nothing new is drawn.
        let _ = self.send_typed(bytes, predict::Input::Other);
    }

    /// Sends `bytes`, which are `input`, to the program, guessing at
    /// their echo if that can be guessed. Returns whether the guesses drawn
    /// changed.
    fn send_typed(&mut self, bytes: Vec<u8>, input: predict::Input) -> bool {
        let number = self
            .predictor
            .sent(input, self.screen.as_ref(), Instant::now());
        self.request(
            Request::SendInput {
                terminal: self.terminal,
                bytes,
            },
            Pending::Input(number),
        );
        self.refresh_overlay()
    }

    fn request(&mut self, request: Request, pending: Pending) {
        self.pending.push_back(pending);
        self.send(ClientMessage::Request(request));
    }

    fn send(&mut self, message: ClientMessage) {
        self.outbox.push(message);
    }

    /// Takes the messages waiting for the daemon.
    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }
}

/// `row` with `cells` drawn over it, each a column and its character,
/// underlined, in the style of the cell it covers.
fn overlaid(row: &Row, cells: &[(usize, char)]) -> Row {
    let mut builder = RowBuilder::new(row.width());
    let mut text = String::new();
    for (col, &cell) in row.cells().iter().enumerate() {
        let mut style = *row.style(cell);
        text.clear();
        match cells.iter().find(|&&(at, _)| at == col) {
            Some(&(_, ch)) => {
                text.push(ch);
                style.underline = Underline::Single;
            }
            None => row.push_text(col, &mut text),
        }
        if !text.is_empty() {
            builder.push(&text, &style, true);
        }
    }
    builder.finish(row.is_wrapped())
}

/// The name of the folder `cwd`, as a tab is named after it: its last
/// part, `~` if it's `home`, and `/` for the root.
fn folder_name<'a>(cwd: &'a str, home: Option<&str>) -> &'a str {
    let cwd = cwd.trim_end_matches('/');
    if home.is_some_and(|home| home.trim_end_matches('/') == cwd) {
        return "~";
    }
    match cwd.rsplit('/').next() {
        Some(name) if !name.is_empty() => name,
        _ => "/",
    }
}

/// Where a selection going on past `row`'s end stops: a cell after its
/// last that isn't blank, for its line break, or the row's width if it
/// goes on over the next.
/// The top left and bottom right corners of `block`, as the window
/// highlights and copies a block.
pub(crate) fn corners(block: Block) -> (Point, Point) {
    (
        Point {
            line: block.first,
            col: block.left,
        },
        Point {
            line: block.last,
            col: block.right,
        },
    )
}

fn past_text(row: &Row) -> usize {
    if row.is_wrapped() {
        return row.width();
    }
    // Where copying it stops, as the daemon copies it.
    (row.text_end() + 1).min(row.width())
}

impl vim::KeyState for Pane {
    fn mode(&self) -> Mode {
        self.mode
    }

    fn prompting(&self) -> bool {
        Pane::prompting(self)
    }

    fn pending(&self) -> bool {
        Pane::pending(self)
    }

    fn counting(&self) -> Option<usize> {
        Pane::counting(self)
    }

    fn forget_count(&mut self) {
        Pane::forget_count(self);
    }
}

/// History and screen as one document, for the normal-mode cursor: history
/// lines by number, then the screen's rows numbered on from the history's
/// end.
#[derive(Clone, Copy)]
struct Document<'a>(&'a Pane);

impl<'a> Document<'a> {
    /// Line `line`, if it's to hand.
    fn line(self, line: u64) -> Option<&'a Row> {
        let pane = self.0;
        let end = pane.history_end();
        if line < end {
            pane.history.get(line)
        } else {
            let row = usize::try_from(line - end).ok()?;
            pane.screen.as_ref()?.rows.get(row)
        }
    }
}

impl<'d> Lines for Document<'d> {
    type Line = Row;
    type Row<'a>
        = &'d Row
    where
        Self: 'a;

    fn row(&self, line: u64) -> Option<&'d Row> {
        self.line(line)
    }

    /// The rows the pane shows: no `scrolloff` in a terminal.
    fn view(&self) -> Option<View> {
        Some(View {
            top: RowAt {
                line: self.0.view_top(),
                row: 0,
            },
            rows: self.0.height(),
            margin: 0,
        })
    }

    fn bounds(&self) -> (u64, u64) {
        let pane = self.0;
        let end = pane.history_end();
        let last = end + pane.height().saturating_sub(1);
        // The alternate screen has no history to move into.
        let alternate = pane
            .screen
            .as_ref()
            .is_some_and(|screen| screen.state.alternate);
        let first = if alternate {
            end
        } else {
            end - pane.max_scroll()
        };
        (first, last)
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{HistoryLine, HistoryLines, Line, LinkTarget, View};
    use clackatoo_term::{Config, Terminal};
    use winit::keyboard::{KeyCode, KeyLocation};

    use super::*;
    use crate::input::Modifiers;
    use crate::render::FLASH;
    use crate::vim::{WRAPPED_TO_BOTTOM, WRAPPED_TO_TOP};

    /// A 12 × 5 terminal after `output`.
    fn terminal(output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(12, 5).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn pane(terminal: &Terminal) -> Pane {
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(terminal)));
        pane.take_outbox();
        pane
    }

    /// Answers the pane's requests as the daemon would, and those it makes
    /// on hearing the answers.
    fn serve(pane: &mut Pane, terminal: &Terminal) {
        let mut outbox = pane.take_outbox();
        while !outbox.is_empty() {
            serve_once(pane, terminal, outbox);
            outbox = pane.take_outbox();
        }
    }

    fn serve_once(pane: &mut Pane, terminal: &Terminal, outbox: Vec<ClientMessage>) {
        for message in outbox {
            let response = match message {
                ClientMessage::Request(Request::History { end, count, .. }) => {
                    let start = end.saturating_sub(u64::from(count));
                    let mut first = None;
                    let lines: Vec<HistoryLine> = terminal
                        .primary()
                        .scrollback_lines(start..end)
                        .map(|(number, row)| {
                            first.get_or_insert(number);
                            HistoryLine {
                                width: u16::try_from(row.width()).unwrap(),
                                line: Line::from_row(&row),
                            }
                        })
                        .collect();
                    let shown = LinkTarget::of(lines.iter().map(|line| &line.line), terminal);
                    let first = first.unwrap_or(end);
                    let count = u16::try_from(lines.len()).unwrap();
                    Response::History(HistoryLines {
                        first,
                        lines,
                        links: shown,
                        marks: clackatoo_protocol::BlockMarks::of_history(terminal, first, count),
                    })
                }
                ClientMessage::Request(Request::Text {
                    from,
                    to,
                    whole_lines,
                    ..
                }) => Response::Text(terminal.text(from, to, whole_lines)),
                ClientMessage::Request(Request::BlockText { from, to, .. }) => {
                    Response::Text(terminal.block_text(from, to))
                }
                ClientMessage::Request(Request::Search {
                    query,
                    from,
                    backward,
                    exact,
                    ..
                }) => Response::Found(terminal.search(&query, from, backward, exact)),
                ClientMessage::Request(Request::Block { line, which, .. }) => {
                    Response::Block(terminal.block(line, which))
                }
                ClientMessage::Request(_) => Response::Done,
                _ => continue,
            };
            pane.answered(response);
        }
    }

    /// Twelve lines, `{prefix}1` to `{prefix}12`.
    fn numbered_lines(prefix: &str) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        for n in 1..=12 {
            write!(output, "{prefix}{n}\r\n").expect("writing to a String");
        }
        output
    }

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(pane: &mut Pane, key: Key, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(text) => Some(text.to_string()),
            _ => None,
        };
        pane.key(&KeyPress {
            key: &key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            code: None,
            modifiers,
        });
    }

    fn keys(pane: &mut Pane, keys: &str) {
        for ch in keys.chars() {
            press(
                pane,
                Key::Character(ch.to_string().into()),
                Modifiers::default(),
            );
        }
    }

    /// Whether the pane asked the daemon to resize the terminal.
    fn asked_to_resize(pane: &mut Pane) -> bool {
        pane.take_outbox()
            .iter()
            .any(|message| matches!(message, ClientMessage::Request(Request::Resize { .. })))
    }

    #[test]
    fn a_bracketed_paste_cant_end_itself_early() {
        // The program asked for bracketed paste.
        let mut pane = pane(&terminal("\x1b[?2004h"));
        // Taking out the terminator once would leave another.
        pane.paste("\x1b[20\x1b[201~1~; cmd\n");
        let sent: Vec<Vec<u8>> = (pane.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { bytes, .. }) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [b"\x1b[200~[20[201~1~; cmd\n\x1b[201~".to_vec()]);
    }

    #[test]
    fn the_size_is_asked_for_again_after_reconnecting_or_another_resize() {
        let size = Size::new(12, 5).unwrap();
        let mut pane = pane(&terminal(""));
        pane.resize(size);
        assert!(asked_to_resize(&mut pane));
        pane.resize(size);
        assert!(!asked_to_resize(&mut pane), "it was asked already");

        pane.reconnected();
        pane.resize(size);
        assert!(asked_to_resize(&mut pane), "a new connection asks again");

        // Another window made it smaller.
        let smaller = Terminal::new(Size::new(10, 4).unwrap(), &Config::default());
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&smaller)));
        pane.resize(size);
        assert!(
            asked_to_resize(&mut pane),
            "the terminal changed size since"
        );
    }

    fn normal_mode(pane: &mut Pane) {
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        press(pane, Key::Named(NamedKey::Escape), shift);
    }

    /// The text under the cursor's line, and the cursor's place in view.
    fn cursor(pane: &Pane) -> (String, usize, usize) {
        let (rows, cursor) = pane.scene(5);
        let cursor = cursor.expect("a cursor in view");
        (rows[cursor.line].text(), cursor.line, cursor.col)
    }

    #[test]
    fn normal_mode_starts_on_the_program_cursor_and_hjkl_move_it() {
        let terminal = terminal("one\r\ntwo\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(cursor(&pane), ("three".into(), 2, 5));
        keys(&mut pane, "k");
        assert_eq!(cursor(&pane), ("two".into(), 1, 2));
        keys(&mut pane, "hk");
        assert_eq!(cursor(&pane), ("one".into(), 0, 1));
    }

    #[test]
    fn moving_above_the_view_scrolls_into_history() {
        let output = numbered_lines("line ");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(cursor(&pane).0, "");
        // From the screen's bottom line to its top, then two lines above.
        keys(&mut pane, "kkkkkk");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 7".into(), 0, 0));
        assert_eq!(pane.label().as_deref(), Some("↑2"));
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 1".into(), 0, 0));
        keys(&mut pane, "G");
        assert_eq!(cursor(&pane).1, 4);
        assert_eq!(pane.label(), None, "back at the live screen");
    }

    #[test]
    fn resizing_keeps_the_view_on_history_it_rewraps() {
        // The long line starts in history and continues onto the screen.
        let mut terminal = terminal(&(numbered_lines("line ") + &"x".repeat(60) + "\r\n$"));
        let mut pane = pane(&terminal);
        pane.scroll_by(3);
        serve(&mut pane, &terminal);
        let shown =
            |pane: &Pane| -> Vec<String> { pane.scene(5).0.iter().map(|row| row.text()).collect() };
        assert_eq!(shown(&pane)[..3], ["line 11", "line 12", &"x".repeat(12)]);

        // Narrower, the line takes more rows, some of them in history.
        terminal.resize(Size::new(10, 5).unwrap());
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        serve(&mut pane, &terminal);
        assert_eq!(
            shown(&pane)[..4],
            ["line 11", "line 12", &"x".repeat(10), &"x".repeat(10)]
        );

        // Wider, it all fits on screen, and history has fewer lines than
        // before. That isn't a reset: the view stays where it was.
        terminal.resize(Size::new(15, 5).unwrap());
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        serve(&mut pane, &terminal);
        assert_eq!(shown(&pane)[..3], ["line 11", "line 12", &"x".repeat(15)]);
    }

    #[test]
    fn a_snapshot_forgets_the_history_fetched_before_it() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        pane.scroll_by(3);
        serve(&mut pane, &terminal);
        assert!(pane.take_outbox().is_empty(), "the lines shown are here");

        // History may have been renumbered at the same size, as by
        // resizing and back between frames.
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        let asked = pane.take_outbox();
        assert!(
            asked
                .iter()
                .any(|message| matches!(message, ClientMessage::Request(Request::History { .. }))),
            "the lines shown are fetched again"
        );
    }

    #[test]
    fn output_in_normal_mode_leaves_the_cursor_on_its_text() {
        let mut terminal = terminal("a\r\nb\r\nc\r\n");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "kk");
        assert_eq!(cursor(&pane).0, "b");
        terminal.advance(b"d\r\ne\r\nf\r\n");
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal)));
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).0, "b");
    }

    #[test]
    fn ctrl_backslash_twice_reaches_the_program() {
        let terminal = terminal("");
        let mut pane = pane(&terminal);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Normal);
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Insert);
        // Ctrl+C reaches it from normal mode too.
        normal_mode(&mut pane);
        press(&mut pane, Key::Character("c".into()), ctrl);
        let sent: Vec<Vec<u8>> = pane
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { bytes, .. }) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [vec![0x1c], vec![0x03]]);
    }

    #[test]
    fn ctrl_semicolon_switches_to_normal_mode_by_where_the_key_is() {
        let mut pane = pane(&terminal(""));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        // On a German layout, the key where `;` is on a US one types `ö`.
        let key = Key::Character("ö".into());
        let press = |pane: &mut Pane| {
            pane.key(&KeyPress {
                key: &key,
                text: None,
                location: KeyLocation::Standard,
                code: Some(KeyCode::Semicolon),
                modifiers: ctrl,
            })
        };
        press(&mut pane);
        assert_eq!(pane.mode, Mode::Normal);
        // Again, it stays there, and the program gets nothing.
        press(&mut pane);
        assert_eq!(pane.mode, Mode::Normal);
        assert!(pane.take_outbox().is_empty());
    }

    #[test]
    fn normal_mode_entered_before_the_screen_arrives_starts_on_its_cursor() {
        // A new pane, focused from one in normal mode before its terminal's
        // first snapshot.
        let mut pane = Pane::new(TerminalId(1));
        pane.enter_normal_mode();
        assert_eq!(pane.mode, Mode::Normal);
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(&terminal(
            "one\r\ntwo",
        ))));
        assert_eq!(cursor(&pane), ("two".into(), 1, 3));
    }

    #[test]
    fn insert_mode_returns_to_the_live_screen() {
        let output = numbered_lines("");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        assert!(pane.label().is_some_and(|label| label.contains('↑')));
        keys(&mut pane, "i");
        assert_eq!(pane.mode, Mode::Insert);
        assert_eq!(pane.label(), None);
    }

    #[test]
    fn arriving_in_insert_mode_returns_to_the_live_screen_and_in_normal_mode_stays() {
        let output = numbered_lines("");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        // Left and arrived at again in normal mode, it's where it was.
        pane.leave();
        pane.enter_normal_mode();
        assert_eq!(pane.mode, Mode::Normal);
        assert!(pane.label().is_some_and(|label| label.contains('↑')));
        pane.enter_insert_mode();
        assert_eq!(pane.mode, Mode::Insert);
        assert_eq!(pane.label(), None, "back at the live screen");
    }

    #[test]
    fn leaving_ends_a_selection_a_half_typed_command_and_a_search_but_not_the_place() {
        let output = numbered_lines("line ");
        let terminal = terminal(&output);
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "kkv");
        assert_eq!(pane.mode, Mode::Visual(Selection::Characters));
        pane.leave();
        assert_eq!(pane.mode, Mode::Normal);
        assert!(pane.highlights(5).is_empty(), "nothing's selected");
        let at = cursor(&pane);
        keys(&mut pane, "y");
        assert!(pane.pending());
        pane.leave();
        assert!(!pane.pending());
        keys(&mut pane, "/line");
        assert!(pane.prompting());
        pane.leave();
        assert!(!pane.prompting());
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(cursor(&pane), at, "the cursor stays");
        // Ctrl+\ waiting to be pressed again is forgotten too.
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let mut pane = self::pane(&terminal);
        press(&mut pane, Key::Character("\\".into()), ctrl);
        pane.leave();
        pane.enter_normal_mode();
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Normal, "not sent to the program");
    }

    fn named(pane: &mut Pane, key: NamedKey) {
        press(pane, Key::Named(key), Modifiers::default());
    }

    #[test]
    fn visual_selections_copy_their_text() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k0ve");
        assert_eq!(pane.mode, Mode::Visual(Selection::Characters));
        let highlights = pane.highlights(5);
        assert_eq!(highlights.len(), 1);
        assert_eq!((highlights[0].line, highlights[0].cols.clone()), (0, 0..5));
        keys(&mut pane, "y");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("hello")
        );
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(pane.label().as_deref(), Some("copied"));

        // Whole lines, and yy for the cursor's line.
        keys(&mut pane, "Vjy");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("hello world\nsecond")
        );
        keys(&mut pane, "jyy");
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("second")
        );
    }

    #[test]
    fn a_characterwise_selection_ends_a_cell_past_each_rows_text_and_lines_go_the_width() {
        // Three lines of 12 cells: one short, one empty, one with wide
        // characters, and one the cursor ends on.
        let terminal = terminal("hello\r\n\r\n日本\r\nend");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0llv3j0l");
        let cols = |pane: &Pane| {
            (pane.highlights(5).iter())
                .map(|highlight| (highlight.line, highlight.cols.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(cols(&pane), [(0, 2..6), (1, 0..1), (2, 0..5), (3, 0..2)]);
        // Whole lines go the pane's width.
        keys(&mut pane, "V");
        assert_eq!(
            cols(&pane),
            [(0, 0..12), (1, 0..12), (2, 0..12), (3, 0..12)]
        );
        // Starting past a row's text, a cell is still shown there.
        named(&mut pane, NamedKey::Escape);
        keys(&mut pane, "gg$v");
        keys(&mut pane, "j");
        assert_eq!(cols(&pane)[0], (0, 4..6));
    }

    #[test]
    fn gv_selects_the_last_selection_again_and_swaps_with_the_one_being_made() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let selected = |pane: &Pane| {
            (pane.selection())
                .map(|(start, end, kind)| ((start.line, start.col), (end.line, end.col), kind))
        };
        // Nothing's been selected yet.
        keys(&mut pane, "gv");
        assert_eq!(pane.mode, Mode::Normal);
        keys(&mut pane, "k0vey");
        serve(&mut pane, &terminal);
        keys(&mut pane, "jgv");
        assert_eq!(
            selected(&pane),
            Some(((0, 0), (0, 4), Selection::Characters))
        );
        assert_eq!(cursor(&pane), ("hello world".into(), 0, 4), "at its end");
        // In visual mode, the two swap.
        named(&mut pane, NamedKey::Escape);
        keys(&mut pane, "jV");
        named(&mut pane, NamedKey::Escape);
        keys(&mut pane, "vgv");
        assert_eq!(selected(&pane), Some(((1, 4), (1, 4), Selection::Lines)));
        keys(&mut pane, "gv");
        assert_eq!(
            selected(&pane),
            Some(((1, 4), (1, 4), Selection::Characters))
        );
        // Ended by leaving the pane, it's kept.
        pane.leave();
        keys(&mut pane, "gv");
        assert_eq!(
            selected(&pane),
            Some(((1, 4), (1, 4), Selection::Characters))
        );
    }

    #[test]
    fn search_moves_to_matches_and_wraps() {
        let terminal = terminal("cat dog\r\nbird cat\r\nfish");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg/cat");
        assert_eq!(pane.label().as_deref(), Some("/cat▏"));
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("bird cat".into(), 1, 5));
        let found = pane.highlights(5);
        assert_eq!(found[0].kind, HighlightKind::Match);
        assert_eq!(found[0].cols, 5..8);
        // Again, round past the end to the first.
        keys(&mut pane, "n");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("cat dog".into(), 0, 0));
        keys(&mut pane, "N");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).1, 1);
        // Nothing found says so.
        keys(&mut pane, "/zebra");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(pane.label().as_deref(), Some("not found: zebra"));
    }

    #[test]
    fn a_search_that_goes_round_an_end_says_so_once() {
        let terminal = terminal("cat dog\r\nbird cat\r\nfish");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg/cat");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(pane.label(), None);
        keys(&mut pane, "n");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).1, 0);
        assert_eq!(pane.label().as_deref(), Some(WRAPPED_TO_TOP));
        keys(&mut pane, "N");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).1, 1);
        assert_eq!(pane.label().as_deref(), Some(WRAPPED_TO_BOTTOM));
        keys(&mut pane, "N");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).1, 0);
        assert_eq!(pane.label(), None, "no end was gone round");
        // The only match, from on it, is all the way round.
        keys(&mut pane, "/dog");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        keys(&mut pane, "n");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("cat dog".into(), 0, 4));
        assert_eq!(pane.label().as_deref(), Some(WRAPPED_TO_TOP));
    }

    #[test]
    fn a_pill_says_how_far_back_the_view_is_where_it_fits() {
        let output = numbered_lines("line ");
        let wide = |rows| {
            let mut terminal = Terminal::new(Size::new(40, rows).unwrap(), &Config::default());
            terminal.advance(output.as_bytes());
            terminal
        };
        let terminal = wide(5);
        let mut pane = pane(&terminal);
        assert_eq!(pane.pill(true), None, "at the live screen");
        pane.wheel(1, ([0, 0], [0.0, 0.0]), Modifiers::default());
        assert_eq!(
            pane.pill(true).as_deref(),
            Some("↓ 1 line · type to return")
        );
        assert_eq!(pane.label(), None, "the pill says it");
        pane.wheel(2, ([0, 0], [0.0, 0.0]), Modifiers::default());
        assert_eq!(
            pane.pill(true).as_deref(),
            Some("↓ 3 lines · type to return")
        );
        pane.enter_normal_mode();
        assert_eq!(pane.pill(true).as_deref(), Some("↓ 3 lines · G to return"));
        assert_eq!(pane.pill(false).as_deref(), Some("↓ 3 lines"), "unfocused");
        // The pane's own size counts, not the terminal's, which another
        // window may decide.
        pane.resize(Size::new(40, 3).unwrap());
        assert_eq!(pane.pill(true), None, "too short a pane");
        pane.resize(Size::new(40, 5).unwrap());
        keys(&mut pane, "G");
        assert_eq!(pane.pill(true), None, "G went back");

        // Too short a pane, or too narrow, and the bar says it.
        let mut short = self::pane(&wide(3));
        short.wheel(2, ([0, 0], [0.0, 0.0]), Modifiers::default());
        assert_eq!(short.pill(true), None);
        assert_eq!(short.label().as_deref(), Some("↑2"));
        let mut narrow = self::pane(&self::terminal(&output));
        narrow.wheel(2, ([0, 0], [0.0, 0.0]), Modifiers::default());
        assert_eq!(narrow.pill(true), None);
        assert_eq!(narrow.label().as_deref(), Some("↑2"));
    }

    #[test]
    fn a_terminal_with_no_title_is_named_after_its_folder() {
        let named = |output: &str, home| {
            let pane = pane(&terminal(output));
            pane.name(home).map(str::to_owned)
        };
        assert_eq!(named("", None).as_deref(), Some(""), "no folder said");
        let src = "\x1b]7;file://host/home/j/src/\x07";
        assert_eq!(named(src, Some("/home/j")).as_deref(), Some("src"));
        let home = "\x1b]7;file://host/home/j\x07";
        assert_eq!(named(home, Some("/home/j/")).as_deref(), Some("~"));
        assert_eq!(named(home, None).as_deref(), Some("j"), "no home known");
        let root = "\x1b]7;file://host/\x07";
        assert_eq!(named(root, Some("/home/j")).as_deref(), Some("/"));
        // A program's title comes first.
        let titled = format!("{src}\x1b]2;vim\x07");
        assert_eq!(named(&titled, None).as_deref(), Some("vim"));
    }

    #[test]
    fn search_matches_case_as_the_options_say() {
        let terminal = terminal("cat Cat\r\nx");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let found = |pane: &mut Pane| {
            keys(pane, "gg0/cat");
            named(pane, NamedKey::Enter);
            serve(pane, &terminal);
            cursor(pane).2
        };
        assert_eq!(found(&mut pane), 4, "case ignored, to begin with");
        pane.set_options(&Options {
            ignorecase: false,
            ..Options::default()
        });
        assert_eq!(found(&mut pane), 0, "round to the only `cat`");
    }

    /// What `keys` copy, the daemon answering as `terminal`.
    fn yanked(pane: &mut Pane, terminal: &Terminal, keys_: &str) -> Option<String> {
        keys(pane, keys_);
        serve(pane, terminal);
        pane.take_yanked().map(|(text, _)| text)
    }

    #[test]
    fn y_takes_a_motion() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("gg0yw").as_deref(), Some("one "));
        assert_eq!(yank("ye").as_deref(), Some("one"));
        assert_eq!(yank("wy$").as_deref(), Some("two six"));
        // Back over a word moves the cursor to its start.
        assert_eq!(yank("yb").as_deref(), Some("one "));
        assert_eq!(yank("yw").as_deref(), Some("one "));
        // From the last word, `w` would go to the next line, so stops at
        // this one's end.
        assert_eq!(yank("$byw").as_deref(), Some("six"));
        assert_eq!(yank("yj").as_deref(), Some("one two six\nfour five"));
        assert_eq!(yank("jygg").as_deref(), Some("one two six\nfour five"));
        // Anything else after `y` cancels it.
        assert_eq!(yank("yxw"), None);
    }

    #[test]
    fn counts_repeat_motions_and_go_to_lines() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        // From the screen's bottom line, up five: into history.
        keys(&mut pane, "5k");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 8".into(), 0, 0));
        assert!(!pane.pending(), "the count is spent");
        keys(&mut pane, "2j");
        assert_eq!(cursor(&pane).0, "line 10");
        // `G` and `gg` with a count go to that line, from the first.
        keys(&mut pane, "10G");
        assert_eq!(cursor(&pane).0, "line 10");
        keys(&mut pane, "2gg");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 2".into(), 0, 0));
        keys(&mut pane, "99G");
        assert_eq!(cursor(&pane).1, 4, "or the last");
        // A count waits for its command, and Esc forgets it.
        keys(&mut pane, "3");
        assert!(pane.pending());
        named(&mut pane, NamedKey::Escape);
        assert!(!pane.pending());
        keys(&mut pane, "k");
        assert_eq!(cursor(&pane).1, 3, "one line, not three");
        // In visual mode too.
        keys(&mut pane, "Gv3k");
        assert_eq!(
            pane.selection()
                .map(|(start, end, _)| end.line - start.line),
            Some(3)
        );
    }

    #[test]
    fn gg_g_h_and_l_keep_the_column_unless_startofline() {
        let terminal = terminal("  one\r\nb\r\nc\r\nd\r\n  five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        // From past `  five`'s end, as near as `  one` goes.
        keys(&mut pane, "gg");
        assert_eq!(cursor(&pane), ("  one".into(), 0, 4));
        keys(&mut pane, "lG");
        assert_eq!(cursor(&pane), ("  five".into(), 4, 4));
        keys(&mut pane, "1G");
        assert_eq!(cursor(&pane), ("  one".into(), 0, 4));
        keys(&mut pane, "hL");
        assert_eq!(cursor(&pane), ("  five".into(), 4, 3));
        // As Vim's do with it, to the first non-blank.
        pane.set_options(&Options {
            startofline: true,
            ..Options::default()
        });
        // From each line's end, which keeping the column would keep to.
        for (keys_, line) in [
            ("gg", "  one"),
            ("G", "  five"),
            ("H", "  one"),
            ("L", "  five"),
            ("1G", "  one"),
            ("5G", "  five"),
        ] {
            keys(&mut pane, "$");
            keys(&mut pane, keys_);
            assert_eq!(cursor(&pane).0, line, "{keys_}");
            assert_eq!(cursor(&pane).2, 2, "{keys_}");
        }
    }

    #[test]
    fn a_line_of_history_not_fetched_yet_gets_its_column_once_it_comes() {
        let terminal = terminal(&numbered_lines("  line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        // The wanted column, as on any line.
        keys(&mut pane, "k3lgg");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 1".into(), 0, 3));
        // Or with `startofline`, its first non-blank.
        pane.set_options(&Options {
            startofline: true,
            ..Options::default()
        });
        keys(&mut pane, "G$gg");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 1".into(), 0, 2));
    }

    #[test]
    fn a_selection_begun_on_a_line_not_fetched_yet_moves_with_it() {
        let terminal = terminal(&numbered_lines("  line "));
        let ends = |pane: &Pane| (pane.anchor, pane.cursor.map(|cursor| cursor.at));
        let at = |line, col| Some(Point { line, col });
        // `gg` then `v` before the line comes: both ends go to its column.
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k3lggv");
        serve(&mut pane, &terminal);
        assert_eq!(ends(&pane), (at(0, 3), at(0, 3)));
        // And `j` after, onto another line not fetched: each end to its own.
        let mut pane = self::pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k3lggvj");
        serve(&mut pane, &terminal);
        assert_eq!(ends(&pane), (at(0, 3), at(1, 3)));
        // Swapped with `o` before the line comes, the anchor still moves.
        let mut pane = self::pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k3lggvjo");
        serve(&mut pane, &terminal);
        assert_eq!(ends(&pane), (at(1, 3), at(0, 3)));
        // Each end by its own rule: the anchor's column, and `H` with
        // `startofline`, the first non-blank, on the same line.
        let mut pane = self::pane(&terminal);
        normal_mode(&mut pane);
        pane.set_options(&Options {
            startofline: true,
            ..Options::default()
        });
        keys(&mut pane, "k3lkkkkvjH");
        serve(&mut pane, &terminal);
        assert_eq!(ends(&pane), (at(7, 3), at(7, 2)));
        // Back and forth onto the anchor's line keeps one place for each.
        let mut pane = self::pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k3lggvjkjkjk");
        assert!(pane.unplaced.cursor.is_some() && pane.unplaced.anchor.is_some());
        serve(&mut pane, &terminal);
        assert_eq!(ends(&pane), (at(0, 3), at(0, 3)));
        assert!(pane.unplaced.cursor.is_none() && pane.unplaced.anchor.is_none());
    }

    #[test]
    fn ctrl_and_shift_with_left_and_right_are_words_and_words_of_anything_but_blanks() {
        let terminal = terminal("a.b c.d\r\n  x.y z\r\n  l.o end");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let named = |pane: &mut Pane, key, modifiers| {
            press(pane, Key::Named(key), modifiers);
            cursor(pane).2
        };
        keys(&mut pane, "gg$");
        assert_eq!(named(&mut pane, NamedKey::ArrowLeft, CTRL), 4, "as B");
        keys(&mut pane, "$");
        assert_eq!(named(&mut pane, NamedKey::ArrowLeft, shift), 5, "as b");
        keys(&mut pane, "0");
        assert_eq!(named(&mut pane, NamedKey::ArrowRight, CTRL), 4, "as W");
        keys(&mut pane, "0");
        assert_eq!(named(&mut pane, NamedKey::ArrowRight, shift), 1, "as w");
        // Ctrl+End to a line's last character, Ctrl+Home as gg.
        keys(&mut pane, "3");
        press(&mut pane, Key::Named(NamedKey::End), CTRL);
        assert_eq!(cursor(&pane), ("  l.o end".into(), 2, 8));
        press(&mut pane, Key::Named(NamedKey::Home), CTRL);
        assert_eq!(cursor(&pane), ("a.b c.d".into(), 0, 6), "keeping to ends");
        press(&mut pane, Key::Named(NamedKey::End), CTRL);
        assert_eq!(cursor(&pane), (String::new(), 4, 0), "the last line");
        // In insert mode they're the program's.
        keys(&mut pane, "i");
        sent(&mut pane);
        press(&mut pane, Key::Named(NamedKey::ArrowLeft), CTRL);
        press(&mut pane, Key::Named(NamedKey::ArrowUp), shift);
        assert_eq!(
            sent(&mut pane),
            [b"\x1b[1;5D".to_vec(), b"\x1b[1;2A".to_vec()]
        );
        assert_eq!(pane.mode, Mode::Insert);
    }

    #[test]
    fn shift_up_and_shift_down_page_in_normal_mode() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        press(&mut pane, Key::Named(NamedKey::ArrowDown), shift);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 4".into(), 0, 0));
        press(&mut pane, Key::Named(NamedKey::ArrowUp), shift);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 5".into(), 4, 0));
    }

    #[test]
    fn pages_land_at_the_first_non_blank_with_startofline_but_ctrl_e_doesnt() {
        let terminal = terminal(&numbered_lines("  line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k3lgg");
        serve(&mut pane, &terminal);
        // Without it, the column is kept.
        press(&mut pane, Key::Character("f".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 4".into(), 0, 3));
        pane.set_options(&Options {
            startofline: true,
            ..Options::default()
        });
        press(&mut pane, Key::Character("d".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 6".into(), 0, 2));
        keys(&mut pane, "3l");
        press(&mut pane, Key::Character("e".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 7".into(), 0, 5));
        // A page that can't move the view, at the live screen, leaves the
        // cursor where it was.
        named(&mut pane, NamedKey::PageDown);
        assert_eq!(cursor(&pane), ("  line 9".into(), 0, 2));
        keys(&mut pane, "3l");
        named(&mut pane, NamedKey::PageDown);
        assert_eq!(cursor(&pane), ("  line 9".into(), 0, 5));
    }

    #[test]
    fn zt_zz_and_zb_put_the_cursors_line_at_the_views_top_middle_and_bottom() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        keys(&mut pane, "5jzt");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 6".into(), 0, 0));
        keys(&mut pane, "zb");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 6".into(), 4, 0));
        keys(&mut pane, "zz");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 6".into(), 2, 0));
        // No further than the live screen, nor back past history's start.
        keys(&mut pane, "Gzt");
        assert_eq!(cursor(&pane).1, 4);
        assert_eq!(pane.label(), None);
        keys(&mut pane, "ggzb");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 1".into(), 0, 0));
        // A count puts that line there, the cursor on it in its column.
        keys(&mut pane, "2l3zt");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 3".into(), 0, 2));
        keys(&mut pane, "10zb");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 10".into(), 4, 2));
        keys(&mut pane, "7zz");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 7".into(), 2, 2));
        // In the column it's in, still wanting the one it wanted: `$` and
        // `k` left it at 5 on line 9, wanting its end. Line 11 goes no higher than
        // the live screen lets it.
        keys(&mut pane, "10G$k11zt");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 11".into(), 2, 5));
        keys(&mut pane, "j");
        assert_eq!(cursor(&pane), ("line 12".into(), 3, 6));
    }

    #[test]
    fn lines_first_and_last_non_blanks_and_columns_wait_for_their_lines() {
        let terminal = terminal(&numbered_lines("  line "));
        let at = |keys_: &str| {
            let mut pane = pane(&terminal);
            normal_mode(&mut pane);
            // None of history has come when the keys are read.
            keys(&mut pane, keys_);
            serve(&mut pane, &terminal);
            let (text, _, col) = cursor(&pane);
            (text, col)
        };
        assert_eq!(at("gg+"), ("  line 2".into(), 2));
        assert_eq!(at("gg2_"), ("  line 2".into(), 2));
        assert_eq!(at("gg2g_"), ("  line 2".into(), 7));
        assert_eq!(at("gg5|"), ("  line 1".into(), 4));
        assert_eq!(at("gg3+-"), ("  line 3".into(), 2));
    }

    #[test]
    fn zt_with_a_count_keeps_off_a_wide_characters_right_half() {
        let terminal = terminal("a中文b\r\nxyzw");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        // Column 2 on line 2 is the right half of the first wide one.
        keys(&mut pane, "j02l1zt");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane).2, 1);
    }

    #[test]
    fn zt_with_a_count_goes_on_waiting_for_the_first_non_blank() {
        let terminal = terminal(&numbered_lines("  line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        pane.set_options(&Options {
            startofline: true,
            ..Options::default()
        });
        // Neither line has come when the keys are read: line 1 waits to
        // go to its first non-blank, so line 3 does.
        keys(&mut pane, "$gg3zt");
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("  line 3".into(), 0, 2));
    }

    #[test]
    fn h_l_and_half_pages_move_as_many_lines_as_they_say() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "H");
        assert_eq!(cursor(&pane), ("line 9".into(), 0, 0));
        keys(&mut pane, "L");
        assert_eq!(cursor(&pane).1, 4);
        // Half of five lines is two, for the view and the cursor alike.
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("u".into()), ctrl);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 11".into(), 4, 0));
    }

    #[test]
    fn a_count_just_after_ctrl_backslash_counts_and_forgets_it() {
        let terminal = terminal("one\r\ntwo\r\nthree\r\nfour");
        let mut pane = pane(&terminal);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("\\".into()), ctrl);
        keys(&mut pane, "2k");
        assert_eq!(cursor(&pane).0, "two");
        // Ctrl+\ again is no longer the program's.
        press(&mut pane, Key::Character("\\".into()), ctrl);
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn counts_repeat_word_and_paragraph_motions() {
        let terminal = terminal("a b c d\r\n\r\ne f\r\n\r\ng");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg03w");
        assert_eq!(cursor(&pane), ("a b c d".into(), 0, 6));
        keys(&mut pane, "2}");
        assert_eq!(cursor(&pane).1, 3);
        keys(&mut pane, "2{");
        assert_eq!(cursor(&pane).1, 0);
        // A 0 that doesn't follow a digit goes to the line's start.
        keys(&mut pane, "$0");
        assert_eq!(cursor(&pane).2, 0);
    }

    #[test]
    fn counts_copy_lines_and_motions() {
        let terminal = terminal("one two six\r\nfour five\r\nseven\r\neight");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(
            yank("gg03yy").as_deref(),
            Some("one two six\nfour five\nseven")
        );
        assert_eq!(yank("2Y").as_deref(), Some("one two six\nfour five"));
        assert_eq!(yank("y2w").as_deref(), Some("one two "));
        assert_eq!(yank("2y2w").as_deref(), Some("one two six\nfour "));
        assert_eq!(
            yank("y3G").as_deref(),
            Some("one two six\nfour five\nseven")
        );
        // More lines than there are copies those there are.
        let rest = yank("3G9yy").unwrap_or_default();
        assert!(rest.starts_with("seven\neight"), "{rest:?}");
    }

    #[test]
    fn yw_stops_at_the_end_of_its_last_words_line() {
        let terminal = terminal("one\r\n  two three");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("gg0yw").as_deref(), Some("one"));
        assert_eq!(yank("gg0y2w").as_deref(), Some("one\n  two "));
    }

    #[test]
    fn a_copy_the_daemon_cut_short_says_so() {
        let terminal = terminal("one");
        let mut pane = pane(&terminal);
        pane.copied("a\nb".into(), Copy::Clipboard, 2);
        assert_eq!(pane.label().as_deref(), Some("copied 2 lines"));
        pane.copied("a\nb".into(), Copy::Primary, clackatoo_term::MOST_LINES + 1);
        assert!(
            pane.label()
                .is_some_and(|label| label.contains("cut short")),
            "{:?}",
            pane.label()
        );
    }

    #[test]
    fn ctrl_v_selects_a_block_and_insert_mode_sends_it() {
        let terminal = terminal("abcdef\r\nab\r\nabc  defg");
        let mut pane = pane(&terminal);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("v".into()), ctrl);
        let sent = pane.take_outbox().into_iter().any(|message| {
            matches!(
                message,
                ClientMessage::Request(Request::SendInput { bytes, .. }) if bytes == [0x16]
            )
        });
        assert!(sent, "the program's in insert mode");
        pane.answered(Response::Done);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0l");
        press(&mut pane, Key::Character("v".into()), ctrl);
        assert_eq!(pane.mode, Mode::Visual(Selection::Block));
        keys(&mut pane, "jjll");
        // Over each line's text, the short one's as far as it goes.
        let lit: Vec<_> = (pane.highlights(5).into_iter())
            .filter(|highlight| highlight.kind == HighlightKind::Selection)
            .map(|highlight| (highlight.line, highlight.cols))
            .collect();
        assert_eq!(lit, [(0, 1..4), (1, 1..2), (2, 1..4)]);
        // Copied a line for each row, by the daemon, the cursor to the top
        // left. Blanks before more text are kept.
        assert_eq!(
            yanked(&mut pane, &terminal, "y").as_deref(),
            Some("bcd\nb\nbc ")
        );
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(
            pane.cursor.map(|cursor| cursor.at),
            Some(Point { line: 0, col: 1 })
        );
        // `$` takes each to its end; `O` goes to the other corner.
        press(&mut pane, Key::Character("v".into()), ctrl);
        assert_eq!(
            yanked(&mut pane, &terminal, "jj$y").as_deref(),
            Some("bcdef\nb\nbc  defg")
        );
        press(&mut pane, Key::Character("v".into()), ctrl);
        keys(&mut pane, "jjllO");
        assert_eq!(
            pane.cursor.map(|cursor| cursor.at),
            Some(Point { line: 2, col: 1 })
        );
        assert_eq!(pane.anchor, Some(Point { line: 0, col: 3 }));
        // Ctrl+V again ends it.
        press(&mut pane, Key::Character("v".into()), ctrl);
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn y_takes_a_text_object_and_visual_mode_selects_one() {
        let terminal = terminal("f(\"a b\")\r\nx");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(
            yanked(&mut pane, &terminal, "gg0lllyiw").as_deref(),
            Some("a")
        );
        assert_eq!(yanked(&mut pane, &terminal, "yi\"").as_deref(), Some("a b"));
        assert_eq!(
            yanked(&mut pane, &terminal, "ya(").as_deref(),
            Some("(\"a b\")")
        );
        assert_eq!(yanked(&mut pane, &terminal, "ll").as_deref(), None);
        assert_eq!(
            yanked(&mut pane, &terminal, "vi(y").as_deref(),
            Some("\"a b\"")
        );
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn words_selected_take_on_more_and_a_count_goes_over_lines() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("gg0vawawy").as_deref(), Some("one two "));
        assert_eq!(yank("gg0wwy2aw").as_deref(), Some("six\nfour "));
    }

    /// The highlights on text just copied, as lines and columns.
    fn flashed(pane: &Pane) -> Vec<(usize, Range<usize>)> {
        pane.highlights(5)
            .into_iter()
            .filter(|highlight| highlight.kind == HighlightKind::Copied)
            .map(|highlight| (highlight.line, highlight.cols))
            .collect()
    }

    #[test]
    fn copying_highlights_what_was_copied_for_a_moment() {
        let terminal = terminal("one two\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        yanked(&mut pane, &terminal, "gg0yiw");
        assert_eq!(flashed(&pane), [(0, 0..3)]);
        let ends = pane.flash_ends().expect("the window wakes to clear it");
        assert!(ends <= Instant::now() + FLASH);

        // Whole lines are highlighted whole.
        yanked(&mut pane, &terminal, "yj");
        assert_eq!(flashed(&pane), [(0, 0..12), (1, 0..12)]);

        // Then it's gone.
        pane.flash.as_mut().unwrap().until = Instant::now();
        assert!(flashed(&pane).is_empty());
        assert_eq!(pane.flash_ends(), None);
    }

    /// A 20 × 10 terminal where fish ran `ls` and `false`, and is showing
    /// its prompt.
    fn shell() -> Terminal {
        let mut terminal = Terminal::new(Size::new(20, 10).unwrap(), &Config::default());
        let prompt = "\x1b]133;A\x07$ ";
        terminal.advance(
            format!(
                "{prompt}ls\r\n\x1b]133;C;cmdline_url=ls\x07a\r\nb\r\n\x1b]133;D;0\x07\
             {prompt}false\r\n\x1b]133;C;cmdline_url=false\x07\x1b]133;D;1\x07{prompt}"
            )
            .as_bytes(),
        );
        terminal
    }

    #[test]
    fn blocks_are_copied_selected_and_jumped_to() {
        let terminal = shell();
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        assert_eq!(
            yanked(&mut pane, &terminal, "ggjyib").as_deref(),
            Some("a\nb")
        );
        assert_eq!(
            yanked(&mut pane, &terminal, "yab").as_deref(),
            Some("$ ls\na\nb")
        );
        assert_eq!(pane.label().as_deref(), Some("copied 3 lines"));
        // `false` printed nothing.
        assert_eq!(yanked(&mut pane, &terminal, "]]"), None);
        assert_eq!(yanked(&mut pane, &terminal, "yib"), None);
        assert_eq!(pane.label().as_deref(), Some("no output"));
        assert_eq!(cursor(&pane).0, "$ false");
        assert_eq!(yanked(&mut pane, &terminal, "]]").as_deref(), None);
        assert_eq!(cursor(&pane).0, "$");
        yanked(&mut pane, &terminal, "[[");
        yanked(&mut pane, &terminal, "[[");
        assert_eq!(yanked(&mut pane, &terminal, "vab"), None);
        let selected = pane
            .highlights(5)
            .iter()
            .filter(|highlight| highlight.kind == HighlightKind::Selection)
            .count();
        assert_eq!(selected, 3, "the block is selected");
        assert_eq!(
            yanked(&mut pane, &terminal, "y").as_deref(),
            Some("$ ls\na\nb")
        );
        assert_eq!(yanked(&mut pane, &terminal, "[[").as_deref(), None);
        assert_eq!(pane.label().as_deref(), Some("no block here"));
    }

    // Normal mode's keys as they are before they move onto the grammar the
    // editor reads (CLACKA-127), so the move changes none of them.

    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        shift: false,
        alt: false,
    };

    /// What `pane` sent the program.
    fn sent(pane: &mut Pane) -> Vec<Vec<u8>> {
        (pane.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { bytes, .. }) => Some(bytes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn ctrl_e_and_ctrl_y_move_the_view_a_line_and_keep_the_cursor_in_it() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        // Down a line, taking the cursor off the top with it.
        press(&mut pane, Key::Character("e".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 2".into(), 0, 0));
        // Up a line, the cursor staying on its line.
        press(&mut pane, Key::Character("y".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 2".into(), 1, 0));
        // Up from the live screen, the cursor kept to the bottom row.
        keys(&mut pane, "G");
        press(&mut pane, Key::Character("y".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 12".into(), 4, 0));
        assert_eq!(pane.label().as_deref(), Some("↑1"));
    }

    #[test]
    fn ctrl_f_ctrl_d_and_the_page_keys_move_the_view_and_the_cursor() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg4j");
        serve(&mut pane, &terminal);
        // The view's height less two, the cursor to its top line, as Vim's,
        // though line 5 is still in view.
        press(&mut pane, Key::Character("f".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 4".into(), 0, 0));
        // Half of five, rounded down: the view and the cursor two lines.
        press(&mut pane, Key::Character("d".into()), CTRL);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 6".into(), 0, 0));
        // The view stops at the live screen, the cursor at its top.
        named(&mut pane, NamedKey::PageDown);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 9".into(), 0, 0));
        assert_eq!(pane.label(), None);
        // Up, the cursor to the view's bottom line.
        named(&mut pane, NamedKey::PageUp);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 10".into(), 4, 0));
        assert_eq!(pane.label().as_deref(), Some("↑3"));
        named(&mut pane, NamedKey::PageUp);
        serve(&mut pane, &terminal);
        assert_eq!(cursor(&pane), ("line 7".into(), 4, 0));
        // A count before Ctrl+D sets how far it goes from then on.
        keys(&mut pane, "1");
        press(&mut pane, Key::Character("d".into()), CTRL);
        assert_eq!(cursor(&pane), ("line 8".into(), 4, 0));
        press(&mut pane, Key::Character("d".into()), CTRL);
        assert_eq!(cursor(&pane), ("line 9".into(), 4, 0));
        // A new width keeps it; only a new height forgets it.
        let size = pane.screen.as_ref().unwrap().size;
        pane.resize(Size::new(size.cols() + 10, size.rows()).unwrap());
        press(&mut pane, Key::Character("d".into()), CTRL);
        assert_eq!(cursor(&pane).0, "line 10");
    }

    #[test]
    fn m_and_counted_h_and_l_go_to_lines_of_the_view() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        // The view is `line 9` to the empty line after `line 12`.
        keys(&mut pane, "M");
        assert_eq!(cursor(&pane), ("line 11".into(), 2, 0));
        keys(&mut pane, "3H");
        assert_eq!(cursor(&pane), ("line 11".into(), 2, 0));
        keys(&mut pane, "2H");
        assert_eq!(cursor(&pane), ("line 10".into(), 1, 0));
        keys(&mut pane, "9H");
        assert_eq!(cursor(&pane), (String::new(), 4, 0), "no further than L");
        keys(&mut pane, "3L");
        assert_eq!(cursor(&pane), ("line 11".into(), 2, 0));
        keys(&mut pane, "2L");
        assert_eq!(cursor(&pane), ("line 12".into(), 3, 0));
        keys(&mut pane, "9L");
        assert_eq!(cursor(&pane), ("line 9".into(), 0, 0), "no further than H");
        assert!(!pane.pending());
        // An operator takes as far as history goes, as Vim's.
        let lines: Vec<String> = (1..=9).map(|n| format!("line {n}")).collect();
        let yank = yanked(&mut pane, &terminal, "y99L");
        assert_eq!(yank, Some(lines.join("\n")));
    }

    #[test]
    fn y_takes_gg_with_a_count_as_a_line_and_multiplies_it() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("3Gygg").as_deref(), Some("line 1\nline 2\nline 3"));
        assert_eq!(yank("4Gy2gg").as_deref(), Some("line 2\nline 3\nline 4"));
        assert_eq!(yank("ggy2gg").as_deref(), Some("line 1\nline 2"));
        assert_eq!(
            yank("gg2y3gg").as_deref(),
            Some("line 1\nline 2\nline 3\nline 4\nline 5\nline 6")
        );
        // `yg` and a key that isn't `g` copies nothing, and spends the key.
        assert_eq!(yank("gg0ygw"), None);
        assert_eq!(cursor(&pane), ("line 1".into(), 0, 0));
        assert!(!pane.pending());
    }

    #[test]
    fn o_and_capital_o_in_visual_mode_go_to_the_other_end() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let ends = |pane: &Pane| (pane.anchor, pane.cursor.map(|cursor| cursor.at));
        let at = |line, col| Some(Point { line, col });
        keys(&mut pane, "k0vjl");
        assert_eq!(ends(&pane), (at(0, 0), at(1, 1)));
        let selected = pane.selection();
        keys(&mut pane, "o");
        assert_eq!(ends(&pane), (at(1, 1), at(0, 0)));
        assert_eq!(pane.selection(), selected, "the same text");
        keys(&mut pane, "O");
        assert_eq!(ends(&pane), (at(0, 0), at(1, 1)));
        // By lines too.
        named(&mut pane, NamedKey::Escape);
        keys(&mut pane, "Vk");
        assert_eq!(ends(&pane), (at(1, 1), at(0, 1)));
        keys(&mut pane, "O");
        assert_eq!(ends(&pane), (at(0, 1), at(1, 1)));
        keys(&mut pane, "o");
        assert_eq!(ends(&pane), (at(1, 1), at(0, 1)));
        assert_eq!(pane.mode, Mode::Visual(Selection::Lines));
    }

    #[test]
    fn f_t_f_and_t_find_on_the_line_and_semicolon_and_comma_again() {
        // Checked against Neovim (`crates/gui/tests/nvim/check.lua`).
        let terminal = terminal("ab.cd.ef.gh");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let col = |pane: &mut Pane, keys_: &str| {
            keys(pane, keys_);
            cursor(pane).2
        };
        assert_eq!(col(&mut pane, "gg0t."), 1);
        // `;` after `t` goes on past the character it stopped before.
        assert_eq!(col(&mut pane, ";"), 4);
        assert_eq!(col(&mut pane, ";"), 7);
        // `,` the other way: `T`.
        assert_eq!(col(&mut pane, ","), 6);
        assert_eq!(col(&mut pane, "0f.2;"), 8);
        assert_eq!(col(&mut pane, "$F."), 8);
        assert_eq!(col(&mut pane, ","), 8, "no `.` after it");
        assert_eq!(col(&mut pane, "02f."), 5);
        // `y` takes to the character, and back to it.
        let mut yank = |keys_: &str| yanked(&mut pane, &terminal, keys_);
        assert_eq!(yank("0yt.").as_deref(), Some("ab"));
        assert_eq!(yank("$yF.").as_deref(), Some(".g"));
        assert_eq!(cursor(&pane).2, 8);
    }

    #[test]
    fn plus_minus_underscore_g_underscore_bar_and_ge_move_as_the_editors_do() {
        // As check.lua's `spaced`, narrower, checked against Neovim.
        let terminal = terminal("  one tw.th\r\nfour  five\r\n\r\n  six sev\r\neight");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let at = |pane: &mut Pane, keys_: &str| {
            keys(pane, keys_);
            serve(pane, &terminal);
            let (text, _, col) = cursor(pane);
            (text, col)
        };
        let line = |text: &str, col| (text.to_owned(), col);
        assert_eq!(at(&mut pane, "gg5l+"), line("four  five", 0));
        assert_eq!(at(&mut pane, "2+"), line("  six sev", 2));
        assert_eq!(at(&mut pane, "3l-"), line("", 0));
        assert_eq!(at(&mut pane, "2-"), line("  one tw.th", 2));
        assert_eq!(at(&mut pane, "6l_"), line("  one tw.th", 2));
        assert_eq!(at(&mut pane, "4_"), line("  six sev", 2));
        assert_eq!(at(&mut pane, "0g_"), line("  six sev", 8));
        assert_eq!(at(&mut pane, "gg$5|"), line("  one tw.th", 4));
        assert_eq!(at(&mut pane, "30|"), line("  one tw.th", 10));
        assert_eq!(at(&mut pane, "0wwge"), line("  one tw.th", 4));
        assert_eq!(at(&mut pane, "j0ge"), line("  one tw.th", 10));
        assert_eq!(at(&mut pane, "$gE"), line("  one tw.th", 4));
        // `y` takes them as `d` would: lines, or both ends of `ge`.
        let mut yank = |keys_: &str| yanked(&mut pane, &terminal, keys_);
        assert_eq!(yank("gg05ly+").as_deref(), Some("  one tw.th\nfour  five"));
        assert_eq!(yank("gg06lyge").as_deref(), Some("e t"));
        assert_eq!(yank("gg010ly5|").as_deref(), Some("e tw.t"));
    }

    #[test]
    fn y_takes_a_paragraph_as_whole_lines() {
        let terminal = terminal("one\r\n  two\r\n\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let mut yank = |keys_: &str| yanked(&mut pane, &terminal, keys_);
        assert_eq!(yank("gg03lyip").as_deref(), Some("one\n  two"));
        assert_eq!(yank("ggyap").as_deref(), Some("one\n  two\n"));
        assert_eq!(cursor(&pane).2, 0, "at the paragraph's start");
    }

    #[test]
    fn commands_a_terminal_doesnt_take_are_nothing_and_spend_their_keys() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        // An operator but `y`, a register, `z` and `m` wait for the rest of
        // their command, which isn't a terminal's: all its keys are spent.
        for keys_ in ["dw", "\"aw", "zw", "mw"] {
            keys(&mut pane, "0");
            keys(&mut pane, &keys_[..keys_.len() - 1]);
            assert!(pane.pending(), "{keys_}");
            keys(&mut pane, "w");
            assert!(!pane.pending(), "{keys_}");
            assert_eq!(cursor(&pane), ("one two six".into(), 0, 0), "{keys_}");
        }
        // Entering insert mode but by `i` does nothing.
        keys(&mut pane, "w");
        for key in ["a", "A", "o"] {
            keys(&mut pane, key);
            assert_eq!(pane.mode, Mode::Normal, "{key}");
            assert_eq!(cursor(&pane), ("one two six".into(), 0, 4), "{key}");
        }
        // Nor does `r` and a character in visual mode.
        keys(&mut pane, "vrw");
        assert_eq!(pane.mode, Mode::Visual(Selection::Characters));
        assert_eq!(cursor(&pane), ("one two six".into(), 0, 4));
        assert!(sent(&mut pane).is_empty());
        assert_eq!(pane.take_yanked(), None);
    }

    #[test]
    fn a_half_typed_command_shows_in_the_bar_and_takes_the_next_key() {
        let terminal = terminal("one two\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let bar = |keys_: &str| {
            let mut pane = self::pane(&terminal);
            normal_mode(&mut pane);
            keys(&mut pane, keys_);
            pane.label()
        };
        assert_eq!(bar("3").as_deref(), Some("3"));
        assert_eq!(bar("y3").as_deref(), Some("y3"));
        assert_eq!(bar("2y").as_deref(), Some("2y"));
        assert_eq!(bar("\"a").as_deref(), Some("\"a"));
        // `d` isn't a terminal's, but waits for its motion like any
        // operator, so the bar says why `:` does nothing after it.
        keys(&mut pane, "d");
        assert!(pane.pending());
        assert_eq!(pane.label().as_deref(), Some("d"));
        keys(&mut pane, ":");
        assert!(!pane.pending() && !pane.prompting());
        assert_eq!(pane.label(), None);
    }

    #[test]
    fn y_takes_every_motion_space_backspace_and_enter_too() {
        let terminal = terminal(&numbered_lines("line "));
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg");
        serve(&mut pane, &terminal);
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("y ").as_deref(), Some("l"), "as yl");
        assert_eq!(yank("lly").as_deref(), None);
        let backspace = Key::Named(NamedKey::Backspace);
        press(&mut pane, backspace, Modifiers::default());
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("i"),
            "as yh"
        );
        keys(&mut pane, "y");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("line 1\nline 2"),
            "as yj"
        );
        // The view's lines, the ends of words of anything but blanks, and
        // the first non-blank.
        let mut yank = |keys: &str| yanked(&mut pane, &terminal, keys);
        assert_eq!(yank("ggyH").as_deref(), Some("line 1"));
        assert_eq!(yank("gg0yE").as_deref(), Some("line"));
        assert_eq!(yank("gg0yW").as_deref(), Some("line "));
        assert_eq!(yank("gg$y^").as_deref(), Some("line "));
    }

    #[test]
    fn the_motions_a_terminal_takes_now() {
        let terminal = terminal("  a.b c-d\r\nx");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg$^");
        assert_eq!(cursor(&pane).2, 2);
        keys(&mut pane, "W");
        assert_eq!(cursor(&pane).2, 6);
        keys(&mut pane, "B");
        assert_eq!(cursor(&pane).2, 2);
        keys(&mut pane, "E");
        assert_eq!(cursor(&pane).2, 4);
    }

    #[test]
    fn g_and_a_key_it_doesnt_take_and_alt_and_a_letter_are_nothing() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0");
        // `gw` was `w`, `gy` began a copy, and `gi` typed.
        for keys_ in ["gw", "gy", "gi"] {
            keys(&mut pane, keys_);
            assert!(!pane.pending(), "{keys_}");
            assert_eq!(pane.mode, Mode::Normal, "{keys_}");
            assert_eq!(cursor(&pane), ("one two six".into(), 0, 0), "{keys_}");
        }
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        press(&mut pane, Key::Character("w".into()), alt);
        press(&mut pane, Key::Character("i".into()), alt);
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(cursor(&pane), ("one two six".into(), 0, 0));
        assert!(sent(&mut pane).is_empty());
    }

    #[test]
    fn esc_after_keys_half_typed_only_forgets_them_but_after_a_count_or_register_leaves() {
        // As Neovim's, checked with check.lua.
        let terminal = terminal("one two\r\nthree");
        for (keys_, visual) in [("vg", true), ("vi", true), ("v3", false), ("v\"a", false)] {
            let mut pane = pane(&terminal);
            normal_mode(&mut pane);
            keys(&mut pane, keys_);
            named(&mut pane, NamedKey::Escape);
            assert_eq!(matches!(pane.mode, Mode::Visual(_)), visual, "{keys_}");
            assert!(!pane.pending(), "{keys_}");
        }
    }

    #[test]
    fn esc_after_a_count_leaves_visual_mode_and_ctrl_c_after_y_reaches_the_program() {
        let terminal = terminal("one two\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "v3");
        named(&mut pane, NamedKey::Escape);
        assert_eq!(pane.mode, Mode::Normal);
        assert!(!pane.pending());
        // Ctrl+C reaches the program whatever's half typed, and forgets it.
        keys(&mut pane, "y");
        press(&mut pane, Key::Character("c".into()), CTRL);
        assert_eq!(sent(&mut pane), [vec![0x03]]);
        assert!(!pane.pending());
    }

    #[test]
    fn a_bracket_or_y_and_a_key_they_dont_take_spend_both() {
        let terminal = terminal("one two six\r\nfour five");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0[w");
        assert_eq!(cursor(&pane), ("one two six".into(), 0, 0), "not by `w`");
        assert!(!pane.pending());
        assert_eq!(yanked(&mut pane, &terminal, "yx"), None);
        assert!(!pane.pending());
        // The next key starts afresh: it moves, and copies nothing.
        assert_eq!(yanked(&mut pane, &terminal, "w"), None);
        assert_eq!(cursor(&pane), ("one two six".into(), 0, 4));
    }

    #[test]
    fn g_and_j_k_0_or_dollar_move_as_the_key_alone() {
        let terminal = terminal("one two\r\nthree\r\nfour\r\nfive\r\nsix");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0gj");
        assert_eq!(cursor(&pane), ("three".into(), 1, 0));
        keys(&mut pane, "gk");
        assert_eq!(cursor(&pane), ("one two".into(), 0, 0));
        keys(&mut pane, "g$");
        assert_eq!(cursor(&pane), ("one two".into(), 0, 6));
        keys(&mut pane, "g0");
        assert_eq!(cursor(&pane), ("one two".into(), 0, 0));
        keys(&mut pane, "3gj");
        assert_eq!(cursor(&pane), ("five".into(), 3, 0));
        assert!(!pane.pending());
    }

    #[test]
    fn esc_forgets_the_match_in_normal_mode_and_leaves_visual_mode() {
        let terminal = terminal("cat dog\r\nbird cat\r\nfish");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        let lit = |pane: &Pane, kind| {
            (pane.highlights(5).iter())
                .filter(|highlight| highlight.kind == kind)
                .count()
        };
        keys(&mut pane, "gg/cat");
        named(&mut pane, NamedKey::Enter);
        serve(&mut pane, &terminal);
        assert_eq!(lit(&pane, HighlightKind::Match), 1);
        let at = cursor(&pane);
        named(&mut pane, NamedKey::Escape);
        assert_eq!(lit(&pane, HighlightKind::Match), 0);
        assert_eq!(cursor(&pane), at);
        keys(&mut pane, "vl");
        assert_eq!(lit(&pane, HighlightKind::Selection), 1);
        named(&mut pane, NamedKey::Escape);
        assert_eq!(pane.mode, Mode::Normal);
        assert_eq!(lit(&pane, HighlightKind::Selection), 0);
    }

    #[test]
    fn ctrl_shift_c_copies_a_visual_selection() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "k0ve");
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        press(&mut pane, Key::Character("C".into()), ctrl_shift);
        serve(&mut pane, &terminal);
        assert_eq!(
            pane.take_yanked().map(|(text, _)| text).as_deref(),
            Some("hello")
        );
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn a_half_typed_command_is_pending_and_a_count_alone_is_counting() {
        let terminal = terminal("one two\r\nthree");
        let after = |keys_: &str| {
            let mut pane = pane(&terminal);
            normal_mode(&mut pane);
            keys(&mut pane, keys_);
            (pane.pending(), pane.counting())
        };
        assert_eq!(after(""), (false, None));
        assert_eq!(after("3"), (true, Some(3)));
        assert_eq!(after("y"), (true, None));
        assert_eq!(after("y3"), (true, None));
        assert_eq!(after("g"), (true, None));
        assert_eq!(after("3g"), (true, None));
        assert_eq!(after("["), (true, None));
        assert_eq!(after("vi"), (true, None));
    }

    #[test]
    fn enter_space_and_backspace_do_nothing() {
        let terminal = terminal("one two\r\nthree");
        let mut pane = pane(&terminal);
        normal_mode(&mut pane);
        keys(&mut pane, "gg0l");
        for key in [NamedKey::Enter, NamedKey::Space, NamedKey::Backspace] {
            named(&mut pane, key);
            assert_eq!(cursor(&pane), ("one two".into(), 0, 1), "{key:?}");
            assert!(!pane.pending(), "{key:?}");
            assert_eq!(pane.mode, Mode::Normal, "{key:?}");
        }
        assert!(sent(&mut pane).is_empty());
    }
}

#[cfg(test)]
mod mouse_tests {
    use clackatoo_protocol::View;
    use clackatoo_term::mouse::{MouseAction, MouseButton};
    use clackatoo_term::{Config, Terminal};

    use super::*;
    use crate::input::Modifiers;

    fn terminal(output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(12, 5).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn pane(terminal: &Terminal) -> Pane {
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(terminal)));
        pane.take_outbox();
        pane
    }

    fn mouse(pane: &mut Pane, action: MouseAction, [line, col]: [i64; 2], clicks: u8) -> bool {
        mouse_with(pane, action, [line, col], clicks, Modifiers::default())
    }

    fn mouse_with(
        pane: &mut Pane,
        action: MouseAction,
        [line, col]: [i64; 2],
        clicks: u8,
        modifiers: Modifiers,
    ) -> bool {
        pane.mouse(&PaneMouse {
            pixel: [0.0; 2],
            action,
            line,
            col,
            clicks,
            modifiers,
            block: modifiers.alt,
        })
    }

    /// Everything the pane sent the program.
    fn input(pane: &mut Pane) -> String {
        let mut sent = Vec::new();
        for message in pane.take_outbox() {
            if let ClientMessage::Request(Request::SendInput { bytes, .. }) = message {
                sent.extend(bytes);
            }
        }
        String::from_utf8(sent).unwrap()
    }

    /// Answers a request for text, and returns what was copied, and where.
    fn copied(pane: &mut Pane, terminal: &Terminal) -> Option<(String, Copy)> {
        for message in pane.take_outbox() {
            if let ClientMessage::Request(Request::Text {
                from,
                to,
                whole_lines,
                ..
            }) = message
            {
                pane.answered(Response::Text(terminal.text(from, to, whole_lines)));
            } else if let ClientMessage::Request(Request::BlockText { from, to, .. }) = message {
                pane.answered(Response::Text(terminal.block_text(from, to)));
            }
        }
        pane.take_yanked()
    }

    const LEFT: MouseButton = MouseButton::Left;

    #[test]
    fn alt_drags_a_block_and_letting_go_copies_it() {
        let terminal = terminal("abcdef\r\nab\r\nabc  defg");
        let mut pane = pane(&terminal);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        mouse_with(&mut pane, MouseAction::Press(LEFT), [0, 1], 1, alt);
        mouse_with(&mut pane, MouseAction::Motion(Some(LEFT)), [2, 3], 1, alt);
        let lit: Vec<_> = (pane.highlights(5).into_iter())
            .map(|highlight| (highlight.line, highlight.cols))
            .collect();
        assert_eq!(lit, [(0, 1..4), (1, 1..2), (2, 1..4)]);
        mouse_with(&mut pane, MouseAction::Release(LEFT), [2, 3], 1, alt);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("bcd\nb\nbc ".into(), Copy::Primary))
        );
        // Ctrl and Alt drag one too, and open no link.
        let ctrl_alt = Modifiers { ctrl: true, ..alt };
        assert!(!pane.link_opens(ctrl_alt));
        mouse_with(&mut pane, MouseAction::Press(LEFT), [2, 3], 1, ctrl_alt);
        mouse_with(
            &mut pane,
            MouseAction::Motion(Some(LEFT)),
            [0, 2],
            1,
            ctrl_alt,
        );
        mouse_with(&mut pane, MouseAction::Release(LEFT), [0, 2], 1, ctrl_alt);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("cd\n\nc ".into(), Copy::Primary))
        );
    }

    #[test]
    fn a_program_that_asks_gets_clicks_drags_and_the_wheel() {
        let terminal = terminal("\x1b[?1002h\x1b[?1006h");
        let mut pane = pane(&terminal);
        mouse(&mut pane, MouseAction::Press(LEFT), [1, 2], 1);
        mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [1, 3], 1);
        // Dragged off the pane, the position stays on its edge.
        mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [9, 40], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [9, 40], 1);
        assert_eq!(
            input(&mut pane),
            "\x1b[<0;3;2M\x1b[<32;4;2M\x1b[<32;12;5M\x1b[<0;12;5m"
        );
        pane.wheel(2, ([0, 0], [0.0; 2]), Modifiers::default());
        assert_eq!(input(&mut pane), "\x1b[<64;1;1M\x1b[<64;1;1M");
        // Shift keeps the mouse clackatoo's.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        mouse_with(&mut pane, MouseAction::Press(LEFT), [0, 0], 1, shift);
        assert_eq!(input(&mut pane), "");
    }

    /// Presses `text`'s key, with no modifiers.
    fn key(pane: &mut Pane, text: &str) {
        let key = Key::Character(text.into());
        pane.key(&KeyPress {
            key: &key,
            text: Some(text),
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
    }

    fn wheel(pane: &mut Pane, lines: i64) -> bool {
        pane.wheel(lines, ([0, 0], [0.0; 2]), Modifiers::default())
    }

    #[test]
    fn on_the_alternate_screen_the_wheel_sends_arrows_only_in_insert_mode() {
        let terminal = terminal("\x1b[?1049hone\r\ntwo\r\nthree\r\nfour\r\nfive");
        let mut pane = pane(&terminal);
        wheel(&mut pane, 2);
        assert_eq!(input(&mut pane), "\x1b[A\x1b[A");
        wheel(&mut pane, -1);
        assert_eq!(input(&mut pane), "\x1b[B");

        // Normal mode keeps the wheel from the program, and moves its
        // cursor instead, within the screen, which has no history.
        pane.enter_normal_mode();
        let end = pane.history_end();
        let line = |pane: &Pane| pane.cursor.expect("a cursor").at.line - end;
        assert_eq!(line(&pane), 4);
        assert!(wheel(&mut pane, 2));
        assert_eq!(line(&pane), 2);
        wheel(&mut pane, 10);
        assert_eq!(line(&pane), 0, "no further than the top");
        wheel(&mut pane, -3);
        assert_eq!(line(&pane), 3);
        wheel(&mut pane, -10);
        assert_eq!(line(&pane), 4, "no further than the bottom");
        assert_eq!(pane.scroll, 0);
        assert_eq!(input(&mut pane), "");

        // In visual mode too, and the selection grows as the cursor goes.
        key(&mut pane, "V");
        wheel(&mut pane, 2);
        let selected: Vec<usize> = (pane.highlights(5).into_iter())
            .filter(|highlight| highlight.kind == HighlightKind::Selection)
            .map(|highlight| highlight.line)
            .collect();
        assert_eq!(selected, [2, 3, 4]);
        assert_eq!(input(&mut pane), "");
    }

    #[test]
    fn a_program_that_asks_gets_no_wheel_in_normal_mode() {
        let terminal = terminal("\x1b[?1049h\x1b[?1002h\x1b[?1006hone\r\ntwo\r\nthree");
        let mut pane = pane(&terminal);
        pane.enter_normal_mode();
        wheel(&mut pane, 2);
        assert_eq!(input(&mut pane), "");
        // The cursor moved from the third line to the first.
        assert_eq!(pane.cursor.expect("a cursor").at.line, pane.history_end());
    }

    #[test]
    fn on_the_main_screen_the_wheel_scrolls_history_in_normal_mode_too() {
        let output: String = (1..=12).map(|n| n.to_string() + "\r\n").collect();
        let mut pane = pane(&terminal(&output));
        pane.enter_normal_mode();
        assert!(wheel(&mut pane, 2));
        assert_eq!(pane.scroll, 2);
        assert_eq!(input(&mut pane), "");
        // The cursor stays in view, on its bottom line.
        let bottom = pane.history_end() - 2 + 4;
        assert_eq!(pane.cursor.expect("a cursor").at.line, bottom);
    }

    #[test]
    fn dragging_selects_and_copies_to_the_primary_selection() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        // A click alone selects nothing.
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 0], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 0], 1);
        assert_eq!(copied(&mut pane, &terminal), None);

        mouse(&mut pane, MouseAction::Press(LEFT), [0, 6], 1);
        assert!(mouse(&mut pane, MouseAction::Motion(Some(LEFT)), [1, 2], 1));
        let highlights = pane.highlights(5);
        assert_eq!(
            highlights
                .iter()
                .map(|highlight| (highlight.line, highlight.cols.clone()))
                .collect::<Vec<_>>(),
            [(0, 6..12), (1, 0..3)]
        );
        mouse(&mut pane, MouseAction::Release(LEFT), [1, 2], 1);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("world\nsec".to_owned(), Copy::Primary))
        );
        // It stays shown, and Ctrl+Shift+C copies it to the clipboard.
        assert_eq!(pane.highlights(5).len(), 2);
        // A short row's band ends a cell past its text.
        let short = self::terminal("hi\r\nsecond");
        let mut other = self::pane(&short);
        mouse(&mut other, MouseAction::Press(LEFT), [0, 1], 1);
        mouse(&mut other, MouseAction::Motion(Some(LEFT)), [1, 2], 1);
        assert_eq!(
            (other.highlights(5).iter())
                .map(|highlight| (highlight.line, highlight.cols.clone()))
                .collect::<Vec<_>>(),
            [(0, 1..3), (1, 0..3)]
        );
        let key = Key::Character("C".into());
        pane.key(&KeyPress {
            key: &key,
            text: None,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                ctrl: true,
                shift: true,
                alt: false,
            },
        });
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("world\nsec".to_owned(), Copy::Clipboard))
        );
        // Nothing went to the program, and the selection is gone, but for
        // a moment the clipboard's copy is highlighted. The primary
        // selection's wasn't: the selection showed it already.
        assert_eq!(input(&mut pane), "");
        let highlights = pane.highlights(5);
        assert_eq!(highlights.len(), 2);
        assert!(
            highlights
                .iter()
                .all(|highlight| highlight.kind == HighlightKind::Copied)
        );
    }

    #[test]
    fn y_copies_the_mouses_selection_only_with_nothing_typed() {
        let terminal = terminal("hello world\r\nsecond");
        let mut pane = pane(&terminal);
        pane.enter_normal_mode();
        let select = |pane: &mut Pane| {
            mouse(pane, MouseAction::Press(LEFT), [0, 6], 1);
            mouse(pane, MouseAction::Motion(Some(LEFT)), [1, 2], 1);
            mouse(pane, MouseAction::Release(LEFT), [1, 2], 1);
            copied(pane, &terminal);
        };
        select(&mut pane);
        key(&mut pane, "y");
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("world\nsec".to_owned(), Copy::Clipboard))
        );
        // After `y`, the next `y` is `yy`, which copies the cursor's line,
        // where the drag began.
        key(&mut pane, "y");
        select(&mut pane);
        key(&mut pane, "y");
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("hello world".to_owned(), Copy::Clipboard))
        );
    }

    #[test]
    fn double_and_triple_clicks_select_a_word_and_a_line() {
        let terminal = terminal("cat ~/a.txt\r\nsecond");
        let mut pane = pane(&terminal);
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 8], 2);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 8], 2);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("~/a.txt".to_owned(), Copy::Primary))
        );
        mouse(&mut pane, MouseAction::Press(LEFT), [1, 1], 3);
        mouse(&mut pane, MouseAction::Release(LEFT), [1, 1], 3);
        assert_eq!(
            copied(&mut pane, &terminal),
            Some(("second".to_owned(), Copy::Primary))
        );
    }

    #[test]
    fn a_click_in_normal_mode_moves_the_cursor() {
        let terminal = terminal("hello world");
        let mut pane = pane(&terminal);
        pane.enter_normal_mode();
        mouse(&mut pane, MouseAction::Press(LEFT), [0, 6], 1);
        mouse(&mut pane, MouseAction::Release(LEFT), [0, 6], 1);
        assert_eq!(pane.cursor.map(|cursor| cursor.at.col), Some(6));
        assert_eq!(pane.mode, Mode::Normal);
    }

    #[test]
    fn programs_that_ask_are_told_about_focus() {
        let terminal = terminal("\x1b[?1004h");
        let mut pane = pane(&terminal);
        pane.focus_changed(false);
        pane.focus_changed(true);
        assert_eq!(input(&mut pane), "\x1b[O\x1b[I");
        let mut quiet = super::mouse_tests::pane(&self::terminal(""));
        quiet.focus_changed(true);
        assert_eq!(input(&mut quiet), "");
    }
}
