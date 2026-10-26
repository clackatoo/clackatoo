//! A chat with an agent, in a pane: the conversation, and the box a
//! message is typed in.
//!
//! The daemon keeps the chat. The window keeps a copy, current with the
//! daemon's patches, lays it out at the pane's width, and draws it with
//! the window's own fills and fonts: messages the user sent are bubbles on
//! the right, and the agent's words, read as Markdown, run the full width
//! of a column down the middle.
//!
//! Editing the message needs the fonts, to lay it out, and keys arrive
//! without them, so what's typed, pasted or clicked waits in a queue until
//! the window [settles](ChatView::settle) it, which it does before sending
//! anything to the daemon and before drawing.

mod card;
mod favorites;
#[cfg(test)]
pub(crate) mod fixtures;
mod handoff;
mod images;
mod paint;
mod pane;
mod picker;
mod queue;
mod read;
mod shortcuts;
mod viewer;
mod yank;

use crate::shortcuts::bindings::Hint;

use clackatoo_protocol::{
    ChatAction, ChatNode, ChatState, ChoiceKind, ProviderModels, Setting, SettingKind,
    TurnFilesState, ViewUpdate,
};
use parley::{LayoutContext, PlainEditor};
use winit::keyboard::{Key, NamedKey};

use crate::fonts::Fonts;
use crate::input::{KeyPress, Modifiers};
use crate::markdown::{self, colours};
use crate::pane::{Copy, CopyTo, Yanked};
use paint::{How, Picked, Position, Shown, TextKey};
use picker::{Row, Spot};

pub(crate) use crate::markdown::layout::{Brush, TextPaint, Tone, warm_fonts};
pub(crate) use pane::ChatPane;

/// Something done to the chat that waits for the fonts.
#[derive(Debug)]
enum Input {
    Key {
        key: Key,
        text: Option<String>,
        modifiers: Modifiers,
    },
    Paste(String),
    Clipboard,
    PasteFailed(String),
    Image(clackatoo_protocol::ChatImage),
    /// A mouse button went down at a point in the pane.
    Press {
        at: [f32; 2],
        clicks: u8,
        shift: bool,
    },
    /// The mouse moved to a point, with a button held.
    Drag([f32; 2]),
    /// The button was let go.
    Release,
    /// The input method is composing this text, with its cursor over this
    /// stretch of it, or has stopped.
    Compose(Option<(String, Option<(usize, usize)>)>),
    /// Yanks the conversation's text from one place to another, whole
    /// lines if `lines`, `to` the registers (see [`yank`]).
    Yank {
        start: Position,
        end: Position,
        lines: bool,
        to: CopyTo,
    },
    /// Yanks this, which the chat has whole already, as a message.
    YankText(Yanked),
    /// Asks this of the daemon.
    Act(ChatAction),
    /// Answers the question numbered so, if it's still the one asked.
    Answer {
        question: u64,
        pick: Pick,
    },
}

/// What settling the chat's queue came to.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    /// It must be redrawn.
    pub(crate) redraw: bool,
    /// Asks for the daemon.
    pub(crate) actions: Vec<ChatAction>,
    pub(crate) cancel_activities: Vec<String>,
    /// Text to put on the clipboard, or the primary selection, oldest
    /// first.
    pub(crate) copied: Vec<(String, Copy)>,
    /// What normal and visual mode yanked, for the registers, as a
    /// terminal's yanks go there, oldest first.
    pub(crate) yanks: Vec<Yanked>,
}

/// Something in the chat that does something when clicked.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    CancelActivity(String),
    ModelSearch,
    MenuSurface,
    ModelRail,
    FastMode,
    ModelFilter(picker::Filter),
    Favorite(String, String),
    RemoveImage(usize),
    CloseImage,
    CloseUsage,
    RefreshUsage,
    UsageSurface,
    CancelHandoff,
    Send,
    Steer,
    RemoveQueued(u64),
    SendQueued(u64),
    QueuePage(i8),
    Stop,
    /// One of the answers to the agent's question, by its ID.
    Choice(String),
    /// The message being typed.
    Editor,
    /// A button that opens the menu for these settings, by their IDs.
    Chip(Vec<String>),
    /// One of a setting's choices, in its menu.
    Pick {
        setting: String,
        value: String,
    },
    /// The button that shows or hides a model family's versions, by its
    /// name.
    Expand(String),
    /// A step's share of a slider, to click or drag to.
    Step {
        setting: String,
        value: String,
    },
    /// The button beside a slider that leaves its setting to the agent.
    Reset {
        setting: String,
        value: String,
    },
    /// A tool's card, which opens and folds, by the turn it's in and the
    /// tool's ID.
    Card(usize, String),
    /// The button that undoes what a turn changed in files, by the turn.
    Undo(usize),
    /// The button that redoes it, once undone.
    Redo(usize),
    /// The line a turn's work folds under, which opens and folds it, by
    /// the turn.
    Fold(usize),
    /// The line of the card for the chats a turn's agent started, which
    /// opens and folds it, by the turn.
    Agents(usize),
    /// A chat's line on that card, which opens the chat in a pane.
    Subagent(clackatoo_protocol::TerminalId),
    /// The button that copies a block of code.
    CopyCode(TextKey),
    /// The button under a turn that copies its answer, by the turn.
    CopyAnswer(usize),
    /// The meter of how much context the chat fills, which shows its
    /// detail.
    Usage,
}

/// What the mouse pointer looks like over a chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pointer {
    /// Over nothing that takes a click.
    Arrow,
    /// Over something to click: a button, a chip, a menu's choice.
    Hand,
    /// Over text to type in or select.
    Text,
}

/// A menu open over the box, to pick the agent's settings.
#[derive(Clone, Debug, Default, PartialEq)]
struct Menu {
    query: String,
    filter: picker::Filter,
    favorites: std::collections::BTreeSet<(String, String)>,
    offset: usize,
    rail_offset: usize,
    reveal: bool,
    /// The settings it picks from, by their IDs: a section each.
    settings: Vec<String>,
    /// Where the keyboard or the mouse is.
    highlighted: Option<Spot>,
    /// The model family whose versions are shown, by its name.
    expanded: Option<String>,
    /// A slider's setting and the value it was last slid to, until the
    /// agent says it's set, for it to stay where it was put meanwhile.
    held: Option<(String, String)>,
}

impl Menu {
    /// What `setting` is, or is about to be.
    fn value<'a>(&'a self, setting: &'a Setting) -> &'a str {
        match &self.held {
            Some((id, value)) if *id == setting.id => value,
            _ => &setting.value,
        }
    }
}

/// The window's side of a chat.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent states of what the pane shows"
)]
pub(crate) struct ChatView {
    pub(crate) node: ChatNode,
    pub(crate) title: String,
    /// The folder its agent works in, as people write it, which a new
    /// chat says: `None` from a daemon too old to say.
    folder: Option<String>,
    /// The message being typed.
    editor: PlainEditor<Brush>,
    images: Vec<clackatoo_protocol::ChatImage>,
    previews: images::Cache,
    image_viewer: Option<viewer::Viewer>,
    handoff: Option<handoff::Pending>,
    handoff_sent: Option<handoff::Draft>,
    pub(crate) handoff_supported: bool,
    pub(crate) queue_supported: bool,
    queue_page: usize,
    /// Enter waits until the clipboard worker has delivered its result.
    pub(crate) pasting: bool,
    /// The size the editor's text was last styled at, and the fonts'
    /// generation it was styled in.
    editor_size: f32,
    editor_fonts: u32,
    /// The width the editor's text was last laid out to. parley lays the
    /// whole message out again whenever it's told a width, the same or
    /// not, so it's told only when the width changes.
    editor_width: Option<f32>,
    layout_cx: LayoutContext<Brush>,
    queued: Vec<Input>,
    /// Laid-out turns, kept while their text and the width stay the same.
    cache: paint::Cache,
    /// How far down the conversation the view is, in pixels from its top.
    scroll: f32,
    /// Whether the view stays at the end as the conversation grows.
    follow: bool,
    history_pending: Option<u32>,
    history_retry: Option<std::time::Instant>,
    /// The most the view can scroll, when last drawn.
    max_scroll: f32,
    /// The view's height, when last drawn.
    view_height: f32,
    /// A line's height, when last drawn.
    line_height: f32,
    /// What was drawn where, last time, from the pane's top left: for
    /// clicks.
    hits: Vec<(Hit, [f32; 4])>,
    /// Where the editor's text was drawn, from the pane's top left.
    editor_origin: [f32; 2],
    /// A drag that started in the editor selects as it goes.
    selecting: bool,
    /// A drag that started on a slider moves it as it goes: its setting.
    sliding: Option<String>,
    /// The settings menu, while it's open.
    menu: Option<Menu>,
    /// Where the chip that opened the menu was drawn, for the menu to open
    /// from, while drawing.
    menu_anchor: Option<[f32; 4]>,
    /// The tools whose cards are open, showing all they have, by their
    /// IDs.
    open_cards: std::collections::HashSet<String>,
    /// What the window says about the chat in the bottom bar, until the
    /// next key, such as that there's nothing to undo.
    said: Option<String>,
    /// The turns whose work was opened or folded, by their index, and
    /// which. A turn not here is open while it's going, and folded once
    /// it's done.
    folds: std::collections::HashMap<usize, bool>,
    /// The turns whose card for the chats their agent started is open, by
    /// their index.
    open_agents: std::collections::HashSet<usize>,
    /// How far tables too wide for the column are scrolled sideways, by
    /// their turn, piece and number in it.
    table_scrolls: std::collections::HashMap<(usize, usize, usize), f32>,
    /// The colours of the code in the agent's words, as the daemon read
    /// them.
    colours: colours::Colours,
    /// The time, as turns count it, if it's not the clock's, as for tests.
    clock: Option<u64>,
    /// When the chat must next be drawn by itself, if anything it drew
    /// changes with time: the line saying what the agent is doing, a card
    /// counting its subagents' seconds, or the view scrolling as a drag
    /// passes its edge.
    live: Option<std::time::Instant>,
    /// Whether it had the keyboard, in a window with the focus, when last
    /// drawn: only then do the dots that show the agent working move as
    /// often as they can. Otherwise they step once a second.
    moving: bool,
    /// When the chat last heard from the daemon, as turns count time, if
    /// it has since it was made: the dots stop moving often once the
    /// agent's been quiet a while.
    heard: Option<u64>,
    /// Whether the daemon is out of reach: what the chat shows of the
    /// agent working is as it was, and may be long over, so it isn't
    /// said, nor counted.
    offline: bool,
    /// The text drawn last, top to bottom, and where.
    shown: Vec<Shown>,
    /// Text selected in the conversation.
    picked: Option<Picked>,
    /// A drag that started on the conversation's text selects as it goes.
    picking: bool,
    /// Whether the pane is in normal or visual mode, reading the chat
    /// rather than typing in it.
    pub(crate) reading: bool,
    /// Whether it was, when last drawn: coming into normal mode puts the
    /// cursor where it shows.
    was_reading: bool,
    /// Whether the daemon quotes what agents wrote in its questions so
    /// nothing in it can end the quote (protocol 84 on), which questions
    /// are then shown with as code.
    quoted: bool,
    /// The cursor in the conversation, in normal mode.
    cursor: Option<Position>,
    /// How far across the column `j` and `k` keep the cursor.
    goal: Option<f32>,
    /// The cursor moved: the view goes to it.
    reveal: bool,
    /// The view scrolled: the cursor stays in it.
    keep_cursor: bool,
    /// The keys of a command half typed in normal or visual mode.
    keys: crate::vim::Keys,
    /// The last `f`, `t`, `F` or `T`, for `;` and `,`.
    finds: crate::vim::Finds,
    /// How far its scrolling keys go.
    paging: crate::vim::Paging,
    /// The block of code under the mouse, which shows a button to copy it.
    hovered_code: Option<TextKey>,
    /// The block of code just copied with its button, which says so.
    copied_code: Option<TextKey>,
    /// The turn whose answer was just copied with the button under it,
    /// which says so until the mouse leaves it.
    copied_answer: Option<usize>,
    /// The turn under the mouse, if its answer has a button to copy it,
    /// which shows only then.
    hovered_turn: Option<usize>,
    /// Where the mouse is, from the pane's top left, while it's over the
    /// pane and no button is held: what's under it is worked out again
    /// when the conversation moves under a mouse that stays still.
    mouse: Option<[f32; 2]>,
    /// Where the mouse is, from the pane's top left, while a drag selects
    /// the conversation's text.
    drag_to: Option<[f32; 2]>,
    /// When the view last scrolled for that drag, as turns count the time,
    /// while the mouse is past the view's top or bottom.
    edge_step: Option<u64>,
    /// The column's right edge, from the pane's left, when last drawn.
    column_right: f32,
    /// The meter's detail, while it's shown, and how it came to be.
    detail: Option<paint::Detail>,
    pub(crate) usage_supported: bool,
    usage_modal: bool,
    usage_scroll: f32,
    usage_max_scroll: f32,
    /// Where the meter was drawn, for its detail to open from, while
    /// drawing: its right, and the box's top.
    detail_anchor: Option<[f32; 2]>,
    /// When the agent's question came, or the focus arrived at the chat
    /// while it asked, as turns count time: for [`ANSWER_PAUSE`] after
    /// either, its answers' numbers don't answer it, as they may have been
    /// meant for what was there before.
    asked_at: Option<u64>,
    /// The answer a number picked in normal mode, by its index, and when,
    /// as turns count time, until the same number again or Enter gives it,
    /// or any other key, a click, the wheel or the window losing the focus
    /// drops it.
    armed: Option<(usize, u64)>,
}

/// How long, in milliseconds, after a number arms an answer in normal mode
/// before the same number again, or Enter, gives it, and how long after
/// it no longer does, only arming it again: a count such as `22j` or
/// `2⏎`, typed in one go, never answers, and nor does an answer armed and
/// forgotten.
pub(crate) const ARMED_FOR: std::ops::Range<u64> = 300..5_000;

/// How long after a question comes, or the focus arrives at a chat asking
/// one, before a number answers it, in milliseconds: long enough for keys
/// typed for what was there before to have stopped, short enough not to
/// be felt.
pub(crate) const ANSWER_PAUSE: u64 = 500;

/// An answer to the question asked, picked by a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    /// The answer of this index.
    Choice(usize),
    /// No, as Esc says.
    Refuse,
}

/// The answer the number `text` names, from 1 to 9, by its index, if it's
/// one.
fn numbered(text: Option<&str>) -> Option<usize> {
    let number = text?.parse::<usize>().ok()?;
    (1..=9).contains(&number).then(|| number - 1)
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
            .field("node", &self.node)
            .field("title", &self.title)
            .field("typed", &self.editor.raw_text())
            .field("scroll", &self.scroll)
            .finish_non_exhaustive()
    }
}

impl ChatView {
    /// The chat `view`, the daemon's first of it, shows: `node`, with its
    /// title and folder.
    pub(crate) fn of_view(view: &clackatoo_protocol::View, node: &ChatNode) -> Self {
        let mut chat = Self::new(view.title.clone(), node.clone());
        chat.folder.clone_from(&view.cwd);
        chat
    }

    /// Takes the window's last find, which `;` and `,` here find again.
    pub(crate) fn share_finds(&mut self, finds: &crate::vim::Finds) {
        self.finds = finds.clone();
    }

    pub(crate) fn new(title: String, node: ChatNode) -> Self {
        let mut view = Self {
            node,
            title,
            folder: None,
            editor: PlainEditor::new(16.0),
            images: Vec::new(),
            previews: images::Cache::default(),
            image_viewer: None,
            handoff: None,
            handoff_sent: None,
            handoff_supported: cfg!(test),
            queue_supported: false,
            queue_page: 0,
            pasting: false,
            editor_size: 0.0,
            editor_fonts: 0,
            editor_width: None,
            layout_cx: LayoutContext::new(),
            queued: Vec::new(),
            cache: paint::Cache::default(),
            scroll: 0.0,
            follow: true,
            history_pending: None,
            history_retry: None,
            max_scroll: 0.0,
            view_height: 0.0,
            line_height: 16.0,
            hits: Vec::new(),
            editor_origin: [0.0; 2],
            selecting: false,
            sliding: None,
            menu: None,
            menu_anchor: None,
            open_cards: std::collections::HashSet::new(),
            said: None,
            folds: std::collections::HashMap::new(),
            open_agents: std::collections::HashSet::new(),
            table_scrolls: std::collections::HashMap::new(),
            colours: colours::Colours::default(),
            clock: None,
            live: None,
            moving: false,
            heard: None,
            offline: false,
            shown: Vec::new(),
            picked: None,
            picking: false,
            reading: false,
            was_reading: false,
            quoted: true,
            cursor: None,
            goal: None,
            reveal: false,
            keep_cursor: false,
            keys: crate::vim::Keys::default(),
            finds: crate::vim::Finds::default(),
            paging: crate::vim::Paging::default(),
            hovered_code: None,
            copied_code: None,
            copied_answer: None,
            hovered_turn: None,
            mouse: None,
            drag_to: None,
            edge_step: None,
            column_right: 0.0,
            detail: None,
            usage_supported: false,
            usage_modal: false,
            usage_scroll: 0.0,
            usage_max_scroll: 0.0,
            detail_anchor: None,
            asked_at: None,
            armed: None,
        };
        // A chat first seen asking may be arrived at already.
        if view.node.question.is_some() {
            view.asked_at = Some(view.now());
        }
        view
    }

