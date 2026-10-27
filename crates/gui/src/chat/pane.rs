//! A pane showing a chat with an agent: the window's side of the chat
//! (see [`ChatView`]), its mode, and what it asks of the daemon.
//!
//! The daemon hosts a chat as a terminal, whose view is a chat rather
//! than a grid of cells: the window attaches to it, sizes it and
//! acknowledges its updates as it does a terminal's. It learns which a
//! terminal is only from its first view, so every terminal's pane starts
//! as one (see [`crate::pane::Pane`]), and one whose view turns out to
//! be a chat is [taken over](ChatPane::taking_over) by a chat's pane.
//!
//! In insert mode, keys edit the message being typed. In normal or
//! visual mode, they read the conversation (see [`ChatView::read_key`]),
//! where an answer's number, pressed twice or with Enter, answers what
//! the agent asks. Esc goes to normal mode, as there's no program to need
//! it, unless the agent is asking something.

use std::time::Instant;

use clackatoo_protocol::{ChatAction, ChatNode, Request, Response, Size, View, ViewUpdate};
use winit::keyboard::{Key, NamedKey};

use super::{ChatView, Pointer, TextPaint};
use crate::fonts::Fonts;
use crate::input::{self, KeyPress};
use crate::options::Options;
use crate::pane::{Copy, Pane, PaneMouse, Requests, Yanked};
use crate::vim::{self, Mode};
use clackatoo_term::mouse::{MouseAction, MouseButton};

/// A request sent to the daemon and not yet answered.
#[derive(Debug)]
enum Pending {
    /// Anything answered with just "done" or an error.
    Other,
    /// The original of an image, asked for by one opening of the image
    /// viewer.
    Original(u64),
    /// A message sent to another agent, or with one.
    Handoff,
    /// A message queued, sent or taken back.
    Control,
    /// Older turns, before the one numbered so.
    History(u32),
}

/// A chat shown in the window.
#[derive(Debug)]
pub(crate) struct ChatPane {
    requests: Requests<Pending>,
    view: ChatView,
    mode: Mode,
    /// About the most of the conversation, in tokens, the chat tells an
    /// agent it moves to, as the options say.
    carryover: u32,
    /// Text the chat copied, and where to put it, oldest first.
    copies: Vec<(String, Copy)>,
    /// What it yanked, for the registers, oldest first.
    yanks: Vec<Yanked>,
}

impl ChatPane {
    /// The chat `pane` turns out to show, as `view`, its first, says: it
    /// takes over what the pane sent and its mode.
    pub(crate) fn taking_over(pane: Pane, view: &View, node: &ChatNode) -> Self {
        let (requests, mode) = pane.handed_over();
        let mut chat = Self {
            requests: requests.handed_over(|()| Pending::Other),
            view: ChatView::of_view(view, node),
            // A chat's visual mode is its own, begun in it.
            mode: if mode == Mode::Insert {
                Mode::Insert
            } else {
                Mode::Normal
            },
            carryover: Options::default().carryover,
            copies: Vec::new(),
            yanks: Vec::new(),
        };
        chat.requests.changed();
        chat
    }

    /// The terminal the daemon hosts it as.
    pub(crate) fn terminal(&self) -> clackatoo_protocol::TerminalId {
        self.requests.terminal()
    }

    /// The window's side of the chat.
    pub(crate) fn view(&self) -> &ChatView {
        &self.view
    }

    /// The same, to change.
    pub(crate) fn view_mut(&mut self) -> &mut ChatView {
        &mut self.view
    }

    pub(crate) fn mode(&self) -> Mode {
        self.mode
    }

    /// The mode the bar shows: visual while text selected with the mouse
    /// is what `y` would yank, as in a terminal.
    pub(crate) fn shown_mode(&self) -> Mode {
        if self.mode == Mode::Normal && !self.prompting() && self.view.mouse_selecting() {
            Mode::Visual(crate::vim::Selection::Characters)
        } else {
            self.mode
        }
    }

    /// Takes in the options, as they've changed.
    pub(crate) fn set_options(&mut self, options: &Options) {
        self.carryover = options.carryover;
        self.view.set_case(crate::options::Case::of(options));
        self.view.set_hlsearch(options.hlsearch);
        self.view.set_incsearch(options.incsearch);
    }

    /// The number of the first turn it holds, the rest being older and
    /// fetched as it's scrolled up to.
    pub(crate) fn first_turn(&self) -> u32 {
        self.view.first_turn()
    }

    /// Its title, which names a tab showing it.
    pub(crate) fn title(&self) -> &str {
        &self.view.title
    }