    pub(crate) fn native_controls(&self) -> Vec<(String, [f32; 4])> {
        self.hits
            .iter()
            .take(128)
            .map(|(hit, bounds)| (format!("{hit:?}").chars().take(200).collect(), *bounds))
            .collect()
    }

    pub(crate) fn first_turn(&self) -> u32 {
        self.node.first_turn
    }

    pub(crate) fn older_request(&mut self) -> Option<u32> {
        if self.offline
            || self.node.first_turn == 0
            || self.history_pending.is_some()
            || (self.follow && self.max_scroll > 0.0)
            || self.scroll > self.view_height.max(240.0)
            || self
                .history_retry
                .is_some_and(|at| std::time::Instant::now() < at)
        {
            return None;
        }
        let before = self.node.first_turn;
        self.history_pending = Some(before);
        Some(before)
    }

    pub(crate) fn history_answer(&mut self, before: u32, response: clackatoo_protocol::Response) {
        if let clackatoo_protocol::Response::Error(error) = response
            && self.history_pending == Some(before)
        {
            self.history_pending = None;
            self.history_retry =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
            self.said = Some(format!("Could not load earlier messages: {error}"));
        }
    }

    fn prepend_history(&mut self, count: usize) {
        self.cache.prepend(count, self.scroll, self.follow);
        self.folds = self.folds.drain().map(|(i, v)| (i + count, v)).collect();
        self.open_agents = self.open_agents.drain().map(|i| i + count).collect();
        self.table_scrolls = self
            .table_scrolls
            .drain()
            .map(|((t, p, n), v)| ((t + count, p, n), v))
            .collect();
        if let Some(cursor) = &mut self.cursor {
            cursor.text.turn += count;
        }
        if let Some(picked) = &mut self.picked {
            picked.anchor.text.turn += count;
            picked.head.text.turn += count;
        }
        for key in [&mut self.hovered_code, &mut self.copied_code]
            .into_iter()
            .flatten()
        {
            key.turn += count;
        }
        for turn in [&mut self.hovered_turn, &mut self.copied_answer]
            .into_iter()
            .flatten()
        {
            *turn += count;
        }
        for shown in &mut self.shown {
            shown.key.turn += count;
        }
        for (hit, _) in &mut self.hits {
            match hit {
                Hit::Card(turn, _)
                | Hit::Undo(turn)
                | Hit::Redo(turn)
                | Hit::Fold(turn)
                | Hit::Agents(turn)
                | Hit::CopyAnswer(turn) => *turn += count,
                Hit::CopyCode(key) => key.turn += count,
                _ => {}
            }
        }
        for input in &mut self.queued {
            match input {
                Input::Yank { start, end, .. } => {
                    start.text.turn += count;
                    end.text.turn += count;
                }
                Input::Act(ChatAction::Undo { turn, .. } | ChatAction::Redo { turn, .. }) => {
                    *turn = turn.saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
                }
                _ => {}
            }
        }
    }