    /// Whether its agent is at work.
    pub(crate) fn working(&self) -> bool {
        self.view.working()
    }

    /// Whether its agent is asking something, which keys in insert mode
    /// would answer.
    pub(crate) fn asking(&self) -> bool {
        self.view.node.question.is_some()
    }

    /// Whether a local image modal owns the chat's input.
    pub(crate) fn viewing_modal(&self) -> bool {
        self.view.viewing_modal()
    }

    /// Whether the model search is being typed in, which takes text.
    pub(crate) fn searching_models(&self) -> bool {
        self.view.searching_models()
    }

    /// Where typed text goes in it: the message, in insert mode, or the
    /// model search.
    pub(crate) fn takes_text(&self) -> bool {
        self.mode == Mode::Insert || self.searching_models()
    }

    /// Tells it the daemon has gone out of reach, if `offline`, or come
    /// back.
    pub(crate) fn set_offline(&mut self, offline: bool) {
        self.view.set_offline(offline);
    }

    /// Asks the daemon to undo what turn `turn` changed in files, or with
    /// `redo` to make it again: or, with no turn, the last that can be
    /// undone, or the first that can be redone, as `u` and Ctrl+R do.
    /// `force` does what can be done though some files can't be. Says why
    /// not, if it can't be asked.
    pub(crate) fn undo_turn(
        &mut self,
        turn: Option<usize>,
        redo: bool,
        force: bool,
    ) -> Result<(), String> {
        let chat = &self.view;
        let undo = if redo { "redo" } else { "undo" };
        // Asked for by its number, which doesn't count where the chat
        // moved to another agent.
        let turn = match turn {
            Some(turn) => {
                let local = turn
                    .checked_sub(chat.node.first_message as usize)
                    .ok_or("Scroll up to load that turn before undoing it")?;
                clackatoo_protocol::turn_numbered(&chat.node.turns, local + 1)
                    .ok_or(format!("there's no turn {}", turn + 1))?
            }
            None => (chat.turn_to_undo(redo)).ok_or(format!("no turn has files to {undo}"))?,
        };
        let turn = u32::try_from(turn)
            .unwrap_or(u32::MAX)
            .saturating_add(chat.first_turn());
        let action = if redo {
            ChatAction::Redo { turn, force }
        } else {
            ChatAction::Undo { turn, force }
        };
        self.ask(action, Pending::Other);
        Ok(())
    }

    /// Lays it out and draws it in a pane `size` pixels with its top left
    /// at `origin`. `caret` draws the cursor in the message being typed.
    pub(crate) fn paint(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        caret: bool,
    ) -> TextPaint {
        self.settle(fonts);
        self.view.reading = self.mode != Mode::Insert;
        let paint = self.view.paint(fonts, origin, size, caret);
        if let Some(before) = self.view.older_request() {
            let terminal = self.terminal();
            self.requests.request(
                Request::ChatHistory { terminal, before },
                Pending::History(before),
            );
        }
        paint
    }

    /// Carries out what was typed or clicked in it: for that, the fonts
    /// are needed. Returns whether it must be redrawn.
    pub(crate) fn settle(&mut self, fonts: &mut Fonts) -> bool {
        if !self.view.unsettled() {
            return false;
        }
        let settled = self.view.settle(fonts);
        let original = self.view.take_original_request();
        let first_turn = self.view.first_turn();
        let terminal = self.terminal();
        for id in settled.cancel_activities {
            self.requests
                .request(Request::CancelActivity { terminal, id }, Pending::Other);
        }
        for mut action in settled.actions {
            if let ChatAction::Undo { turn, .. } | ChatAction::Redo { turn, .. } = &mut action {
                *turn += first_turn;
            }
            if let ChatAction::Switch { budget, .. } | ChatAction::SendTo { budget, .. } =
                &mut action
            {
                *budget = self.carryover;
            }
            if let ChatAction::Submit {
                target: Some(target),
                ..
            } = &mut action
            {
                target.budget = self.carryover;
            }
            let pending = match action {
                ChatAction::SendTo { .. } | ChatAction::Submit { .. } => Pending::Handoff,
                ChatAction::RemoveQueued { .. } | ChatAction::SendQueued { .. } => Pending::Control,
                _ => Pending::Other,
            };
            self.ask(action, pending);
        }
        if let Some((serial, original)) = original {
            self.requests.request(
                Request::ChatOriginal { terminal, original },
                Pending::Original(serial),
            );
        }
        self.copies.extend(settled.copied);
        self.yanks.extend(settled.yanks);
        settled.redraw
    }