    /// Applies an update from the daemon.
    #[allow(
        clippy::too_many_lines,
        reason = "handles each update while preserving layout caches"
    )]
    pub(crate) fn apply(&mut self, update: &ViewUpdate) {
        self.heard = Some(self.now());
        let asked = self.node.question.as_ref().map(|question| question.id);
        match update {
            ViewUpdate::Snapshot(view) => {
                if let Some(chat) = view.chat() {
                    if chat.first_turn < self.node.first_turn {
                        self.prepend_history((self.node.first_turn - chat.first_turn) as usize);
                    }
                    self.history_pending = None;
                    chat.clone_into(&mut self.node);
                }
                view.title.clone_into(&mut self.title);
                view.cwd.clone_into(&mut self.folder);
                for slot in &mut self.cache.turns {
                    slot.stale = true;
                }
                // Turns gone take how far their tables were scrolled.
                let turns = self.node.turns.len();
                (self.table_scrolls).retain(|&(turn, _, _), _| turn < turns);
            }
            ViewUpdate::Patch(patches) => {
                for patch in patches {
                    match patch {
                        clackatoo_protocol::Patch::Title(title) => {
                            title.clone_into(&mut self.title);
                        }
                        clackatoo_protocol::Patch::Chat(patch) => {
                            if let Some(turn) = touches(patch)
                                .and_then(|t| t.checked_sub(self.node.first_turn as usize))
                            {
                                self.restyled(turn);
                            }
                            if let clackatoo_protocol::ChatPatch::History {
                                first,
                                before,
                                turns,
                                ..
                            } = patch
                            {
                                if self.history_pending == Some(*before) {
                                    self.history_pending = None;
                                }
                                if *before == self.node.first_turn
                                    && first < before
                                    && turns.len() == (before - first) as usize
                                {
                                    self.prepend_history(turns.len());
                                }
                            }
                            // When a turn ended says whether the time
                            // shows over the next turn the chat didn't
                            // move to another agent in.
                            if let clackatoo_protocol::ChatPatch::Turn { index, .. } = patch
                                && let Some(index) =
                                    index.checked_sub(self.node.first_turn).map(|i| i as usize)
                            {
                                for next in index + 1..self.node.turns.len() {
                                    self.restyled(next);
                                    if self.node.turns[next].switch.is_none() {
                                        break;
                                    }
                                }
                            }
                            // The turns with cards, before and after.
                            if let clackatoo_protocol::ChatPatch::Subagents(new) = patch {
                                let last = self.node.turns.len().saturating_sub(1);
                                let turns: Vec<usize> = (self.node.subagents.iter().chain(new))
                                    .filter_map(|entry| {
                                        entry
                                            .turn
                                            .checked_sub(self.node.first_turn)
                                            .map(|i| (i as usize).min(last))
                                    })
                                    .collect();
                                for turn in turns {
                                    self.restyled(turn);
                                }
                            }
                            if let clackatoo_protocol::ChatPatch::Activities(new) = patch {
                                let turns: Vec<_> = self
                                    .node
                                    .activities
                                    .iter()
                                    .chain(new)
                                    .filter_map(|a| a.turn.checked_sub(self.node.first_turn))
                                    .collect();
                                for turn in turns {
                                    self.restyled(turn as usize);
                                }
                            }
                            self.node.apply(patch.clone());
                        }
                        clackatoo_protocol::Patch::Grid { .. }
                        | clackatoo_protocol::Patch::Cwd(_) => {}
                    }
                }
            }
        }
        // A question that's just come waits a moment for its answer, and
        // an answer armed for the one before is no answer to it.
        let asking = self.node.question.as_ref().map(|question| question.id);
        if asking != asked {
            self.armed = None;
            if asking.is_some() {
                self.asked_at = Some(self.now());
            }
        }
        // A chat that moved to another agent has nothing to say in detail
        // until the new one says how much it uses.
        if self.node.usage.is_none() {
            self.detail = None;
        }
        // The agent has set what a slider was slid to.
        if let Some(menu) = &mut self.menu
            && let Some((id, value)) = &menu.held
            && (self.node.settings.iter())
                .any(|setting| setting.id == *id && setting.value == *value)
        {
            menu.held = None;
        }
    }

    /// Turn `index` must be laid out again, as it's changed, or how it's
    /// shown has.
    fn restyled(&mut self, index: usize) {
        if let Some(slot) = self.cache.turns.get_mut(index) {
            slot.stale = true;
        }
    }

    /// When the chat must be drawn again by itself, if what it showed
    /// when last drawn changes with time: `None` once nothing does.
    pub(crate) fn live(&self) -> Option<std::time::Instant> {
        if self
            .image_viewer
            .as_ref()
            .is_some_and(viewer::Viewer::loading)
        {
            return Some(std::time::Instant::now() + std::time::Duration::from_millis(16));
        }
        if self.offline { None } else { self.live }
    }

    /// Whether its agent is at work, for its tab to say so, as far as the
    /// chat knows: not while the daemon is out of reach.
    pub(crate) fn working(&self) -> bool {
        !self.offline && self.node.working()
    }

    /// The daemon has gone out of reach, if `offline`, or come back:
    /// meanwhile, the chat says nothing of its agent working, and nothing
    /// it shows wakes the window to count.
    pub(crate) fn set_offline(&mut self, offline: bool) {
        if offline {
            self.history_pending = None;
        }
        if self.offline && !offline && self.usage_modal {
            self.queued
                .push(Input::Act(ChatAction::RefreshUsage { force: true }));
        }
        self.offline = offline;
        if offline {
            self.live = None;
            self.handoff_failed(
                "Disconnected before send was confirmed. Check the history before retrying.",
            );
            if let Some(viewer) = &mut self.image_viewer {
                viewer.offline();
            }
            // What was asked won't be answered: it's asked again once the
            // daemon's back, as what's waiting for it is laid out again.
            self.colours.forget_asked();
        } else {
            self.recolour();
        }
    }

    /// Whether the daemon quotes what agents wrote so nothing in it can end
    /// the quote, as it has since protocol 84.
    pub(crate) fn set_quoted(&mut self, quoted: bool) {
        self.quoted = quoted;
    }

    /// The blocks of code to ask the daemon to colour, in asks no bigger
    /// than it colours: those laid out since last asked whose colours
    /// aren't known.
    pub(crate) fn take_colour_asks(&mut self) -> Vec<Vec<clackatoo_protocol::Code>> {
        self.colours.take_wanted()
    }

    /// `code` was asked for, numbered `id`; or with none, couldn't be, as
    /// the daemon doesn't colour chats' code, and stays plain.
    pub(crate) fn colours_asked(&mut self, id: Option<u64>, code: Vec<clackatoo_protocol::Code>) {
        if let Some(id) = id {
            self.colours.asked(id, code);
        } else {
            self.colours.plain(code);
            self.recolour();
        }
    }

    /// The daemon's answer to the ask numbered `id`: each block's spans.
    /// Returns whether the chat must be drawn again, as it was its ask.
    pub(crate) fn coloured(&mut self, id: u64, spans: Vec<Vec<clackatoo_protocol::Span>>) -> bool {
        let ours = self.colours.answered(id, spans);
        if ours {
            self.recolour();
        }
        ours
    }

    /// Whether the agent is answering, or starting to.
    fn busy(&self) -> bool {
        self.node
            .turns
            .last()
            .is_some_and(|turn| turn.end.is_none())
    }

    /// Whether there's something for Stop to stop: the agent answering, or
    /// starting with no message typed, as one slow to open its session
    /// may be. A message typed while it starts goes instead, and waits.
    fn stoppable(&self) -> bool {
        self.busy()
            || (self.node.state == ChatState::Starting
                && self.editor.raw_text().trim().is_empty()
                && self.images.is_empty())
    }

    // --- Input, queued until the fonts are at hand --------------------------

    pub(crate) fn key(&mut self, press: &KeyPress<'_>) {
        if self.chat_modal_key(press) {
            return;
        }
        self.said = None;
        if self.pasting
            && self.stoppable()
            && press.modifiers.ctrl
            && matches!(press.key, Key::Character(text) if text.eq_ignore_ascii_case("c"))
            && self.editor.selected_text().is_none_or(str::is_empty)
        {
            self.queue_control(Input::Act(ChatAction::Stop));
            return;
        }
        if self.pasting
            && self.node.question.is_none()
            && *press.key == Key::Named(NamedKey::Enter)
            && !press.modifiers.shift
        {
            self.say("Reading the clipboard… press Enter once it appears.".into());
            return;
        }
        // An answer to the question asked waits as one, so if the question
        // has gone by the time it's settled, it's dropped, not typed.
        if let Some((question, pick)) = self.pick_of(press) {
            self.queue_control(Input::Answer { question, pick });
            return;
        }
        self.queued.push(Input::Key {
            key: press.key.clone(),
            text: press.text.map(str::to_owned),
            modifiers: press.modifiers,
        });
    }

    /// Cancellation and permission answers never wait for clipboard I/O.
    fn queue_control(&mut self, input: Input) {
        let at = self
            .queued
            .iter()
            .position(|input| matches!(input, Input::Clipboard))
            .unwrap_or(self.queued.len());
        self.queued.insert(at, input);
    }

    fn remove_image(&mut self, index: usize) {
        if index < self.images.len() {
            self.images.remove(index);
        }
    }

    #[cfg(test)]
    pub(crate) fn paste_image(&mut self, image: clackatoo_protocol::ChatImage) {
        self.queued.push(Input::Image(image));
    }

    pub(crate) fn searching_models(&self) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| picker::offers_models(&self.offer(), menu))
    }

    fn append_search(&mut self, text: &str) {
        if let Some(menu) = &mut self.menu {
            let room = 256_usize.saturating_sub(menu.query.chars().count());
            menu.query
                .extend(text.chars().filter(|c| !c.is_control()).take(room));
        }
    }

    fn search_changed(&mut self) {
        let rows = self.choice_rows(self.menu.as_ref().unwrap());
        let menu = self.menu.as_mut().unwrap();
        menu.highlighted = rows.iter().find_map(Row::spot);
        menu.offset = 0;
        menu.reveal = true;
    }

    pub(crate) fn begin_paste(&mut self) {
        self.pasting = true;
        self.queued.push(Input::Clipboard);
    }

    pub(crate) fn finish_paste(&mut self, result: Result<crate::clipboard_image::Pasted, String>) {
        self.pasting = false;
        let Some(input) = self
            .queued
            .iter_mut()
            .find(|input| matches!(input, Input::Clipboard))
        else {
            return;
        };
        *input = match result {
            Ok(crate::clipboard_image::Pasted::Image(image)) => Input::Image(image),
            Ok(crate::clipboard_image::Pasted::Text(text)) => Input::Paste(text),
            Err(why) => Input::PasteFailed(why),
        };
    }

    pub(crate) fn paste(&mut self, text: &str) {
        if self.viewing_modal() {
            return;
        }
        self.queued.push(Input::Paste(text.to_owned()));
    }

    /// A mouse button went down at `at`, from the pane's top left.
    pub(crate) fn press(&mut self, at: [f32; 2], clicks: u8, shift: bool) {
        // Clicking drops an answer armed, as a key does.
        self.armed = None;
        // A held button isn't hovering: the turns a drag's scrolling
        // brings under it don't show their buttons in turn.
        self.mouse = None;
        if self.pasting
            && self
                .hits
                .iter()
                .any(|(hit, rect)| *hit == Hit::Stop && contains(*rect, at))
        {
            self.queue_control(Input::Act(ChatAction::Stop));
            return;
        }
        if self.pasting
            && self
                .hits
                .iter()
                .any(|(hit, rect)| matches!(hit, Hit::Send | Hit::Steer) && contains(*rect, at))
        {
            self.say("Reading the clipboard… click Send once it appears.".into());
            return;
        }
        self.queued.push(Input::Press { at, clicks, shift });
    }

    /// The mouse moved to `at` with a button held.
    pub(crate) fn drag(&mut self, at: [f32; 2]) {
        self.queued.push(Input::Drag(at));
    }

    pub(crate) fn release(&mut self) {
        self.queued.push(Input::Release);
    }

    /// The window lost the focus mid-drag, so the button's release may
    /// never come: the drag ends as if let go, and the view stops
    /// scrolling by itself at once.
    pub(crate) fn drag_lost(&mut self) {
        self.drag_to = None;
        self.edge_step = None;
        self.release();
    }

    /// The input method is composing `text`, with its cursor over that
    /// stretch of it, or has stopped, with `None`.
    pub(crate) fn compose(&mut self, composing: Option<(String, Option<(usize, usize)>)>) {
        if self.image_viewer.is_some() {
            return;
        }
        self.queued.push(Input::Compose(composing));
    }

    /// How the mouse pointer looks at `at`, from the pane's top left, by
    /// what was drawn there last.
    pub(crate) fn pointer(&self, at: [f32; 2]) -> Pointer {
        if self.image_viewer.is_none() && self.menu.is_none() && self.previews.at(at).is_some() {
            return Pointer::Hand;
        }
        let hit = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .map(|(hit, _)| hit);
        match hit {
            Some(Hit::Editor | Hit::ModelSearch) => Pointer::Text,
            Some(Hit::MenuSurface | Hit::ModelRail) => Pointer::Arrow,
            Some(_) => Pointer::Hand,
            None if self.over_text(at) => Pointer::Text,
            None => Pointer::Arrow,
        }
    }

    /// The mouse moved to `at` with no button held: over a menu, its
    /// choice or slider there lights up, and over a turn, the button that
    /// copies its answer shows. Returns whether the chat must be redrawn.
    pub(crate) fn hover(&mut self, at: [f32; 2]) -> bool {
        if self.image_viewer.is_some() {
            return false;
        }
        // A block of code under the mouse shows a button to copy it.
        let code = self.code_at(at).filter(|_| self.menu.is_none());
        let mut redraw = code != self.hovered_code;
        if redraw {
            self.hovered_code = code;
            self.copied_code = None;
        }
        // The button under an answer just copied says so until the mouse
        // leaves it.
        if let Some(turn) = self.copied_answer
            && !(self.hits.iter())
                .any(|(hit, rect)| *hit == Hit::CopyAnswer(turn) && contains(*rect, at))
        {
            self.copied_answer = None;
            redraw = true;
        }
        // The button under an answer shows only while the mouse is over
        // its turn, so a long chat isn't a column of them.
        redraw |= self.hits.iter().any(|(hit, rect)| {
            matches!(
                hit,
                Hit::Undo(_)
                    | Hit::Redo(_)
                    | Hit::Chip(_)
                    | Hit::ModelFilter(_)
                    | Hit::FastMode
                    | Hit::Step { .. }
                    | Hit::Reset { .. }
            ) && contains(*rect, at) != self.mouse.is_some_and(|old| contains(*rect, old))
        });
        self.mouse = Some(at);
        let turn = self.turn_to_copy_at(at);
        if turn != self.hovered_turn {
            self.hovered_turn = turn;
            redraw = true;
        }
        redraw |= self.hover_menu(at);
        redraw |= self.hover_meter(at);
        redraw
    }

    /// The mouse moved to `at` with no button held: over the meter, its
    /// detail shows, until the mouse leaves it. Returns whether the chat
    /// must be redrawn.
    fn hover_meter(&mut self, at: [f32; 2]) -> bool {
        let over = (self.hits.iter()).any(|(hit, rect)| *hit == Hit::Usage && contains(*rect, at));
        match (over, self.detail) {
            (true, None) => self.detail = Some(paint::Detail::Hovered),
            (false, Some(paint::Detail::Hovered)) => self.detail = None,
            _ => return false,
        }
        true
    }

    /// Shows the meter's detail, as `K` does, or hides it again. Returns
    /// whether there's a meter to show it of.
    pub(crate) fn toggle_detail(&mut self) -> bool {
        if self.usage_supported {
            if self.usage_modal {
                self.usage_modal = false;
            } else {
                self.open_usage();
            }
            return true;
        }
        self.detail = match self.detail {
            Some(paint::Detail::Asked) => None,
            _ if self.node.usage.is_none() => return false,
            _ => Some(paint::Detail::Asked),
        };
        true
    }

    fn show_usage(&mut self) {
        self.usage_modal = true;
        self.usage_scroll = 0.0;
        self.detail = None;
        self.menu = None;
        self.selecting = false;
        self.picking = false;
    }

    fn open_usage(&mut self) {
        self.show_usage();
        self.queued
            .push(Input::Act(ChatAction::RefreshUsage { force: true }));
    }

    /// Hides the meter's detail if a key or a click showed it, as the
    /// next does. Returns whether it was shown.
    fn hide_detail(&mut self) -> bool {
        self.detail
            .take_if(|detail| *detail == paint::Detail::Asked)
            .is_some()
    }

    /// The mouse left the pane: the buttons that copy a block of code and
    /// an answer go, and the one under an answer stops saying it copied.
    /// Returns whether the chat must be redrawn.
    pub(crate) fn unhover(&mut self) -> bool {
        self.copied_code = None;
        let copied = self.copied_answer.take().is_some();
        self.mouse = None;
        let over_turn = self.hovered_turn.take().is_some();
        let detail = (self.detail)
            .take_if(|detail| *detail == paint::Detail::Hovered)
            .is_some();
        self.hovered_code.take().is_some() || copied || over_turn || detail
    }

    /// The mouse moved to `at` with no button held, over the menu, if
    /// it's open. Returns whether it must be redrawn.
    fn hover_menu(&mut self, at: [f32; 2]) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        // What's drawn last is on top, as the menu is over the cards.
        let under = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .and_then(|(hit, _)| match hit {
                Hit::Pick { setting, value } => Some(Spot::Choice {
                    setting: setting.clone(),
                    value: value.clone(),
                }),
                Hit::Step { setting, .. } | Hit::Reset { setting, .. } => {
                    Some(Spot::Slider(setting.clone()))
                }
                _ => None,
            });
        if under != menu.highlighted {
            menu.highlighted = under;
            return true;
        }
        false
    }

    /// The text drawn last, a piece a line, top to bottom, then the
    /// question asked, if one is, and the message being typed: what
    /// someone looking at the chat reads. For looking at the window
    /// without a desktop (see `crate::app::look`).
    pub(crate) fn shown_text(&self) -> String {
        let mut text = String::new();
        let mut last = None;
        for shown in &self.shown {
            // A piece drawn in several blocks is said once.
            let piece = (shown.key.turn, shown.key.piece);
            if last == Some(piece) {
                continue;
            }
            last = Some(piece);
            if let Some(piece) = self.piece_text(shown.key) {
                text.push_str(&piece);
                text.push('\n');
            }
        }
        if let Some(question) = &self.node.question {
            text.push_str(&question.title);
            text.push('\n');
            for choice in &question.choices {
                text.push_str(&choice.name);
                text.push('\n');
            }
        }
        text.push_str(self.editor.raw_text());
        text
    }

    /// Whether input waits to be settled.
    pub(crate) fn unsettled(&self) -> bool {
        !self.queued.is_empty()
            || self
                .image_viewer
                .as_ref()
                .is_some_and(viewer::Viewer::loading)
    }

    /// Carries out what's waiting in the queue, now that the fonts are at
    /// hand.
    pub(crate) fn settle(&mut self, fonts: &mut Fonts) -> Settled {
        let mut settled = Settled {
            redraw: self.image_viewer.as_mut().is_some_and(viewer::Viewer::poll),
            ..Settled::default()
        };
        if self.queued.is_empty() {
            return settled;
        }
        self.style_editor(fonts);
        let mut ready = std::mem::take(&mut self.queued);
        if let Some(waiting) = ready
            .iter()
            .position(|input| matches!(input, Input::Clipboard))
        {
            self.queued = ready.split_off(waiting);
        }
        for input in ready {
            // A drag that started nothing changes nothing.
            settled.redraw |= !matches!(input, Input::Drag(_))
                || self.selecting
                || self.sliding.is_some()
                || self.picking;
            match input {
                Input::Key {
                    key,
                    text,
                    modifiers,
                } => self.typed(fonts, &key, text.as_deref(), modifiers, &mut settled),
                Input::Clipboard => unreachable!("only ready inputs are settled"),
                Input::PasteFailed(why) => {
                    self.say(why);
                }
                Input::Image(image) => {
                    if self.node.question.is_some() {
                        self.typed_while_asking();
                    } else if !image.valid()
                        || self.images.len()
                            + self
                                .handoff_sent
                                .as_ref()
                                .map_or(0, |draft| draft.images.len())
                            >= clackatoo_protocol::ChatImage::MAX_COUNT
                        || self
                            .images
                            .iter()
                            .chain(self.handoff_sent.iter().flat_map(|draft| &draft.images))
                            .map(|image| image.data.len())
                            .sum::<usize>()
                            + image.data.len()
                            > clackatoo_protocol::ChatImage::MAX_DATA
                    {
                        self.say("Paste up to four images, 8 MiB encoded in total.".into());
                    } else {
                        self.images.push(image);
                    }
                }
                Input::Paste(text) => self.settle_paste(fonts, &text),
                Input::Press { at, clicks, shift } => {
                    self.pressed(fonts, at, clicks, shift, &mut settled);
                }
                // A drag does what the press it follows started, if anything.
                Input::Drag(at) if self.sliding.is_some() => self.slide(at[0], &mut settled),
                Input::Drag(at) if self.selecting => {
                    let [x, y] = self.in_editor(at);
                    self.drive(fonts, |driver| driver.extend_selection_to_point(x, y));
                }
                Input::Drag(at) if self.picking => {
                    self.drag_to = Some(at);
                    self.pick_at(at);
                }
                Input::Drag(_) => {}
                Input::Release => {
                    self.selecting = false;
                    self.sliding = None;
                    self.drag_to = None;
                    self.edge_step = None;
                    // Text selected with the mouse goes to the primary
                    // selection, as elsewhere on Linux.
                    if std::mem::take(&mut self.picking) {
                        match self.picked_range() {
                            Some((start, end)) => {
                                let text = self.text_between(fonts, start, end);
                                settled.copied.push((text, Copy::Primary));
                            }
                            // A click selects nothing.
                            None => self.picked = None,
                        }
                    }
                }
                input @ (Input::Yank { .. } | Input::YankText(_)) => {
                    self.settle_reading(fonts, input, &mut settled);
                }
                Input::Answer { question, pick } => self.pick(question, pick, &mut settled),
                Input::Act(action) => settled.actions.push(action),
                Input::Compose(_) if self.searching_models() => {}
                Input::Compose(Some((text, cursor))) => {
                    self.drive(fonts, |driver| driver.set_compose(&text, cursor));
                }
                Input::Compose(None) => self.drive(fonts, |driver| {
                    driver.clear_compose();
                }),
            }
        }
        settled
    }

    /// Edits the message with parley's editor.
    fn drive(
        &mut self,
        fonts: &mut Fonts,
        edit: impl FnOnce(&mut parley::PlainEditorDriver<'_, Brush>),
    ) {
        // set_text (send or restored draft) leaves parley's selection tied
        // to its previous layout until refreshed, even if keys arrive before paint.
        self.editor
            .refresh_layout(&mut fonts.font_cx, &mut self.layout_cx);
        let mut driver = self.editor.driver(&mut fonts.font_cx, &mut self.layout_cx);
        edit(&mut driver);
    }

    fn settle_paste(&mut self, fonts: &mut Fonts, text: &str) {
        if self.viewing_modal() {
            return;
        }
        if self.searching_models() {
            self.append_search(&text.replace(['\n', '\r'], " "));
            self.search_changed();
        } else if self.node.question.is_none() {
            let text = text.replace("\r\n", "\n");
            self.drive(fonts, |driver| driver.insert_or_replace_selection(&text));
        } else {
            self.typed_while_asking();
        }
    }

    /// A key typed into the chat in insert mode.
    fn typed(
        &mut self,
        fonts: &mut Fonts,
        key: &Key,
        text: Option<&str>,
        modifiers: Modifiers,
        settled: &mut Settled,
    ) {
        if self.modal_key(key, false) || self.menu_key(key, modifiers, settled) {
            return;
        }
        if self.node.question.is_some() {
            self.answer_key(key, text, modifiers, settled);
            return;
        }
        self.composer_key(key, modifiers, fonts, text, settled);
    }

    crate::shortcuts::bindings::described_keys! {
        fn composer_key / describe_composer_key(&mut self, key: &Key, modifiers: Modifiers) -> ();
        execute(fonts: &mut Fonts, text: Option<&str>, settled: &mut Settled);
        prepare {
            let (shift, ctrl) = (modifiers.shift, modifiers.ctrl);
            let character = match key {
                Key::Character(character) => Some(character.as_str()),
                _ => None,
            };
        }
        match (key, character) => {
            (Key::Named(NamedKey::Tab), _) if shift =>
                Hint::Action("Cycle agent modes"),
                {
                    self.next_mode(settled);
                };
            (Key::Named(NamedKey::Enter), _) if !shift || ctrl && self.queue_supported =>
                Hint::Action(if ctrl && self.queue_supported && self.busy() {
                    "Steer current turn"
                } else if self.busy() && self.queue_supported {
                    "Queue message"
                } else {
                    "Send message"
                }),
                {
                    self.send_or_steer(ctrl, settled);
                };
            (Key::Named(NamedKey::Enter), _) =>
                Hint::Action("New line"),
                {
                    self.drive(fonts, |driver| driver.insert_or_replace_selection("\n"));
                };
            (Key::Named(NamedKey::Backspace), _)
                if self.editor.raw_text().is_empty() && !self.images.is_empty() =>
                Hint::Action("Remove last attached image"),
                {
                    self.images.pop();
                };
            (Key::Named(NamedKey::Backspace), _) =>
                Hint::Action(if ctrl {
                    "Delete previous word"
                } else {
                    "Delete previous character"
                }),
                self.drive(fonts, |driver| {
                    if ctrl {
                        driver.backdelete_word();
                    } else {
                        driver.backdelete();
                    }
                });
            (Key::Named(NamedKey::Delete), _) =>
                Hint::Action(if ctrl {
                    "Delete next word"
                } else {
                    "Delete next character"
                }),
                self.drive(fonts, |driver| {
                    if ctrl {
                        driver.delete_word();
                    } else {
                        driver.delete();
                    }
                });
            (Key::Named(named @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)), _) =>
                Hint::Action(match (ctrl, shift) {
                    (true, true) => "Extend selection by word",
                    (true, false) => "Move by words",
                    (false, true) => "Extend selection by character",
                    (false, false) => "Move caret horizontally",
                }),
                {
                    let left = *named == NamedKey::ArrowLeft;
                    self.drive(fonts, |driver| match (left, ctrl, shift) {
                        (true, true, true) => driver.select_word_left(),
                        (true, true, false) => driver.move_word_left(),
                        (true, false, true) => driver.select_left(),
                        (true, false, false) => driver.move_left(),
                        (false, true, true) => driver.select_word_right(),
                        (false, true, false) => driver.move_word_right(),
                        (false, false, true) => driver.select_right(),
                        (false, false, false) => driver.move_right(),
                    });
                };
            (Key::Named(named @ (NamedKey::ArrowUp | NamedKey::ArrowDown)), _) =>
                Hint::Action(if shift {
                    "Extend selection vertically"
                } else {
                    "Move caret vertically"
                }),
                {
                    let up = *named == NamedKey::ArrowUp;
                    self.drive(fonts, |driver| match (up, shift) {
                        (true, true) => driver.select_up(),
                        (true, false) => driver.move_up(),
                        (false, true) => driver.select_down(),
                        (false, false) => driver.move_down(),
                    });
                };
            (Key::Named(NamedKey::Home), _) =>
                Hint::Action(if shift {
                    "Select to line start"
                } else {
                    "Start of line"
                }),
                self.drive(fonts, |driver| {
                    if shift {
                        driver.select_to_line_start();
                    } else {
                        driver.move_to_line_start();
                    }
                });
            (Key::Named(NamedKey::End), _) =>
                Hint::Action(if shift {
                    "Select to line end"
                } else {
                    "End of line"
                }),
                self.drive(fonts, |driver| {
                    if shift {
                        driver.select_to_line_end();
                    } else {
                        driver.move_to_line_end();
                    }
                });
            (_, Some("a" | "A")) if ctrl =>
                Hint::Action("Select draft"),
                self.drive(fonts, |driver| {
                    driver.select_all();
                });
            (_, Some("c" | "C")) if ctrl =>
                Hint::Action(
                    if self
                        .editor
                        .selected_text()
                        .is_some_and(|text| !text.is_empty())
                    {
                        "Copy draft selection"
                    } else if self.stoppable() {
                        "Stop agent"
                    } else {
                        "Copy conversation selection"
                    },
                ),
                match self.editor.selected_text() {
                    Some(selected) if !selected.is_empty() => {
                        settled.copied.push((selected.to_owned(), Copy::Clipboard));
                    }
                    _ if self.stoppable() => settled.actions.push(ChatAction::Stop),
                    _ => {
                        if let Some((start, end)) = self.picked_range() {
                            let text = self.text_between(fonts, start, end);
                            settled.copied.push((text, Copy::Clipboard));
                            self.picked = None;
                        }
                    }
                };
            _ if ctrl || modifiers.alt =>
                Hint::None,
                {};
            _ =>
                Hint::Text("Compose message"),
                {
                    if let Some(text) = text.filter(|text| !text.chars().any(char::is_control)) {
                        self.drive(fonts, |driver| driver.insert_or_replace_selection(text));
                    }
                };
        }
        then ()
    }

    /// Sends the message typed, unless the agent is still answering the
    /// last.
    fn send_or_steer(&mut self, steer: bool, settled: &mut Settled) {
        if steer && self.queue_supported {
            self.submit_draft(true, settled);
        } else {
            self.send(settled);
        }
    }

    fn send(&mut self, settled: &mut Settled) {
        if self.queue_supported {
            self.submit_draft(false, settled);
            return;
        }
        let text = self.editor.raw_text().trim().to_owned();
        if (text.is_empty() && self.images.is_empty()) || self.busy() || self.handoff_sent.is_some()
        {
            return;
        }
        if self.handoff.is_some() {
            self.send_handoff(text, settled);
            return;
        }
        self.editor.set_text("");
        self.follow = true;
        self.picked = None;
        settled.actions.push(if self.images.is_empty() {
            ChatAction::Send(text)
        } else {
            ChatAction::SendImages {
                text,
                images: std::mem::take(&mut self.images),
            }
        });
    }

    /// A key typed while the agent waits for an answer to its question,
    /// past [`pick_of`](Self::pick_of), as when the menu was open: an
    /// answer's number picks it, Esc says no, and Ctrl+C stops the agent.
    /// Any other key says in the bar which keys answer it, rather than
    /// going nowhere unseen.
    fn answer_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        modifiers: Modifiers,
        settled: &mut Settled,
    ) {
        let Some(question) = self.node.question.as_ref().map(|question| question.id) else {
            return;
        };
        let pick = match key {
            Key::Character(character) if modifiers.ctrl && character.eq_ignore_ascii_case("c") => {
                settled.actions.push(ChatAction::Stop);
                return;
            }
            Key::Named(NamedKey::Escape) => Some(Pick::Refuse),
            _ if modifiers.ctrl || modifiers.alt => None,
            _ => numbered(text).map(Pick::Choice),
        };
        match pick {
            Some(pick) => self.pick(question, pick, settled),
            None => self.typed_while_asking(),
        }
    }

    /// What insert mode typed while a question waits went nowhere: the bar
    /// says which numbers answer it, and they wait a moment again, as one
    /// typed among words, meant for the message, mustn't answer it.
    fn typed_while_asking(&mut self) {
        self.asked_at = Some(self.now());
        self.say(self.answer_hint());
    }

    /// The answer `press`, typed in insert mode, gives the question asked,
    /// and that question's number: an answer's number, or Esc for no.
    /// While the menu is open, it has the keys first.
    fn pick_of(&self, press: &KeyPress<'_>) -> Option<(u64, Pick)> {
        let question = self
            .node
            .question
            .as_ref()
            .filter(|_| self.menu.is_none())?;
        if press.modifiers.ctrl || press.modifiers.alt {
            return None;
        }
        let pick = match press.key {
            Key::Named(NamedKey::Escape) => Pick::Refuse,
            _ => Pick::Choice(numbered(press.text)?),
        };
        Some((question.id, pick))
    }

    /// Answers question `question` with `pick`, if it's still the one
    /// asked; one gone by is dropped. A number pressed as the question
    /// came, or as the focus arrived, says to press it again (see
    /// [`ANSWER_PAUSE`]); saying no never waits.
    fn pick(&mut self, question: u64, pick: Pick, settled: &mut Settled) {
        let Some(asked) = (self.node.question.as_ref()).filter(|asked| asked.id == question) else {
            return;
        };
        let option = match pick {
            Pick::Refuse => self.refusal(),
            Pick::Choice(index) => {
                let Some(id) = asked.choices.get(index).map(|choice| choice.id.clone()) else {
                    self.typed_while_asking();
                    return;
                };
                if self.just_asked() {
                    self.say(format!(
                        "a question just came: press {} again to answer it",
                        index + 1
                    ));
                    return;
                }
                Some(id)
            }
        };
        settled
            .actions
            .push(ChatAction::Answer { question, option });
    }

    /// Whether the question came, or the focus arrived at it, too short
    /// a time ago for a number to answer it (see [`ANSWER_PAUSE`]). Should
    /// the clock go back, it's long enough.
    fn just_asked(&self) -> bool {
        (self.asked_at)
            .and_then(|at| self.now().checked_sub(at))
            .is_some_and(|since| since < ANSWER_PAUSE)
    }

    /// What the bar says to a key that doesn't answer the question asked:
    /// which keys do, as they do in either mode.
    fn answer_hint(&self) -> String {
        let choices = (self.node.question.as_ref()).map_or(0, |question| question.choices.len());
        match choices {
            0 => "a question waits".to_owned(),
            1 => "a question waits: 1 answers it".to_owned(),
            count => format!("a question waits: 1–{} answer it", count.min(9)),
        }
    }

    /// The focus has arrived at the chat: if it's asking something, its
    /// answers' numbers wait a moment before they answer it, as they may
    /// have been typed for the pane that had the focus before.
    pub(crate) fn arrived(&mut self) {
        if self.node.question.is_some() {
            self.asked_at = Some(self.now());
        }
    }

    /// A key while the settings menu is open: Up and Down move through
    /// it, Left and Right move a slider or show and hide a model family's
    /// versions, Enter picks a choice, and Esc closes it, as does any other
    /// key, which then does what it would have. Returns whether it took
    /// the key.
    fn menu_key(&mut self, key: &Key, modifiers: Modifiers, settled: &mut Settled) -> bool {
        if self.menu.is_none() {
            return false;
        }
        if self.model_search_key(key, modifiers) {
            return true;
        }
        let menu = self.menu.clone().expect("menu checked above");
        let rows = self.choice_rows(&menu);
        let spots: Vec<Spot> = rows.iter().filter_map(Row::spot).collect();
        let at = menu
            .highlighted
            .as_ref()
            .and_then(|highlighted| spots.iter().position(|spot| spot == highlighted));
        self.menu_choice_key(key, menu, rows, spots, at, settled)
    }

    crate::shortcuts::bindings::described_keys! {
        fn model_search_key / describe_model_search_key(&mut self, key: &Key, modifiers: Modifiers) -> bool;
        match key => {
            _ if !self.searching_models() =>
                Hint::None,
                return false;
            Key::Character(_) if modifiers.ctrl || modifiers.alt =>
                Hint::None,
                return true;
            Key::Character(text) =>
                Hint::Text("Search all models, favourites first"),
                self.append_search(text);
            Key::Named(NamedKey::Backspace) =>
                Hint::Action("Delete last search character"),
                {
                    self.menu.as_mut().unwrap().query.pop();
                };
            _ =>
                Hint::None,
                return false;
        }
        then {
            self.search_changed();
            true
        }
    }

    crate::shortcuts::bindings::described_keys! {
        fn menu_choice_key / describe_menu_choice_key(&mut self, key: &Key) -> bool;
        execute(menu: Menu, rows: Vec<Row>, spots: Vec<Spot>, at: Option<usize>, settled: &mut Settled);
        match key => {
            Key::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) =>
                Hint::Action(if *direction == NamedKey::ArrowUp {
                    "Previous choice"
                } else {
                    "Next choice"
                }),
                {
                    if spots.is_empty() {
                        return true;
                    }
                    let last = spots.len() - 1;
                    let next = match (at, *direction == NamedKey::ArrowUp) {
                        (None, true) => last,
                        (None, false) => 0,
                        (Some(at), true) => at.checked_sub(1).unwrap_or(last),
                        (Some(at), false) => {
                            if at == last {
                                0
                            } else {
                                at + 1
                            }
                        }
                    };
                    if let Some(menu) = &mut self.menu {
                        menu.highlighted = Some(spots[next].clone());
                        menu.reveal = true;
                    }
                    true
                };
            Key::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) =>
                Hint::Action("Adjust value or expand model family"),
                {
                    let row = (rows.iter()).find(|row| row.spot() == menu.highlighted);
                    self.sideways(&rows, row, *direction == NamedKey::ArrowRight, settled);
                    true
                };
            Key::Named(NamedKey::Enter) =>
                Hint::Action("Choose highlighted item"),
                {
                    if let Some(Spot::Choice { setting, value }) = menu.highlighted {
                        self.choose(setting, value, settled);
                    }
                    self.menu = None;
                    true
                };
            Key::Named(NamedKey::Escape) =>
                Hint::Action("Close popup"),
                {
                    self.menu = None;
                    true
                };
            _ =>
                Hint::None,
                {
                    self.menu = None;
                    false
                };
        }
        result
    }

    /// An open popup owns its keys before pane-mode bindings see them.
    pub(crate) fn take_original_request(&mut self) -> Option<(u64, u64)> {
        self.image_viewer.as_mut()?.take_request()
    }

    pub(crate) fn original_answer(&mut self, serial: u64, response: clackatoo_protocol::Response) {
        if let Some(viewer) = &mut self.image_viewer {
            viewer.answer(serial, response);
        }
    }

    pub(crate) fn viewing_modal(&self) -> bool {
        self.image_viewer.is_some() || self.usage_modal
    }

    /// The modal owns keys before bindings, permission answers, or either mode.
    pub(crate) fn chat_modal_key(&mut self, press: &KeyPress<'_>) -> bool {
        self.modal_key(press.key, crate::vim::enters_normal_mode(press))
    }

    crate::shortcuts::bindings::described_keys! {
        fn modal_key / describe_modal_key(&mut self, key: &Key, normal: bool) -> bool;
        match (key, normal) => {
            _ if !self.viewing_modal() =>
                Hint::None,
                return false;
            (Key::Named(NamedKey::Escape), _) | (_, true) =>
                Hint::Action("Close modal"),
                {
                    self.image_viewer = None;
                    self.usage_modal = false;
                };
            (Key::Named(NamedKey::ArrowDown), _) if self.usage_modal =>
                Hint::Action("Scroll limits down"),
                {
                    self.scroll_by(40.0);
                };
            (Key::Named(NamedKey::ArrowUp), _) if self.usage_modal =>
                Hint::Action("Scroll limits up"),
                {
                    self.scroll_by(-40.0);
                };
            (Key::Named(NamedKey::PageDown), _) if self.usage_modal =>
                Hint::Action("Page down"),
                {
                    self.scroll_by(240.0);
                };
            (Key::Named(NamedKey::PageUp), _) if self.usage_modal =>
                Hint::Action("Page up"),
                {
                    self.scroll_by(-240.0);
                };
            _ =>
                Hint::None,
                {};
        }
        then true
    }

    pub(crate) fn menu_press(&mut self, press: &KeyPress<'_>) -> bool {
        let mut settled = Settled::default();
        let handled = self.menu_key(press.key, press.modifiers, &mut settled);
        self.queued
            .extend(settled.actions.into_iter().map(Input::Act));
        handled
    }

    /// Left or Right, on `row` of the menu's `rows`: along a slider, or
    /// into and out of a model family's versions.
    fn sideways(&mut self, rows: &[Row], row: Option<&Row>, right: bool, settled: &mut Settled) {
        match row {
            Some(Row::Slider(slider)) => {
                let last = slider.steps.len() - 1;
                let next = match slider.at {
                    Some(at) if right => (at + 1).min(last),
                    Some(at) => at.saturating_sub(1),
                    None => 0,
                };
                let value = slider.steps[next].0.clone();
                self.slide_to(&slider.setting, &value, settled);
            }
            Some(Row::Choice(choice)) => {
                let Some(menu) = &mut self.menu else {
                    return;
                };
                if let Some((family, _)) = &choice.family {
                    menu.expanded = right.then(|| family.clone());
                } else if choice.nested && !right {
                    // Back out to the family's row.
                    menu.highlighted = (rows.iter())
                        .find(|row| {
                            matches!(row, Row::Choice(head) if head.family.as_ref().is_some_and(|(_, open)| *open))
                        })
                        .and_then(Row::spot);
                    menu.expanded = None;
                }
            }
            _ => {}
        }
    }

    /// Moves the slider for `setting` to `value`, and asks the agent to
    /// set it, unless it's there.
    fn slide_to(&mut self, setting: &str, value: &str, settled: &mut Settled) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        menu.highlighted = Some(Spot::Slider(setting.to_owned()));
        let Some(current) = (self.node.settings.iter()).find(|each| each.id == setting) else {
            return;
        };
        if menu.value(current) == value {
            return;
        }
        menu.held = Some((setting.to_owned(), value.to_owned()));
        settled.actions.push(ChatAction::Set {
            setting: setting.to_owned(),
            value: value.to_owned(),
        });
    }

    /// The mouse, dragging a slider, moved to `x`, from the pane's left:
    /// the slider goes to the step whose share of it that's in, or the end
    /// nearest.
    fn slide(&mut self, x: f32, settled: &mut Settled) {
        let Some(setting) = self.sliding.clone() else {
            return;
        };
        let steps: Vec<(&String, [f32; 4])> = (self.hits.iter())
            .filter_map(|(hit, rect)| match hit {
                Hit::Step { setting: of, value } if *of == setting => Some((value, *rect)),
                _ => None,
            })
            .collect();
        let within = (steps.iter()).find(|(_, rect)| x >= rect[0] && x < rect[0] + rect[2]);
        let nearest = match steps.first() {
            Some((_, first)) if x < first[0] => steps.first(),
            _ => steps.last(),
        };
        let Some(value) = within.or(nearest).map(|(value, _)| (*value).clone()) else {
            return;
        };
        self.slide_to(&setting, &value, settled);
    }

    /// Opens the menu for `settings`, on the choice made, with the versions
    /// of a model family shown if one of them is chosen, or closes it if
    /// it's open for them. Returns whether the daemon is to be asked for
    /// the other agents' models, as the menu offers them and they aren't
    /// all known.
    fn toggle_menu(&mut self, settings: Vec<String>) -> bool {
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.settings == settings)
        {
            self.menu = None;
            return false;
        }
        let expanded = (settings.iter())
            .filter_map(|id| self.setting(id))
            .find_map(picker::pinned_family);
        let mut menu = Menu {
            settings,
            highlighted: None,
            expanded,
            held: None,
            favorites: favorites::load(),
            reveal: true,
            ..Menu::default()
        };
        menu.filter = picker::Filter::initial(
            self.handoff
                .as_ref()
                .map_or_else(|| self.agent(), |pending| pending.title.as_str()),
            &menu.favorites,
        );
        menu.highlighted = (self.choice_rows(&menu).iter())
            .find(|row| matches!(row, Row::Choice(choice) if choice.current))
            .and_then(Row::spot);
        let learn = picker::offers_models(&self.offer(), &menu)
            && (self.node.providers.iter()).any(|provider| {
                matches!(
                    provider.models,
                    ProviderModels::Unknown | ProviderModels::Failed(_)
                )
            });
        self.menu = Some(menu);
        learn
    }

    /// What the menus offer: the agent's settings, and the other agents
    /// the chat could move to.
    fn offer(&self) -> picker::Offer<'_> {
        picker::Offer {
            settings: &self.node.settings,
            agent: self.agent(),
            providers: &self.node.providers,
        }
    }

    /// Moves the agent's mode on to the next it offers, round to the first.
    fn next_mode(&mut self, settled: &mut Settled) {
        let Some(mode) =
            (self.node.settings.iter()).find(|setting| setting.kind == SettingKind::Mode)
        else {
            return;
        };
        let at = (mode.choices.iter()).position(|choice| choice.value == mode.value);
        let next = at.map_or(0, |at| (at + 1) % mode.choices.len());
        if let Some(choice) = mode.choices.get(next) {
            settled.actions.push(ChatAction::Set {
                setting: mode.id.clone(),
                value: choice.value.clone(),
            });
        }
    }

    /// The answer that says no: the agent's own, or none.
    fn refusal(&self) -> Option<String> {
        let choices = &self.node.question.as_ref()?.choices;
        (choices.iter())
            .find(|choice| choice.kind == ChoiceKind::RejectOnce)
            .or_else(|| (choices.iter()).find(|choice| choice.kind == ChoiceKind::RejectAlways))
            .map(|choice| choice.id.clone())
    }

    /// A mouse button went down: on a button, it's pressed, and in the
    /// message, the cursor goes there.
    #[allow(
        clippy::too_many_lines,
        reason = "one action for each kind of chat control"
    )]
    fn pressed(
        &mut self,
        fonts: &mut Fonts,
        at: [f32; 2],
        clicks: u8,
        shift: bool,
        settled: &mut Settled,
    ) {
        // A drag whose release was lost, as outside the window, is over.
        self.drag_to = None;
        self.edge_step = None;
        // Over everything, a menu takes the click first.
        let hit = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .map(|(hit, _)| hit.clone());
        if self.usage_modal {
            match hit {
                Some(Hit::CloseUsage) | None => self.usage_modal = false,
                Some(Hit::RefreshUsage) => settled
                    .actions
                    .push(ChatAction::RefreshUsage { force: true }),
                _ => {}
            }
            return;
        }
        if self.image_viewer.is_some() {
            if hit == Some(Hit::CloseImage) {
                self.image_viewer = None;
            }
            return;
        }
        if self.menu.is_none()
            && !matches!(hit, Some(Hit::RemoveImage(_)))
            && let Some(image) = self.previews.at(at)
        {
            let local =
                self.images
                    .iter()
                    .find(|candidate| {
                        candidate.preview.as_ref().is_some_and(|preview| {
                            std::sync::Arc::ptr_eq(&preview.rgba, &image.rgba)
                        })
                    })
                    .map(|image| image.data.clone());
            let mut viewer = viewer::Viewer::new(image, local);
            if self.offline {
                viewer.offline();
            }
            self.image_viewer = Some(viewer);
            self.hide_detail();
            self.selecting = false;
            self.picking = false;
            return;
        }
        if hit == Some(Hit::Usage) && self.usage_supported {
            self.show_usage();
            settled
                .actions
                .push(ChatAction::RefreshUsage { force: true });
            return;
        }
        // A click closes the meter's detail, if a click showed it, unless
        // it's on the meter, which shows it as long as the mouse is there.
        if self.hide_detail() && hit == Some(Hit::Usage) {
            self.detail = Some(paint::Detail::Hovered);
        } else if hit == Some(Hit::Usage) {
            self.detail = Some(paint::Detail::Asked);
        }
        // A click anywhere else closes the menu.
        if !matches!(
            hit,
            Some(
                Hit::Pick { .. }
                    | Hit::Chip(_)
                    | Hit::Expand(_)
                    | Hit::Step { .. }
                    | Hit::Reset { .. }
                    | Hit::MenuSurface
                    | Hit::ModelRail
                    | Hit::FastMode
                    | Hit::ModelSearch
                    | Hit::ModelFilter(_)
                    | Hit::Favorite(..)
            )
        ) {
            self.menu = None;
        }
        match hit {
            Some(
                Hit::ModelSearch
                | Hit::MenuSurface
                | Hit::ModelRail
                | Hit::Usage
                | Hit::CloseImage
                | Hit::CloseUsage
                | Hit::RefreshUsage
                | Hit::UsageSurface,
            ) => {}
            Some(Hit::FastMode) => {
                if let Some(setting) = picker::fast_mode(&self.node.settings).cloned() {
                    let value = self
                        .menu
                        .as_ref()
                        .map_or(setting.value.as_str(), |m| m.value(&setting));
                    self.slide_to(
                        &setting.id,
                        if value == "on" { "off" } else { "on" },
                        settled,
                    );
                }
            }
            Some(Hit::ModelFilter(filter)) => {
                if let Some(menu) = &mut self.menu {
                    menu.filter = filter;
                    menu.query.clear();
                    menu.offset = 0;
                    menu.highlighted = None;
                }
            }
            Some(Hit::Favorite(provider, value)) => {
                if let Some(menu) = &mut self.menu {
                    let key = (provider, value);
                    let selected = if menu.favorites.remove(&key) {
                        false
                    } else {
                        menu.favorites.insert(key.clone());
                        true
                    };
                    favorites::save(key, selected);
                }
            }
            Some(Hit::Chip(settings)) => {
                if self.toggle_menu(settings) {
                    settled.actions.push(ChatAction::LearnModels);
                }
            }
            Some(Hit::Expand(family)) => {
                if let Some(menu) = &mut self.menu {
                    menu.expanded = (menu.expanded.as_ref() != Some(&family)).then_some(family);
                }
            }
            Some(Hit::Step { setting, value }) => {
                self.sliding = Some(setting.clone());
                self.slide_to(&setting, &value, settled);
            }
            Some(Hit::Reset { setting, value }) => self.slide_to(&setting, &value, settled),
            Some(Hit::Pick { setting, value }) => {
                self.menu = None;
                self.choose(setting, value, settled);
            }
            Some(Hit::CancelActivity(id)) => settled.cancel_activities.push(id),
            Some(Hit::Card(turn, tool)) => {
                self.restyled(turn);
                if !self.open_cards.remove(&tool) {
                    self.open_cards.insert(tool);
                }
            }
            Some(Hit::Undo(turn)) => settled.actions.push(ChatAction::Undo {
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
                force: false,
            }),
            Some(Hit::Redo(turn)) => settled.actions.push(ChatAction::Redo {
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
                force: false,
            }),
            Some(Hit::Fold(turn)) => self.toggle_fold(turn),
            Some(Hit::Agents(turn)) => self.toggle_agents(turn),
            Some(Hit::Subagent(chat)) => settled.actions.push(ChatAction::Open { chat }),
            Some(Hit::CopyCode(key)) => {
                if let Some(text) = self.text(key) {
                    settled.copied.push((text.text.to_owned(), Copy::Clipboard));
                    self.copied_code = Some(key);
                }
            }
            // As `y` copies it: the Markdown it came as.
            Some(Hit::CopyAnswer(turn)) => {
                let answer = (self.node.turns.get(turn)).and_then(paint::answer_to_copy);
                let key = answer.map(|item| TextKey {
                    turn,
                    piece: paint::ITEMS + item,
                    part: 0,
                });
                if let Some(text) = key.and_then(|key| self.piece_text(key)) {
                    settled.copied.push((text, Copy::Clipboard));
                    self.copied_answer = Some(turn);
                }
            }
            Some(Hit::RemoveImage(index)) => self.remove_image(index),
            Some(Hit::CancelHandoff) => {
                if self.handoff_sent.is_none() {
                    self.handoff = None;
                }
            }
            Some(Hit::Send) => self.send(settled),
            Some(Hit::Steer) => self.submit_draft(true, settled),
            Some(Hit::QueuePage(delta)) => {
                self.queue_page = self.queue_page.saturating_add_signed(isize::from(delta));
            }
            Some(Hit::RemoveQueued(id)) => settled.actions.push(ChatAction::RemoveQueued { id }),
            Some(Hit::SendQueued(id)) => settled.actions.push(ChatAction::SendQueued {
                id,
                steer: self.busy(),
            }),
            Some(Hit::Stop) => settled.actions.push(ChatAction::Stop),
            Some(Hit::Choice(id)) => {
                // The question the button was drawn for, which may have
                // been answered since.
                if let Some(question) = &self.node.question {
                    settled.actions.push(ChatAction::Answer {
                        question: question.id,
                        option: Some(id),
                    });
                }
            }
            Some(Hit::Editor) => {
                let [x, y] = self.in_editor(at);
                self.selecting = true;
                self.drive(fonts, |driver| match (clicks, shift) {
                    (_, true) => driver.shift_click_extension(x, y),
                    (2, _) => driver.select_word_at_point(x, y),
                    (3.., _) => driver.select_hard_line_at_point(x, y),
                    _ => driver.move_to_point(x, y),
                });
            }
            None => self.press_text(at, clicks, shift),
        }
    }

    /// A mouse button went down at `at`, on no button: on the
    /// conversation's text, it starts selecting, a character, word or line
    /// at a time as `clicks` says, or with `shift`, goes on with the
    /// selection there is; and the cursor goes there. Anywhere else, what
    /// was selected isn't.
    fn press_text(&mut self, at: [f32; 2], clicks: u8, shift: bool) {
        let Some(spot) = self.spot_at(at, false) else {
            self.picked = None;
            return;
        };
        let (anchor, head) = match (clicks, self.picked) {
            (_, Some(picked)) if shift => (picked.anchor, spot),
            (2, _) => self.word_at(at, false).unwrap_or((spot, spot)),
            (3.., _) => self.word_at(at, true).unwrap_or((spot, spot)),
            _ => (spot, spot),
        };
        self.picked = Some(Picked {
            anchor,
            head,
            how: How::Mouse,
        });
        self.picking = true;
        self.cursor = Some(spot);
        self.goal = None;
    }

    /// The setting with `id`, if the agent offers it.
    fn setting(&self, id: &str) -> Option<&Setting> {
        self.node.settings.iter().find(|setting| setting.id == id)
    }

    /// `at`, from the pane's top left, from the editor's text's instead.
    fn in_editor(&self, at: [f32; 2]) -> [f32; 2] {
        [at[0] - self.editor_origin[0], at[1] - self.editor_origin[1]]
    }

    /// Styles the editor's text for the fonts, if they've changed.
    fn style_editor(&mut self, fonts: &Fonts) {
        let size = paint::text_size(fonts);
        if (size.to_bits(), fonts.generation) == (self.editor_size.to_bits(), self.editor_fonts) {
            return;
        }
        self.editor_size = size;
        self.editor_fonts = fonts.generation;
        let styles = self.editor.edit_styles();
        for property in paint::body_style(fonts, size) {
            styles.insert(property);
        }
    }

    /// Lays the editor's text out `width` wide, if it isn't already: a key
    /// typed lays the message out once, as it's typed, and drawing it
    /// again costs nothing more.
    fn size_editor(&mut self, width: f32) {
        if self.editor_width.map(f32::to_bits) != Some(width.to_bits()) {
            self.editor_width = Some(width);
            self.editor.set_width(Some(width));
        }
    }

    // --- Scrolling ------------------------------------------------------------

    /// Scrolls `lines` lines up, or down if negative. Returns whether the
    /// view moved.
    pub(crate) fn scroll_lines(&mut self, lines: f32) -> bool {
        if self.usage_modal {
            return self.scroll_by(-lines * self.line_height);
        }
        if self.image_viewer.is_some() {
            return false;
        }
        let rail = self.mouse.is_some_and(|at| {
            self.hits
                .iter()
                .any(|(hit, rect)| *hit == Hit::ModelRail && contains(*rect, at))
        });
        if let Some(menu) = &mut self.menu {
            let offset = if rail {
                &mut menu.rail_offset
            } else {
                &mut menu.offset
            };
            if lines > 0.0 {
                *offset = offset.saturating_sub(1);
            } else if lines < 0.0 {
                *offset = offset.saturating_add(1);
            }
            menu.reveal = false;
            return true;
        }
        self.scroll_by(-lines * self.line_height)
    }

    /// Scrolls half a view up, or down, as tests scroll it.
    #[cfg(test)]
    pub(crate) fn scroll_half(&mut self, up: bool) -> bool {
        let half = self.view_height / 2.0;
        self.scroll_by(if up { -half } else { half })
    }

    pub(crate) fn wheel_metrics(&self) -> Option<(f32, f32, f32)> {
        if self.usage_modal {
            return Some((self.usage_scroll, self.usage_max_scroll, self.line_height));
        }
        (self.menu.is_none() && !self.viewing_modal()).then_some((
            self.scroll,
            self.max_scroll,
            self.line_height,
        ))
    }

    pub(crate) fn scroll_by(&mut self, pixels: f32) -> bool {
        if self.usage_modal {
            let old = self.usage_scroll;
            self.usage_scroll = (old + pixels).clamp(0.0, self.usage_max_scroll);
            return (old - self.usage_scroll).abs() > f32::EPSILON;
        }
        if self.image_viewer.is_some() {
            return false;
        }
        let old = self.scroll;
        self.scroll = (self.scroll + pixels).clamp(0.0, self.max_scroll);
        self.follow = self.scroll >= self.max_scroll - 1.0;
        (self.scroll - old).abs() > f32::EPSILON
    }

    /// What the bottom bar says about the chat: what the window last said
    /// of it, or how far up the view is.
    pub(crate) fn label(&self) -> Option<String> {
        let said = (self.said.clone())
            .or_else(|| (!self.follow && self.max_scroll > 0.0).then(|| "↑ scrolled".to_owned()));
        // A command half typed shows first, as Vim's `showcmd` shows it.
        match (self.keys.shown(), said) {
            (Some(keys), Some(said)) => Some(format!("{keys}  {said}")),
            (keys, said) => keys.or(said),
        }
    }

    /// Says `text` in the bottom bar, until the next key in the chat.
    /// Returns that the pane must be drawn again.
    pub(crate) fn say(&mut self, text: String) -> bool {
        self.said = Some(text);
        true
    }

    /// Stops saying what [`say`](Self::say) said. Returns whether it said
    /// anything, so the pane must be drawn again.
    pub(crate) fn unsay(&mut self) -> bool {
        self.said.take().is_some()
    }

    /// The turn `u` undoes, or with `redo`, the one Ctrl+R redoes, as Vim
    /// steps through its undo: the last whose changes to files are made,
    /// or the first of those undone. `None` if there's none.
    pub(crate) fn turn_to_undo(&self, redo: bool) -> Option<usize> {
        let mut turns = self.node.turns.iter().enumerate();
        let found = if redo {
            turns.find(|(_, turn)| undoable(turn, redo))
        } else {
            turns.rfind(|(_, turn)| undoable(turn, redo))
        };
        found.map(|(at, _)| at)
    }
}

/// Whether what `turn` changed in files can be undone, or with `redo`,
/// made again.
fn undoable(turn: &clackatoo_protocol::Turn, redo: bool) -> bool {
    turn.end.is_some()
        && turn.files.is_some_and(|files| match files.state {
            TurnFilesState::Made => !redo,
            TurnFilesState::Undone => redo,
            TurnFilesState::Partly => true,
            TurnFilesState::TooOld => false,
        })
}

#[cfg(test)]
impl ChatView {
    /// Stops the clock at `now`, in milliseconds since the Unix epoch, for
    /// what turns still going say to stay the same.
    pub(crate) fn set_clock(&mut self, now: u64) {
        self.clock = Some(now);
    }

    /// Opens turn `turn`'s work, as clicking the line it folds under does.
    pub(crate) fn open_fold(&mut self, turn: usize) {
        self.set_fold(turn, true);
    }

    /// Shows the button that copies the first block of code drawn, as
    /// the mouse over it does.
    pub(crate) fn hover_first_code(&mut self) {
        self.hovered_code = self
            .shown
            .iter()
            .find(|shown| shown.code)
            .map(|shown| shown.key);
    }

    /// Opens the card of the tool with `id`, as clicking it does.
    pub(crate) fn open_card(&mut self, id: &str) {
        self.open_cards.insert(id.to_owned());
    }