    fn ask(&mut self, action: ChatAction, pending: Pending) {
        let terminal = self.terminal();
        self.requests
            .request(Request::Chat { terminal, action }, pending);
    }

    /// Takes what it copied, oldest first.
    pub(crate) fn take_copies(&mut self) -> Vec<(String, Copy)> {
        std::mem::take(&mut self.copies)
    }

    /// Whether a yank or search failed since this was last asked.
    pub(crate) fn take_failed(&mut self) -> bool {
        self.view.take_failed()
    }

    /// Whether a yank or a search waits for the fonts, to be settled.
    pub(crate) fn reading_unsettled(&self) -> bool {
        self.view.reading_unsettled()
    }

    /// Takes what it yanked, oldest first, for the registers.
    pub(crate) fn take_yanks(&mut self) -> Vec<Yanked> {
        std::mem::take(&mut self.yanks)
    }

    /// Whether a search is being typed in it, in the bar.
    pub(crate) fn prompting(&self) -> bool {
        self.view.prompting()
    }

    /// The search being typed, for the desk to put a register or a
    /// search typed before in.
    pub(crate) fn search_line(&mut self) -> Option<&mut crate::cmdline::Line> {
        self.view.search_line()
    }

    /// What the bar shows of the search being typed, if one is.
    pub(crate) fn prompt_label(&self) -> Option<String> {
        self.view.prompt_label()
    }

    /// When it must be drawn again, for a turn still going to count on a
    /// second.
    pub(crate) fn live(&self) -> Option<Instant> {
        self.view.live().filter(|&live| live > Instant::now())
    }

    /// How the mouse pointer looks at `at`, from the pane's top left.
    pub(crate) fn pointer(&self, at: [f32; 2]) -> Pointer {
        self.view.pointer(at)
    }

    /// Shows what an input method is composing in its box, if it's being
    /// typed in. Returns whether it is.
    pub(crate) fn compose(&mut self, composing: Option<(String, Option<(usize, usize)>)>) -> bool {
        if self.mode != Mode::Insert {
            return false;
        }
        self.view.compose(composing);
        true
    }

    /// Handles a key press: in insert mode, it edits the message, and in
    /// normal or visual mode, it reads the conversation. Returns whether
    /// it must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        if self.mode == Mode::Insert {
            if self.view.leaves_composer(press) {
                self.mode = Mode::Normal;
            } else {
                self.view.key(press);
            }
            return true;
        }
        let (redraw, insert) = self.view.read_key(press);
        let mode = if insert {
            Mode::Insert
        } else {
            self.view.visual().map_or(Mode::Normal, Mode::Visual)
        };
        let changed = mode != self.mode;
        self.mode = mode;
        redraw || changed
    }

    /// Whether a command is half typed, or a count for one, which the next
    /// key is for.
    pub(crate) fn pending(&self) -> bool {
        self.view.keys().pending()
    }

    /// The count typed, if that's all that's waiting: a key it doesn't
    /// take after it is the window's, as `:` is.
    pub(crate) fn counting(&self) -> Option<usize> {
        self.view.keys().counting()
    }

    /// Forgets a count typed, as a binding of the window's took the key
    /// after it.
    pub(crate) fn forget_count(&mut self) {
        self.view.forget_keys();
    }

    /// Switches to normal mode, as when the focus arrives while the
    /// window is in normal mode.
    pub(crate) fn enter_normal_mode(&mut self) {
        if self.mode == Mode::Insert {
            self.mode = Mode::Normal;
        }
    }

    /// Switches to insert mode, to the message being typed, as when the
    /// focus arrives while the window is in insert mode.
    pub(crate) fn enter_insert_mode(&mut self) {
        self.mode = Mode::Insert;
    }

    /// The focus has arrived at it: a question waits a moment before keys
    /// answer it (see [`ChatView::arrived`]).
    pub(crate) fn arrived(&mut self) {
        self.view.arrived();
    }

    /// Drops the answer it armed, if it did, as the window losing the
    /// focus does. Returns whether it did.
    pub(crate) fn disarm(&mut self) -> bool {
        self.view.disarm()
    }

    /// Ends what was half done in it as the focus leaves: a visual
    /// selection, which leaves normal mode, or a command half typed.
    pub(crate) fn leave(&mut self) {
        self.view.leave();
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
    }

    /// Whether `press` is one of the keys that answer the question it
    /// asks, if it asks one: a number, Enter or Esc. Held down, such a key
    /// repeats, and its repeats are dropped, so one held can't wait out
    /// the pause after the question comes, or arm and give an answer.
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
        // Typed into a search, a key is the search's.
        answer_key && self.asking() && !self.prompting()
    }

    /// Pastes `text` into the message.
    pub(crate) fn paste(&mut self, text: &str) {
        self.view.paste(text);
    }

    /// Scrolls `lines` up (or down, if negative), as from the mouse wheel
    /// turned with the pointer at `pixel`, from the pane's top left: with
    /// Shift over a table too wide for it, the table scrolls sideways.
    /// Returns whether it must be redrawn.
    pub(crate) fn wheel(
        &mut self,
        lines: i64,
        pixel: [f32; 2],
        modifiers: input::Modifiers,
    ) -> bool {
        // The wheel drops an answer armed, as a key does.
        let disarmed = self.view.disarm();
        #[allow(clippy::cast_precision_loss)]
        let lines = lines as f32 * 3.0;
        if modifiers.shift
            && let Some(moved) = self.view.wheel_sideways(pixel, lines)
        {
            return moved || disarmed;
        }
        self.view.scroll_lines(lines) || disarmed
    }

    /// Handles a mouse event. Returns whether it must be redrawn.
    pub(crate) fn mouse(&mut self, event: &PaneMouse) -> bool {
        match event.action {
            MouseAction::Press(MouseButton::Left) => {
                self.view
                    .press(event.pixel, event.clicks, event.modifiers.shift);
                // The mouse selects in its own way: visual mode's
                // selection goes.
                if matches!(self.mode, Mode::Visual(_)) {
                    self.mode = Mode::Normal;
                }
            }
            MouseAction::Motion(Some(MouseButton::Left)) => self.view.drag(event.pixel),
            MouseAction::Motion(None) => return self.view.hover(event.pixel),
            MouseAction::Release(_) => self.view.release(),
            _ => return false,
        }
        true
    }

    /// The mouse has left it: what it showed there goes. Returns whether
    /// it must be redrawn.
    pub(crate) fn unhover(&mut self) -> bool {
        self.view.unhover()
    }

    /// The window lost the focus during a drag in it: the drag ends, as
    /// if let go.
    pub(crate) fn drag_lost(&mut self) {
        self.view.drag_lost();
    }

    /// The note for the bottom bar.
    pub(crate) fn label(&self) -> Option<String> {
        self.view.label()
    }

    /// The keys shown by Ctrl+?, if something in it takes every key, as
    /// the image viewer does.
    pub(crate) fn exclusive_shortcuts(&self) -> Option<crate::shortcuts::Context> {
        self.view.exclusive_shortcuts()
    }

    /// The keys shown by Ctrl+?.
    pub(crate) fn shortcut_context(&self) -> crate::shortcuts::Context {
        self.view.shortcut_context(self.mode)
    }

    // --- Talking to the daemon --------------------------------------------

    /// Starts over on a new connection to the daemon, which sends a fresh
    /// snapshot. Whatever was sent on the old one is forgotten, unanswered.
    pub(crate) fn reconnected(&mut self) {
        self.requests.reconnected();
    }

    /// Asks the daemon to make the chat `size`. Asked every time the
    /// panes are fitted, as the daemon never says a chat's size back: so
    /// the size a window last fitted it to is the size it is.
    pub(crate) fn resize(&mut self, size: Size) {
        let terminal = self.terminal();
        self.requests
            .request(Request::Resize { terminal, size }, Pending::Other);
    }

    /// Applies a view update. Returns whether it must be redrawn.
    pub(crate) fn view_changed(&mut self, update: &ViewUpdate) -> bool {
        self.view.apply(update);
        self.requests.changed();
        true
    }

    /// Takes the daemon's answer to the oldest request. Returns whether it
    /// must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        match (self.requests.answered(), response) {
            (Some(Pending::History(before)), response) => {
                self.view.history_answer(before, response);
            }
            (Some(Pending::Control), response) => self.view.queue_answer(response),
            (Some(Pending::Handoff), response) => self.view.handoff_answer(response),
            (Some(Pending::Original(serial)), response) => {
                self.view.original_answer(serial, response);
            }
            (_, Response::Error(err)) => {
                log::warn!("the daemon refused a request: {err}");
                return false;
            }
            _ => return false,
        }
        true
    }

    /// Tells the daemon the last update is on screen, so it sends the next.
    pub(crate) fn drawn(&mut self) {
        self.requests.drawn();
    }

    /// Takes the messages waiting for the daemon.
    pub(crate) fn take_outbox(&mut self) -> Vec<clackatoo_protocol::ClientMessage> {
        self.requests.take_outbox()
    }
}

impl vim::KeyState for ChatPane {
    fn mode(&self) -> Mode {
        self.mode
    }