    /// Opens the menu for the setting of `kind`, as clicking its chip does.
    /// Returns whether it asks the daemon for other agents' models.
    pub(crate) fn open_menu(&mut self, kinds: &[SettingKind]) -> bool {
        let ids = (kinds.iter())
            .filter_map(|kind| {
                self.node
                    .settings
                    .iter()
                    .find(|setting| setting.kind == *kind)
            })
            .map(|setting| setting.id.clone())
            .collect();
        self.toggle_menu(ids)
    }
}

/// The turn `patch` changes, if it changes one.
fn touches(patch: &clackatoo_protocol::ChatPatch) -> Option<usize> {
    use clackatoo_protocol::ChatPatch;
    match patch {
        ChatPatch::Turn { index: turn, .. }
        | ChatPatch::Item { turn, .. }
        | ChatPatch::Append { turn, .. }
        | ChatPatch::Output { turn, .. }
        | ChatPatch::Files { turn, .. } => usize::try_from(*turn).ok(),
        ChatPatch::State(_)
        | ChatPatch::Question(_)
        | ChatPatch::Notice(_)
        | ChatPatch::Settings(_)
        | ChatPatch::Usage(_)
        | ChatPatch::Agent(_)
        | ChatPatch::Providers(_)
        | ChatPatch::Subagents(_)
        | ChatPatch::Delivery(_)
        | ChatPatch::Activities(_)
        | ChatPatch::ProviderUsage(_)
        | ChatPatch::History { .. } => None,
    }
}