    /// Whether a search is being typed in it, which takes every key.
    fn prompting(&self) -> bool {
        ChatPane::prompting(self)
    }

    fn pending(&self) -> bool {
        ChatPane::pending(self)
    }

    fn counting(&self) -> Option<usize> {
        ChatPane::counting(self)
    }

    fn forget_count(&mut self) {
        ChatPane::forget_count(self);
    }
}

#[cfg(test)]
impl ChatPane {
    /// The chat `terminal` is, as `snapshot`, its first view, says.
    pub(crate) fn of_snapshot(
        terminal: clackatoo_protocol::TerminalId,
        snapshot: &ViewUpdate,
    ) -> Self {
        let ViewUpdate::Snapshot(view) = snapshot else {
            panic!("a chat's first view is a snapshot");
        };
        let node = view.chat().expect("the view of a chat");
        Self::taking_over(Pane::new(terminal), view, node)
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        ChatAction, ClientMessage, Node, Request, TerminalId, TurnFiles, TurnFilesState, View,
        ViewUpdate,
    };

    use super::ChatPane;
    use crate::pane::Pane;
    use crate::vim::Mode;

    fn snapshot(node: clackatoo_protocol::ChatNode) -> ViewUpdate {
        ViewUpdate::Snapshot(View {
            title: "agent".into(),
            cwd: None,
            root: Node::Chat(Box::new(node)),
        })
    }

    #[test]
    fn keys_typed_into_a_search_never_answer_a_question() {
        use clackatoo_protocol::{Choice, ChoiceKind, Question};
        use winit::keyboard::{Key, KeyLocation};

        let mut node = crate::chat::fixtures::talk(&[("hi", "hello")]);
        node.question = Some(Question {
            id: 1,
            title: "run ls".into(),
            choices: vec![Choice {
                id: "yes".into(),
                name: "yes".into(),
                kind: ChoiceKind::AllowOnce,
            }],
            own: false,
        });
        let mut pane = ChatPane::of_snapshot(TerminalId(1), &snapshot(node));
        pane.enter_normal_mode();
        let (one, slash) = (Key::Character("1".into()), Key::Character("/".into()));
        let press = |key| crate::input::KeyPress {
            key,
            text: None,
            location: KeyLocation::Standard,
            code: None,
            modifiers: crate::input::Modifiers::default(),
        };
        assert!(pane.answers_question(&press(&one)));
        pane.key(&press(&slash));
        assert!(pane.prompting());
        // Held down, a number repeats into the search.
        assert!(!pane.answers_question(&press(&one)));
    }

    #[test]
    fn paged_chat_undo_targets_the_absolute_turn_and_rejects_unloaded_numbers() {
        let mut node = crate::chat::fixtures::talk(&[("visible", "answer")]);
        node.first_turn = 10;
        node.first_message = 8;
        node.turns[0].files = Some(TurnFiles {
            count: 1,
            state: TurnFilesState::Made,
        });
        let mut pane = ChatPane::of_snapshot(TerminalId(1), &snapshot(node));
        pane.take_outbox();
        assert!(pane.undo_turn(Some(0), false, false).is_err());
        pane.undo_turn(Some(8), false, false).unwrap();
        assert!(
            pane.take_outbox()
                .contains(&ClientMessage::Request(Request::Chat {
                    terminal: TerminalId(1),
                    action: ChatAction::Undo {
                        turn: 10,
                        force: false
                    }
                }))
        );
    }

    #[test]
    fn a_chat_takes_over_its_terminals_mode_and_the_requests_it_sent() {
        let mut terminal = Pane::new(TerminalId(4));
        terminal.enter_normal_mode();
        terminal.resize(clackatoo_protocol::Size::new(80, 24).unwrap());
        let resize = terminal.take_outbox();
        let ViewUpdate::Snapshot(view) = snapshot(crate::chat::fixtures::talk(&[("hi", "hello")]))
        else {
            unreachable!()
        };
        let mut chat = ChatPane::taking_over(terminal, &view, view.chat().unwrap());
        assert_eq!(chat.mode(), Mode::Normal);
        // The resize the terminal's pane asked waits for its answer in the
        // chat's, so later answers go to what they answer.
        assert_eq!(resize.len(), 1);
        assert_eq!(chat.requests.waiting(), 1);
        assert!(!chat.answered(clackatoo_protocol::Response::Done));
        assert_eq!(chat.requests.waiting(), 0);
        // The view taken over is acknowledged once drawn.
        chat.drawn();
        assert_eq!(
            chat.take_outbox(),
            [ClientMessage::FrameDone {
                terminal: TerminalId(4)
            }]
        );
    }
}