/// Whether `rect` (left, top, width and height) holds `at`.
fn contains([left, top, width, height]: [f32; 4], at: [f32; 2]) -> bool {
    at[0] >= left && at[0] < left + width && at[1] >= top && at[1] < top + height
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Choice, Question, Turn};

    use super::*;

    fn press(view: &mut ChatView, key: &Key, text: Option<&str>, modifiers: Modifiers) {
        view.key(&KeyPress {
            key,
            text,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers,
        });
    }

    fn typed(view: &mut ChatView, text: &str) {
        for ch in text.chars() {
            let text = ch.to_string();
            press(
                view,
                &Key::Character(text.as_str().into()),
                Some(&text),
                Modifiers::default(),
            );
        }
    }

    fn enter(view: &mut ChatView, shift: bool) {
        press(
            view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers {
                shift,
                ..Modifiers::default()
            },
        );
    }

    fn fonts() -> Fonts {
        Fonts::load(&crate::fonts::FontOptions::default(), 1.0).expect("a font")
    }

    fn pasted_image() -> clackatoo_protocol::ChatImage {
        clackatoo_protocol::ChatImage {
            preview: None,
            data: "cG5n".into(),
            width: 2,
            height: 3,
        }
    }

    #[test]
    fn busy_drafts_queue_or_steer_and_rejections_restore_attachments() {
        let mut fonts = fonts();
        for steer in [false, true] {
            let mut node = fixtures::talk(&[("working", "")]);
            node.turns[0].end = None;
            let mut view = ChatView::new("chat".into(), node);
            view.queue_supported = true;
            view.paste_image(pasted_image());
            typed(&mut view, "next");
            press(
                &mut view,
                &Key::Named(NamedKey::Enter),
                None,
                Modifiers {
                    ctrl: steer,
                    ..Modifiers::default()
                },
            );
            assert_eq!(
                view.settle(&mut fonts).actions,
                vec![ChatAction::Submit {
                    text: "next".into(),
                    images: vec![pasted_image()],
                    target: None,
                    steer
                }]
            );
            assert!(view.editor.raw_text().is_empty());
            enter(&mut view, false);
            assert!(view.settle(&mut fonts).actions.is_empty());
            view.handoff_answer(clackatoo_protocol::Response::Error(
                clackatoo_protocol::Error::Other("queue full".into()),
            ));
            assert_eq!(view.editor.raw_text(), "next");
            assert_eq!(view.images, vec![pasted_image()]);
        }
    }

    #[test]
    fn pasted_images_go_with_text_or_alone_and_can_be_removed() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), ChatNode::default());
        view.paste_image(pasted_image());
        typed(&mut view, "look");
        enter(&mut view, false);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [ChatAction::SendImages {
                text: "look".into(),
                images: vec![pasted_image()],
            }]
        );
        assert!(view.images.is_empty());
        view.paste_image(pasted_image());
        enter(&mut view, false);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [ChatAction::SendImages {
                text: String::new(),
                images: vec![pasted_image()],
            }]
        );
        view.paste_image(pasted_image());
        press(
            &mut view,
            &Key::Named(NamedKey::Backspace),
            None,
            Modifiers::default(),
        );
        enter(&mut view, false);
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert!(view.images.is_empty());
    }

    #[test]
    fn enter_waits_for_clipboard_and_busy_chat_keeps_draft_images() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), ChatNode::default());
        typed(&mut view, "look");
        view.begin_paste();
        typed(&mut view, " after");
        enter(&mut view, false);
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.editor.raw_text(), "look");
        view.finish_paste(Ok(crate::clipboard_image::Pasted::Text(" pasted".into())));
        view.settle(&mut fonts);
        assert_eq!(view.editor.raw_text(), "look pasted after");
        view.paste_image(pasted_image());
        view.node.turns.push(Turn {
            previews: Vec::new(),
            prompt: "previous".into(),
            sent: 0,
            items: vec![],
            end: None,
            files: None,
            from: None,
            switch: None,
        });
        enter(&mut view, false);
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.images.len(), 1);
        view.begin_paste();
        press(
            &mut view,
            &Key::Character("c".into()),
            None,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(view.settle(&mut fonts).actions, [ChatAction::Stop]);
    }

    #[test]
    fn excess_images_are_refused_without_losing_the_draft() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), ChatNode::default());
        for _ in 0..5 {
            view.paste_image(pasted_image());
        }
        view.settle(&mut fonts);
        assert_eq!(view.images.len(), 4);
        assert!(view.said.as_ref().unwrap().contains("four images"));
    }

    #[test]
    fn enter_sends_what_was_typed_and_shift_enter_starts_a_line() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), ChatNode::default());
        typed(&mut view, "hello");
        enter(&mut view, true);
        typed(&mut view, "there");
        enter(&mut view, false);
        let settled = view.settle(&mut fonts);
        assert_eq!(settled.actions, [ChatAction::Send("hello\nthere".into())]);
        assert_eq!(view.editor.raw_text(), "");
        // Nothing typed: nothing sent.
        enter(&mut view, false);
        assert_eq!(view.settle(&mut fonts).actions, []);
    }

    #[test]
    fn nothing_is_sent_while_the_agent_answers_and_ctrl_c_stops_it() {
        let mut fonts = fonts();
        let mut node = ChatNode::default();
        node.turns.push(Turn {
            previews: Vec::new(),
            prompt: "hi".into(),
            sent: 0,
            items: Vec::new(),
            end: None,
            files: None,
            from: None,
            switch: None,
        });
        let mut view = ChatView::new(String::new(), node);
        typed(&mut view, "more");
        enter(&mut view, false);
        press(
            &mut view,
            &Key::Character("c".into()),
            None,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(view.settle(&mut fonts).actions, [ChatAction::Stop]);
        assert_eq!(view.editor.raw_text(), "more");
    }

    #[test]
    fn ctrl_c_stops_an_agent_starting_with_no_message_and_one_typed_goes() {
        let mut fonts = fonts();
        let node = ChatNode {
            state: ChatState::Starting,
            ..ChatNode::default()
        };
        let mut view = ChatView::new(String::new(), node);
        let ctrl_c = |view: &mut ChatView| {
            press(
                view,
                &Key::Character("c".into()),
                None,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
            );
        };
        assert!(view.stoppable());
        ctrl_c(&mut view);
        assert_eq!(view.settle(&mut fonts).actions, [ChatAction::Stop]);
        // Typed while it starts, a message goes, and waits for it.
        typed(&mut view, "hi");
        assert_eq!(view.settle(&mut fonts).actions, []);
        assert!(!view.stoppable());
        enter(&mut view, false);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [ChatAction::Send("hi".into())]
        );
    }

    /// When the tests' clock starts, in milliseconds.
    const START: u64 = 1_000_000;

    /// A chat asking whether to run `ls`, yes or no, on the tests' clock,
    /// asked long enough ago for a number to answer it.
    fn asking() -> ChatView {
        let choice = |id: &str, kind| Choice {
            id: id.into(),
            name: id.into(),
            kind,
        };
        let node = ChatNode {
            question: Some(Question {
                id: 4,
                title: "run ls".into(),
                choices: vec![
                    choice("yes", ChoiceKind::AllowOnce),
                    choice("no", ChoiceKind::RejectOnce),
                ],
                own: false,
            }),
            ..ChatNode::default()
        };
        let mut view = ChatView::new(String::new(), node);
        view.set_clock(START);
        view.asked_at = Some(START - ANSWER_PAUSE);
        view
    }

    fn answer(option: &str) -> ChatAction {
        ChatAction::Answer {
            question: 4,
            option: Some(option.into()),
        }
    }

    #[test]
    fn a_question_is_answered_by_number_or_turned_down_with_esc() {
        let mut fonts = fonts();
        let mut view = asking();
        typed(&mut view, "1");
        press(
            &mut view,
            &Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        typed(&mut view, "7");
        assert_eq!(
            view.settle(&mut fonts).actions,
            [answer("yes"), answer("no")]
        );
        // Typing went to the answer, not the message, and a number no
        // answer has says which do.
        assert_eq!(view.editor.raw_text(), "");
        assert_eq!(
            view.label().as_deref(),
            Some("a question waits: 1–2 answer it")
        );
    }

    /// `key`, typing `text`, pressed in normal mode: whether the chat must
    /// be redrawn, and whether it goes to insert mode.
    fn read(view: &mut ChatView, key: &Key, text: Option<&str>) -> (bool, bool) {
        view.read_key(&KeyPress {
            key,
            text,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        })
    }

    /// `keys` pressed in normal mode, each a character, or `⏎` for Enter.
    fn read_keys(view: &mut ChatView, keys: &str) {
        for ch in keys.chars() {
            if ch == '⏎' {
                read(view, &Key::Named(NamedKey::Enter), None);
            } else {
                let text = ch.to_string();
                read(view, &Key::Character(text.as_str().into()), Some(&text));
            }
        }
    }

    /// Moves the chat's clock on `ms` milliseconds.
    fn step(view: &mut ChatView, ms: u64) {
        let now = view.now();
        view.set_clock(now + ms);
    }

    #[test]
    fn in_normal_mode_a_number_arms_its_answer_and_again_or_enter_gives_it() {
        let mut fonts = fonts();
        let mut view = asking();
        let two = Key::Character("2".into());
        assert_eq!(read(&mut view, &two, Some("2")), (true, false));
        assert_eq!(view.armed(), Some(1));
        assert!(!view.unsettled(), "armed, not answered");
        assert_eq!(view.label().as_deref(), Some("2 no · 2 again or Enter"));
        // `2j`, a count from habit, answers nothing: `j` drops it and
        // moves, as ever.
        read_keys(&mut view, "j");
        assert_eq!(view.armed(), None);
        assert!(view.settle(&mut fonts).actions.is_empty());
        // The same number again, or Enter, a moment later gives it.
        read_keys(&mut view, "2");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "2");
        assert_eq!(view.settle(&mut fonts).actions, [answer("no")]);
        read_keys(&mut view, "1");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "⏎");
        assert_eq!(view.settle(&mut fonts).actions, [answer("yes")]);
        // As late as it can be.
        read_keys(&mut view, "2");
        step(&mut view, ARMED_FOR.end - 1);
        read_keys(&mut view, "2");
        assert_eq!(view.settle(&mut fonts).actions, [answer("no")]);
        // Another number arms that one instead.
        read_keys(&mut view, "1");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "2");
        assert_eq!(view.armed(), Some(1));
        assert!(view.settle(&mut fonts).actions.is_empty());
        // Esc, pressed from habit, drops it and leaves the question be.
        read(&mut view, &Key::Named(NamedKey::Escape), None);
        assert_eq!(view.armed(), None);
        assert!(view.settle(&mut fonts).actions.is_empty());
        // A number no answer has says which do; `0` and `j` read.
        read_keys(&mut view, "7");
        assert_eq!(
            view.label().as_deref(),
            Some("a question waits: 1–2 answer it")
        );
        read_keys(&mut view, "0j");
        assert!(!view.unsettled());
        // And `i` still goes to type.
        assert_eq!(
            read(&mut view, &Key::Character("i".into()), Some("i")),
            (true, true)
        );
    }

    #[test]
    fn a_count_typed_in_one_go_never_answers() {
        let mut fonts = fonts();
        let mut view = asking();
        // Too soon after the number, the same again or Enter leaves it
        // armed, and the key after drops it.
        for keys in ["22j", "2⏎k", "11G", "22", "2⏎"] {
            read_keys(&mut view, keys);
            assert!(view.settle(&mut fonts).actions.is_empty(), "{keys}");
            read_keys(&mut view, "j");
            assert_eq!(view.armed(), None, "{keys}");
        }
        // Still armed when the moment comes, it gives it then.
        read_keys(&mut view, "22");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "2");
        assert_eq!(view.settle(&mut fonts).actions, [answer("no")]);
    }

    #[test]
    fn an_answer_armed_and_forgotten_is_only_armed_again() {
        let mut fonts = fonts();
        let mut view = asking();
        read_keys(&mut view, "1");
        step(&mut view, ARMED_FOR.end);
        read_keys(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty(), "armed again");
        assert_eq!(view.armed(), Some(0));
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "⏎");
        assert_eq!(view.settle(&mut fonts).actions, [answer("yes")]);
        // A click, the wheel, or the window losing the focus drops it.
        read_keys(&mut view, "1");
        view.press([0.0; 2], 1, false);
        assert_eq!(view.armed(), None);
        read_keys(&mut view, "1");
        assert!(view.disarm());
        assert_eq!(view.armed(), None);
        assert!(!view.disarm());
    }

    #[test]
    fn the_answer_armed_is_ringed_on_the_card() {
        let mut fonts = fonts();
        let mut view = asking();
        let ringed = |view: &mut ChatView, fonts: &mut Fonts| {
            let paint = view.paint(fonts, [0.0; 2], [720.0, 640.0], true);
            (paint.fills.iter())
                .filter(|fill| matches!(fill.border, Some((_, crate::render::Paint::Ink))))
                .count()
        };
        assert_eq!(ringed(&mut view, &mut fonts), 0);
        read_keys(&mut view, "2");
        assert_eq!(ringed(&mut view, &mut fonts), 1);
        read_keys(&mut view, "k");
        assert_eq!(ringed(&mut view, &mut fonts), 0);
    }

    #[test]
    fn numbers_as_a_question_comes_or_the_focus_arrives_at_it_dont_answer_it() {
        let mut fonts = fonts();
        let mut view = asking();
        view.arrived();
        // In insert mode, a number says to press it again; Esc still says
        // no at once.
        typed(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(
            view.label().as_deref(),
            Some("a question just came: press 1 again to answer it")
        );
        press(
            &mut view,
            &Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert_eq!(view.settle(&mut fonts).actions, [answer("no")]);
        // In normal mode, it stays armed until the moment's past.
        read_keys(&mut view, "1");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(
            view.label().as_deref(),
            Some("a question just came: press 1 again or Enter to answer it")
        );
        assert_eq!(view.armed(), Some(0));
        view.set_clock(START + ANSWER_PAUSE);
        read_keys(&mut view, "1");
        assert_eq!(view.settle(&mut fonts).actions, [answer("yes")]);
        // A question that comes waits too.
        let mut next = asking().node;
        if let Some(question) = &mut next.question {
            question.id = 5;
        }
        view.apply(&ViewUpdate::Snapshot(clackatoo_protocol::View {
            title: String::new(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(next)),
        }));
        typed(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
        step(&mut view, ANSWER_PAUSE);
        typed(&mut view, "1");
        assert_eq!(view.settle(&mut fonts).actions.len(), 1);
    }

    #[test]
    fn letters_typed_while_a_question_waits_make_numbers_wait_again() {
        let mut fonts = fonts();
        let mut view = asking();
        view.asked_at = Some(START);
        step(&mut view, 500);
        typed(&mut view, "ab");
        view.settle(&mut fonts);
        // 600 ms after the question, but 100 after the last letter.
        step(&mut view, 100);
        typed(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
        step(&mut view, ANSWER_PAUSE);
        typed(&mut view, "1");
        assert_eq!(view.settle(&mut fonts).actions, [answer("yes")]);
        // So does a paste.
        view.paste("words");
        view.settle(&mut fonts);
        step(&mut view, 100);
        typed(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
    }

    #[test]
    fn a_chat_first_seen_asking_waits_a_moment_too() {
        let mut fonts = fonts();
        let node = asking().node;
        let mut view = ChatView::new(String::new(), node);
        let asked = view.asked_at.expect("the moment it was seen");
        view.set_clock(asked + ANSWER_PAUSE - 1);
        typed(&mut view, "1");
        assert!(view.settle(&mut fonts).actions.is_empty());
        view.set_clock(asked + ANSWER_PAUSE);
        typed(&mut view, "1");
        assert_eq!(view.settle(&mut fonts).actions, [answer("yes")]);
    }

    #[test]
    fn an_answer_waiting_when_the_question_goes_is_dropped_not_typed() {
        let mut fonts = fonts();
        let answered = || {
            ViewUpdate::Snapshot(clackatoo_protocol::View {
                title: String::new(),
                cwd: None,
                root: clackatoo_protocol::Node::Chat(Box::default()),
            })
        };
        let mut view = asking();
        typed(&mut view, "1");
        view.apply(&answered());
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.editor.raw_text(), "", "never typed");
        // So too one given in normal mode.
        let mut view = asking();
        read_keys(&mut view, "1");
        step(&mut view, ARMED_FOR.start);
        read_keys(&mut view, "1");
        assert!(view.unsettled(), "given");
        view.apply(&answered());
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.editor.raw_text(), "");
    }

    #[test]
    fn what_typing_does_while_a_question_waits_is_said_in_the_bar() {
        let mut fonts = fonts();
        let mut view = asking();
        typed(&mut view, "x");
        view.settle(&mut fonts);
        assert_eq!(view.editor.raw_text(), "");
        assert_eq!(
            view.label().as_deref(),
            Some("a question waits: 1–2 answer it")
        );
        // Until the next key.
        typed(&mut view, "1");
        assert_eq!(view.label(), None);
        view.settle(&mut fonts);
        view.paste("pasted");
        view.settle(&mut fonts);
        assert_eq!(view.editor.raw_text(), "");
        assert!(
            view.label()
                .is_some_and(|said| said.contains("1–2 answer it"))
        );
    }

    fn with_settings() -> ChatView {
        use clackatoo_protocol::SettingChoice;
        let choice = |value: &str| SettingChoice {
            value: value.into(),
            name: value.into(),
            description: None,
        };
        let node = ChatNode {
            settings: vec![
                Setting {
                    id: "mode".into(),
                    name: "Mode".into(),
                    kind: SettingKind::Mode,
                    value: "auto".into(),
                    choices: vec![choice("default"), choice("plan"), choice("auto")],
                },
                Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: "haiku".into(),
                    choices: vec![choice("haiku"), choice("sonnet")],
                },
            ],
            ..ChatNode::default()
        };
        ChatView::new(String::new(), node)
    }

    fn set(setting: &str, value: &str) -> ChatAction {
        ChatAction::Set {
            setting: setting.into(),
            value: value.into(),
        }
    }

    #[test]
    fn shift_tab_steps_through_the_modes() {
        let mut fonts = fonts();
        let mut view = with_settings();
        press(
            &mut view,
            &Key::Named(NamedKey::Tab),
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        // From the last, round to the first.
        assert_eq!(view.settle(&mut fonts).actions, [set("mode", "default")]);
    }

    #[test]
    fn picking_another_agents_model_stages_a_handoff() {
        use clackatoo_protocol::{Provider, SettingChoice};
        let mut fonts = fonts();
        let mut view = with_settings();
        view.node.agent = "Claude Code".into();
        let luna = SettingChoice {
            value: "gpt-6-luna".into(),
            name: "6 Luna".into(),
            description: None,
        };
        view.node.providers = vec![
            Provider {
                agent: "codex".into(),
                title: "Codex".into(),
                models: ProviderModels::Known(vec![luna]),
            },
            Provider {
                agent: "gemini".into(),
                title: "Gemini CLI".into(),
                models: ProviderModels::NotInstalled,
            },
        ];
        // Every model known: nothing to ask the daemon.
        assert!(!view.open_menu(&[SettingKind::Model]));
        // Search across providers from Claude's page.
        typed(&mut view, "Luna");
        press(
            &mut view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers::default(),
        );
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.handoff.as_ref().unwrap().agent, "codex");
        assert_eq!(view.node.agent, "Claude Code");
        // An uninstalled provider can also be found globally.
        view.open_menu(&[SettingKind::Model]);
        typed(&mut view, "Gemini");
        press(
            &mut view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers::default(),
        );
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.handoff.as_ref().unwrap().agent, "gemini");
        // An agent whose models aren't known is asked for them as the
        // model menu opens, but not as the mode menu does.
        view.node.providers[0].models = ProviderModels::Unknown;
        assert!(view.open_menu(&[SettingKind::Model]));
        view.menu = None;
        assert!(!view.open_menu(&[SettingKind::Mode]));
    }

    #[test]
    fn a_pending_handoff_sends_once_and_restores_a_refused_draft() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.handoff = Some(handoff::Pending {
            agent: "codex".into(),
            model: Some("gpt-6-luna".into()),
            title: "Codex".into(),
            label: "6 Luna".into(),
        });
        view.paste_image(pasted_image());
        typed(&mut view, "continue");
        enter(&mut view, false);
        let actions = view.settle(&mut fonts).actions;
        assert!(
            matches!(&actions[..], [ChatAction::SendTo { agent, text, .. }] if agent == "codex" && text == "continue")
        );
        assert!(view.editor.raw_text().is_empty());
        assert!(
            matches!(&actions[0], ChatAction::SendTo { images, .. } if images == &[pasted_image()])
        );
        // A second Enter before the response cannot submit twice.
        typed(&mut view, "more");
        enter(&mut view, false);
        assert!(view.settle(&mut fonts).actions.is_empty());
        view.handoff_answer(clackatoo_protocol::Response::Error(
            clackatoo_protocol::Error::Other("busy".into()),
        ));
        assert_eq!(view.editor.raw_text(), "continue\nmore");
        assert_eq!(view.images, [pasted_image()]);
        assert!(view.handoff.is_some());
        // An old daemon refuses locally without clearing the recovered draft.
        view.handoff_supported = false;
        enter(&mut view, false);
        assert!(view.settle(&mut fonts).actions.is_empty());
        assert_eq!(view.editor.raw_text(), "continue\nmore");
        view.handoff_supported = true;
        enter(&mut view, false);
        assert_eq!(view.settle(&mut fonts).actions.len(), 1);
        view.handoff_answer(clackatoo_protocol::Response::Done);
        assert!(view.handoff.is_none());
        assert!(view.handoff_sent.is_none());
    }

    #[test]
    fn pending_handoff_can_be_cancelled_without_touching_the_draft() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.handoff = Some(handoff::Pending {
            agent: "codex".into(),
            model: None,
            title: "Codex".into(),
            label: "Codex".into(),
        });
        typed(&mut view, "my draft");
        view.settle(&mut fonts);
        let mut settled = Settled::default();
        view.choose("model".into(), "haiku".into(), &mut settled);
        assert!(view.handoff.is_none());
        assert_eq!(view.editor.raw_text(), "my draft");
        assert_eq!(settled.actions, [set("model", "haiku")]);
    }

    #[test]
    fn a_menu_is_picked_from_with_the_keyboard() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.open_menu(&[SettingKind::Model]);
        press(
            &mut view,
            &Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers::default(),
        );
        assert_eq!(view.settle(&mut fonts).actions, [set("model", "sonnet")]);
        assert!(view.menu.is_none());
        // Esc closes it, picking nothing; any other key closes it and
        // types.
        view.open_menu(&[SettingKind::Model]);
        press(
            &mut view,
            &Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        view.open_menu(&[SettingKind::Model]);
        typed(&mut view, "x");
        assert_eq!(view.settle(&mut fonts).actions, []);
        assert_eq!((view.menu.as_ref(), view.editor.raw_text()), (None, "x"));
    }

    #[test]
    fn the_pointer_is_a_hand_over_what_takes_a_click() {
        let mut view = with_settings();
        view.hits = vec![
            (Hit::Editor, [0.0, 0.0, 100.0, 20.0]),
            (Hit::Send, [100.0, 0.0, 20.0, 20.0]),
        ];
        assert_eq!(view.pointer([10.0, 10.0]), Pointer::Text);
        assert_eq!(view.pointer([110.0, 10.0]), Pointer::Hand);
        assert_eq!(view.pointer([10.0, 40.0]), Pointer::Arrow);
    }

    /// A chat with Claude Code's settings, in short: Opus, with a pinned
    /// version, and effort from low to max.
    fn with_claude() -> ChatView {
        use clackatoo_protocol::SettingChoice;
        let choice = |value: &str, name: &str| SettingChoice {
            value: value.into(),
            name: name.into(),
            description: None,
        };
        let node = ChatNode {
            settings: vec![
                Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: "opus".into(),
                    choices: vec![
                        choice("opus", "Opus 5.5"),
                        choice("claude-opus-4-8", "Opus 4.8"),
                    ],
                },
                Setting {
                    id: "effort".into(),
                    name: "Effort".into(),
                    kind: SettingKind::Effort,
                    value: "high".into(),
                    choices: vec![
                        choice("default", "Default"),
                        choice("low", "Low"),
                        choice("high", "High"),
                        choice("max", "Max"),
                    ],
                },
            ],
            ..ChatNode::default()
        };
        ChatView::new(String::new(), node)
    }

    fn key(view: &mut ChatView, named: NamedKey) {
        press(view, &Key::Named(named), None, Modifiers::default());
    }

    #[test]
    fn searching_and_pasting_in_the_picker_preserve_the_message() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.paste("keep this draft");
        view.settle(&mut fonts);
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        typed(&mut view, "OP");
        view.paste("US 4.8");
        press(
            &mut view,
            &Key::Character("c".into()),
            None,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        view.compose(Some(("composition".into(), None)));
        view.settle(&mut fonts);
        assert_eq!(view.menu.as_ref().unwrap().query, "OPUS 4.8");
        assert_eq!(view.editor.raw_text(), "keep this draft");
        enter(&mut view, false);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [set("model", "claude-opus-4-8")]
        );
        assert_eq!(view.editor.raw_text(), "keep this draft");
    }

    #[test]
    fn model_search_is_global_favorites_first_and_restores_the_page() {
        use clackatoo_protocol::{Provider, ProviderModels, SettingChoice};
        let mut view = with_claude();
        view.node.agent = "Claude Code".into();
        view.node.providers.push(Provider {
            agent: "codex".into(),
            title: "Codex".into(),
            models: ProviderModels::Known(vec![SettingChoice {
                value: "codex-model".into(),
                name: "Opus alternative".into(),
                description: None,
            }]),
        });
        view.open_menu(&[SettingKind::Model]);
        let provider = picker::Filter::Provider("Claude Code".into());
        assert_eq!(view.menu.as_ref().unwrap().filter, provider);
        let favorite = ("Codex".into(), "codex-model".into());
        view.menu.as_mut().unwrap().favorites.insert(favorite);
        assert_eq!(
            picker::Filter::initial(view.agent(), &view.menu.as_ref().unwrap().favorites),
            picker::Filter::Favorites
        );
        view.append_search("opus");
        view.search_changed();
        let rows = picker::rows(&view.offer(), view.menu.as_ref().unwrap());
        let choices: Vec<_> = rows
            .iter()
            .filter_map(|r| {
                if let Row::Choice(c) = r {
                    Some(c)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(choices[0].value, "codex-model");
        assert!(choices.iter().any(|c| c.value == "opus"));
        assert_eq!(view.menu.as_ref().unwrap().filter, provider);
        for _ in 0..4 {
            key(&mut view, NamedKey::Backspace);
        }
        view.settle(&mut fonts());
        assert!(view.menu.as_ref().unwrap().query.is_empty());
        let rows = picker::rows(&view.offer(), view.menu.as_ref().unwrap());
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Choice(c) if c.value == "codex-model"))
        );
    }

    #[test]
    fn provider_pages_keep_the_same_bounds_and_scroll_separately() {
        use clackatoo_protocol::{Provider, ProviderModels};
        let mut view = with_claude();
        view.node.providers = (0..20)
            .map(|i| Provider {
                agent: format!("agent-{i}"),
                title: format!("Agent {i}"),
                models: ProviderModels::Known(view.node.settings[0].choices.clone()),
            })
            .collect();
        // More than the viewport on the current provider, two on another,
        // and none in favorites: all three must keep identical bounds.
        view.node.settings[0]
            .choices
            .extend((0..20).map(|i| clackatoo_protocol::SettingChoice {
                value: format!("model-{i}"),
                name: format!("Model {i}"),
                description: None,
            }));
        let mut fonts = fonts();
        view.paste(&"A tall draft\n".repeat(12));
        view.settle(&mut fonts);
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        fixtures::paint(&mut view, &mut fonts);
        let bounds =
            |view: &ChatView, hit: &Hit| view.hits.iter().find(|(h, _)| h == hit).unwrap().1;
        let popup = bounds(&view, &Hit::MenuSurface);
        let chip = view
            .hits
            .iter()
            .find(|(h, _)| matches!(h, Hit::Chip(ids) if ids.contains(&"model".to_owned())))
            .unwrap()
            .1;
        assert!((4.0..12.0).contains(&(chip[1] - popup[1] - popup[3])));
        view.menu.as_mut().unwrap().filter = picker::Filter::Provider("Agent 1".into());
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(
            bounds(&view, &Hit::MenuSurface).map(f32::to_bits),
            popup.map(f32::to_bits)
        );
        view.menu.as_mut().unwrap().filter = picker::Filter::Favorites;
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(
            bounds(&view, &Hit::MenuSurface).map(f32::to_bits),
            popup.map(f32::to_bits)
        );
        let rail = bounds(&view, &Hit::ModelRail);
        view.hover([rail[0] + 2.0, rail[1] + 2.0]);
        for _ in 0..10 {
            view.scroll_lines(-1.0);
        }
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(view.menu.as_ref().unwrap().rail_offset, 10);
        assert_eq!(view.menu.as_ref().unwrap().offset, 0);
        assert!(
            view.hits
                .iter()
                .any(|(h, _)| *h == Hit::ModelFilter(picker::Filter::Provider("Agent 10".into())))
        );
        assert!(
            view.hits
                .iter()
                .filter(|(h, _)| matches!(h, Hit::ModelFilter(_)))
                .all(|(_, r)| r[1] >= rail[1] && r[1] + r[3] <= rail[1] + rail[3])
        );
    }

    #[test]
    fn fast_mode_uses_advertised_capability_and_keeps_the_picker_open() {
        use clackatoo_protocol::SettingChoice;
        let mut fonts = fonts();
        for id in ["fast", "fast-mode"] {
            let mut view = with_claude();
            view.node.settings.push(Setting {
                id: id.into(),
                name: "Fast mode".into(),
                kind: SettingKind::Other,
                value: "off".into(),
                choices: ["off", "on"]
                    .into_iter()
                    .map(|v| SettingChoice {
                        name: v.into(),
                        value: v.into(),
                        description: None,
                    })
                    .collect(),
            });
            view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
            for expected in ["on", "off"] {
                fixtures::paint(&mut view, &mut fonts);
                let rect = view
                    .hits
                    .iter()
                    .find(|(h, _)| *h == Hit::FastMode)
                    .unwrap()
                    .1;
                view.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
                assert_eq!(view.settle(&mut fonts).actions, [set(id, expected)]);
                assert!(view.menu.is_some());
            }
            view.node.settings.pop();
            fixtures::paint(&mut view, &mut fonts);
            assert!(!view.hits.iter().any(|(h, _)| *h == Hit::FastMode));
        }
    }

    #[test]
    fn unified_permissions_keep_their_individual_actions() {
        let mut fonts = fonts();
        let mut view = with_every_chip();
        fixtures::paint(&mut view, &mut fonts);
        let (hit, _) = view.hits.iter().find(|(hit, _)| matches!(hit, Hit::Chip(ids) if ids.contains(&"clackatoo.tools".to_owned()))).unwrap();
        let Hit::Chip(ids) = hit else { unreachable!() };
        assert_eq!(ids.len(), 3);
        let ids = ids.clone();
        view.toggle_menu(ids);
        let rows = picker::rows(&view.offer(), view.menu.as_ref().unwrap());
        assert!(
            rows.iter()
                .any(|row| matches!(row, Row::Choice(c) if c.setting == "clackatoo.browser"))
        );
    }

    #[test]
    fn a_long_picker_scrolls_without_moving_history_and_reveals_keyboard_choices() {
        let mut fonts = fonts();
        let mut view = with_settings();
        let model = view
            .node
            .settings
            .iter_mut()
            .find(|s| s.kind == SettingKind::Model)
            .unwrap();
        model.choices = (0..30)
            .map(|i| clackatoo_protocol::SettingChoice {
                name: format!("Model {i}"),
                value: format!("model-{i}"),
                description: None,
            })
            .collect();
        model.value = "model-0".into();
        view.open_menu(&[SettingKind::Model]);
        fixtures::paint(&mut view, &mut fonts);
        let scroll = view.scroll;
        for _ in 0..10 {
            view.scroll_lines(-1.0);
        }
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(view.scroll.to_bits(), scroll.to_bits());
        assert_eq!(view.menu.as_ref().unwrap().offset, 10);
        key(&mut view, NamedKey::ArrowUp);
        view.settle(&mut fonts);
        fixtures::paint(&mut view, &mut fonts);
        assert!(
            view.hits
                .iter()
                .any(|(hit, _)| matches!(hit, Hit::Pick { value, .. } if value == "model-29"))
        );
        let popup = view
            .hits
            .iter()
            .find(|(hit, _)| *hit == Hit::MenuSurface)
            .unwrap()
            .1;
        for (hit, rect) in &view.hits {
            if matches!(hit, Hit::Pick { .. } | Hit::Favorite(..)) {
                assert!(rect[1] >= popup[1] && rect[1] + rect[3] <= popup[1] + popup[3] + 0.1);
            }
        }
    }

    #[test]
    fn right_shows_a_model_familys_versions_to_pick_from() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        key(&mut view, NamedKey::ArrowRight);
        key(&mut view, NamedKey::ArrowDown);
        key(&mut view, NamedKey::Enter);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [set("model", "claude-opus-4-8")]
        );
    }

    #[test]
    fn left_and_right_slide_the_effort_and_leave_the_menu_open() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        // Up, from the model, round to the slider at the bottom.
        key(&mut view, NamedKey::ArrowUp);
        for _ in 0..2 {
            key(&mut view, NamedKey::ArrowRight);
        }
        key(&mut view, NamedKey::ArrowLeft);
        // Max, then nothing past it, then back to high: the slider stays
        // where it was put while the agent sets it.
        assert_eq!(
            view.settle(&mut fonts).actions,
            [set("effort", "max"), set("effort", "high")]
        );
        assert!(view.menu.is_some());
    }

    #[test]
    fn dragging_a_slider_sets_each_step_it_crosses() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Effort]);
        let step = |value: &str| Hit::Step {
            setting: "effort".into(),
            value: value.into(),
        };
        view.hits = vec![
            (step("low"), [0.0, 0.0, 10.0, 10.0]),
            (step("high"), [10.0, 0.0, 10.0, 10.0]),
            (step("max"), [20.0, 0.0, 10.0, 10.0]),
        ];
        view.press([5.0, 5.0], 1, false);
        view.drag([12.0, 40.0]);
        // Past the end: the last step.
        view.drag([90.0, 5.0]);
        view.release();
        view.drag([5.0, 5.0]);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [
                set("effort", "low"),
                set("effort", "high"),
                set("effort", "max")
            ]
        );
    }

    #[test]
    fn clicking_a_card_opens_it_and_clicking_again_folds_it() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.hits = vec![(Hit::Card(0, "t".into()), [0.0, 0.0, 100.0, 50.0])];
        assert_eq!(view.pointer([10.0, 10.0]), Pointer::Hand);
        view.press([10.0, 10.0], 1, false);
        view.settle(&mut fonts);
        assert!(view.open_cards.contains("t"));
        view.press([10.0, 10.0], 1, false);
        view.settle(&mut fonts);
        assert!(view.open_cards.is_empty());
    }

    /// A chat whose turns changed files, made or undone, as `states` say.
    fn with_files(states: &[clackatoo_protocol::TurnFilesState]) -> ChatView {
        use clackatoo_protocol::{Outcome, TurnEnd, TurnFiles};
        let mut node = ChatNode::default();
        for &state in states {
            node.turns.push(Turn {
                previews: Vec::new(),
                from: None,
                prompt: "change it".into(),
                sent: 0,
                items: Vec::new(),
                end: Some(TurnEnd {
                    at: 0,
                    outcome: Outcome::Done,
                }),
                files: Some(TurnFiles { count: 2, state }),
                switch: None,
            });
        }
        ChatView::new(String::new(), node)
    }

    #[test]
    fn a_turn_that_changed_files_has_a_button_to_undo_it() {
        use clackatoo_protocol::TurnFilesState::{Made, Undone};
        let mut fonts = fonts();
        let mut view = with_files(&[Undone, Made]);
        view.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let button = |view: &ChatView, wanted: &Hit| {
            let (_, [x, y, width, height]) = (view.hits.iter())
                .find(|(hit, _)| hit == wanted)
                .expect("the button is drawn");
            [x + width / 2.0, y + height / 2.0]
        };
        let at = button(&view, &Hit::Undo(1));
        assert_eq!(view.pointer(at), Pointer::Hand);
        view.press(at, 1, false);
        let undo = ChatAction::Undo {
            turn: 1,
            force: false,
        };
        assert_eq!(view.settle(&mut fonts).actions, [undo]);
        let at = button(&view, &Hit::Redo(0));
        view.press(at, 1, false);
        let redo = ChatAction::Redo {
            turn: 0,
            force: false,
        };
        assert_eq!(view.settle(&mut fonts).actions, [redo]);
    }

    /// The middle of where `wanted` was drawn, if it was.
    fn drawn_at(view: &ChatView, wanted: &Hit) -> Option<[f32; 2]> {
        let (_, [x, y, width, height]) = view.hits.iter().find(|(hit, _)| hit == wanted)?;
        Some([x + width / 2.0, y + height / 2.0])
    }

    /// Somewhere over turn `turn`'s text, as last drawn.
    fn over_turn(view: &ChatView, turn: usize) -> [f32; 2] {
        let shown = (view.shown.iter())
            .find(|shown| shown.key.turn == turn)
            .expect("the turn's text");
        [shown.at[0] + 1.0, shown.at[1] + shown.size[1] / 2.0]
    }

    #[test]
    fn the_button_under_an_answer_shows_only_while_the_mouse_is_over_its_turn() {
        let mut fonts = fonts();
        let node = fixtures::talk(&[("one", "first"), ("two", "second")]);
        let mut view = ChatView::new(String::new(), node);
        fixtures::paint(&mut view, &mut fonts);
        // Not at first: a long chat isn't a column of them.
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(0)), None);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(1)), None);
        // Over the first turn, its button, and only its.
        assert!(view.hover(over_turn(&view, 0)));
        fixtures::paint(&mut view, &mut fonts);
        let button = drawn_at(&view, &Hit::CopyAnswer(0)).expect("the button");
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(1)), None);
        // It stays over the button itself, and moving within the turn
        // draws nothing again.
        assert!(!view.hover(button));
        assert!(!view.hover(over_turn(&view, 0)));
        // Over the second, it goes with the mouse.
        assert!(view.hover(over_turn(&view, 1)));
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(0)), None);
        assert!(drawn_at(&view, &Hit::CopyAnswer(1)).is_some());
        // Its place is kept while it's hidden: the turns don't move.
        let tops: Vec<f32> = view.shown.iter().map(|shown| shown.at[1]).collect();
        assert!(view.unhover());
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(1)), None);
        let again: Vec<f32> = view.shown.iter().map(|shown| shown.at[1]).collect();
        assert_eq!(tops, again);
        // Nor over the box, below the conversation.
        assert!(!view.hover([1.0, view.view_height + 5.0]));
    }

    #[test]
    fn the_button_follows_a_still_mouse_as_the_conversation_scrolls_under_it() {
        let mut fonts = fonts();
        let pairs: Vec<(String, String)> = (0..30)
            .map(|turn| (format!("question {turn}"), format!("answer {turn}")))
            .collect();
        let pairs: Vec<(&str, &str)> = (pairs.iter())
            .map(|(prompt, answer)| (prompt.as_str(), answer.as_str()))
            .collect();
        let mut view = ChatView::new(String::new(), fixtures::talk(&pairs));
        fixtures::paint(&mut view, &mut fonts);
        // The mouse rests over a turn in the middle of the view.
        let shown: Vec<usize> = (view.shown.iter()).map(|shown| shown.key.turn).collect();
        let first = shown[shown.len() / 2];
        let at = over_turn(&view, first);
        view.hover(at);
        fixtures::paint(&mut view, &mut fonts);
        assert!(drawn_at(&view, &Hit::CopyAnswer(first)).is_some());
        // The wheel takes the conversation up under it, by three turns'
        // worth: the button goes with the turn now under the mouse.
        let height = |turn: usize| over_turn(&view, turn)[1];
        let step = height(first) - height(first - 3);
        assert!(view.scroll_by(-step));
        fixtures::paint(&mut view, &mut fonts);
        let under = view.turn_to_copy_at(at).expect("a turn under the mouse");
        assert_ne!(under, first);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(first)), None);
        assert!(drawn_at(&view, &Hit::CopyAnswer(under)).is_some());
    }

    #[test]
    fn a_held_button_shows_no_turns_button_as_it_drags_over_them() {
        let mut fonts = fonts();
        let node = fixtures::talk(&[("one", "first answer"), ("two", "second answer")]);
        let mut view = ChatView::new(String::new(), node);
        fixtures::paint(&mut view, &mut fonts);
        let at = over_turn(&view, 0);
        view.hover(at);
        fixtures::paint(&mut view, &mut fonts);
        assert!(drawn_at(&view, &Hit::CopyAnswer(0)).is_some());
        // Pressed on the answer's text to select it, and dragged on: no
        // button shows, here or under the drag.
        view.press(at, 1, false);
        view.settle(&mut fonts);
        view.drag(over_turn(&view, 1));
        view.settle(&mut fonts);
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(0)), None);
        assert_eq!(drawn_at(&view, &Hit::CopyAnswer(1)), None);
        view.release();
        view.settle(&mut fonts);
        // Moved again with nothing held, it's back.
        view.hover(over_turn(&view, 1));
        fixtures::paint(&mut view, &mut fonts);
        assert!(drawn_at(&view, &Hit::CopyAnswer(1)).is_some());
    }

    #[test]
    fn the_button_under_an_answer_copies_it_as_y_does() {
        let mut fonts = fonts();
        let node = fixtures::talk(&[("one", "**bold** answer"), ("two", "second")]);
        let mut view = ChatView::new(String::new(), node);
        fixtures::paint(&mut view, &mut fonts);
        view.hover(over_turn(&view, 0));
        fixtures::paint(&mut view, &mut fonts);
        let at = drawn_at(&view, &Hit::CopyAnswer(0)).expect("the button");
        assert_eq!(view.pointer(at), Pointer::Hand);
        // Under the answer, on the left.
        let answer = (view.shown.iter())
            .find(|shown| shown.key.turn == 0 && shown.key.piece == paint::ITEMS)
            .expect("the answer");
        assert!(at[1] > answer.at[1] + answer.size[1]);
        view.press(at, 1, false);
        view.release();
        let settled = view.settle(&mut fonts);
        assert_eq!(
            settled.copied,
            [("**bold** answer".to_owned(), Copy::Clipboard)]
        );
        // It says so until the mouse leaves it.
        assert_eq!(view.copied_answer, Some(0));
        fixtures::paint(&mut view, &mut fonts);
        assert!(drawn_at(&view, &Hit::CopyAnswer(0)).is_some(), "Copied");
        // The press let go of the hover; moving over it again takes it
        // back, and still says it copied.
        view.hover(at);
        assert!(!view.hover(at));
        assert_eq!(view.copied_answer, Some(0));
        assert!(view.hover([1.0, 1.0]));
        assert_eq!(view.copied_answer, None);
    }

    #[test]
    fn only_a_finished_turn_ending_in_words_has_the_button() {
        use clackatoo_protocol::{Item, Tool, ToolKind, ToolStatus};

        let mut fonts = fonts();
        let mut node = fixtures::talk(&[
            ("one", "first"),
            ("two", "second"),
            ("three", ""),
            ("four", "shown"),
        ]);
        // Ending with a tool: nothing to copy.
        node.turns[1].items.push(Item::Tool(Tool {
            id: "t".into(),
            title: "Read a.rs".into(),
            status: ToolStatus::Done,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
        }));
        // Still going.
        node.turns[0].end = None;
        let mut view = ChatView::new(String::new(), node);
        fixtures::paint(&mut view, &mut fonts);
        for turn in 0..3 {
            view.hover(over_turn(&view, turn));
            fixtures::paint(&mut view, &mut fonts);
            assert_eq!(drawn_at(&view, &Hit::CopyAnswer(turn)), None, "{turn}");
        }
        // Done, ending in words: it has.
        view.hover(over_turn(&view, 3));
        fixtures::paint(&mut view, &mut fonts);
        assert!(drawn_at(&view, &Hit::CopyAnswer(3)).is_some());
    }

    #[test]
    fn the_button_shares_its_line_with_what_the_turn_changed_in_files() {
        use clackatoo_protocol::Item;
        use clackatoo_protocol::TurnFilesState::Made;

        let mut fonts = fonts();
        let mut view = with_files(&[Made]);
        view.node.turns[0].items.push(Item::Words("Done.".into()));
        let mut alone = with_files(&[Made]);
        view.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        view.hover(over_turn(&view, 0));
        view.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        alone.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let copy = drawn_at(&view, &Hit::CopyAnswer(0)).expect("the button");
        let undo = drawn_at(&view, &Hit::Undo(0)).expect("Undo");
        assert!((copy[1] - undo[1]).abs() < 1.0);
        assert!(copy[0] > undo[0]);
        // Hidden or absent Copy never leaves a gap before the summary.
        assert_eq!(drawn_at(&alone, &Hit::CopyAnswer(0)), None);
        assert!((undo[0] - drawn_at(&alone, &Hit::Undo(0)).unwrap()[0]).abs() < 0.01);
        // Narrow, the line is laid out in what's left of the column.
        view.paint(&mut fonts, [0.0, 0.0], [220.0, 800.0], false);
        let undo = (view.hits.iter()).find(|(hit, _)| *hit == Hit::Undo(0));
        let (_, undo) = undo.expect("Undo, narrow");
        assert!(
            undo[0] + undo[2] <= view.column_right + 1.0,
            "{undo:?} {}",
            view.column_right
        );
    }

    #[test]
    fn u_takes_the_last_turn_made_and_ctrl_r_the_first_undone() {
        use clackatoo_protocol::TurnFilesState::{Made, TooOld, Undone};
        let view = with_files(&[TooOld, Made, Made, Undone, Undone]);
        assert_eq!(view.turn_to_undo(false), Some(2));
        assert_eq!(view.turn_to_undo(true), Some(3));
        let view = with_files(&[TooOld]);
        assert_eq!(view.turn_to_undo(false), None);
        assert_eq!(view.turn_to_undo(true), None);
    }

    #[test]
    fn a_menu_over_a_card_lights_up_under_the_mouse() {
        let mut view = with_settings();
        view.open_menu(&[SettingKind::Model]);
        view.hits = vec![
            (Hit::Card(0, "t".into()), [0.0, 0.0, 100.0, 100.0]),
            (
                Hit::Pick {
                    setting: "model".into(),
                    value: "sonnet".into(),
                },
                [10.0, 10.0, 50.0, 20.0],
            ),
        ];
        assert!(view.hover([20.0, 20.0]));
        assert_eq!(
            view.menu.unwrap().highlighted,
            Some(Spot::Choice {
                setting: "model".into(),
                value: "sonnet".into()
            })
        );
    }

    /// A chat with its agent's mode and model, and the daemon's own Tools
    /// and Browser settings, as every chat has them, that fills 84k of
    /// 200k tokens of context, with its account's limit over five hours
    /// 91% used.
    pub(crate) fn with_every_chip() -> ChatView {
        use clackatoo_protocol::SettingChoice;
        let mut view = with_settings();
        let own = |id: &str, name: &str, kind, value: &str, values: &[&str]| Setting {
            id: id.into(),
            name: name.into(),
            kind,
            value: value.into(),
            choices: (values.iter())
                .map(|value| SettingChoice {
                    value: value.to_lowercase(),
                    name: (*value).into(),
                    description: None,
                })
                .collect(),
        };
        view.node.settings.extend([
            own(
                "clackatoo.tools",
                "Tools",
                SettingKind::Tools,
                "ask",
                &["Ask", "Allow", "Off"],
            ),
            own(
                "clackatoo.browser",
                "Browser",
                SettingKind::Browser,
                "off",
                &["Off", "On"],
            ),
        ]);
        view.node.usage = Some(clackatoo_protocol::Usage {
            used: 84_000,
            size: 200_000,
            five_hour: Some(clackatoo_protocol::Limit {
                used: 91,
                resets: None,
            }),
            weekly: None,
        });
        view
    }

    #[test]
    fn context_gauge_fits_beside_send_at_every_width() {
        let mut fonts = fonts();
        let mut view = with_every_chip();
        let overlaps = |a: [f32; 4], b: [f32; 4]| {
            a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
        };
        let mut widths = Vec::new();
        for width in [
            900.0, 720.0, 680.0, 640.0, 600.0, 560.0, 480.0, 420.0, 340.0, 260.0,
        ] {
            view.paint(&mut fonts, [0.0; 2], [width, 800.0], false);
            let meter = (view.hits.iter())
                .find(|(hit, _)| *hit == Hit::Usage)
                .map(|(_, rect)| *rect)
                .expect("the meter, its bar at least");
            for (hit, rect) in &view.hits {
                if matches!(hit, Hit::Chip(_) | Hit::Send | Hit::Stop) {
                    assert!(
                        !overlaps(meter, *rect),
                        "{width}: {hit:?} {rect:?} {meter:?}"
                    );
                }
            }
            let chips = (view.hits.iter())
                .filter(|(hit, _)| matches!(hit, Hit::Chip(_)))
                .count();
            widths.push((meter[2], chips));
        }
        // Model and permissions stay available without overlapping the gauge.
        assert!(widths.iter().all(|&(_, chips)| chips == 2), "{widths:?}");
        assert!(
            widths.windows(2).all(|pair| pair[1].0 <= pair[0].0),
            "{widths:?}"
        );
    }

    #[test]
    fn the_meters_detail_shows_while_hovered_and_once_clicked_or_asked_for_with_k() {
        use paint::Detail::{Asked, Hovered};

        let mut fonts = fonts();
        let mut view = with_settings();
        // No usage said yet: no meter, and `K` says why.
        fixtures::paint(&mut view, &mut fonts);
        assert_eq!(drawn_at(&view, &Hit::Usage), None);
        fixtures::read(&mut view, "K");
        assert_eq!(view.detail, None);
        assert!(view.said.is_some());
        view.node.usage = Some(clackatoo_protocol::Usage {
            used: 84_000,
            size: 200_000,
            five_hour: None,
            weekly: None,
        });
        fixtures::paint(&mut view, &mut fonts);
        let meter = drawn_at(&view, &Hit::Usage).expect("the meter");
        // The mouse over it, and off it.
        assert!(view.hover(meter));
        assert_eq!(view.detail, Some(Hovered));
        assert!(!view.hover([meter[0], meter[1] + 1.0]), "still over it");
        assert!(view.hover([1.0, 1.0]));
        assert_eq!(view.detail, None);
        assert!(view.hover(meter));
        assert!(view.unhover());
        assert_eq!(view.detail, None);
        // Clicked, it stays after the mouse leaves, until a click
        // elsewhere.
        view.press(meter, 1, false);
        view.settle(&mut fonts);
        assert_eq!(view.detail, Some(Asked));
        assert!(!view.hover([1.0, 1.0]));
        assert_eq!(view.detail, Some(Asked));
        view.press([1.0, 1.0], 1, false);
        view.settle(&mut fonts);
        assert_eq!(view.detail, None);
        // `K` shows it until `K` again, or any other key.
        assert_eq!(fixtures::read(&mut view, "K"), (true, false));
        assert_eq!(view.detail, Some(Asked));
        fixtures::read(&mut view, "K");
        assert_eq!(view.detail, None);
        fixtures::read(&mut view, "K");
        assert_eq!(fixtures::read(&mut view, "j"), (true, false));
        assert_eq!(view.detail, None);
        // `K` hiding what a click showed ends a two-key command half
        // typed, as any key does.
        fixtures::read(&mut view, "g");
        assert!(view.keys().pending());
        view.detail = Some(Asked);
        fixtures::read(&mut view, "K");
        assert_eq!(view.detail, None);
        assert!(!view.keys().pending());
    }

    #[test]
    fn k_shows_the_meters_detail_in_normal_mode_only() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), fixtures::talk(&[("one", "first answer")]));
        view.node.usage = with_every_chip().node.usage;
        view.reading = true;
        fixtures::paint(&mut view, &mut fonts);
        fixtures::read(&mut view, "v");
        assert!(view.visual().is_some());
        fixtures::read(&mut view, "K");
        assert_eq!(view.detail, None);
        assert!(view.visual().is_some(), "still selecting");
        fixtures::read_named(&mut view, NamedKey::Escape);
        fixtures::read(&mut view, "K");
        assert_eq!(view.detail, Some(paint::Detail::Asked));
    }
    #[test]
    fn usage_modal_keeps_draft_and_owns_keys_scroll_refresh_and_reconnect() {
        let mut view = ChatView::new("chat".into(), fixtures::talk(&[("hello", "answer")]));
        let mut fonts = fonts();
        view.usage_supported = true;
        view.paste("keep this draft");
        view.settle(&mut fonts);
        fixtures::paint(&mut view, &mut fonts);
        let meter = drawn_at(&view, &Hit::Usage).expect("gauge even before context is reported");
        view.press(meter, 1, false);
        let opened = view.settle(&mut fonts);
        assert!(view.usage_modal);
        assert!(
            opened
                .actions
                .iter()
                .any(|a| matches!(a, ChatAction::RefreshUsage { force: true }))
        );
        for index in 0..12 {
            view.node
                .provider_usage
                .push(clackatoo_protocol::ProviderUsage {
                    agent: "claude".into(),
                    title: format!("Provider {index}"),
                    ..Default::default()
                });
        }
        fixtures::paint(&mut view, &mut fonts);
        assert!(view.usage_max_scroll > 0.0);
        assert!(view.modal_key(&Key::Named(NamedKey::PageDown), false));
        assert!(view.usage_scroll > 0.0);
        assert!(view.modal_key(&Key::Named(NamedKey::Enter), false));
        assert_eq!(view.editor.text(), "keep this draft");
        let refresh = drawn_at(&view, &Hit::RefreshUsage).unwrap();
        view.press(refresh, 1, false);
        assert!(
            view.settle(&mut fonts)
                .actions
                .iter()
                .any(|a| matches!(a, ChatAction::RefreshUsage { force: true }))
        );
        view.set_offline(true);
        view.set_offline(false);
        assert!(
            view.settle(&mut fonts)
                .actions
                .iter()
                .any(|a| matches!(a, ChatAction::RefreshUsage { force: true }))
        );
        assert!(view.modal_key(&Key::Named(NamedKey::Escape), false));
        assert!(!view.viewing_modal());
        view.toggle_detail();
        assert!(view.modal_key(&Key::Character(";".into()), true));
        assert!(!view.viewing_modal());
        assert_eq!(view.editor.text(), "keep this draft");
    }
}
