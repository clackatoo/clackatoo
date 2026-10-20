//! What the window shows of the daemon's workspaces, and what keys do to
//! them: which workspace, tab and pane are shown, the panes' terminals,
//! the command line, and the messages for the daemon. It draws nothing
//! and does no I/O, so tests can drive it as a window would.
//!
//! Every terminal of the workspace shown is attached, including those in
//! tabs not shown: each sends one update, which waits undrawn and
//! unacknowledged, so the daemon sends no more until the tab is shown, and
//! then it shows at once. Panes of other workspaces keep their state, such
//! as their scroll, for when their workspace is shown again.
//!
//! The window has one mode, which every pane it arrives at takes (see
//! [`mode`]).

use std::collections::{HashMap, HashSet, VecDeque};

use clackatoo_protocol::{
    Axis, BufferId, ClientMessage, Content, FileLocation, Layout, NewAgent, NewTerminal, PaneId,
    PaneKind, PaneTerminal, Request, Response, Side, Size, Tab, TabId, TerminalId, ViewUpdate,
    WorkspaceId, Workspaces, wire,
};
use clackatoo_term::mouse::{MouseAction, MouseButton};
use winit::keyboard::{Key, NamedKey};

use crate::browser::BrowserPane;
use crate::chrome::{ModeName, NoteKind, Status, WorkspaceName};
use crate::cmdline;
use crate::command::{Command, Kind, Place};
use crate::finder::Finder;
use crate::input::{self, KeptPress, KeyPress, Modifiers};
use crate::keymap::{Chord, Pressed};
use crate::layout::{Area, Direction, Divider, Placement};
use crate::options::Options;
use crate::other::{OtherPane, Why};
use crate::pane::{self, Copy, Mode, Pane, PaneMouse, Selection};
use bindings::Binding;
pub(crate) use updating::UpdateAsked;

/// What a mouse event did, for the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MouseOutcome {
    /// The window must be redrawn.
    pub(crate) redraw: bool,
    /// The window is to paste the primary selection into the focused pane.
    pub(crate) paste_primary: bool,
}

/// What the window shows: a workspace, one of its tabs, and the pane in
/// that tab with the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) workspace: WorkspaceId,
    pub(crate) tab: TabId,
    pub(crate) pane: PaneId,
}

/// Where typed text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextTarget {
    /// The focused pane's program, in insert mode.
    Pane,
    /// The bottom bar: the command line, or a search being typed.
    Bar,
}

/// Who sent a request, and so gets its answer.
#[derive(Clone, Debug)]
enum Asker {
    /// A pane, which keeps track of its own requests.
    Pane(TerminalId),
    /// The window itself.
    Window(Asked),
}

/// What the window asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    /// A new pane, to switch to once made.
    Pane,
    /// Something typed on the command line, whose failure is shown.
    Command,
    /// Something done for the window's own sake, whose failure is logged.
    Quiet,
    /// A split resized, whose shares are shown ahead of the daemon's until
    /// it has answered.
    Resize,
    /// A pane made for the config's Lua, which waits for it by this
    /// request's number.
    Lua(u64),
    /// A file written, and the pane to close once it is, as after `:wq`.
    Saved {
        buffer: BufferId,
        then_close: Option<PaneId>,
    },
    /// A file opened in place of the one this pane showed, whose failure
    /// is shown, and forgets where its cursor was to go in that file.
    OpenedHere(PaneId),
    /// A new pane showing a file, to switch to once made, with its cursor
    /// put at this place once the file's text has come.
    PaneAt(Option<FileLocation>),
    /// A new file explorer, to switch to once made, showing the file at
    /// `reveal` once it can, and editing a folder as text if `edit`.
    Explorer { reveal: Option<String>, edit: bool },
    /// A new tab's pane, showing a buffer with no file yet for `:e` alone,
    /// to switch to once made, with an explorer of this folder beside it.
    Blank(Option<String>),
    /// Something for the tutor's lesson.
    Lesson(lessons::Serial, lessons::LessonAsked),
    /// Something asked of reviewing, whose answer is said in the bar.
    Reviewed,
    /// `:recover`, whose answer is said in the bar, or is a new tab's
    /// pane, to switch to.
    Recovered,
    /// `:PortForward` in a workspace from another machine, whose answer
    /// is said in the bar.
    Forwarded,
    /// Something asked about the devices paired with the web app, whose
    /// answer is said in the bar.
    Devices,
    /// What a new chat could be, for the start page.
    ChatChoices,
    /// The chats, every one, to mark the tabs and workspaces with what
    /// they're doing.
    Chats,
    /// The workspaces, asked for again after a newer daemon sent them as
    /// this window couldn't read.
    Workspaces,
    /// A terminal attached to again after a newer daemon sent a change to
    /// its view as this window couldn't read: refused, no snapshot comes.
    Reattached(TerminalId),
    /// The screen of a background tab's terminal, for the list's preview
    /// of the tab.
    BackgroundPreview(TabId),
}

/// What the window attaches to again, or asks for again, when a newer
/// daemon sends something about it the window can't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Missed {
    View(TerminalId),
    Buffer(BufferId),
    Workspaces,
}

/// How many times a minute the window asks again for one thing it missed,
/// before it takes it that it will only miss it again.
const MOST_MISSED: usize = 5;

/// Why a pane, tab or workspace showing a buffer with no file yet that has
/// changes isn't closed: they'd go with it.
const BLANK_KEPT: &str = "[No Name] has changes: :w NAME writes them, :e! in it drops them";

/// The time those are counted over.
const MISSED_OVER: std::time::Duration = std::time::Duration::from_mins(1);

mod background;
mod badges;
mod bindings;
mod boards;
mod browsers;
mod clipboard;
mod colours;
mod connecting;
mod devices;
mod editors;
mod elsewhere;
mod explorers;
mod finding;
mod formatting;
mod jumps;
mod lessons;
mod links;
mod macros;
mod markdown;
mod messages;
mod mode;
mod offers;
mod restored;
pub(crate) use restored::Offer;
mod reviews;
mod script;
mod settings;
mod starting;
mod typing;
mod updating;

pub(crate) use mode::WindowMode;
pub(crate) use script::LuaStart;

/// Something being typed in the palette.
#[derive(Debug)]
struct Input {
    purpose: Purpose,
    line: cmdline::Line,
    /// Which of the choices listed under it is picked, if it lists any.
    selected: usize,
    /// The word Tab is completing, and what it could be, until another
    /// key.
    completing: Option<cmdline::complete::Completion>,
}

impl Input {
    /// `text` typed for `purpose`, with the caret at its end.
    fn new(purpose: Purpose, text: impl Into<String>) -> Self {
        Self {
            purpose,
            line: cmdline::Line::new(text),
            selected: 0,
            completing: None,
        }
    }

    /// A new name for `purpose`, starting as the old one, `was`, all of
    /// it selected, so a name typed replaces it and an arrow keeps it.
    fn renaming(purpose: Purpose, was: impl Into<String>) -> Self {
        Self {
            line: cmdline::Line::selected(was),
            ..Self::new(purpose, String::new())
        }
    }
}

/// What's typed in the palette is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Purpose {
    /// A command, after `:`.
    Command,
    /// A workspace's new name.
    RenameWorkspace(WorkspaceId),
    /// A tab's new name: empty for none.
    RenameTab(TabId),
    /// A workspace to switch to, or the name of one to make.
    Workspace,
    /// A comment on a line, for the agent of chat `chat`.
    Comment {
        chat: TerminalId,
        place: clackatoo_protocol::CommentPlace,
    },
}

impl Purpose {
    /// What the palette says it's for, above what's typed, and the
    /// prefix written before it.
    fn label(&self) -> (Option<String>, &'static str) {
        let label = match self {
            Self::Command => return (None, ":"),
            Self::RenameWorkspace(_) => "Rename workspace".to_owned(),
            Self::RenameTab(_) => "Rename tab".to_owned(),
            Self::Workspace => "Workspace".to_owned(),
            Self::Comment { place, .. } => {
                let file = place.path.rsplit('/').next().unwrap_or(&place.path);
                format!("Comment on {file}:{}", place.line)
            }
        };
        (Some(label), "")
    }
}

/// How long a pane stays lit when it turns down a key it can't send.
const REFUSED_FOR: std::time::Duration = std::time::Duration::from_millis(300);

/// How long a pane an agent points out stays lit.
const POINTED_FOR: std::time::Duration = std::time::Duration::from_millis(1500);

/// The window's view of the daemon's workspaces.
#[derive(Debug, Default)]
#[allow(clippy::struct_excessive_bools, reason = "each is so or not")]
pub(crate) struct Desk {
    /// The daemon's workspaces, as last heard.
    workspaces: Workspaces,
    /// The background tab whose screen the list of them last asked for,
    /// and when (see [`background`]).
    background_preview: Option<(TabId, std::time::Instant)>,
    /// The background tab still running that Ctrl+D in the list was
    /// pressed on once, which a second press dismisses.
    dismissing: Option<TabId>,
    /// The failures of background tabs the list said were seen as it
    /// opened, by tab, which it goes on saying while it's open.
    background_seen: Vec<(TabId, i32)>,
    /// The TCP ports each background tab's programs listen on, as the
    /// daemon last said, while the list of them is open.
    background_ports: Vec<clackatoo_protocol::Listening>,
    /// The pane an agent last pointed out, and until when it's lit.
    pointed: Option<(PaneId, std::time::Instant)>,
    /// The chats, as a list, to mark the tabs and workspaces with.
    watched: badges::Watched,
    /// What's shown, once connected.
    shown: Option<Shown>,
    /// Panes by the terminal each shows: those of the workspace shown, and
    /// of others shown before.
    panes: HashMap<TerminalId, Pane>,
    /// Whether there's a connection to send messages on.
    connected: bool,
    /// Whether the daemon is on another machine, which makes terminals
    /// guess at the echo of what's typed (see `pane::predict`).
    remote: bool,
    /// This machine's home directory, for a daemon on it too old to say
    /// its own (see [`Workspaces::home`]).
    own_home: Option<String>,
    /// The terminals attached to on this connection.
    attached: HashSet<TerminalId>,
    /// Who sent each request not yet answered, oldest first: answers come
    /// in the order requests were sent.
    asked: VecDeque<Asker>,
    /// Messages for the daemon, oldest first, besides the panes' own.
    outbox: Vec<ClientMessage>,
    /// The chats that asked for their code's colours, by the number of the
    /// ask not yet answered, and the number of the last.
    colour_asks: HashMap<u64, TerminalId>,
    next_colour: u64,
    /// The editors whose rendered Markdown asked for its code's colours, by
    /// the number of the ask not yet answered.
    rendered_colour_asks: HashMap<u64, PaneId>,
    /// The space the panes' tiles share, once the window is open.
    area: Option<Area>,
    /// What's being typed in the palette, such as a command after `:`.
    input: Option<Input>,
    /// The commands and searches typed before, for Up and Down.
    recall: typing::Recalling,
    /// Folders listed for Tab to complete a path with.
    listings: typing::Listings,
    /// A count typed before the binding being carried out, as `5:` gives
    /// an editor's command line `.,.+4`.
    bound_count: Option<usize>,
    /// A message for the focused pane's label, until the next key, such as
    /// why a command failed.
    message: Option<String>,
    /// What's been said lately, for `:messages`, and whether the message
    /// is a warning.
    said: messages::Said,
    /// The daemon's offer to install a language, asked in the bar until
    /// the next key.
    offer: Option<offers::Offer>,
    /// The devices asking to pair with the daemon's web app, asked about
    /// in the bar until a window answers.
    pairing: clackatoo_protocol::PairingState,
    /// When the daemon was last told the window's user is there, while
    /// they are.
    present_said: Option<std::time::Instant>,
    /// Text copied, waiting for the window to put on the clipboard or the
    /// primary selection, oldest first.
    yanked: Vec<(String, Copy)>,
    /// URLs links asked to open, waiting for the window to hand to the
    /// desktop's opener.
    opened: Vec<String>,
    /// The terminal whose pane a mouse button went down in, and the
    /// button: it gets the drag, wherever the pointer goes.
    dragging: Option<(TerminalId, MouseButton)>,
    /// The editor whose pane the left button went down in: it gets the
    /// drag, wherever the pointer goes, until the button comes up.
    editor_drag: Option<PaneId>,
    /// The cell the mouse was last over, so motion is passed on once per
    /// cell, not once per pixel.
    mouse_cell: Option<(TerminalId, [i64; 2])>,
    /// The window doesn't have the focus, so no pane does.
    unfocused: bool,
    /// The divider being dragged, as it was when grabbed.
    dragging_divider: Option<Divider>,
    /// Shares given to a split of a tab here, shown ahead of the daemon's
    /// while a divider is dragged and until the daemon has answered every
    /// resize sent, so the divider never jumps back.
    split_shares: Option<(TabId, Vec<u16>, Vec<u32>)>,
    /// Resizes sent and not answered yet.
    resizes_pending: usize,
    /// Every workspace has closed, so the window should too.
    finished: bool,
    /// What keys are bound to, and those waiting for the rest of a
    /// binding, as after Ctrl+B.
    keymap: bindings::Keys,
    /// The config's Lua, if there's a config.
    script: script::Script,
    /// Panes editing files.
    editors: editors::Editors,
    /// Panes showing file explorers.
    explorers: explorers::Explorers,
    /// Panes showing boards.
    boards: boards::Boards,
    /// Panes showing chats' pages.
    browsers: browsers::Browsers,
    /// Panes of kinds this window doesn't know, which a newer daemon has.
    others: HashMap<PaneId, OtherPane>,
    /// Reviews of what agents changed.
    reviews: reviews::Reviews,
    /// The macros being typed, and what's noted of keys pressed for the
    /// one being recorded.
    macros: macros::Macros,
    /// Where you've been, to go back to.
    jumps: jumps::Jumps,
    /// The options, as config and `:set` have set them.
    options: Options,
    /// The options as the window started: their defaults, but for the font
    /// it was told to draw with. Config run again starts from these.
    start_options: Options,
    /// The options the command line set, or the environment, which config
    /// doesn't change.
    fixed: Vec<&'static crate::options::Info>,
    /// The `font` and `fontsize` the window draws text in, as last put
    /// into its fonts.
    font_in_use: settings::FontInUse,
    /// The finder, while it's open.
    finder: Option<Finder>,
    /// The start page `:Agent` opens, while it's open.
    start: Option<crate::start::StartPage>,
    /// What the last new chat started from the start page had, for the
    /// next to start with.
    last_chat: crate::start::LastChat,
    /// How many searches the window has asked the daemon for, which
    /// numbers them.
    searches: u64,
    /// When the finder's grep is to search for what's been typed, a moment
    /// after the last key.
    grep_at: Option<std::time::Instant>,
    /// Files being formatted, by the number of the query that asked.
    formatting: HashMap<u64, formatting::Formatting>,
    /// Why files being written weren't formatted first, to say when
    /// they're written.
    unformatted: HashMap<BufferId, String>,
    /// What the bar says when nothing else has a say, such as that a new
    /// release is out.
    notice: Option<String>,
    /// `:update`, asked for and waiting for the window to carry out.
    update_asked: Option<UpdateAsked>,
    /// The protocol version the connection to the daemon runs at, and the
    /// daemon's release, if it said: see [`Desk::daemon_has`].
    daemon: Option<(u32, Option<String>)>,
    /// When the window last asked again for each thing it missed, a newer
    /// daemon having sent it as this window couldn't read: see
    /// [`MOST_MISSED`].
    missed: HashMap<Missed, VecDeque<std::time::Instant>>,
    /// The terminals attached to again, for a snapshot not yet come.
    reattaching: HashSet<TerminalId>,
    /// Whether `:connect` asked for the picker of a machine, waiting for
    /// the window to open it.
    connect_asked: bool,
    /// The machine `@host` named, while the command after it is carried
    /// out: new panes and workspaces go there (see [`elsewhere`]).
    opening_on: Option<clackatoo_protocol::MachineId>,
    /// The machine `@host` named for the chat the start page starts, while
    /// it's open.
    start_on: Option<clackatoo_protocol::MachineId>,
    /// The daemon's connections to other machines, as last heard: where
    /// workspaces from them are, and how each is reached.
    connections: Vec<clackatoo_protocol::Connection>,
    /// The machine `:connect HOST` last asked the daemon to connect to,
    /// until the bar has said how that went.
    connecting: Option<String>,
    /// The machines the config asked the daemon to connect to, with
    /// `client.connect`, and whether to pass each on, if it said, asked
    /// again on each new connection to it.
    config_hosts: Vec<(String, Option<bool>)>,
    /// What `:PortForward` asked for, waiting for the window to do it, as
    /// the window keeps the forwards (see [`crate::forwards`]).
    forwards_asked: Vec<crate::command::Forwarding>,
    /// The tutor's lessons, and the lesson open.
    tutor: lessons::Tutor,
    /// The window's mode, as last taken from a pane that has one (see
    /// [`mode`]).
    mode: WindowMode,
    /// The pane the focus last arrived at, and what it showed then.
    arrived: Option<(PaneId, Content)>,
    /// Panes this window asked the daemon to close, until they have.
    closing: HashSet<PaneId>,
    /// The focused editor, arrived at in insert mode before its text
    /// came, which goes to insert mode once it has, and when the focus
    /// arrived.
    insert_on_load: Option<(PaneId, std::time::Instant)>,
    /// The editor the keys held are for, held for its text to come, which
    /// they may only ever reach.
    held_for: Option<PaneId>,
    /// Terminals whose program the daemon said has ended.
    exited: HashSet<TerminalId>,
    /// Whether a key or a click is being had as the focused pane's own
    /// doing, as Enter in an explorer is.
    acting: bool,
    /// How many requests have been sent on this connection, and answered,
    /// which numbers them.
    asks_sent: u64,
    asks_heard: u64,
    /// The panes each close this window asked for closes, by the number
    /// of its request, until it's answered.
    closes_asked: VecDeque<(u64, Vec<PaneId>)>,
    /// The focused chat, arrived at in insert mode as it asked something,
    /// and so kept in normal mode, while it's left so.
    held_normal: Option<PaneId>,
}

impl Desk {
    // --- What's shown -----------------------------------------------------

    pub(crate) fn shown(&self) -> Option<Shown> {
        self.shown
    }

    pub(crate) fn pane(&self, terminal: TerminalId) -> Option<&Pane> {
        self.panes.get(&terminal)
    }

    pub(crate) fn pane_mut(&mut self, terminal: TerminalId) -> Option<&mut Pane> {
        self.panes.get_mut(&terminal)
    }

    /// How the mouse pointer looks at `point`, if it's over a chat, which
    /// says by what it drew there.
    pub(crate) fn chat_pointer(&self, point: [f32; 2]) -> Option<crate::chat::Pointer> {
        let (terminal, _, pixel) = self.cell_at(point, None)?;
        self.panes.get(&terminal)?.chat_pointer(pixel)
    }

    /// Passes what an input method is composing to the focused pane, if
    /// it's a chat being typed in, which shows it in its box. Returns
    /// whether it was: the window shows it over a terminal itself.
    pub(crate) fn compose_in_chat(
        &mut self,
        composing: Option<(String, Option<(usize, usize)>)>,
    ) -> bool {
        self.focused_pane()
            .is_some_and(|pane| pane.compose_in_chat(composing))
    }

    /// Carries out what was typed or clicked in chats, which needs the
    /// fonts, and takes what they copied, as other copies are taken: `"+`
    /// or `"*` holds it, and the window puts it on the clipboard. Returns
    /// whether the window must be redrawn.
    pub(crate) fn settle_chats(&mut self, fonts: &mut crate::fonts::Fonts) -> bool {
        let mut redraw = false;
        let mut copied = Vec::new();
        for pane in self.panes.values_mut() {
            redraw |= pane.settle_chat(fonts);
            copied.extend(pane.take_chat_copies());
        }
        for (text, to) in copied {
            self.copied(text, to);
        }
        redraw
    }

    /// When a chat must be drawn again by itself, as for its agent's
    /// seconds to count: the soonest. Never while the daemon is out of
    /// reach, however long that is.
    pub(crate) fn chat_wakes(&self) -> Option<std::time::Instant> {
        if !self.connected {
            return None;
        }
        self.panes.values().filter_map(Pane::chat_live).min()
    }

    /// Where typed text goes now, if keys type text rather than being
    /// commands, as they are in normal mode.
    pub(crate) fn text_goes_to(&self) -> Option<TextTarget> {
        if self.input.is_some() || self.finding() {
            return Some(TextTarget::Bar);
        }
        if let Some((editor, _)) = self.focused_editor() {
            return if editor.prompting() {
                Some(TextTarget::Bar)
            } else {
                (editor.mode == Mode::Insert).then_some(TextTarget::Pane)
            };
        }
        if let Some(explorer) = self.focused_explorer() {
            return explorer.prompting().then_some(TextTarget::Bar);
        }
        if let Some(board) = self.focused_board() {
            return board.prompting().then_some(TextTarget::Bar);
        }
        if self.focused_browser().is_some() {
            return None;
        }
        let pane = self.pane(self.focused_terminal()?)?;
        if pane.prompting() {
            Some(TextTarget::Bar)
        } else {
            (pane.mode == Mode::Insert).then_some(TextTarget::Pane)
        }
    }

    /// Whether one of the window's own prompts has the keys: the command
    /// line, a search, a name or an answer being typed, or the finder.
    /// The focused pane's cursor is drawn hollow meanwhile.
    pub(crate) fn prompt_has_keys(&self) -> bool {
        self.text_goes_to() == Some(TextTarget::Bar)
    }

    /// The options, as config and `:set` have set them.
    pub(crate) fn options(&self) -> &Options {
        &self.options
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// Settles the terminals' guesses at the echo of what's typed whose
    /// time is up at `now`. Returns whether the window must be redrawn.
    pub(crate) fn tick_panes(&mut self, now: std::time::Instant) -> bool {
        (self.panes.values_mut()).fold(false, |redraw, pane| pane.tick(now) | redraw)
    }

    /// When a terminal's guess at the echo of what's typed is next due to
    /// be settled, the soonest of them, for [`tick_panes`](Self::tick_panes).
    pub(crate) fn pane_wakes(&self) -> Option<std::time::Instant> {
        self.panes.values().filter_map(Pane::wakes).min()
    }

    /// When a pane's highlight on text just copied goes, the soonest of
    /// them, or anything else timed is due, such as the panel of keys
    /// waiting: the window must be drawn again then.
    pub(crate) fn flash_ends(&self) -> Option<std::time::Instant> {
        let panes = self.panes.values().filter_map(Pane::flash_ends);
        let pointed = (self.pointed)
            .and_then(|(_, until)| (until > std::time::Instant::now()).then_some(until));
        // The panel of keys waiting, once it's due.
        let keys = self
            .which_keys_due()
            .filter(|&due| due > std::time::Instant::now());
        (panes.chain(self.editor_flash_ends()).chain(pointed))
            .chain(self.listing_said())
            .chain(keys)
            .min()
    }

    /// Takes text copied, for the window to put on the clipboard or the
    /// primary selection, oldest first.
    pub(crate) fn take_yanked(&mut self) -> Vec<(String, Copy)> {
        std::mem::take(&mut self.yanked)
    }

    /// The terminal the focused pane shows.
    pub(crate) fn focused_terminal(&self) -> Option<TerminalId> {
        let shown = self.shown?;
        let (_, _, pane) = self.workspaces.find_pane(shown.pane)?;
        pane.content.terminal()
    }

    /// Whether keys can be taken with no daemon to hear them: by the
    /// command line, or an editor, which edits its copy and sends what it
    /// did once there's a connection again (see [`BufferCopy`]). Anything
    /// else's keys must reach the daemon at once, or not at all: a key
    /// typed into a shell seconds after it was meant can do harm.
    pub(crate) fn takes_keys_offline(&self) -> bool {
        if self.input.is_some() {
            return true;
        }
        (self.shown)
            .and_then(|shown| self.workspaces.find_pane(shown.pane))
            .is_some_and(|(_, _, pane)| matches!(pane.content, Content::Buffer(_)))
    }

    /// Whether `press` would go to the focused chat's question, as one of
    /// the keys that answer it (see [`Pane::answers_question`]), which a
    /// key held down mustn't repeat.
    pub(crate) fn answers_question(&self, press: &KeyPress<'_>) -> bool {
        if self.input.is_some() || self.finder.is_some() || self.start.is_some() {
            return false;
        }
        (self.focused_terminal())
            .and_then(|terminal| self.panes.get(&terminal))
            .is_some_and(|pane| pane.answers_question(press))
    }

    /// Turns down a key that can't reach the daemon in time: says so, and
    /// lights the focused pane up for a moment.
    pub(crate) fn refuse_offline(&mut self) {
        self.warn("not connected: keys aren't sent".to_owned());
        if let Some(shown) = self.shown {
            self.pointed = Some((shown.pane, std::time::Instant::now() + REFUSED_FOR));
        }
    }

    fn focused_pane(&mut self) -> Option<&mut Pane> {
        let terminal = self.focused_terminal()?;
        self.panes.get_mut(&terminal)
    }

    /// The tab shown.
    pub(crate) fn shown_tab(&self) -> Option<&Tab> {
        let shown = self.shown?;
        self.workspaces.get(shown.workspace)?.tab(shown.tab)
    }

    /// Where the shown tab's panes go, with any split being resized at its
    /// new shares.
    pub(crate) fn placement(&self) -> Option<Placement> {
        let tab = self.shown_tab()?;
        let area = self.area.as_ref()?;
        if let Some((resized, path, new_shares)) = &self.split_shares
            && *resized == tab.id
        {
            let mut layout = tab.layout.clone();
            if let Some(Layout::Split { shares, .. }) = layout.at_mut(path)
                && shares.len() == new_shares.len()
            {
                shares.clone_from(new_shares);
            }
            return Some(Placement::new(&layout, area));
        }
        Some(Placement::new(&tab.layout, area))
    }

    /// The title for the window: the focused pane's program's, or file's.
    pub(crate) fn title(&self) -> Option<&str> {
        if let Some(title) = self.shown.and_then(|shown| self.editor_title(shown.pane)) {
            return Some(title);
        }
        if let Some(explorer) = self.focused_explorer() {
            return Some(explorer.title());
        }
        if let Some(board) = self.focused_board() {
            return Some(board.title());
        }
        if let Some(review) = self.focused_review() {
            return Some(review.title());
        }
        if let Some(browser) = self.focused_browser() {
            return Some(browser.title());
        }
        let terminal = self.focused_terminal()?;
        let title = self.panes.get(&terminal)?.title()?;
        (!title.is_empty()).then_some(title)
    }

    /// Every workspace, by name, how many tabs it has, whether it's the
    /// one shown, what it says of its chats, and for one from another
    /// machine, where it is.
    pub(crate) fn workspace_names(&self) -> Vec<WorkspaceName> {
        let shown = self.shown.map(|shown| shown.workspace);
        self.workspaces
            .list
            .iter()
            .map(|workspace| WorkspaceName {
                id: workspace.id,
                name: self.shown_name(workspace),
                tabs: workspace.tabs.len(),
                shown: Some(workspace.id) == shown,
                mark: self.workspace_mark(workspace.id),
                away: self
                    .elsewhere(workspace)
                    .map(|elsewhere| crate::chrome::Away {
                        said: elsewhere.said(),
                        reach: elsewhere.reach,
                    }),
            })
            .collect()
    }

    /// What `pane`, on another machine not connected just now, says over
    /// its last screen, as its pill does.
    pub(crate) fn away_notice(&self, pane: PaneId) -> Option<String> {
        self.pane_elsewhere(pane)?.notice()
    }

    /// The shown workspace's tabs, by name, and whether each is the one
    /// shown.
    pub(crate) fn tab_names(&self) -> Vec<(TabId, String, bool)> {
        let Some(shown) = self.shown else {
            return Vec::new();
        };
        let Some(workspace) = self.workspaces.get(shown.workspace) else {
            return Vec::new();
        };
        workspace
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let mut name = self.tab_name(index, tab);
                // A chat at work, asking, or finished unseen, anywhere in
                // the tab: in another tab, or beside the pane with the
                // keyboard, it says so. Still: it never wakes the window.
                if let Some(mark) = self.tab_mark(tab) {
                    // A name cut short ends with an ellipsis already.
                    if name.ends_with('…') {
                        name.pop();
                    }
                    name.push_str(mark.suffix());
                }
                // A pane an agent pointed out, in a tab not shown.
                if tab.id != shown.tab
                    && self.pointed().is_some_and(|pane| tab.layout.contains(pane))
                {
                    name.push_str(" ●");
                }
                (tab.id, name, tab.id == shown.tab)
            })
            .collect()
    }

    /// What the status line shows: the command line or a search being
    /// typed, over the tabs and the focused pane's mode, or else the tabs,
    /// with a message or the focused pane's mode and scroll position.
    pub(crate) fn status(&self) -> Status {
        match self.status_alone() {
            Status::Typing {
                label,
                text,
                choices,
                selected,
                ..
            } => Status::Typing {
                label,
                text,
                choices,
                selected,
                under: Some(Box::new(self.bar_under_typing())),
            },
            tabs @ Status::Tabs { .. } => tabs,
        }
    }

    /// What the bottom bar shows while something's typed: the focused
    /// pane's mode and the tabs, and a message, if there is one, but not
    /// the pane's label, whose place a search typed in it takes.
    fn bar_under_typing(&self) -> Status {
        let mode = match self.focused_editor() {
            Some((editor, _)) => Some(editor_mode_name(editor)),
            None if self.panel_label().is_some() => Some(ModeName::Normal),
            None => (self.focused_terminal())
                .and_then(|terminal| self.panes.get(&terminal))
                .map(|pane| mode_name(pane.mode)),
        };
        let (note, kind) = self.note(None, None);
        Status::Tabs {
            mode,
            note,
            kind,
            panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
        }
    }

    /// What the status line shows, but with nothing under what's typed.
    fn status_alone(&self) -> Status {
        if let Some(input) = &self.input {
            return self.typing_status(input);
        }
        if let Some(status) = self.offer_status() {
            return status;
        }
        if let Some(status) = self.pairing_status() {
            return status;
        }
        if let Some((editor, copy)) = self.focused_editor() {
            let label = editor.label(copy);
            if editor.prompting() {
                return Status::Typing {
                    label: None,
                    text: label,
                    choices: Vec::new(),
                    selected: editor.label_selected(),
                    under: None,
                };
            }
            let said = editor.said().map(str::to_owned);
            let (note, kind) = self.note(said, Some(label));
            return Status::Tabs {
                mode: Some(editor_mode_name(editor)),
                note: note.map(|note| self.recording_before(note)),
                kind,
                panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
            };
        }
        if let Some((label, prompting)) = self.panel_label() {
            if prompting {
                return Status::Typing {
                    label: None,
                    text: label,
                    choices: Vec::new(),
                    selected: None,
                    under: None,
                };
            }
            let (note, kind) = self.note(None, Some(label));
            return Status::Tabs {
                mode: Some(ModeName::Normal),
                note: note.map(|note| self.recording_before(note)),
                kind,
                panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
            };
        }
        let pane = self
            .focused_terminal()
            .and_then(|terminal| self.panes.get(&terminal));
        if let Some(pane) = pane
            && pane.prompting()
            && let Some(prompt) = pane.label()
        {
            return Status::Typing {
                label: None,
                text: prompt,
                choices: Vec::new(),
                selected: None,
                under: None,
            };
        }
        let mode = pane.map(|pane| mode_name(pane.mode));
        let label = pane.and_then(Pane::label).or_else(|| self.notice());
        let (note, kind) = self.note(None, label);
        Status::Tabs {
            mode,
            note,
            kind,
            panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
        }
    }

    /// The bottom bar's note, and what kind it is: the window's message,
    /// or what the focused pane has `said`, until the next key; or else
    /// its `label`. The keys that can come next, while some wait, are in a
    /// panel of their own (see [`Desk::which_keys`]).
    fn note(&self, said: Option<String>, label: Option<String>) -> (Option<String>, NoteKind) {
        if let Some((message, kind)) = self.message_note() {
            return (Some(message), kind);
        }
        match (said, label) {
            (Some(said), _) => (Some(said), NoteKind::Message),
            (None, label) => (label, NoteKind::Label),
        }
    }

    /// `note`, after the register a macro is being recorded into, if one is.
    fn recording_before(&self, note: String) -> String {
        match self.recording() {
            Some(register) => format!("recording @{register}  {note}"),
            None => note,
        }
    }

    /// What the bar calls a tab: its number, and its name, or else its
    /// focused pane's title.
    fn tab_name(&self, index: usize, tab: &Tab) -> String {
        /// Longer titles are cut short, to leave room for the other tabs.
        const MAX_TITLE: usize = 24;
        let number = index + 1;
        // A program's title, and a folder's name, are its own to choose:
        // what would move the cursor or turn the text round is left out.
        let title: Option<String> = (tab.name.as_deref().or_else(|| self.tab_title(tab)))
            .map(|title| title.chars().filter(|&ch| shown_in_names(ch)).collect())
            .filter(|title: &String| !title.trim().is_empty());
        // Its focused pane on another machine than its workspace says
        // which, unless `showhost` says never: `@local` for this machine's
        // own, in a workspace of another's.
        let own = self.workspaces.machine.as_ref().map(|machine| machine.id);
        let away = (self.options.showhost != crate::options::ShowHost::Never)
            .then(|| {
                let (workspace, _, pane) = self.workspaces.find_pane(tab.focused)?;
                let home = pane.home.filter(|&home| Some(home) != workspace.home)?;
                if Some(home) == own {
                    return Some("local".to_owned());
                }
                Some(self.elsewhere_on(home).host)
            })
            .flatten();
        let away = away.map(|host| format!("@{host}")).unwrap_or_default();
        match title.as_deref() {
            Some(title) if title.chars().count() > MAX_TITLE => {
                let short: String = title.chars().take(MAX_TITLE - 1).collect();
                format!("{number} {short}…{away}")
            }
            Some(title) => format!("{number} {title}{away}"),
            None if away.is_empty() => number.to_string(),
            None => format!("{number} {away}"),
        }
    }

    /// The title of `tab`'s focused pane's program, if it set one.
    fn tab_title(&self, tab: &Tab) -> Option<&str> {
        self.workspaces
            .find_pane(tab.focused)
            .and_then(|(_, _, pane)| match pane.content {
                Content::Terminal(terminal) => self.panes.get(&terminal)?.name(self.home()),
                Content::Buffer(_) => self.editor_title(pane.id),
                Content::Files(_) | Content::Folder(_) => {
                    self.explorer(pane.id).map(crate::explorer::Explorer::title)
                }
                Content::Board(_) => self.board(pane.id).map(crate::board::BoardPane::title),
                Content::Review(_) => self.review(pane.id).map(crate::review::Review::title),
                Content::Browser(_) => self.browser(pane.id).map(BrowserPane::title),
                Content::Other { .. } => self.other(pane.id).map(OtherPane::title),
            })
            .map(str::trim)
            .filter(|title| !title.is_empty())
    }

    /// The daemon's user's home directory, which paths are written from
    /// `~` under, as the daemon says, or as this machine has it for a
    /// daemon on it too old to say.
    fn home(&self) -> Option<&str> {
        (self.workspaces.home.as_deref())
            .or_else(|| self.own_home.as_deref().filter(|_| !self.remote))
    }

    /// Opens the palette to rename `workspace`, starting from its name.
    pub(crate) fn rename_workspace(&mut self, workspace: WorkspaceId) {
        if let Some(found) = self.workspaces.get(workspace) {
            self.input = Some(Input::renaming(
                Purpose::RenameWorkspace(workspace),
                found.name.clone(),
            ));
        }
    }

    /// Opens the palette to name `tab`, starting from its name or title.
    pub(crate) fn rename_tab(&mut self, tab: TabId) {
        if let Some((_, found)) = self.workspaces.find_tab(tab) {
            // What a name can't show isn't put in one.
            let text: String = (found.name.as_deref())
                .or_else(|| self.tab_title(found))
                .unwrap_or_default()
                .chars()
                .filter(|&ch| shown_in_names(ch))
                .collect();
            self.input = Some(Input::renaming(Purpose::RenameTab(tab), text));
        }
    }

    /// Closes `workspace`, with every terminal in it, unless that would
    /// lose a buffer with no file yet that has changes.
    pub(crate) fn close_workspace(&mut self, workspace: WorkspaceId) {
        let panes: Vec<PaneId> = (self.workspaces.get(workspace).into_iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .map(|pane| pane.id)
            .collect();
        if self.loses_blank(&panes) {
            self.say(BLANK_KEPT.into());
            return;
        }
        self.request(Request::CloseWorkspace { workspace }, Asked::Command);
    }

    /// Closes `tab`, with every terminal in it, unless that would lose a
    /// buffer with no file yet that has changes.
    pub(crate) fn close_tab(&mut self, tab: TabId) {
        let panes: Vec<PaneId> = (self.workspaces.find_tab(tab).into_iter())
            .flat_map(|(_, tab)| tab.layout.panes())
            .map(|pane| pane.id)
            .collect();
        if self.loses_blank(&panes) {
            self.say(BLANK_KEPT.into());
            return;
        }
        self.request(Request::CloseTab { tab }, Asked::Command);
    }

    /// Makes a workspace, and switches to it.
    pub(crate) fn new_workspace(&mut self) {
        self.run(Command::Workspace(None));
    }

    /// Opens a terminal in a new tab of the workspace shown.
    pub(crate) fn new_tab(&mut self) {
        self.run(Command::Open {
            place: Place::Tab,
            kind: Kind::Terminal,
            agent: None,
        });
    }

    /// Carries out `:workspace`: switches to the workspace `name` names,
    /// or makes it if there's none, on the machine it names after `@`, as
    /// `:ws new@desk NAME` does too; or with no name, makes a new one.
    fn switch_workspace(&mut self, name: Option<String>) {
        let existing = (name.as_deref())
            .and_then(|name| self.workspace_named(name.trim()))
            .map(|workspace| workspace.id);
        if let Some(workspace) = existing {
            return self.show_workspace(workspace);
        }
        match name.as_deref().and_then(elsewhere::new_elsewhere) {
            Some((name, host)) => {
                let host = host.to_owned();
                self.new_workspace_named_on(name, &host);
            }
            None => self.new_workspace_here(name),
        }
    }

    /// Switches to `workspace`, at its active tab and that tab's focused
    /// pane, as when its name is clicked.
    pub(crate) fn show_workspace(&mut self, workspace: WorkspaceId) {
        let Some(workspace) = self.workspaces.get(workspace) else {
            return;
        };
        let pane = workspace
            .tab(workspace.active_tab)
            .unwrap_or(&workspace.tabs[0])
            .focused;
        self.show_pane(pane);
    }

    /// Switches to `tab` of the workspace shown, at its focused pane.
    pub(crate) fn show_tab(&mut self, tab: TabId) {
        if let Some(tab) = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .and_then(|workspace| workspace.tab(tab))
        {
            self.show_pane(tab.focused);
        }
    }

    /// Focuses the pane whose tile is at `point`, as when it's clicked.
    /// Returns whether one is.
    pub(crate) fn focus_at(&mut self, point: [f32; 2]) -> bool {
        let pane = self
            .placement()
            .and_then(|placement| placement.pane_at(point));
        if let Some(pane) = pane {
            self.show_pane(pane);
        }
        pane.is_some()
    }

    /// Sets the space the panes share, as the window opens or resizes, and
    /// sizes the terminals shown to fit.
    pub(crate) fn set_area(&mut self, area: Option<Area>) {
        self.area = area;
        self.fit_panes();
    }

    /// Makes sure what's shown still exists, after the workspaces changed:
    /// what was shown if it's still there, or else the nearest to it.
    fn settle(&mut self) {
        let workspaces = &self.workspaces;
        let Some(workspace) = self
            .shown
            .and_then(|shown| workspaces.get(shown.workspace))
            .or_else(|| workspaces.active.and_then(|id| workspaces.get(id)))
            .or_else(|| workspaces.list.first())
        else {
            self.shown = None;
            return;
        };
        let tab = self
            .shown
            .and_then(|shown| workspace.tab(shown.tab))
            .or_else(|| workspace.tab(workspace.active_tab))
            .unwrap_or(&workspace.tabs[0]);
        let pane = self
            .shown
            .map(|shown| shown.pane)
            .filter(|&pane| tab.layout.contains(pane))
            .unwrap_or(tab.focused);
        self.shown = Some(Shown {
            workspace: workspace.id,
            tab: tab.id,
            pane,
        });
    }

    /// An agent asked to show `pane`: it's lit for a moment, or its tab
    /// is marked if another is shown, and its cursor goes to `at`, in its
    /// file. Only in the workspace shown, and never moving the focus: an
    /// agent that could would have the user's next keys typed where it
    /// chose.
    pub(crate) fn agent_shows(&mut self, pane: PaneId, at: Option<FileLocation>) {
        let shown = self.shown.map(|shown| shown.workspace);
        let Some((workspace, _, _)) = self.workspaces.find_pane(pane) else {
            return;
        };
        if Some(workspace.id) != shown {
            return;
        }
        if let Some(at) = at {
            self.land_at(pane, at);
        }
        self.pointed = Some((pane, std::time::Instant::now() + POINTED_FOR));
    }

    /// The pane an agent pointed out, while it's lit.
    pub(crate) fn pointed(&self) -> Option<PaneId> {
        let (pane, until) = self.pointed?;
        (std::time::Instant::now() < until).then_some(pane)
    }

    /// Switches to `pane`, wherever it is, which takes the window's mode.
    /// The pane left keeps its place, but not what was half done in it.
    fn show_pane(&mut self, pane: PaneId) {
        self.move_focus(pane, true);
    }

    /// Switches to `pane`, as [`show_pane`](Self::show_pane) does, adding
    /// the pane left to the jump list if `record`.
    fn move_focus(&mut self, pane: PaneId, record: bool) {
        let Some((workspace, tab, _)) = self.workspaces.find_pane(pane) else {
            return;
        };
        let shown = Shown {
            workspace: workspace.id,
            tab: tab.id,
            pane,
        };
        if self.shown == Some(shown) {
            return;
        }
        if record && let Some(left) = self.shown {
            self.record_jump(left.pane, None);
        }
        let unfocused = self.unfocused;
        self.remember_mode();
        if !unfocused && let Some(left) = self.focused_pane() {
            left.focus_changed(false);
        }
        if let Some(left) = self.shown {
            self.leave_pane(left.pane);
        }
        self.shown = Some(shown);
        self.note_focus(pane);
        self.see_shown();
        // Before the mode: a pane of another workspace may not exist yet.
        self.attach_shown();
        self.arrive();
        if !unfocused && let Some(arrived) = self.focused_pane() {
            arrived.focus_changed(true);
        }
        self.request(Request::Focus { pane }, Asked::Quiet);
        self.fit_panes();
    }

    /// Attaches to every terminal in the workspace shown, and detaches from
    /// the rest, making panes for terminals new to the window and
    /// forgetting those of terminals gone.
    fn attach_shown(&mut self) {
        let shown: HashSet<TerminalId> = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(|workspace| workspace.terminals().into_iter().collect())
            .unwrap_or_default();
        let anywhere: HashSet<TerminalId> = self
            .workspaces
            .list
            .iter()
            .flat_map(clackatoo_protocol::Workspace::terminals)
            .collect();
        self.panes.retain(|terminal, _| anywhere.contains(terminal));
        for &terminal in &shown {
            self.panes.entry(terminal).or_insert_with(|| {
                let mut pane = Pane::new(terminal);
                pane.set_options(&self.options);
                pane.set_remote(self.remote);
                pane
            });
        }
        self.attach_shown_buffers();
        self.attach_shown_explorers();
        self.attach_shown_boards();
        self.attach_shown_reviews();
        self.attach_shown_browsers();
        self.show_others();
        if !self.connected {
            return;
        }
        let gone: Vec<TerminalId> = self.attached.difference(&shown).copied().collect();
        for terminal in gone {
            self.attached.remove(&terminal);
            // A closed terminal's view went with it.
            if anywhere.contains(&terminal) {
                self.request(Request::Detach { terminal }, Asked::Quiet);
            }
        }
        let new: Vec<TerminalId> = shown.difference(&self.attached).copied().collect();
        for terminal in new {
            self.attached.insert(terminal);
            self.request(Request::Attach { terminal }, Asked::Quiet);
        }
    }

    /// Makes a pane saying it needs a newer clackatoo for every pane of
    /// the tab shown of a kind this window doesn't know, and forgets those
    /// gone.
    fn show_others(&mut self) {
        // On another machine, one of a kind not relayed yet, which says
        // so, by its host; in a workspace of the daemon's own, one waiting
        // for its machine to say what it shows; one on a machine the
        // daemon can't reach; or one of the daemon's own machine, parked
        // while the machine whose workspace it's in is away.
        let shown_workspace = (self.shown).and_then(|shown| self.workspaces.get(shown.workspace));
        let own = shown_workspace.is_some_and(|workspace| workspace.home.is_none());
        let own_machine = self.workspaces.machine.as_ref().map(|machine| machine.id);
        let layout_host = shown_workspace
            .and_then(|workspace| self.elsewhere(workspace))
            .map(|away| away.host);
        let shown: Vec<(PaneId, PaneKind, Why)> = (self.shown_tab())
            .map(|tab| {
                (tab.layout.panes().iter())
                    .filter_map(|pane| {
                        let Content::Other { kind } = pane.content else {
                            return None;
                        };
                        let why = match self.pane_elsewhere(pane.id) {
                            _ if pane.home.is_some() && pane.home == own_machine => {
                                Why::Parked(layout_host.clone().unwrap_or_default())
                            }
                            Some(away) if away.reach == crate::chrome::Reach::Unreachable => {
                                Why::Unreachable(away.host)
                            }
                            Some(away) if own => Why::Waiting(away.host),
                            Some(away) => Why::TooOld(away.host),
                            None => Why::Newer,
                        };
                        Some((pane.id, kind, why))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.others.retain(|pane, other| {
            (shown.iter()).any(|(shown, kind, why)| {
                shown == pane && *kind == other.kind() && why == other.reason()
            })
        });
        for (pane, kind, why) in shown {
            self.others
                .entry(pane)
                .or_insert_with(|| OtherPane::why(kind, why));
        }
    }

    /// The pane `pane` is, if it's of a kind this window doesn't know.
    pub(crate) fn other(&self, pane: PaneId) -> Option<&OtherPane> {
        self.others.get(&pane)
    }

    /// Asks the daemon to size each terminal shown to its pane.
    fn fit_panes(&mut self) {
        let Some(placement) = self.placement() else {
            return;
        };
        let Some(tab) = self.shown_tab() else { return };
        let panes = tab.layout.panes();
        for (id, tile) in placement.panes {
            let Some(pane) = panes.iter().find(|pane| pane.id == id) else {
                continue;
            };
            let terminal = match pane.content {
                Content::Terminal(terminal) => terminal,
                Content::Buffer(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_editor(id, size);
                    }
                    continue;
                }
                Content::Files(_) | Content::Folder(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_explorer(id, size);
                        // A folder's listing being edited in it.
                        self.fit_editor(id, size);
                    }
                    continue;
                }
                Content::Board(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_board(id, size);
                    }
                    continue;
                }
                Content::Review(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_review(id, size);
                    }
                    continue;
                }
                Content::Browser(_) => {
                    self.fit_browser(id, tile);
                    continue;
                }
                Content::Other { .. } => {
                    if let (Some(other), Ok(size)) =
                        (self.others.get_mut(&id), Size::new(tile.cols, tile.rows))
                    {
                        other.resize(size);
                    }
                    continue;
                }
            };
            if let (Some(pane), Ok(size)) = (
                self.panes.get_mut(&terminal),
                Size::new(tile.cols, tile.rows),
            ) {
                pane.resize(size);
            }
        }
    }

    // --- The daemon's news ------------------------------------------------

    /// A new connection, watching `workspaces`, to a daemon on another
    /// machine if `remote`. Nothing is attached or asked yet, and panes
    /// kept from the last one start over, keeping their mode, scroll and
    /// history.
    pub(crate) fn connected(&mut self, workspaces: Workspaces, remote: bool) {
        self.connected = true;
        self.remote = remote;
        self.own_home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
        self.forget_asked();
        self.tell_places_again();
        for pane in self.panes.values_mut() {
            pane.reconnected();
            pane.set_remote(remote);
            pane.chat_offline(false);
        }
        self.workspaces_changed(workspaces);
        self.watch_chats();
        self.connect_config_hosts();
        self.tutor_connected();
        // A new daemon, or one that lost the last connection, doesn't know.
        self.pairing = clackatoo_protocol::PairingState::default();
        if !self.unfocused {
            self.say_present(true);
        }
    }

    /// The connection was lost: nothing is attached, and nothing asked
    /// will be answered.
    pub(crate) fn lost(&mut self) {
        self.connected = false;
        self.forget_asked();
        // A chat's agent may have long finished by the time it's back:
        // none says it's working, nor counts, meanwhile.
        for pane in self.panes.values_mut() {
            pane.chat_offline(true);
        }
        // A macro can't go on without the daemon, nor wait for it.
        if self.macro_waiting() {
            self.stop_macro(Some(
                "the macro was stopped: the daemon is out of reach".into(),
            ));
            self.drop_held();
        }
        // Its searches went with the connection.
        self.finder = None;
        self.start = None;
    }

    /// Forgets everything sent on the last connection, which will never be
    /// answered, and what was waiting on it: a split's new shares, and
    /// drags under way.
    fn forget_asked(&mut self) {
        self.attached.clear();
        self.reattaching.clear();
        self.editors_reconnected();
        self.explorers_reconnected();
        self.boards_reconnected();
        self.reviews_reconnected();
        self.browsers_reconnected();
        self.asked.clear();
        self.asks_heard = self.asks_sent;
        self.closes_asked.clear();
        self.macros.requests_forgotten();
        self.outbox.clear();
        self.forget_colour_asks();
        self.formatting.clear();
        self.unformatted.clear();
        self.resizes_pending = 0;
        self.split_shares = None;
        self.dragging_divider = None;
        self.dragging = None;
        self.mouse_cell = None;
    }

    /// Takes the daemon's workspaces, as they are now.
    pub(crate) fn workspaces_changed(&mut self, workspaces: Workspaces) {
        // The focused pane may close, or show something else.
        self.remember_mode();
        let ended = self.focused_exited();
        self.workspaces = workspaces;
        if self.workspaces.list.is_empty() {
            // Every pane closed, as when the last shell exits.
            self.finished = true;
            return;
        }
        let shown = self.shown;
        self.settle();
        // A pane a macro waiting closed, as with `:q`, moves the focus as it
        // asked: it goes on in the pane that has it. Closed by anyone else,
        // the macro stops, as its keys were for that pane.
        if self.macro_asked() && self.shown != shown {
            self.macro_follows_focus();
        }
        self.attach_shown();
        self.see_shown();
        self.arrive_if_moved(ended);
        self.fit_panes();
        self.forget_gone_handlers();
        self.close_leftovers();
        self.settle_lesson();
        self.relist_background();
    }

    /// A view update for `terminal` from a newer daemon couldn't be read,
    /// a snapshot if `snapshot`: the window attaches to it again, for a
    /// fresh snapshot, unless it has already and the snapshot hasn't come.
    /// One that couldn't be read is what it waited for, so it asks again.
    /// Every miss counts towards how often it asks.
    pub(crate) fn missed_view(&mut self, terminal: TerminalId, snapshot: bool) {
        if !self.connected || !self.attached.contains(&terminal) {
            return;
        }
        if snapshot {
            self.reattaching.remove(&terminal);
        }
        if self.ask_again(Missed::View(terminal)) && self.reattaching.insert(terminal) {
            self.request(Request::Attach { terminal }, Asked::Reattached(terminal));
        }
    }

    /// The workspaces, from a newer daemon, couldn't be read: the window
    /// asks for them again.
    pub(crate) fn missed_workspaces(&mut self) {
        if self.connected && self.ask_again(Missed::Workspaces) {
            self.request(Request::WatchWorkspaces, Asked::Workspaces);
        }
    }

    /// Whether to ask again for `what`, which a newer daemon sent as this
    /// window couldn't read: not if it's been asked for again too often
    /// lately, as it would only come the same way again. The bar says so
    /// then.
    fn ask_again(&mut self, what: Missed) -> bool {
        let now = std::time::Instant::now();
        let times = self.missed.entry(what).or_default();
        while times
            .front()
            .is_some_and(|&at| now.saturating_duration_since(at) >= MISSED_OVER)
        {
            times.pop_front();
        }
        if times.len() >= MOST_MISSED {
            let what = match what {
                Missed::View(terminal) => format!("terminal {terminal}"),
                Missed::Buffer(_) => "a file being edited".to_owned(),
                Missed::Workspaces => "the workspaces".to_owned(),
            };
            self.warn(format!(
                "can't read what the daemon says about {what}: the daemon is newer · :update"
            ));
            return false;
        }
        times.push_back(now);
        true
    }

    /// Applies a view update for `terminal`. Returns whether it's shown,
    /// and so the window must be redrawn.
    pub(crate) fn view_changed(&mut self, terminal: TerminalId, update: &ViewUpdate) -> bool {
        if matches!(update, ViewUpdate::Snapshot(_)) {
            self.reattaching.remove(&terminal);
        }
        let Some(pane) = self.panes.get_mut(&terminal) else {
            return false;
        };
        let doing = (pane.chat_working(), pane.asking());
        if !pane.view_changed(update) {
            return false;
        }
        // A chat starting or stopping work, or asking, changes its tab's
        // name, shown or not.
        let flipped = (pane.chat_working(), pane.asking()) != doing;
        self.lesson_view_changed(terminal);
        self.settle_lesson();
        // Tabs not shown wait, undrawn, until they are.
        flipped
            || self.shown_tab().is_some_and(|tab| {
                tab.layout
                    .panes()
                    .iter()
                    .any(|pane| pane.content == Content::Terminal(terminal))
            })
    }

    /// Hands the daemon's answer to whoever asked, and goes on with a
    /// macro that waited for it, unless it's refused, which stops it, as a
    /// key that fails does. Returns whether the window must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        if matches!(response, Response::Error(_))
            && self.asked.front().is_some_and(macros::waits_for)
        {
            self.stop_macro(None);
        }
        // An answer the macro waited for may move the focus, as to a pane
        // it asked for: the macro goes on there.
        let asked_by_macro = self.macro_asked();
        if let Some(asker) = self.asked.front() {
            self.macros.request_answered(asker);
        }
        let shown = self.shown;
        let redraw = self.hand_answer(response);
        if asked_by_macro && self.shown != shown {
            self.macro_follows_focus();
        }
        let typed = self.type_macros();
        self.settle_lesson();
        redraw | typed
    }

    /// Hands the daemon's answer to whoever asked. Returns whether the
    /// window must be redrawn.
    #[allow(clippy::too_many_lines, reason = "one arm per asker, each short")]
    fn hand_answer(&mut self, response: Response) -> bool {
        if !self.asked.is_empty() {
            self.heard_back(self.asks_heard, matches!(response, Response::Error(_)));
            self.asks_heard += 1;
        }
        match self.asked.pop_front() {
            Some(Asker::Pane(terminal)) => {
                let Some(pane) = self.panes.get_mut(&terminal) else {
                    return false;
                };
                let redraw = pane.answered(response);
                if let Some((text, to)) = pane.take_yanked() {
                    self.copied(text, to);
                }
                redraw
            }
            Some(Asker::Window(Asked::Resize)) => {
                self.resizes_pending -= 1;
                if let Response::Error(err) = response {
                    log::warn!("the daemon wouldn't resize the split: {err}");
                }
                // The daemon's workspaces, sent before this answer, have the
                // last shares asked for.
                if self.resizes_pending == 0 && self.dragging_divider.is_none() {
                    self.split_shares = None;
                }
                false
            }
            Some(Asker::Window(Asked::Lua(request))) => {
                let result = match response {
                    Response::PaneCreated(pane) => {
                        // As a pane split with a key is.
                        self.show_pane(pane);
                        Ok(pane)
                    }
                    Response::Error(err) => Err(err.to_string()),
                    other => Err(format!("the daemon answered {other:?}")),
                };
                self.answer_lua(request, result);
                true
            }
            Some(Asker::Window(Asked::Saved { buffer, then_close })) => {
                self.saved(buffer, then_close, &response);
                true
            }
            Some(Asker::Window(Asked::PaneAt(landing))) => {
                match response {
                    Response::PaneCreated(pane) => {
                        self.show_pane(pane);
                        if let Some(landing) = landing {
                            self.land_at(pane, landing);
                        }
                    }
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Reviewed)) => {
                match response {
                    Response::Reviewed(said) if !said.is_empty() => self.say(said),
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Recovered)) => {
                match response {
                    Response::Text(said) => self.say(said),
                    Response::PaneCreated(pane) => self.show_pane(pane),
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Forwarded)) => {
                match response {
                    Response::Text(said) => self.say(said),
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Devices)) => self.devices_answered(response),
            Some(Asker::Window(Asked::BackgroundPreview(tab))) => {
                self.background_preview_came(tab, response)
            }
            Some(Asker::Window(Asked::ChatChoices)) => self.chat_choices(response),
            Some(Asker::Window(Asked::Chats)) => self.chats_answered(response),
            Some(Asker::Window(Asked::Reattached(terminal))) => {
                if let Response::Error(err) = response {
                    log::warn!("the daemon wouldn't attach to terminal {terminal} again: {err}");
                    self.reattaching.remove(&terminal);
                }
                false
            }
            Some(Asker::Window(Asked::Workspaces)) => match response {
                Response::Workspaces(workspaces) => {
                    self.workspaces_changed(workspaces);
                    true
                }
                other => {
                    log::warn!("the daemon answered the workspaces with {other:?}");
                    false
                }
            },
            Some(Asker::Window(Asked::Blank(dir))) => {
                match response {
                    Response::PaneCreated(pane) => self.blank_made(pane, dir),
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Explorer { reveal, edit })) => {
                match response {
                    Response::PaneCreated(pane) => self.explorer_made(pane, reveal, edit),
                    Response::Error(err) => self.warn(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Lesson(serial, asked))) => {
                self.lesson_answered(serial, asked, response)
            }
            Some(Asker::Window(Asked::OpenedHere(pane))) => {
                let failed = match response {
                    Response::Error(err) => Some(err.to_string()),
                    _ => None,
                };
                self.opened_here(pane, failed.is_some());
                if let Some(err) = failed {
                    self.warn(err);
                }
                true
            }
            Some(Asker::Window(asked)) => match (asked, response) {
                (Asked::Quiet, Response::Error(err)) => {
                    log::warn!("the daemon refused a request: {err}");
                    false
                }
                (_, Response::Error(err)) => {
                    self.warn(err.to_string());
                    true
                }
                // The workspaces came first, so the pane is there to show.
                (Asked::Pane, Response::PaneCreated(pane)) => {
                    self.show_pane(pane);
                    true
                }
                _ => false,
            },
            None => {
                log::warn!("the daemon answered a request nobody sent");
                false
            }
        }
    }

    /// Tells the daemon `terminals` are on screen, so each sends its next
    /// update.
    pub(crate) fn drawn(&mut self, terminals: &[TerminalId]) {
        for terminal in terminals {
            if let Some(pane) = self.panes.get_mut(terminal) {
                pane.drawn();
            }
        }
    }

    // --- Talking to the daemon --------------------------------------------

    /// Queues a request of the window's own.
    fn request(&mut self, request: Request, asked: Asked) {
        let request = self.aimed(request);
        if self.connected {
            self.sending(&request, &asked);
            let window = Asker::Window(asked);
            self.macros.requested(&window);
            self.asked.push_back(window);
            self.asks_sent += 1;
            self.outbox.push(ClientMessage::Request(request));
        }
    }

    /// Takes the messages waiting for the daemon: the window's, then each
    /// pane's.
    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        let mut messages = std::mem::take(&mut self.outbox);
        let blocks = self.daemon_has(wire::BLOCKS);
        let mut copied_across = false;
        for (&terminal, pane) in &mut self.panes {
            for mut message in pane.take_outbox() {
                if !self.connected {
                    continue;
                }
                // A daemon too old to copy a block copies from one corner
                // to the other, as `v` would, and the bar says why.
                if let ClientMessage::Request(Request::BlockText { terminal, from, to }) = message
                    && !blocks
                {
                    message = ClientMessage::Request(Request::Text {
                        terminal,
                        from,
                        to,
                        whole_lines: false,
                    });
                    copied_across = true;
                }
                if matches!(message, ClientMessage::Request(_)) {
                    self.asked.push_back(Asker::Pane(terminal));
                    self.asks_sent += 1;
                }
                messages.push(message);
            }
        }
        if copied_across {
            self.needs_daemon(
                "copied from corner to corner: copying a block",
                wire::BLOCKS,
            );
        }
        messages.extend(self.take_buffer_outbox());
        messages.extend(self.take_explorer_outbox());
        messages.extend(self.take_board_outbox());
        messages
    }

    // --- Keys -------------------------------------------------------------

    /// Handles a key press: the command line's, a binding's, then the
    /// focused pane's, noting it in the macro being recorded, if one is,
    /// and then the keys of any macro it types. Returns whether the
    /// window must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        if input::is_modifier(press.key) {
            return false;
        }
        self.user_active();
        self.key_coming();
        self.record_press(press);
        // Any key but Shift and the like closes `:messages`.
        let closed = !input::is_modifier(press.key) && self.key_closes_messages();
        let redraw = self.key_with(press, true);
        let typed = self.type_macros();
        self.recorded_press();
        self.settle_lesson();
        redraw || typed || closed
    }

    /// Handles a key press, as [`key`](Self::key) does, but for the
    /// config's keypress handlers if not `handlers`, as for a key they've
    /// had already, and without noting it in a macro. Keys a binding types
    /// meet neither them nor the config's bindings, only the window's.
    #[allow(
        clippy::too_many_lines,
        reason = "where a key goes, in order, each step short"
    )]
    fn key_with(&mut self, press: &KeyPress<'_>, handlers: bool) -> bool {
        // The keyboard reports Shift, Ctrl and the like as keys of their
        // own, on the way to a chord such as Ctrl+Shift+C. They mustn't
        // count as the next key: that would drop a selection or a message,
        // or the first key of a two-key command, before the chord arrives.
        if input::is_modifier(press.key) {
            return false;
        }
        // While an editor's command waits for the grammar, or a macro for
        // what its keys started, keys wait too, so none, not even a
        // binding's, gets ahead of it.
        let typing = self.script.typing;
        let recorded = self.shared_mut().registers.press_start();
        let held = || editors::Held::Key {
            press: KeptPress::of(press),
            handlers,
            typing,
            recorded,
        };
        // Esc pressed cancels it; one a macro or binding types waits its
        // turn.
        let escape = *press.key == Key::Named(NamedKey::Escape) || pane::enters_normal_mode(press);
        let cancel = escape && !typing && !self.key_from_frame();
        if let Some(redraw) = self.hold_while_waiting(cancel, held) {
            return redraw;
        }
        let asked = self.offer_asked();
        if asked && self.offer_key(press) {
            return true;
        }
        if self.input.is_some() {
            // A key the command line takes never reaches a chat, so an
            // answer it armed is dropped, as any other key drops it there.
            self.disarm_focused_chat();
            // Ctrl+; and its kin close the command line, and go on to the
            // pane, which stays in normal mode.
            if !pane::enters_normal_mode(press) {
                self.input_key(press);
                return true;
            }
            self.input = None;
        } else if self.finding() {
            // And so the finder.
            self.disarm_focused_chat();
            if !pane::enters_normal_mode(press) {
                self.finder_key(press);
                return true;
            }
            self.close_finder();
        } else if let Some(chord) = Chord::of(press) {
            // The config's keypress handlers come first, then the bindings.
            if handlers && !self.script.typing && self.keypress_handled(chord, press) {
                self.disarm_focused_chat();
                return true;
            }
            // In a lesson's tab, `]s` and `[s` step through the lesson.
            if let Some(redraw) = self.lesson_bracket(press) {
                self.disarm_focused_chat();
                return redraw;
            }
            let mode = self.key_mode();
            // A key a half-typed command in the pane waits for is the
            // pane's, as `d<Space>`'s Space is, not a binding's first.
            // But a count alone is the bindings' too, and a chat's half
            // typed `g` or `z`: `5:` is still `:`.
            // And `0` after a count is the count's, as Vim never maps it
            // then: `10j` is ten, whatever `0` is bound to.
            let counting = self.pane_counting().is_some();
            let pressed = if self.keymap.waiting().is_empty()
                && self.pane_pending()
                && (!counting || chord == Chord::typed('0'))
            {
                Pressed::Unbound
            } else {
                self.keymap.press(mode, chord)
            };
            // And a key the bindings take, as the command line's.
            if !matches!(pressed, Pressed::Unbound) {
                self.disarm_focused_chat();
            }
            match pressed {
                Pressed::Bound(Binding::Window(action)) => {
                    self.forget_waiting();
                    self.message = None;
                    self.bound_count = self.take_pane_count();
                    self.act(action);
                    self.bound_count = None;
                    return true;
                }
                // Keys a binding types are past the config's bindings.
                Pressed::Bound(Binding::Lua(_) | Binding::Keys(_)) if self.script.typing => {
                    self.type_bound(chord, press);
                    return true;
                }
                Pressed::Bound(Binding::Lua(binding)) => {
                    self.message = None;
                    // A chat's half-typed `g` or `z` is no count.
                    let chat = self.focused_pane().is_some_and(|pane| pane.is_chat());
                    let count = self.take_pane_count().filter(|_| !chat);
                    self.run_lua_binding(binding, count, chord, press);
                    return true;
                }
                Pressed::Waiting => {
                    self.keep_waiting(chord, press);
                    self.message = None;
                    return true;
                }
                Pressed::Bound(Binding::Keys(to)) => {
                    self.forget_waiting();
                    self.message = None;
                    self.type_keys(&to);
                    return true;
                }
                Pressed::Dropped => {
                    if !self.type_dropped() {
                        return true;
                    }
                    // Looked up again on its own, past the handlers,
                    // which have had it.
                    return self.key_with(press, false);
                }
                Pressed::Unbound => {}
            }
        }
        if pane::enters_normal_mode(press) {
            self.normal_mode_asked();
        }
        let had_message = self.message.take().is_some() || asked;
        if let Some(redraw) = self.prompt_line_key(press) {
            return redraw || had_message;
        }
        if self.focused_editor().is_some() {
            return self.listing_key(press) || self.editor_key(press) || had_message;
        }
        if let Some(redraw) = self.acting_from_list(|desk| desk.list_key(press)) {
            return redraw || had_message;
        }
        if self.stop_recording_in_terminal(press) {
            return true;
        }
        if self.block_needs_daemon(press) {
            return true;
        }
        // The offer to run again what a terminal ran before a restart.
        if self.ran_key(press) {
            return true;
        }
        let redraw = self.focused_pane().is_some_and(|pane| pane.key(press));
        if let Some(terminal) = self.focused_terminal() {
            self.open_pane_link(terminal);
        }
        redraw || had_message
    }

    /// Whether `press` is Ctrl+V in a terminal's normal or visual mode,
    /// which selects a block, from a daemon too old to copy one: the bar
    /// says so, and the key goes no further.
    fn block_needs_daemon(&mut self, press: &KeyPress<'_>) -> bool {
        let ctrl_v = press.modifiers.ctrl
            && !press.modifiers.shift
            && matches!(press.key, Key::Character(text) if text.eq_ignore_ascii_case("v"));
        let terminal = self.focused_pane().is_some_and(|pane| {
            !pane.is_chat() && matches!(pane.mode, Mode::Normal | Mode::Visual(_))
        });
        ctrl_v && terminal && !self.needs_daemon("block selection", wire::BLOCKS)
    }

    /// Drops the answer the focused chat armed, if it did, as a key that
    /// doesn't reach it is had elsewhere: only the number again, or Enter,
    /// in the chat, may give it.
    fn disarm_focused_chat(&mut self) {
        if let Some(pane) = self.focused_pane() {
            pane.disarm();
        }
    }

    /// `:AgentUndo` and `:AgentRedo`: the focused chat undoes turn `turn`,
    /// or redoes it if `redo`, `force`d with `!`.
    fn agent_undo(&mut self, turn: Option<usize>, redo: bool, force: bool) {
        let asked = (self.focused_pane()).map_or_else(
            || Err("that's for a chat with an agent".to_owned()),
            |pane| pane.undo_turn(turn, redo, force),
        );
        if let Err(why) = asked {
            self.refuse(why);
        }
    }

    /// Hands `press` to the focused explorer, board or review, unless it
    /// stops a macro being recorded. Returns whether the window must be
    /// redrawn, or `None` if the focused pane is none of them.
    fn list_key(&mut self, press: &KeyPress<'_>) -> Option<bool> {
        if self.focused_explorer().is_some() || self.focused_board().is_some() {
            if self.stop_recording_in_explorer(press) || self.stop_recording_in_board(press) {
                return Some(true);
            }
            return Some(self.explorer_key(press) || self.board_key(press));
        }
        if self.focused_browser().is_some() {
            return Some(self.browser_key(press));
        }
        if self.focused_review().is_some() {
            return Some(self.stop_recording_in_review(press) || self.review_key(press));
        }
        None
    }

    /// Pastes `text` into the command line, if it's open, or else the
    /// focused pane.
    pub(crate) fn paste(&mut self, text: &str) {
        if self
            .hold_while_waiting(false, || editors::Held::Paste(text.to_owned()))
            .is_some()
        {
            return;
        }
        if let Some(input) = &mut self.input {
            input.line.put(text);
            input.completing = None;
        } else if self.finding() {
            self.paste_into_finder(text);
        } else if !self.paste_into_editor(text)
            && let Some(pane) = self.focused_pane()
        {
            pane.paste(text);
        }
    }

    /// Scrolls the pane at `point` `lines` up, or down if negative, as the
    /// mouse wheel does. Returns whether the window must be redrawn.
    pub(crate) fn wheel(&mut self, point: [f32; 2], lines: i64, modifiers: Modifiers) -> bool {
        if let Some((pane, _)) = self.editor_at(point) {
            return self.editor_wheel(pane, lines);
        }
        if let Some((pane, _)) = self.explorer_at(point) {
            return self.explorer_wheel(pane, lines);
        }
        if let Some((pane, _)) = self.board_at(point) {
            return self.board_wheel(pane, lines);
        }
        if let Some((pane, _)) = self.review_at(point) {
            return self.review_wheel(pane, lines);
        }
        // Away from the panes, as over the bars, the focused pane scrolls.
        let Some((terminal, cell, pixel)) = self
            .cell_at(point, None)
            .or_else(|| Some((self.focused_terminal()?, [0, 0], [-1.0; 2])))
        else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.wheel(lines, (cell, pixel), modifiers))
    }

    // --- The mouse --------------------------------------------------------

    /// Handles a mouse button going down or up, or the mouse moving, at
    /// `point`: `clicks` is 2 for a double click and 3 for a triple. A
    /// press focuses the pane it's in, and that pane gets the drag that
    /// follows, wherever it goes.
    #[allow(clippy::too_many_lines, reason = "a short route for each kind of pane")]
    pub(crate) fn mouse(
        &mut self,
        point: [f32; 2],
        action: MouseAction,
        clicks: u8,
        modifiers: Modifiers,
    ) -> MouseOutcome {
        let mut outcome = MouseOutcome::default();
        // A click gives up waiting for an editor's text, as any key but
        // those insert mode types does.
        if let MouseAction::Press(_) = action {
            self.stop_waiting_to_insert(None);
        }
        // A drag that began in an editor is its, wherever it goes.
        if let Some(redraw) = self.editor_drag_mouse(point, action) {
            outcome.redraw = redraw;
            return outcome;
        }
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.editor_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                let focused = self.focus_at(point);
                // Rendered Markdown takes a click on a link, and selects
                // nothing.
                let clicked = self.rendered_click(point)
                    || self.editor_press(pane, cell, clicks, modifiers.alt);
                outcome.redraw = focused | clicked;
            }
            return outcome;
        }
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.explorer_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                let focused = self.focus_at(point);
                let clicked = self.acting_from_list(|desk| desk.explorer_click(pane, cell, clicks));
                outcome.redraw = focused | clicked;
            }
            return outcome;
        }
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.board_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                let focused = self.focus_at(point);
                let clicked = self.acting_from_list(|desk| desk.board_click(pane, cell));
                outcome.redraw = focused | clicked;
            }
            return outcome;
        }
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.review_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                let focused = self.focus_at(point);
                let clicked = self.acting_from_list(|desk| desk.review_click(pane, cell, clicks));
                outcome.redraw = focused | clicked;
            }
            return outcome;
        }
        let dragging = self.dragging.map(|(terminal, _)| terminal);
        let Some((terminal, cell, pixel)) = self.cell_at(point, dragging) else {
            // Over a bar, or a gap: the pane it was over has it no more.
            outcome.redraw |= self.mouse_left();
            return outcome;
        };
        // A click on the offer to run again what the terminal ran before a
        // restart is the offer's, not the program's.
        if dragging.is_none()
            && action == MouseAction::Press(MouseButton::Left)
            && self.ran_click(terminal, cell)
        {
            self.focus_at(point);
            outcome.redraw = true;
            return outcome;
        }
        let action = match action {
            MouseAction::Press(button) => {
                outcome.redraw = self.focus_at(point);
                self.dragging = Some((terminal, button));
                action
            }
            MouseAction::Release(_) => {
                self.dragging = None;
                action
            }
            // Motion is passed on once per cell, with the button held.
            // A chat follows the mouse by the pixel.
            MouseAction::Motion(_) => {
                let chat = self.panes.get(&terminal).is_some_and(Pane::is_chat);
                if self.mouse_cell == Some((terminal, cell)) && !chat {
                    return outcome;
                }
                MouseAction::Motion(self.dragging.map(|(_, button)| button))
            }
        };
        // The pane the mouse was over no longer has a link under it.
        if let Some((before, _)) = self.mouse_cell
            && before != terminal
        {
            outcome.redraw |= self.mouse_left();
        }
        self.mouse_cell = Some((terminal, cell));
        let block = modifiers.alt && self.daemon_has(wire::BLOCKS);
        let Some(pane) = self.panes.get_mut(&terminal) else {
            return outcome;
        };
        // A middle click pastes the primary selection, unless the program
        // gets the mouse.
        if action == MouseAction::Press(MouseButton::Middle) && !pane.reports_mouse(modifiers.shift)
        {
            outcome.paste_primary = true;
            return outcome;
        }
        let event = PaneMouse {
            action,
            line: cell[0],
            col: cell[1],
            pixel,
            clicks,
            modifiers,
            block,
        };
        outcome.redraw |= pane.mouse(&event);
        self.open_pane_link(terminal);
        outcome
    }

    /// The mouse has left the pane it was over, which no longer has a
    /// link under it. Returns whether the window must be redrawn.
    pub(crate) fn mouse_left(&mut self) -> bool {
        let Some((terminal, _)) = self.mouse_cell.take() else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.hover(None))
    }

    /// Shift, Ctrl or Alt went down or up: holding Ctrl underlines the
    /// link under the mouse. Returns whether the window must be redrawn.
    pub(crate) fn modifiers_changed(&mut self, modifiers: Modifiers) -> bool {
        let Some((terminal, [line, col])) = self.mouse_cell else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.hover(Some((line, col, modifiers))))
    }

    /// Whether a click at `point` with `modifiers` held would open a link.
    pub(crate) fn link_at(&self, point: [f32; 2], modifiers: Modifiers) -> bool {
        let Some((terminal, [line, col], _)) = self.cell_at(point, None) else {
            return false;
        };
        self.panes.get(&terminal).is_some_and(|pane| {
            pane.link_opens(modifiers) && pane.link_in_view(line, col).is_some()
        })
    }

    /// Whether the program in the pane at `point` gets the mouse, rather
    /// than clackatoo.
    pub(crate) fn reports_mouse_at(&self, point: [f32; 2], shift: bool) -> bool {
        self.cell_at(point, None)
            .and_then(|(terminal, _, _)| self.panes.get(&terminal))
            .is_some_and(|pane| pane.reports_mouse(shift))
    }

    // --- Dividers -----------------------------------------------------------

    /// Which way the divider at `point` runs, if there is one: dragging it
    /// resizes the panes either side.
    pub(crate) fn divider_at(&self, point: [f32; 2]) -> Option<Axis> {
        if let Some(divider) = &self.dragging_divider {
            return Some(divider.axis);
        }
        Some(self.placement()?.divider_at(point)?.axis)
    }

    /// Grabs the divider at `point`, if there is one, to drag it. A double
    /// click shares its split's space equally instead. Returns whether
    /// there was one.
    pub(crate) fn grab_divider(&mut self, point: [f32; 2], clicks: u8) -> bool {
        let Some(divider) = self
            .placement()
            .and_then(|placement| placement.divider_at(point).cloned())
        else {
            return false;
        };
        if clicks == 2 {
            let equal = vec![1; divider.parts.len()];
            self.resize_split(&divider.split, equal);
        } else {
            self.dragging_divider = Some(divider);
        }
        true
    }

    /// Moves the divider being dragged to `point`. Returns whether the
    /// window must be redrawn.
    pub(crate) fn drag_divider(&mut self, point: [f32; 2]) -> bool {
        let (Some(divider), Some(area)) = (&self.dragging_divider, &self.area) else {
            return false;
        };
        let position = match divider.axis {
            Axis::LeftToRight => point[0],
            Axis::TopToBottom => point[1],
        };
        let shares = divider.shares_at(position, area);
        let path = divider.split.clone();
        let unchanged = self
            .split_shares
            .as_ref()
            .is_some_and(|(_, at, current)| *at == path && *current == shares);
        if unchanged {
            return false;
        }
        self.resize_split(&path, shares);
        true
    }

    /// Lets go of the divider being dragged. Returns whether one was.
    pub(crate) fn drop_divider(&mut self) -> bool {
        let dropped = self.dragging_divider.take().is_some();
        if self.resizes_pending == 0 {
            self.split_shares = None;
        }
        dropped
    }

    /// In normal mode, Alt with `h`, `j`, `k` or `l`: moves an edge of the
    /// focused pane a cell that way, the edge on that side if it has one,
    /// or else the other.
    fn move_edge(&mut self, direction: Direction) {
        let (Some(shown), Some(placement), Some(area)) = (self.shown, self.placement(), self.area)
        else {
            return;
        };
        let Some(tile) = placement.tile(shown.pane) else {
            return;
        };
        let rect = tile.rect;
        let middle = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
        let half_gap = area.gap / 2.0;
        let (near, far, step) = match direction {
            Direction::Left => (
                [rect.x - half_gap, middle[1]],
                [rect.x + rect.width + half_gap, middle[1]],
                -area.cell[0],
            ),
            Direction::Right => (
                [rect.x + rect.width + half_gap, middle[1]],
                [rect.x - half_gap, middle[1]],
                area.cell[0],
            ),
            Direction::Up => (
                [middle[0], rect.y - half_gap],
                [middle[0], rect.y + rect.height + half_gap],
                -area.cell[1],
            ),
            Direction::Down => (
                [middle[0], rect.y + rect.height + half_gap],
                [middle[0], rect.y - half_gap],
                area.cell[1],
            ),
        };
        let Some(divider) = placement
            .divider_at(near)
            .or_else(|| placement.divider_at(far))
        else {
            return;
        };
        let shares = divider.shares_at(divider.position(&area) + step, &area);
        let path = divider.split.clone();
        self.resize_split(&path, shares);
    }

    /// Shows the split at `path` in the tab shown with `shares` at once,
    /// and asks the daemon to keep them.
    fn resize_split(&mut self, path: &[u16], shares: Vec<u32>) {
        let Some(tab) = self.shown.map(|shown| shown.tab) else {
            return;
        };
        self.split_shares = Some((tab, path.to_vec(), shares.clone()));
        self.resizes_pending += 1;
        self.request(
            Request::ResizeSplit {
                tab,
                split: path.to_vec(),
                shares,
            },
            Asked::Resize,
        );
        self.fit_panes();
    }

    /// The window gained or lost the focus: the focused pane's program is
    /// told, if it asked. Losing it ends a drag, whose release may then
    /// never come, as in a chat, which would scroll on by itself.
    pub(crate) fn window_focus(&mut self, focused: bool) {
        self.unfocused = !focused;
        self.say_present(focused);
        // What finished in the tab shown meanwhile is seen now.
        self.see_shown();
        if !focused
            && let Some((terminal, _)) = self.dragging.take()
            && let Some(pane) = self.panes.get_mut(&terminal)
        {
            pane.drag_lost();
        }
        if !focused {
            self.editor_drag_lost();
        }
        if let Some(pane) = self.focused_pane() {
            // An answer armed in a chat waits for no one away.
            if !focused {
                pane.disarm();
            }
            pane.focus_changed(focused);
        }
    }

    /// The terminal of the pane at `point`, or of the pane showing `within`
    /// wherever `point` is, and the cell of its view under `point`, which
    /// is off the view if `point` is off the pane, and `point` from the
    /// pane's top left.
    fn cell_at(
        &self,
        point: [f32; 2],
        within: Option<TerminalId>,
    ) -> Option<(TerminalId, [i64; 2], [f32; 2])> {
        let area = self.area.as_ref()?;
        let placement = self.placement()?;
        let tab = self.shown_tab()?;
        let panes = tab.layout.panes();
        let terminal_of = |id: PaneId| {
            let pane = panes.iter().find(|pane| pane.id == id)?;
            pane.content.terminal()
        };
        let (terminal, tile) = if let Some(within) = within {
            placement.panes.iter().find_map(|&(id, tile)| {
                (terminal_of(id) == Some(within)).then_some((within, tile))
            })?
        } else {
            let id = placement.pane_at(point)?;
            let tile = placement.panes.iter().find(|(pane, _)| *pane == id)?.1;
            (terminal_of(id)?, tile)
        };
        #[allow(clippy::cast_possible_truncation)]
        let cell = |offset: f32, size: f32| (offset / size).floor() as i64;
        let pixel = [point[0] - tile.origin[0], point[1] - tile.origin[1]];
        Some((
            terminal,
            [cell(pixel[1], area.cell[1]), cell(pixel[0], area.cell[0])],
            pixel,
        ))
    }

    /// The editor at `point`, and the cell of its view under it.
    fn editor_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.editor(pane).is_some())
    }

    /// The explorer at `point`, if there's one there, and the cell of it
    /// there, as [`editor_at`](Self::editor_at) finds an editor's.
    fn explorer_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.explorer(pane).is_some())
    }

    /// The review at `point`, if there's one there, and the cell of it
    /// there.
    fn review_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.review(pane).is_some())
    }

    /// The board pane at `point`, if there's one there, and the cell of it
    /// there.
    fn board_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.board(pane).is_some())
    }

    /// The cell of `pane`'s tile at `point`, as its line and column, which
    /// is off the tile if `point` is, as a drag out of it goes: if the pane
    /// is shown.
    fn cell_of(&self, point: [f32; 2], pane: PaneId) -> Option<[i64; 2]> {
        let area = self.area.as_ref()?;
        let tile = (self.placement()?.panes.iter())
            .find(|(id, _)| *id == pane)?
            .1;
        #[allow(clippy::cast_possible_truncation)]
        let cell = |offset: f32, size: f32| (offset / size).floor() as i64;
        Some([
            cell(point[1] - tile.origin[1], area.cell[1]),
            cell(point[0] - tile.origin[0], area.cell[0]),
        ])
    }

    /// The pane at `point`, if `wanted` takes it, and the cell of it there,
    /// as its line and column.
    fn cell_in(
        &self,
        point: [f32; 2],
        wanted: impl Fn(&Self, PaneId) -> bool,
    ) -> Option<(PaneId, [i64; 2])> {
        let id = self.placement()?.pane_at(point)?;
        if !wanted(self, id) {
            return None;
        }
        Some((id, self.cell_of(point, id)?))
    }

    /// Focuses the pane beside the focused one towards `direction`, if
    /// there is one.
    fn focus_towards(&mut self, direction: Direction) {
        if let Some(shown) = self.shown
            && let Some(next) = self
                .placement()
                .and_then(|placement| placement.neighbour(shown.pane, direction))
        {
            self.show_pane(next);
        }
    }

    /// Carries out a command.
    #[allow(clippy::too_many_lines, reason = "one arm per command, each short")]
    fn run(&mut self, command: Command) {
        match command {
            Command::Reload => return self.reload_lua(),
            Command::Set(words) => return self.run_set(&words),
            Command::Update(host) => return self.ask_update(host),
            Command::Connect { host, share } => return self.ask_connect(host, share),
            Command::Disconnect(host) => return self.ask_disconnect(host),
            Command::PortForward(forwarding) => return self.port_forward(forwarding),
            Command::On { host, command } => return self.run_on(&host, *command),
            Command::Workspace(name) => return self.switch_workspace(name),
            _ => {}
        }
        let Some(shown) = self.shown else {
            // With no workspace shown, a new pane opens one.
            if let Command::Open { kind, agent, .. } = command {
                let terminal = new_pane(kind, agent, self.new_pane_size(None), &self.options);
                let new = Request::NewWorkspace {
                    name: None,
                    terminal,
                    on: None,
                };
                self.request(new, Asked::Pane);
            }
            return;
        };
        // A new pane beside a folder, or a path, is from the folder shown.
        self.root_folder_first(shown.pane);
        let new = |kind, agent, size| new_pane(kind, agent, size, &self.options);
        match command {
            // A chat with no agent named: the start page, to pick one.
            Command::Open {
                place,
                kind: Kind::Agent,
                agent: None,
            } => self.open_start_page(place),
            Command::Open {
                place: Place::Tab | Place::Here,
                kind,
                agent,
            } => {
                let terminal = new(kind, agent, self.new_pane_size(None));
                self.request(
                    Request::NewTab {
                        workspace: shown.workspace,
                        terminal,
                        on: None,
                    },
                    Asked::Pane,
                );
            }
            Command::Open {
                place: Place::Beside(side),
                kind,
                agent,
            } => {
                let terminal = new(kind, agent, self.new_pane_size(Some(side)));
                self.request(
                    Request::Split {
                        pane: shown.pane,
                        side,
                        terminal,
                        on: None,
                    },
                    Asked::Pane,
                );
            }
            // In an editor, a number is a line, as in Vim.
            Command::Tab(number) if self.focused_editor().is_some() => {
                self.editor_command(&number.to_string());
            }
            Command::Tab(number) => self.show_tab_numbered(shown.workspace, number),
            Command::Rename(name) => self.request(
                Request::RenameWorkspace {
                    workspace: shown.workspace,
                    name,
                },
                Asked::Command,
            ),
            // Not a buffer with no file yet with changes, which would go
            // with its pane: a file's waits, unseen, to be opened again.
            Command::Close if self.loses_blank(&[shown.pane]) => self.say(BLANK_KEPT.into()),
            Command::Close => {
                self.request(Request::ClosePane { pane: shown.pane }, Asked::Command);
            }
            Command::Edit { place, path } => self.open_file(place, path),
            Command::EditBlank => self.edit_blank(),
            Command::Quit { force } => self.quit(force),
            Command::Tutor(topic) => self.tutor(topic.as_deref()),
            Command::Write { force, quit, name } => self.write_file(force, quit, name),
            Command::Discard => self.discard_changes(),
            Command::Recover { drop } => self.recover(drop),
            Command::Format => self.format_file(),
            Command::Markdown => self.toggle_markdown(),
            Command::Find { kind, query } => self.open_finder(kind, query),
            Command::Hunk(action) => self.editor_hunk(action),
            Command::Registers => self.show_registers(),
            Command::Messages => self.show_messages(),
            Command::Devices(command) => self.devices_command(command),
            Command::Marks => self.open_finder(crate::finder::Kind::Marks, String::new()),
            Command::Editor(line) => self.editor_command(&line),
            Command::Explore {
                dir,
                edit,
                side: None,
            } => self.open_explorer(dir, edit),
            Command::Explore {
                dir,
                edit,
                side: Some(side),
            } => self.open_explorer_beside(dir, edit, side),
            Command::AgentUndo { turn, redo, force } => self.agent_undo(turn, redo, force),
            Command::Board { place, name } => self.open_board(name, place),
            Command::Browser(place) => self.open_browser(place),
            Command::Review(review) => self.run_review(review),
            Command::Background(command) => self.background_command(command),
            Command::BackgroundStart(command) => self.start_in_background(command),
            Command::Reload
            | Command::Set(_)
            | Command::Update(_)
            | Command::Connect { .. }
            | Command::Disconnect(_)
            | Command::PortForward(_)
            | Command::On { .. }
            | Command::Workspace(_) => {}
        }
    }

    /// Shows tab `number` of `workspace`, counting from 1, or says there's
    /// no such tab.
    fn show_tab_numbered(&mut self, workspace: WorkspaceId, number: usize) {
        let tabs =
            (self.workspaces.get(workspace)).map_or(&[][..], |workspace| &workspace.tabs[..]);
        match number.checked_sub(1).and_then(|index| tabs.get(index)) {
            Some(tab) => self.show_pane(tab.focused),
            None => self.refuse(match tabs.len() {
                1 => format!("there's no tab {number}; this workspace has one"),
                count => format!("there's no tab {number}; this workspace has {count}"),
            }),
        }
    }

    /// About the size a new pane will be: the size of every pane together,
    /// for a new tab or workspace, or half the focused pane's, beside it on
    /// `side`. The window resizes it to fit as soon as it's shown.
    fn new_pane_size(&self, side: Option<Side>) -> Size {
        let fallback = Size::new(80, 24).expect("80x24 is a valid size");
        let Some(area) = self.area else {
            return fallback;
        };
        let whole = Placement::new(
            &clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(0),
                content: Content::Terminal(TerminalId(0)),
                home: None,
                key: None,
            }),
            &area,
        )
        .panes[0]
            .1;
        let focused = self
            .shown
            .zip(self.placement())
            .and_then(|(shown, placement)| placement.tile(shown.pane));
        let (cols, rows) = match (side, focused) {
            (Some(side), Some(tile)) => match side.axis() {
                Axis::LeftToRight => ((tile.cols / 2).max(1), tile.rows),
                Axis::TopToBottom => (tile.cols, (tile.rows / 2).max(1)),
            },
            _ => (whole.cols, whole.rows),
        };
        Size::new(cols, rows).unwrap_or(fallback)
    }
}

/// What to start in a new pane of `kind`, of `size`: for a chat, with
/// `agent`, or else the one the `agent` option names, or else the
/// daemon's default.
fn new_pane(kind: Kind, agent: Option<String>, size: Size, options: &Options) -> PaneTerminal {
    match kind {
        Kind::Terminal => PaneTerminal::New(NewTerminal {
            size,
            command: None,
            cwd: None,
            record_to: None,
        }),
        Kind::Agent => PaneTerminal::Agent(NewAgent {
            size,
            cwd: None,
            agent: agent.or_else(|| Some(options.agent.clone()).filter(|name| !name.is_empty())),
            tools: options.agenttools.asked(),
            task: options.claudetask,
            browser: options.chatbrowser,
            model: None,
            resume: None,
        }),
    }
}

/// The bar's name for `editor`'s mode: visual mode the mouse started
/// from insert mode is `(insert) VISUAL`, as in Neovim.
fn editor_mode_name(editor: &crate::editor::Editor) -> ModeName {
    match mode_name(editor.mode) {
        ModeName::Visual if editor.visual_from_insert() => ModeName::InsertVisual,
        ModeName::VisualLine if editor.visual_from_insert() => ModeName::InsertVisualLine,
        ModeName::VisualBlock if editor.visual_from_insert() => ModeName::InsertVisualBlock,
        name => name,
    }
}

/// The bar's name for `mode`.
fn mode_name(mode: Mode) -> ModeName {
    match mode {
        Mode::Insert => ModeName::Insert,
        Mode::Normal => ModeName::Normal,
        Mode::Visual(Selection::Characters) => ModeName::Visual,
        Mode::Visual(Selection::Lines) => ModeName::VisualLine,
        Mode::Visual(Selection::Block) => ModeName::VisualBlock,
    }
}

/// Whether `ch` may show in a tab's name: not a control character, nor
/// one that changes which way text runs (the marks U+200E and U+200F, the
/// embeddings and overrides U+202A to U+202E, and the isolates U+2066 to
/// U+2069), which a title or a folder's name could hold.
pub(super) fn shown_in_names(ch: char) -> bool {
    !ch.is_control()
        && !matches!(ch, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    mod background;
    mod blank;
    mod folders;
    mod lessons;
    mod machines;
    mod markdown;
    mod messages;
    mod mode;
    mod mouse;
    mod restored;
    mod typing;

    use clackatoo_protocol::{ChatState, ChatSummary, Layout, Pane as LayoutPane, View, Workspace};
    use clackatoo_term::mouse::{MouseAction, MouseButton};
    use clackatoo_term::{Config, Terminal};
    use winit::keyboard::{KeyCode, KeyLocation};

    use super::*;
    use crate::chrome::Mark;
    use crate::input::Modifiers;

    fn pane(id: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
            home: None,
            key: None,
        })
    }

    fn side_by_side(parts: Vec<Layout>) -> Layout {
        Layout::Split {
            axis: Axis::LeftToRight,
            shares: vec![1; parts.len()],
            parts,
        }
    }

    /// A tab, focused on its first pane.
    fn tab(id: u32, layout: Layout) -> Tab {
        Tab {
            id: TabId(id),
            focused: layout.panes()[0].id,
            layout,
            name: None,
        }
    }

    fn workspace(id: u32, name: &str, tabs: Vec<Tab>) -> Workspace {
        Workspace {
            id: WorkspaceId(id),
            name: name.into(),
            active_tab: tabs[0].id,
            tabs,
            home: None,
            lent: None,
            background: Vec::new(),
        }
    }

    fn workspaces(list: Vec<Workspace>) -> Workspaces {
        Workspaces {
            active: Some(list[0].id),
            list,
            home: None,
            restored: Vec::new(),
            machine: None,
            others: Vec::new(),
            passed: Vec::new(),
        }
    }

    /// A desk with 810 × 480 pixels for panes, connected to `workspaces`, with
    /// what it asked for so far answered.
    fn desk(workspaces: Workspaces) -> Desk {
        let mut desk = Desk::default();
        desk.set_area(Some(Area {
            rect: crate::layout::Rect {
                x: 0.0,
                y: 0.0,
                width: 810.0,
                height: 480.0,
            },
            cell: [10.0, 20.0],
            gap: 10.0,
            inset: 5.0,
        }));
        desk.connected(workspaces, false);
        answer_all(&mut desk);
        desk
    }

    /// Answers every request waiting with "done", and returns them.
    fn answer_all(desk: &mut Desk) -> Vec<Request> {
        let requests: Vec<Request> = desk
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                ClientMessage::FrameDone { .. }
                | ClientMessage::Edit { .. }
                | ClientMessage::Query { .. }
                | ClientMessage::Rename { .. }
                | ClientMessage::Find { .. }
                | ClientMessage::StopFinding { .. }
                | ClientMessage::Stage { .. }
                | ClientMessage::Clipboard { .. }
                | ClientMessage::Files { .. }
                | ClientMessage::Board { .. }
                | ClientMessage::FileOp { .. }
                | ClientMessage::Hello { .. }
                | ClientMessage::Present { .. }
                | ClientMessage::ListSessions { .. }
                | ClientMessage::Colour { .. }
                | ClientMessage::Place { .. }
                | ClientMessage::Dismiss { .. }
                | ClientMessage::Login { .. }
                | ClientMessage::Upgrade { .. } => None,
            })
            .collect();
        for _ in &requests {
            desk.answered(Response::Done);
        }
        requests
    }

    /// Gives `terminal` its first screen, as attaching does.
    fn snapshot(desk: &mut Desk, terminal: u32) {
        let screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        desk.view_changed(
            TerminalId(terminal),
            &ViewUpdate::Snapshot(View::capture(&screen)),
        );
    }

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(desk: &mut Desk, key: Key, code: Option<KeyCode>, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(text) if !modifiers.ctrl => Some(text.to_string()),
            _ => None,
        };
        desk.key(&KeyPress {
            key: &key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            code,
            modifiers,
        });
    }

    const CTRL: Modifiers = Modifiers {
        shift: false,
        ctrl: true,
        alt: false,
    };

    fn ctrl(desk: &mut Desk, letter: &str) {
        press(desk, Key::Character(letter.into()), None, CTRL);
    }

    fn normal_mode(desk: &mut Desk) {
        press(
            desk,
            Key::Character(";".into()),
            Some(KeyCode::Semicolon),
            CTRL,
        );
    }

    /// Types `line` on the command line, and Enter.
    fn command(desk: &mut Desk, line: &str) {
        for ch in format!(":{line}").chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
        press(
            desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
    }

    fn mode(desk: &Desk, terminal: u32) -> Mode {
        desk.pane(TerminalId(terminal)).expect("a pane").mode
    }

    fn shown_pane(desk: &Desk) -> PaneId {
        desk.shown().expect("something shown").pane
    }

    /// The agent each chat asked for among `requests` would talk to.
    fn agents_asked_for(requests: &[Request]) -> Vec<Option<String>> {
        (requests.iter())
            .filter_map(|request| match request {
                Request::NewTab {
                    terminal: PaneTerminal::Agent(new),
                    ..
                }
                | Request::Split {
                    terminal: PaneTerminal::Agent(new),
                    ..
                } => Some(new.agent.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_chat_talks_to_the_agent_named_or_else_the_start_pages_pick() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1)]))],
        )]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        // Named, it starts at once.
        command(&mut desk, "Agent codex");
        assert!(!desk.finding());
        // Otherwise the page picks the agent the option names, and Enter
        // starts it, before the daemon has said what there is.
        command(&mut desk, "set agent=gemini");
        command(&mut desk, "|Agent");
        assert!(desk.finding(), "the start page is open");
        enter(&mut desk);
        // Then the one last started from it.
        command(&mut desk, "set agent=");
        command(&mut desk, "-Agent");
        enter(&mut desk);
        assert_eq!(
            agents_asked_for(&answer_all(&mut desk)),
            [
                Some("codex".to_owned()),
                Some("gemini".to_owned()),
                Some("gemini".to_owned()),
            ]
        );
        // With none named, and none started from it, it's the daemon's
        // default.
        let mut other = self::desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut other, 1);
        normal_mode(&mut other);
        command(&mut other, "set agent=");
        ctrl_b(&mut other, "a");
        enter(&mut other);
        assert_eq!(agents_asked_for(&answer_all(&mut other)), [None]);
    }

    /// Answers the requests waiting: what a chat could be with `choices`,
    /// and the rest with "done". Returns them, and the agents' sessions
    /// asked for, by their numbers.
    fn answer_choices(
        desk: &mut Desk,
        choices: &clackatoo_protocol::ChatChoices,
    ) -> (Vec<Request>, Vec<u64>) {
        let mut requests = Vec::new();
        let mut listings = Vec::new();
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Request(request) => requests.push(request),
                ClientMessage::ListSessions { id, agent, cwd } => {
                    assert_eq!((agent.as_str(), cwd.as_str()), ("claude", "/home/me/x"));
                    listings.push(id);
                }
                _ => {}
            }
        }
        for request in &requests {
            desk.answered(match request {
                Request::ChatChoices { .. } => Response::ChatChoices(choices.clone()),
                _ => Response::Done,
            });
        }
        // What the choices asked for.
        for message in desk.take_outbox() {
            if let ClientMessage::ListSessions { id, .. } = message {
                listings.push(id);
            }
        }
        (requests, listings)
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "one way to go on after another")]
    fn the_start_page_starts_shows_or_resumes_a_chat_where_agent_asked() {
        use clackatoo_protocol::{
            AgentSession, ChatChoices, ChatState, ChatSummary, Provider, ProviderModels,
            ResumeSession,
        };
        let summary = |chat: u32, workspace: u32| ChatSummary {
            chat: TerminalId(chat),
            title: format!("Chat {chat}"),
            agent: "Claude Code".to_owned(),
            state: ChatState::Ready,
            asking: false,
            unseen: false,
            parent: None,
            workspace: Some(WorkspaceId(workspace)),
            active: 0,
        };
        let choices = ChatChoices {
            agents: vec![Provider {
                agent: "claude".to_owned(),
                title: "Claude Code".to_owned(),
                models: ProviderModels::Unknown,
            }],
            default: "claude".to_owned(),
            cwd: "/home/me/x".to_owned(),
            home: "/home/me".to_owned(),
            // Another workspace's isn't this one's to show.
            unshown: vec![summary(7, 1), summary(8, 2)],
        };
        let kept = AgentSession {
            session: "past-1".to_owned(),
            title: "Fix the parser".to_owned(),
            cwd: "/home/me/x".to_owned(),
            updated: None,
            chat: None,
        };
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);

        // A chat in no pane, shown beside.
        command(&mut desk, "|Agent");
        let (requests, listings) = answer_choices(&mut desk, &choices);
        assert_eq!(
            requests,
            [Request::ChatChoices {
                pane: Some(PaneId(1))
            }]
        );
        assert_eq!(listings.len(), 1, "the agent's sessions asked for");
        assert!(desk.sessions_listed(listings[0], Ok(vec![kept.clone()])));
        let rows = desk.finder_view((10, 60), None).unwrap().rows;
        assert_eq!(rows.len(), 3, "the new chat, chat 7, the session");
        type_keys(&mut desk, "j");
        enter(&mut desk);
        assert!(!desk.finding(), "closed");
        assert_eq!(
            answer_all(&mut desk),
            [Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::Existing(TerminalId(7)),
                on: None,
            }]
        );

        // A session the agent kept, gone on with in a new tab: what's
        // typed picks it.
        command(&mut desk, "Agent");
        let (_, listings) = answer_choices(&mut desk, &choices);
        // One no longer wanted is dropped.
        assert!(!desk.sessions_listed(listings[0] + 1, Ok(Vec::new())));
        desk.sessions_listed(listings[0], Ok(vec![kept.clone()]));
        type_keys(&mut desk, "parser");
        enter(&mut desk);
        let requests = answer_all(&mut desk);
        let [
            Request::NewTab {
                terminal: PaneTerminal::Agent(new),
                ..
            },
        ] = &requests[..]
        else {
            panic!("{requests:?}");
        };
        assert_eq!(new.agent.as_deref(), Some("claude"));
        assert_eq!(new.cwd.as_deref(), Some("/home/me/x"));
        assert_eq!(
            new.resume,
            Some(ResumeSession {
                session: "past-1".to_owned(),
                title: "Fix the parser".to_owned(),
            })
        );

        // `n` starts a new chat, whatever's picked; Esc closes, asking
        // nothing.
        command(&mut desk, "Agent");
        answer_choices(&mut desk, &choices);
        type_keys(&mut desk, "jn");
        let requests = answer_all(&mut desk);
        assert!(
            matches!(&requests[..], [Request::NewTab { terminal: PaneTerminal::Agent(new), .. }]
                if new.resume.is_none() && new.cwd.as_deref() == Some("/home/me/x")),
            "{requests:?}"
        );
        command(&mut desk, "Agent");
        answer_choices(&mut desk, &choices);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(!desk.finding());
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn an_agent_points_out_panes_but_never_moves_the_typing() {
        let mut desk = desk(workspaces(vec![
            workspace(
                1,
                "work",
                vec![
                    tab(1, side_by_side(vec![pane(1), pane(2)])),
                    tab(2, pane(3)),
                ],
            ),
            workspace(2, "play", vec![tab(3, pane(4))]),
        ]));
        snapshot(&mut desk, 1);
        snapshot(&mut desk, 2);
        assert_eq!(shown_pane(&desk), PaneId(1));
        // A pane beside: lit, and the typing stays.
        desk.agent_shows(PaneId(2), None);
        assert_eq!(desk.pointed(), Some(PaneId(2)));
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert!(
            desk.flash_ends().is_some(),
            "the window wakes to unlight it"
        );
        ctrl_b(&mut desk, "");
        ctrl(&mut desk, "b");
        assert!(answer_all(&mut desk).contains(&Request::SendInput {
            terminal: TerminalId(1),
            bytes: vec![0x02],
        }));
        // In another tab: the tab is marked, and still nothing moves.
        desk.agent_shows(PaneId(3), None);
        assert_eq!(shown_pane(&desk), PaneId(1));
        let names = desk.tab_names();
        assert!(names[1].1.ends_with(" ●"), "{names:?}");
        assert!(!names[0].1.ends_with(" ●"));
        // Never another workspace, or a pane that's gone.
        desk.agent_shows(PaneId(4), None);
        desk.agent_shows(PaneId(9), None);
        assert_eq!(desk.pointed(), Some(PaneId(3)));
        assert_eq!(shown_pane(&desk), PaneId(1));
    }

    #[test]
    fn a_terminal_tab_with_no_title_is_named_after_its_folder_from_the_daemons_home() {
        let tabs = vec![
            tab(1, pane(1)),
            tab(2, pane(2)),
            tab(3, pane(3)),
            tab(4, pane(4)),
        ];
        let mut list = workspaces(vec![workspace(1, "w", tabs)]);
        list.home = Some("/home/me".into());
        let mut desk = desk(list.clone());
        for (terminal, output) in [
            (1, "\x1b]7;file://host/home/me/src\x07"),
            (2, "\x1b]7;file://host/home/me\x07"),
            (3, ""),
            // What a folder's name holds is decoded, escapes and all.
            (
                4,
                "\x1b]7;file://host/home/me/a%1B[31m%09b%E2%80%AEc%E2%81%A6d\x07",
            ),
        ] {
            let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
            screen.advance(output.as_bytes());
            desk.view_changed(
                TerminalId(terminal),
                &ViewUpdate::Snapshot(View::capture(&screen)),
            );
        }
        let names = |desk: &Desk| -> Vec<String> {
            (desk.tab_names().into_iter())
                .map(|(_, name, _)| name)
                .collect()
        };
        assert_eq!(names(&desk), ["1 src", "2 ~", "3", "4 a[31mbcd"]);
        // A daemon too old to say its home: this machine's, if it's on it.
        list.home = None;
        desk.workspaces_changed(list);
        desk.own_home = Some("/home/me".into());
        assert_eq!(names(&desk), ["1 src", "2 ~", "3", "4 a[31mbcd"]);
        desk.remote = true;
        assert_eq!(names(&desk), ["1 src", "2 me", "3", "4 a[31mbcd"]);
        // Renaming starts from the name as shown: Enter mustn't store a
        // line break.
        desk.rename_tab(TabId(4));
        assert_eq!(desk.input.as_ref().unwrap().line.text(), "a[31mbcd");
    }

    #[test]
    fn a_tab_with_a_chat_at_work_says_so_and_nothing_wakes_once_its_done() {
        use clackatoo_protocol::{ChatPatch, ChatState, Outcome, Patch, Turn, TurnEnd};

        let (mut desk, turn) = working_chat_desk();
        let names = desk.tab_names();
        assert!(names[1].1.ends_with(Mark::Working.suffix()), "{names:?}");
        assert!(!names[0].1.contains(Mark::Working.suffix()), "{names:?}");
        // Its long title, cut short, says so once.
        assert!(!names[1].1.contains('…'), "{names:?}");
        // Drawn, it wakes the window to count.
        let mut fonts = crate::chat::fixtures::fonts();
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.chat_wakes().is_some());
        // Its turn ends: the tab, though not shown, is drawn again, without
        // the mark, and nothing wakes the window.
        let done = Turn {
            end: Some(TurnEnd {
                at: 2_000,
                outcome: Outcome::Done,
            }),
            ..turn
        };
        assert!(desk.view_changed(
            TerminalId(2),
            &ViewUpdate::Patch(vec![
                Patch::Chat(ChatPatch::Turn {
                    index: 0,
                    turn: done,
                }),
                Patch::Chat(ChatPatch::State(ChatState::Ready)),
            ]),
        ));
        assert!(!desk.tab_names()[1].1.contains(Mark::Working.suffix()));
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.chat_wakes().is_none(), "zero CPU when idle");
    }

    #[test]
    fn a_chats_code_is_asked_to_be_coloured_once_and_its_colours_go_to_it() {
        use clackatoo_protocol::{ChatPatch, Item, Patch, Span, wire};

        let (mut desk, mut turn) = working_chat_desk();
        turn.items = vec![Item::Words("Here:\n\n```rust\nfn main() {}\n```".into())];
        desk.view_changed(
            TerminalId(2),
            &ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn { index: 0, turn })]),
        );
        let mut fonts = crate::chat::fixtures::fonts();
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.ask_colours());
        let asks: Vec<(u64, Vec<clackatoo_protocol::Code>)> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Colour { id, code } => Some((id, code)),
                _ => None,
            })
            .collect();
        assert_eq!(asks.len(), 1);
        let (id, code) = &asks[0];
        assert_eq!(code[0].language, "rust");
        assert_eq!(code[0].text, "fn main() {}");
        assert_eq!(desk.colour_asker(*id), Some(TerminalId(2)));
        // Drawn again, it isn't asked again.
        paint_chat_two(&mut desk, &mut fonts);
        assert!(!desk.ask_colours());
        // The answer goes to the chat, once, and it's drawn again.
        let spans = vec![vec![Span {
            start: 0,
            end: 2,
            kind: clackatoo_protocol::Kind::Keyword,
        }]];
        assert!(desk.coloured(*id, spans.clone()));
        assert!(!desk.coloured(*id, spans), "answered already");
        // A daemon too old to colour chats' code isn't asked: the code is
        // plain.
        let (mut desk, mut turn) = working_chat_desk();
        desk.daemon_is(wire::COLOURS - 1, None);
        turn.items = vec![Item::Words("```rust\nlet x = 1;\n```".into())];
        desk.view_changed(
            TerminalId(2),
            &ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn { index: 0, turn })]),
        );
        paint_chat_two(&mut desk, &mut fonts);
        assert!(!desk.ask_colours());
        assert!(
            !(desk.take_outbox().iter())
                .any(|message| matches!(message, ClientMessage::Colour { .. }))
        );
    }

    #[test]
    fn ctrl_v_in_a_terminal_needs_a_daemon_that_can_copy_a_block() {
        let mut desk = three_tabs();
        let focused = desk.focused_terminal().unwrap().0;
        snapshot(&mut desk, focused);
        normal_mode(&mut desk);
        desk.daemon_is(wire::BLOCKS - 1, None);
        ctrl(&mut desk, "v");
        assert_eq!(
            desk.focused_pane().map(|pane| pane.mode),
            Some(Mode::Normal)
        );
        assert!(
            (desk.message.as_deref())
                .is_some_and(|message| message.starts_with("block selection needs a newer daemon")),
            "{:?}",
            desk.message
        );
        desk.daemon_is(wire::BLOCKS, None);
        ctrl(&mut desk, "v");
        assert_eq!(
            desk.focused_pane().map(|pane| pane.mode),
            Some(Mode::Visual(Selection::Block))
        );
        // A block selected before the daemon went back to an older one is
        // copied from corner to corner, never asked of it as a block.
        desk.take_outbox();
        desk.daemon_is(wire::BLOCKS - 1, None);
        type_keys(&mut desk, "y");
        let sent = desk.take_outbox();
        assert!(sent.iter().any(|message| matches!(
            message,
            ClientMessage::Request(Request::Text {
                whole_lines: false,
                ..
            })
        )));
        assert!(
            !sent.iter().any(|message| matches!(
                message,
                ClientMessage::Request(Request::BlockText { .. })
            ))
        );
        assert!(
            (desk.message.as_deref()).is_some_and(|message| {
                message.starts_with(
                    "copied from corner to corner: copying a block needs a newer daemon",
                )
            }),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn colours_asked_for_as_the_daemon_went_are_asked_for_again_when_its_back() {
        use clackatoo_protocol::{ChatPatch, Item, Patch};

        let (mut desk, mut turn) = working_chat_desk();
        turn.items = vec![Item::Words("```rust\nfn main() {}\n```".into())];
        desk.view_changed(
            TerminalId(2),
            &ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn { index: 0, turn })]),
        );
        let mut fonts = crate::chat::fixtures::fonts();
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.ask_colours());
        desk.take_outbox();
        // Gone before it answered: nothing is asked while it's away.
        desk.lost();
        paint_chat_two(&mut desk, &mut fonts);
        assert!(!desk.ask_colours());
        // Back: the code is laid out again, and asked for again.
        let layout = vec![
            tab(1, pane(1)),
            tab(2, side_by_side(vec![pane(2), pane(3)])),
        ];
        desk.connected(workspaces(vec![workspace(1, "work", layout)]), false);
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.ask_colours());
        let asked = (desk.take_outbox().into_iter())
            .filter(|message| matches!(message, ClientMessage::Colour { .. }))
            .count();
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_chat_at_work_neither_says_so_nor_wakes_the_window_while_the_daemon_is_out_of_reach() {
        let (mut desk, _) = working_chat_desk();
        let mut fonts = crate::chat::fixtures::fonts();
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.chat_wakes().is_some());
        desk.lost();
        assert!(desk.chat_wakes().is_none(), "no wakes through hours away");
        assert!(!desk.tab_names()[1].1.contains(Mark::Working.suffix()));
        paint_chat_two(&mut desk, &mut fonts);
        assert!(desk.chat_wakes().is_none());
        let pane = desk.pane_mut(TerminalId(2)).expect("the chat's pane");
        assert!(!pane.chat_working());
    }

    /// Draws the chat in terminal 2 of [`working_chat_desk`].
    fn paint_chat_two(desk: &mut Desk, fonts: &mut crate::fonts::Fonts) {
        let pane = desk.pane_mut(TerminalId(2)).expect("the chat's pane");
        pane.paint_chat(fonts, [0.0; 2], [900.0, 1000.0], true);
    }

    /// A desk with a terminal in one tab, and in another, a chat in
    /// terminal 2, with a long title, beside a terminal: its agent is
    /// answering the turn returned.
    fn working_chat_desk() -> (Desk, clackatoo_protocol::Turn) {
        use clackatoo_protocol::{ChatNode, ChatState, Node, Turn};

        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![
                tab(1, pane(1)),
                tab(2, side_by_side(vec![pane(2), pane(3)])),
            ],
        )]));
        for terminal in [1, 3] {
            snapshot(&mut desk, terminal);
        }
        let turn = Turn {
            from: None,
            prompt: "Why does CI fail?".into(),
            sent: 1_000,
            items: Vec::new(),
            end: None,
            files: None,
            switch: None,
        };
        let node = ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Answering,
            turns: vec![turn.clone()],
            ..ChatNode::default()
        };
        // The chat, in a tab not shown, beside a terminal: its starting
        // to work is drawn.
        assert!(desk.view_changed(
            TerminalId(2),
            &ViewUpdate::Snapshot(View {
                title: "a chat whose title is far too long for a tab".into(),
                cwd: None,
                root: Node::Chat(Box::new(node)),
            }),
        ));
        (desk, turn)
    }

    /// Chat `chat`, in workspace `workspace`, as the list of chats sums it
    /// up.
    fn summary(chat: u32, workspace: u32, state: ChatState) -> ChatSummary {
        ChatSummary {
            chat: TerminalId(chat),
            title: format!("chat {chat}"),
            agent: "Claude Code".into(),
            state,
            asking: false,
            unseen: false,
            parent: None,
            workspace: Some(WorkspaceId(workspace)),
            active: 0,
        }
    }

    /// A desk showing workspace 1's first tab, a terminal, with chats in
    /// terminals 2 and 3 in its second tab, side by side, and workspace 2,
    /// not shown, with a chat in terminal 5 in its first tab and one in
    /// terminal 6 in its second.
    fn chats_desk() -> Desk {
        desk(workspaces(vec![
            workspace(
                1,
                "work",
                vec![
                    tab(1, pane(1)),
                    tab(2, side_by_side(vec![pane(2), pane(3)])),
                ],
            ),
            workspace(2, "peios", vec![tab(3, pane(5)), tab(4, pane(6))]),
        ]))
    }

    /// The marks the desk shows: each workspace's, and each tab's name.
    fn marks(desk: &Desk) -> (Vec<Option<Mark>>, Vec<String>) {
        let workspaces = (desk.workspace_names().into_iter())
            .map(|workspace| workspace.mark)
            .collect();
        let tabs = (desk.tab_names().into_iter())
            .map(|(_, name, _)| name)
            .collect();
        (workspaces, tabs)
    }

    #[test]
    fn the_window_watches_the_chats_as_it_connects() {
        let mut desk = Desk::default();
        desk.connected(
            workspaces(vec![workspace(1, "work", vec![tab(1, pane(1))])]),
            false,
        );
        assert!(answer_all(&mut desk).contains(&Request::WatchChats));
    }

    #[test]
    fn a_chat_that_finishes_in_a_tab_not_shown_is_unseen_until_its_tab_is() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        assert!(!desk.chats_changed(vec![summary(2, 1, Ready), summary(3, 1, Ready)]));
        assert_eq!(
            marks(&desk),
            (vec![None, None], vec!["1".into(), "2".into()])
        );
        // At work: the workspace says so from the list.
        assert!(desk.chats_changed(vec![summary(2, 1, Answering), summary(3, 1, Ready)]));
        assert_eq!(marks(&desk).0, [Some(Mark::Working), None]);
        // Done, while its tab wasn't shown: unseen, the tab and the
        // workspace say.
        assert!(desk.chats_changed(vec![summary(2, 1, Ready), summary(3, 1, Ready)]));
        assert_eq!(
            marks(&desk),
            (
                vec![Some(Mark::Unseen), None],
                vec!["1".into(), "2 •".into()]
            )
        );
        // The list again, the same: nothing to draw.
        assert!(!desk.chats_changed(vec![summary(2, 1, Ready), summary(3, 1, Ready)]));
        // Its tab shown: seen, and it stays so.
        desk.show_tab(TabId(2));
        assert_eq!(
            marks(&desk),
            (vec![None, None], vec!["1".into(), "2".into()])
        );
        desk.show_tab(TabId(1));
        assert_eq!(marks(&desk).1, ["1", "2"]);
    }

    #[test]
    fn a_chat_that_finishes_in_view_is_seen_and_one_going_again_is_at_work() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        desk.show_tab(TabId(2));
        desk.chats_changed(vec![summary(2, 1, Answering)]);
        desk.chats_changed(vec![summary(2, 1, Ready)]);
        desk.show_tab(TabId(1));
        assert_eq!(marks(&desk).1, ["1", "2"], "seen as it finished");
        // Unseen, then going again: at work, not unseen, and once done
        // in view, nothing.
        desk.chats_changed(vec![summary(2, 1, Answering)]);
        desk.chats_changed(vec![summary(2, 1, Ready)]);
        desk.chats_changed(vec![summary(2, 1, Answering)]);
        assert_eq!(marks(&desk).0, [Some(Mark::Working), None]);
        desk.show_tab(TabId(2));
        desk.chats_changed(vec![summary(2, 1, Ready)]);
        assert_eq!(marks(&desk).0, [None, None]);
    }

    #[test]
    fn another_workspaces_chats_mark_it_asking_first() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        let asking = ChatSummary {
            asking: true,
            ..summary(5, 2, Answering)
        };
        desk.chats_changed(vec![asking.clone(), summary(6, 2, Answering)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Asking)]);
        desk.chats_changed(vec![summary(5, 2, Answering), summary(6, 2, Answering)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Working)]);
        // One done, one still at work: at work, the workspace says.
        desk.chats_changed(vec![summary(5, 2, Ready), summary(6, 2, Answering)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Working)]);
        desk.chats_changed(vec![summary(5, 2, Ready), summary(6, 2, Ready)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        // Shown, at its first tab: chat 5 is seen, chat 6, in the tab
        // not shown, isn't.
        desk.show_workspace(WorkspaceId(2));
        assert_eq!(
            marks(&desk),
            (
                vec![None, Some(Mark::Unseen)],
                vec!["1".into(), "2 •".into()]
            )
        );
        desk.show_tab(TabId(4));
        assert_eq!(marks(&desk).0, [None, None]);
    }

    #[test]
    fn a_chat_no_one_saw_finish_is_unseen_to_a_window_new_to_it_and_a_subagent_never() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        let unseen = ChatSummary {
            unseen: true,
            ..summary(5, 2, Ready)
        };
        let subagent = ChatSummary {
            parent: Some(TerminalId(5)),
            ..summary(9, 2, Answering)
        };
        desk.chats_changed(vec![unseen, subagent.clone()]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        desk.show_workspace(WorkspaceId(2));
        desk.show_workspace(WorkspaceId(1));
        assert_eq!(marks(&desk).0, [None, None]);
        // A subagent finishing tells its chat, not the user.
        desk.chats_changed(vec![
            summary(5, 2, Ready),
            ChatSummary {
                state: Ready,
                ..subagent
            },
        ]);
        assert_eq!(marks(&desk).0, [None, None]);
        // A chat closing takes its mark with it.
        desk.chats_changed(vec![summary(5, 2, Answering)]);
        desk.chats_changed(vec![summary(5, 2, Ready)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        assert!(desk.chats_changed(Vec::new()));
        assert_eq!(marks(&desk).0, [None, None]);
    }

    #[test]
    fn a_chat_finishing_in_view_of_a_window_in_the_background_is_unseen_till_its_focused() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        desk.show_tab(TabId(2));
        desk.window_focus(false);
        desk.chats_changed(vec![summary(2, 1, Answering)]);
        desk.chats_changed(vec![summary(2, 1, Ready)]);
        assert_eq!(
            marks(&desk),
            (
                vec![Some(Mark::Unseen), None],
                vec!["1".into(), "2 •".into()]
            )
        );
        // Shown again, still in the background: still unseen.
        desk.show_tab(TabId(1));
        desk.show_tab(TabId(2));
        assert_eq!(marks(&desk).0, [Some(Mark::Unseen), None]);
        // Focused on another tab: still unseen; then on its own, seen.
        desk.show_tab(TabId(1));
        desk.window_focus(true);
        assert_eq!(marks(&desk).0, [Some(Mark::Unseen), None]);
        desk.window_focus(false);
        desk.show_tab(TabId(2));
        desk.window_focus(true);
        assert_eq!(
            marks(&desk),
            (vec![None, None], vec!["1".into(), "2".into()])
        );
    }

    #[test]
    fn what_was_unseen_is_forgotten_on_another_machine_and_kept_on_the_same_again() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        desk.daemon_at(None);
        desk.chats_changed(vec![summary(5, 2, Answering)]);
        desk.chats_changed(vec![summary(5, 2, Ready)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        // Connected again to the same daemon: kept.
        desk.lost();
        desk.daemon_at(None);
        desk.connected(desk.workspaces.clone(), false);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        // To another machine's, whose chat 5 is another: forgotten, and
        // its list then is the first heard.
        desk.lost();
        desk.daemon_at(Some("laptop"));
        desk.connected(desk.workspaces.clone(), true);
        assert_eq!(marks(&desk).0, [None, None]);
        assert!(!desk.chats_changed(vec![summary(5, 2, Ready)]));
        assert_eq!(marks(&desk).0, [None, None]);
    }

    #[test]
    fn out_of_reach_no_chat_is_said_to_work_or_ask_but_whats_unseen_stays() {
        use ChatState::{Answering, Ready};
        let mut desk = chats_desk();
        desk.chats_changed(vec![summary(5, 2, Answering), summary(6, 2, Answering)]);
        desk.chats_changed(vec![summary(5, 2, Ready), summary(6, 2, Answering)]);
        assert_eq!(marks(&desk).0, [None, Some(Mark::Working)]);
        desk.lost();
        assert_eq!(marks(&desk).0, [None, Some(Mark::Unseen)]);
        // Back: chat 6 finished meanwhile, as the list says once it comes.
        desk.connected(desk.workspaces.clone(), false);
        assert!(answer_all(&mut desk).contains(&Request::WatchChats));
        desk.chats_changed(vec![summary(5, 2, Ready), summary(6, 2, Ready)]);
        desk.show_workspace(WorkspaceId(2));
        assert_eq!(marks(&desk).1, ["1", "2 •"], "chat 6 finished unseen");
    }

    #[test]
    fn a_tab_with_a_chat_asking_says_so_before_one_at_work() {
        use clackatoo_protocol::{ChatNode, Node, Question};

        let mut desk = chats_desk();
        let chat = |question| ChatNode {
            agent: "Claude Code".into(),
            state: ChatState::Starting,
            question,
            ..ChatNode::default()
        };
        let question = Question {
            id: 1,
            title: "Run ls".into(),
            choices: Vec::new(),
            own: false,
        };
        for (terminal, node) in [(2, chat(None)), (3, chat(Some(question)))] {
            // Asking in a tab not shown: drawn, for its name to say so.
            assert!(desk.view_changed(
                TerminalId(terminal),
                &ViewUpdate::Snapshot(View {
                    title: "chat".into(),
                    cwd: None,
                    root: Node::Chat(Box::new(node)),
                }),
            ));
        }
        assert_eq!(
            marks(&desk),
            (
                vec![Some(Mark::Asking), None],
                vec!["1".into(), "2 chat ?".into()]
            )
        );
    }

    #[test]
    fn ctrl_hjkl_move_between_panes_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )]));
        snapshot(&mut desk, 1);
        snapshot(&mut desk, 2);
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal, "the mode came along");
        assert_eq!(mode(&desk, 1), Mode::Normal, "the pane left kept its mode");
        assert!(answer_all(&mut desk).contains(&Request::Focus { pane: PaneId(2) }));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2), "nothing further right");
        ctrl(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(1));
    }

    #[test]
    fn ctrl_semicolon_goes_to_normal_mode_from_anywhere() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut desk, 1);
        let typing = |desk: &Desk| matches!(desk.status(), Status::Typing { .. });
        let key = |desk: &mut Desk, text: &str| {
            press(
                desk,
                Key::Character(text.into()),
                None,
                Modifiers::default(),
            );
        };
        normal_mode(&mut desk);
        assert_eq!(mode(&desk, 1), Mode::Normal);
        // From a visual selection.
        key(&mut desk, "v");
        assert_eq!(mode(&desk, 1), Mode::Visual(Selection::Characters));
        normal_mode(&mut desk);
        assert_eq!(mode(&desk, 1), Mode::Normal);
        // From a search being typed, and from the command line.
        for opens in ["/", ":"] {
            key(&mut desk, opens);
            key(&mut desk, "x");
            assert!(typing(&desk), "{opens} opens a prompt");
            normal_mode(&mut desk);
            assert!(!typing(&desk), "{opens}'s prompt closed");
            assert_eq!(mode(&desk, 1), Mode::Normal);
        }
        // Ctrl+\ twice from insert mode still reaches the program.
        key(&mut desk, "i");
        ctrl(&mut desk, "\\");
        assert_eq!(mode(&desk, 1), Mode::Normal);
        ctrl(&mut desk, "\\");
        assert_eq!(mode(&desk, 1), Mode::Insert);
    }

    #[test]
    fn a_split_opens_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        command(&mut desk, "|");
        let split = desk.take_outbox();
        assert!(matches!(
            split.as_slice(),
            [ClientMessage::Request(Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                ..
            })]
        ));
        // The daemon tells of the new pane, then answers.
        desk.workspaces_changed(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(3)]))],
        )]));
        desk.answered(Response::PaneCreated(PaneId(3)));
        assert_eq!(shown_pane(&desk), PaneId(3));
        assert_eq!(mode(&desk, 3), Mode::Normal);
        // Its cursor goes where the program's is, once there's a screen.
        snapshot(&mut desk, 3);
        let (_, cursor) = desk.pane(TerminalId(3)).unwrap().scene(24);
        assert!(cursor.is_some());
    }

    /// How the cursor of the shown tab's first pane is drawn, in a window
    /// with the focus.
    fn drawn_cursor(desk: &mut Desk) -> Option<crate::render::CursorStyle> {
        drawn_cursor_picking(desk, false)
    }

    /// `drawn_cursor`, with the picker of a machine open if `picking`.
    fn drawn_cursor_picking(desk: &mut Desk, picking: bool) -> Option<crate::render::CursorStyle> {
        desk.settle_editors();
        desk.settle_explorers();
        let area = crate::layout::Area {
            rect: crate::layout::Rect {
                x: 0.0,
                y: 0.0,
                width: 800.0,
                height: 600.0,
            },
            cell: [10.0, 20.0],
            gap: 1.0,
            inset: 4.0,
        };
        crate::app::drawn_cursors(desk, &area, picking)[0]
    }

    /// Whether the bar, under what's typed, still shows the mode and the
    /// tabs.
    fn bar_under_prompt(desk: &Desk) -> bool {
        matches!(
            desk.status(),
            Status::Typing { under: Some(under), .. }
                if matches!(*under, Status::Tabs { mode: Some(_), panes: 1.., .. })
        )
    }

    #[test]
    fn while_a_prompt_has_the_keys_the_panes_cursor_is_hollow_and_the_bar_stays() {
        use crate::render::CursorStyle;
        let hollow = Some(CursorStyle::Hollow);
        // A terminal: the command line, a search, and the finder.
        let mut desk = desk(one_tab(pane(1)));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        let solid = drawn_cursor(&mut desk);
        assert!(solid.is_some() && solid != hollow, "{solid:?}");
        for keys in [":", "/4"] {
            type_keys(&mut desk, keys);
            assert_eq!(drawn_cursor(&mut desk), hollow, "{keys}");
            assert!(bar_under_prompt(&desk), "{keys}: {:?}", desk.status());
            escape(&mut desk);
            assert_eq!(drawn_cursor(&mut desk), solid, "{keys}");
        }
        // The picker of a machine, Ctrl+B r, too.
        assert_eq!(drawn_cursor_picking(&mut desk, true), hollow, "the picker");
        desk.open_finder(crate::finder::Kind::Files, String::new());
        assert_eq!(drawn_cursor(&mut desk), hollow, "the finder");
        // An editor's search.
        let mut desk = editing("/p/a.txt", TEN_LINES);
        let solid = drawn_cursor(&mut desk);
        assert!(solid.is_some() && solid != hollow, "{solid:?}");
        type_keys(&mut desk, "/l5");
        assert_eq!(drawn_cursor(&mut desk), hollow);
        assert!(bar_under_prompt(&desk), "{:?}", desk.status());
        escape(&mut desk);
        assert_eq!(drawn_cursor(&mut desk), solid);
        // An explorer's search.
        let mut desk = self::desk(one_tab(files_pane(2, 1)));
        explore_p(&mut desk);
        answer_all(&mut desk);
        let solid = drawn_cursor(&mut desk);
        assert!(solid.is_some() && solid != hollow, "{solid:?}");
        type_keys(&mut desk, "/a");
        assert_eq!(drawn_cursor(&mut desk), hollow);
        assert!(bar_under_prompt(&desk), "{:?}", desk.status());
        escape(&mut desk);
        assert_eq!(drawn_cursor(&mut desk), solid);
    }

    #[test]
    fn switching_workspaces_keeps_the_mode_and_each_panes_state() {
        let mut desk = desk(workspaces(vec![
            workspace(1, "a", vec![tab(1, pane(1))]),
            workspace(2, "b", vec![tab(2, pane(2))]),
        ]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        command(&mut desk, "ws b");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal, "the mode came along");
        let requests = answer_all(&mut desk);
        assert!(requests.contains(&Request::Detach {
            terminal: TerminalId(1)
        }));
        assert!(requests.contains(&Request::Attach {
            terminal: TerminalId(2)
        }));

        // Back in a, its pane is as it was left.
        press(
            &mut desk,
            Key::Character("i".into()),
            None,
            Modifiers::default(),
        );
        assert_eq!(mode(&desk, 2), Mode::Insert);
        normal_mode(&mut desk);
        command(&mut desk, "ws a");
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(mode(&desk, 1), Mode::Normal);
    }

    #[test]
    fn a_new_workspace_opens_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(1, "a", vec![tab(1, pane(1))])]));
        normal_mode(&mut desk);
        command(&mut desk, "ws b");
        assert!(matches!(
            desk.take_outbox().as_slice(),
            [ClientMessage::Request(Request::NewWorkspace { name: Some(name), .. })] if name == "b"
        ));
        desk.workspaces_changed(workspaces(vec![
            workspace(1, "a", vec![tab(1, pane(1))]),
            workspace(2, "b", vec![tab(2, pane(2))]),
        ]));
        desk.answered(Response::PaneCreated(PaneId(2)));
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal);
    }

    /// Ctrl+B, then `keys`, each as typed.
    fn ctrl_b(desk: &mut Desk, keys: &str) {
        ctrl(desk, "b");
        type_keys(desk, keys);
    }

    fn type_keys(desk: &mut Desk, keys: &str) {
        for ch in keys.chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
    }

    fn three_tabs() -> Desk {
        desk(workspaces(vec![workspace(
            1,
            "work",
            vec![
                tab(1, side_by_side(vec![pane(1), pane(2), pane(3)])),
                tab(2, pane(4)),
                tab(3, pane(5)),
            ],
        )]))
    }

    #[test]
    fn ctrl_b_moves_between_tabs_and_panes_in_either_mode() {
        let mut desk = three_tabs();
        // In insert mode, the keys don't reach the program.
        ctrl_b(&mut desk, "3");
        assert_eq!(desk.shown().unwrap().tab, TabId(3));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::SendInput { .. }))
        );
        ctrl_b(&mut desk, "]");
        assert_eq!(desk.shown().unwrap().tab, TabId(1), "round to the first");
        ctrl_b(&mut desk, "[");
        assert_eq!(desk.shown().unwrap().tab, TabId(3), "round to the last");
        ctrl_b(&mut desk, "1");
        // While Ctrl+B waits, a panel says what it takes, and the panes,
        // three here, are numbered.
        ctrl(&mut desk, "b");
        let which = desk.which_keys().expect("the keys, at once after Ctrl");
        assert_eq!(
            (which.title.as_str(), desk.keys_waiting().as_deref()),
            ("Ctrl+B", Some("^B"))
        );
        assert!(desk.numbering_panes());
        assert!(desk.cancel_keys());
        assert_eq!(desk.which_keys(), None);
        // Panes by number, shown on screen while one is awaited.
        ctrl_b(&mut desk, "p");
        assert!(desk.numbering_panes());
        let which = desk.which_keys().expect("the keys");
        assert_eq!(which.rows, [("1-9".to_owned(), "pick a pane".to_owned())]);
        type_keys(&mut desk, "3");
        assert!(!desk.numbering_panes());
        assert_eq!(shown_pane(&desk), PaneId(3));
        // A click cancels it, as the window says when the mouse goes down.
        ctrl_b(&mut desk, "p");
        assert!(desk.cancel_keys());
        assert!(!desk.numbering_panes());
        ctrl_b(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(2));
        // And in normal mode alike, where Ctrl+B used to page.
        normal_mode(&mut desk);
        ctrl_b(&mut desk, "p1");
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(mode(&desk, 1), Mode::Normal);
    }

    #[test]
    fn ctrl_b_numbers_one_pane_only_after_p_and_lists_every_key_it_takes() {
        let mut desk = desk(one_tab(pane(1)));
        ctrl(&mut desk, "b");
        assert!(!desk.numbering_panes(), "one pane needs no number");
        // Every key Ctrl+B takes, from the bindings, saying what it does.
        let which = desk.which_keys().unwrap();
        let rows: Vec<(&str, &str)> = (which.rows.iter())
            .map(|(keys, does)| (keys.as_str(), does.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("1-9", "go to tab"),
                ("c", "new tab"),
                ("[", "tab before"),
                ("]", "next tab"),
                (",", "rename tab"),
                ("p", "+pick a pane"),
                ("h", "pane left"),
                ("j", "pane below"),
                ("k", "pane above"),
                ("l", "pane right"),
                ("|", "split right"),
                ("-", "split below"),
                ("x", "close pane"),
                ("o", "jump back"),
                ("i", "jump forward"),
                ("a", "agent chat"),
                ("w", "workspaces"),
                ("r", "connect"),
                ("b", "+background"),
                ("^B", "send Ctrl+B"),
            ]
        );
        type_keys(&mut desk, "b");
        let which = desk.which_keys().unwrap();
        let rows: Vec<(&str, &str)> = (which.rows.iter())
            .map(|(keys, does)| (keys.as_str(), does.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("t", "tab to background"),
                ("p", "pane to background"),
                ("l", "background tabs"),
            ]
        );
        assert!(desk.cancel_keys());
        ctrl(&mut desk, "b");
        type_keys(&mut desk, "p");
        assert!(desk.numbering_panes(), "asked for, it's numbered");
        assert!(desk.cancel_keys());
        // A key bound by the config is listed too, and a key bound again
        // says what it does now.
        desk.lua_from(
            r#"
            client.keys.map("niv", "<C-b>g", "gri")
            client.keys.map("niv", "<C-b>c", function() end)
            "#,
        );
        ctrl(&mut desk, "b");
        let rows = desk.which_keys().unwrap().rows;
        assert!(
            rows.contains(&("c".into(), "from your config".into())),
            "{rows:?}"
        );
        assert!(rows.contains(&("g".into(), "types gri".into())), "{rows:?}");
        assert!(desk.cancel_keys());
        // Two panes are numbered as soon as Ctrl+B waits.
        let mut desk = three_tabs();
        ctrl(&mut desk, "b");
        assert!(desk.numbering_panes());
    }

    #[test]
    fn every_default_key_after_another_says_what_it_does() {
        let keys = bindings::Keys::default();
        for (chords, label) in keys.labels() {
            if chords.len() > 1 {
                let keys = crate::keymap::display(chords);
                assert!(label.is_some_and(|label| !label.is_empty()), "{keys}");
            }
        }
    }

    #[test]
    fn space_typed_quickly_never_shows_the_panel_of_keys() {
        let mut desk = desk(one_tab(pane(1)));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Space),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.which_keys(), None);
        type_keys(&mut desk, "p");
        assert_eq!(desk.keys_waiting().as_deref(), Some("␣p"));
        assert_eq!(desk.which_keys(), None);
        type_keys(&mut desk, "f");
        assert!(desk.finding(), "the finder opened");
        assert_eq!((desk.which_keys(), desk.keys_waiting()), (None, None));
        // With no delay, it shows at once.
        desk.close_finder();
        command(&mut desk, "set whichkeydelay=0");
        press(
            &mut desk,
            Key::Named(NamedKey::Space),
            None,
            Modifiers::default(),
        );
        assert!(desk.which_keys().is_some());
    }

    #[test]
    fn space_lists_its_keys_with_each_that_starts_more_on_a_row_of_its_own() {
        let mut desk = desk(one_tab(pane(1)));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Space),
            None,
            Modifiers::default(),
        );
        // Marked at once, but listed only after a pause.
        assert_eq!(desk.keys_waiting().as_deref(), Some("␣"));
        assert_eq!(desk.which_keys(), None);
        let due = desk.flash_ends().expect("a wake for the panel");
        while std::time::Instant::now() < due {
            std::thread::sleep(due - std::time::Instant::now());
        }
        let which = desk.which_keys().expect("the keys");
        assert_eq!(which.title, "Space");
        assert_eq!(desk.flash_ends(), None, "nothing more to wake for");
        let rows: Vec<(&str, &str)> = (which.rows.iter())
            .map(|(keys, does)| (keys.as_str(), does.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("p", "+project"),
                ("?", "open files"),
                ("d", "+problems"),
                ("b", "+file symbols"),
                ("h", "+git hunks"),
                ("e", "explorer"),
                ("t", "+toggles"),
                ("g", "+agent changes"),
                ("c", "comment"),
            ]
        );
        assert!(!desk.numbering_panes());
        assert!(desk.cancel_keys());
        // The config's keys say what its `desc` says, and its groups are
        // named as it names them, even over the window's own.
        desk.lua_from(
            r#"
            client.keys.map("n", "<Space>ft", function() end, {desc = "find\ntodos"})
            client.keys.map("n", "<Space>fn", "gri", {desc = "  "})
            client.keys.group("n", "<Space>f", "my files")
            client.keys.group("n", "<Space>g", "agents")
            client.keys.map("n", "<Space>x", function() end, {desc = "explain"})
            "#,
        );
        command(&mut desk, "set whichkeydelay=0");
        press(
            &mut desk,
            Key::Named(NamedKey::Space),
            None,
            Modifiers::default(),
        );
        let rows = desk.which_keys().unwrap().rows;
        let row = |keys: &str, does: &str| (keys.to_owned(), does.to_owned());
        assert!(rows.contains(&row("f", "+my files")), "{rows:?}");
        assert!(rows.contains(&row("g", "+agents")), "{rows:?}");
        assert!(rows.contains(&row("x", "explain")), "{rows:?}");
        type_keys(&mut desk, "f");
        let rows = desk.which_keys().unwrap().rows;
        assert_eq!(
            rows,
            [row("t", "find todos"), row("n", "types gri")],
            "on one line, and a blank one is none"
        );
    }

    #[test]
    fn zero_after_a_count_is_the_counts_whatever_it_is_bound_to() {
        let text = (1..=30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut desk = editing("/t.txt", &text);
        desk.lua_from(r#"client.keys.map("n", "0", function() client.opt.scrolloff = 9 end)"#);
        type_text(&mut desk, "0");
        assert_eq!(desk.options.scrolloff, 9, "bound, alone");
        command(&mut desk, "set scrolloff=5");
        type_text(&mut desk, "10j");
        assert_eq!(note(&desk), "t.txt  11:1");
        type_text(&mut desk, "gg20j");
        assert_eq!(note(&desk), "t.txt  21:1");
        assert_eq!(desk.options.scrolloff, 5, "never the binding's");
    }

    #[test]
    fn a_config_binding_hears_the_count_and_passes_it_on_with_its_keys() {
        let mut desk = editing("/t.txt", "abcdefgh\n");
        desk.lua_from(
            r#"
            client.keys.map("n", "x", function(ev)
                client.opt.scrolloff = ev.count
                ev:propagate()
            end)
            client.keys.map("n", "s", function(ev)
                client.opt.tabstop = ev.count1
            end)
            "#,
        );
        // Let go on, the count goes on with its keys: three deleted.
        type_text(&mut desk, "3x");
        assert_eq!(desk.options.scrolloff, 3);
        assert_eq!(editor_text(&desk), "defgh\n");
        type_text(&mut desk, "x");
        assert_eq!(desk.options.scrolloff, 0, "as Vim's v:count");
        type_text(&mut desk, "s");
        assert_eq!(desk.options.tabstop, 1, "as Vim's v:count1");
        // Stopped, the count goes with it.
        type_text(&mut desk, "12s");
        assert_eq!(desk.options.tabstop, 12);
        assert_eq!(editor_text(&desk), "efgh\n");
        assert_eq!(desk.focused_editor().unwrap().0.counting(), None);
        // In a terminal too: the count goes on before the keys.
        let mut desk = self::desk(one_tab(pane(1)));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        desk.lua_from(r#"client.keys.map("n", "j", function(ev) ev:propagate() end)"#);
        type_keys(&mut desk, "3j");
        let pane = &desk.panes[&TerminalId(1)];
        assert_eq!(pane.scene(24).1.map(|cursor| cursor.line), Some(3));
    }

    #[test]
    fn ctrl_b_makes_and_closes_panes_and_passes_itself_on() {
        let mut desk = three_tabs();
        answer_all(&mut desk);
        ctrl_b(&mut desk, "c");
        ctrl_b(&mut desk, "|");
        ctrl_b(&mut desk, "-");
        ctrl_b(&mut desk, "x");
        let requests = answer_all(&mut desk);
        assert!(matches!(requests[0], Request::NewTab { .. }));
        assert!(matches!(
            requests[1],
            Request::Split {
                side: Side::Right,
                ..
            }
        ));
        assert!(matches!(
            requests[2],
            Request::Split {
                side: Side::Below,
                ..
            }
        ));
        assert!(matches!(requests[3], Request::ClosePane { .. }));
        // Twice sends one to the program.
        ctrl_b(&mut desk, "");
        ctrl(&mut desk, "b");
        assert!(answer_all(&mut desk).contains(&Request::SendInput {
            terminal: TerminalId(1),
            bytes: vec![0x02],
        }));
        // Anything else does nothing, and goes nowhere.
        ctrl_b(&mut desk, "z");
        assert!(desk.keymap.waiting().is_empty());
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn the_configs_handlers_and_bindings_come_before_the_pane() {
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.keys.map("niv", '<C-b>"', function(ev) ev.pane:split("below") end)
            client.keys.unmap("niv", "<C-b>x")
            client.keys.map("i", "jk", function(ev) ev:propagate() end)
            client.on("keypress", function(ev) if ev.key == "q" then ev:stop() end end)
            "#,
        );
        let sent = |desk: &mut Desk| -> Vec<u8> {
            (answer_all(desk).into_iter())
                .filter_map(|request| match request {
                    Request::SendInput { bytes, .. } => Some(bytes),
                    _ => None,
                })
                .flatten()
                .collect()
        };
        answer_all(&mut desk);
        ctrl_b(&mut desk, "\"");
        assert!(matches!(
            answer_all(&mut desk)[..],
            [Request::Split {
                pane: PaneId(1),
                side: Side::Below,
                ..
            }]
        ));
        ctrl_b(&mut desk, "x");
        assert!(answer_all(&mut desk).is_empty(), "unbound");
        type_keys(&mut desk, "q");
        assert_eq!(sent(&mut desk), b"", "stopped");
        type_keys(&mut desk, "jkw");
        assert_eq!(sent(&mut desk), b"jkw", "let go on, in order");
        // Keys given up on, as a click gives up on Ctrl+B, aren't.
        ctrl(&mut desk, "b");
        assert!(desk.cancel_keys());
        type_keys(&mut desk, "jk");
        assert_eq!(sent(&mut desk), b"jk");

        // A reload starts again from the window's own keys.
        normal_mode(&mut desk);
        command(&mut desk, "reload");
        assert_eq!(desk.message.as_deref(), Some("reloaded the config"));
        desk.lua_ready();
        ctrl_b(&mut desk, "x");
        assert!(answer_all(&mut desk).is_empty(), "unbound again");
    }

    #[test]
    fn options_are_set_by_config_and_set_and_lua_reads_them() {
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.opt.rnu = false
            client.opt.numberwidth = 6
            client.opt.cc = { 80, 120 }
            client.opt.ic = false
            client.keys.map("n", "<C-b>n", function()
                client.opt.number = not client.opt.number
            end)
            "#,
        );
        assert!(!desk.options.relativenumber);
        assert_eq!(desk.options.numberwidth, 6);
        normal_mode(&mut desk);
        command(&mut desk, "set rnu? nu");
        assert_eq!(desk.message.as_deref(), Some("norelativenumber"));
        command(&mut desk, "set nonu");
        assert!(!desk.options.number);
        // Lua's copy heard, so it turns it back on.
        ctrl_b(&mut desk, "n");
        assert!(desk.options.number);
        command(&mut desk, "set");
        assert_eq!(
            desk.message.as_deref(),
            Some("norelativenumber  numberwidth=6  colorcolumn=80,120  noignorecase")
        );
        command(&mut desk, "set cc= ts=8");
        command(&mut desk, "set cc? ts");
        assert_eq!(desk.message.as_deref(), Some("colorcolumn=  tabstop=8"));
        // The terminal's search heard: without ignorecase, it's exact.
        answer_all(&mut desk);
        type_keys(&mut desk, "/x");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::Search { exact: true, .. }]
        ));
        command(&mut desk, "set nuw=99");
        assert_eq!(
            desk.message.as_deref(),
            Some("numberwidth is a whole number from 1 to 20")
        );
        // A reload starts again from the options as they start.
        command(&mut desk, "reload");
        assert!(desk.options.relativenumber);
        desk.lua_ready();
        assert!(!desk.options.relativenumber);
    }

    #[test]
    fn the_command_lines_font_outdoes_configs_but_not_set() {
        use crate::options::{self, Decimal};
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        // `--font Iosevka`, and startup.toml's size.
        let start = Options::drawing_with(&crate::FontOptions {
            family: Some("Iosevka".into()),
            size: 13.0,
        });
        let font = options::find("font").unwrap();
        desk.start_options(start, vec![font]);
        desk.lua_from(
            r#"
            client.opt.font = "JetBrains Mono"
            client.opt.fontsize = 15
            client.keys.map("n", "<C-b>n", function()
                client.opt.nu = client.opt.font == "Iosevka"
            end)
            "#,
        );
        // Config's size outdoes startup.toml's; the command line's font
        // outdoes config's.
        assert_eq!(desk.options.font, "Iosevka");
        assert_eq!(desk.options.fontsize, Decimal::whole(15));
        normal_mode(&mut desk);
        command(&mut desk, "set nonu");
        // Lua heard the font it set was put back.
        ctrl_b(&mut desk, "n");
        assert!(desk.options.number);
        command(&mut desk, "set");
        assert_eq!(desk.message.as_deref(), Some("fontsize=15"));
        // `:set` is you, now, and outdoes everything.
        command(&mut desk, "set gfn=Fira\\ Code");
        assert_eq!(desk.options.font, "Fira Code");
        command(&mut desk, "set");
        assert_eq!(
            desk.message.as_deref(),
            Some("font=Fira\\ Code  fontsize=15")
        );
        // A reload starts again from the command line's and startup.toml's.
        command(&mut desk, "reload");
        assert_eq!(desk.options.font, "Iosevka");
        assert_eq!(desk.options.fontsize, Decimal::whole(13));
        desk.lua_ready();
        assert_eq!(desk.options.font, "Iosevka");
        assert_eq!(desk.options.fontsize, Decimal::whole(15));
    }

    #[test]
    fn once_config_has_run_its_handlers_change_what_the_command_line_set() {
        use crate::options::{self, Decimal};
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        // `--font-size 14`.
        let start = Options::drawing_with(&crate::FontOptions {
            family: None,
            size: 14.0,
        });
        desk.start_options(start, vec![options::find("fontsize").unwrap()]);
        desk.lua_from(
            r#"
            client.opt.fsz = 20
            client.keys.map("n", "<C-b>n", function()
                client.opt.fsz = client.opt.fsz + 1
            end)
            "#,
        );
        assert_eq!(desk.options.fontsize, Decimal::whole(14));
        normal_mode(&mut desk);
        ctrl_b(&mut desk, "n");
        assert_eq!(desk.options.fontsize, Decimal::whole(15));
        ctrl_b(&mut desk, "n");
        assert_eq!(desk.options.fontsize, Decimal::whole(16));
    }

    /// Fonts as `tests::fonts` loads them, and a desk starting in them.
    fn desk_in_fonts(desk: &mut Desk) -> crate::fonts::Fonts {
        let fonts = crate::tests::fonts(1.0);
        let start = Options::drawing_with(&crate::FontOptions {
            family: fonts.mono_family.clone(),
            size: 12.0,
        });
        desk.start_options(start, Vec::new());
        fonts
    }

    #[test]
    fn a_reload_changes_the_font_once_config_has_run_and_only_if_it_must() {
        use crate::options::Decimal;
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        let mut fonts = desk_in_fonts(&mut desk);
        desk.lua_from("client.opt.fontsize = 20");
        assert!(desk.use_font(&mut fonts));
        let cell = fonts.cell;
        normal_mode(&mut desk);
        // The options go back as they started while it runs again, and
        // the font with them, but the font doesn't change until it's run:
        // and then, it's as it was.
        command(&mut desk, "reload");
        assert_eq!(desk.options.fontsize, Decimal::whole(12));
        assert!(!desk.use_font(&mut fonts), "not while it runs");
        desk.lua_ready();
        assert_eq!(desk.options.fontsize, Decimal::whole(20));
        assert!(!desk.use_font(&mut fonts), "nor after: it's as it was");
        assert_eq!(fonts.cell, cell);
        // A `:set` while config runs waits for it.
        command(&mut desk, "reload");
        command(&mut desk, "set fsz=24");
        assert!(!desk.use_font(&mut fonts));
        desk.lua_ready();
        assert_eq!(
            desk.options.fontsize,
            Decimal::whole(20),
            "config ran after"
        );
        assert!(!desk.use_font(&mut fonts));
        // A config that no longer sets it puts the start's back, once.
        desk.lua_from("");
        assert!(!desk.use_font(&mut fonts), "it's not been reloaded yet");
        command(&mut desk, "reload");
        assert!(!desk.use_font(&mut fonts));
        desk.lua_ready();
        assert!(desk.use_font(&mut fonts));
        assert!(fonts.cell.height < cell.height);
    }

    #[test]
    fn the_font_set_several_times_between_frames_resizes_each_terminal_once() {
        let mut desk = three_tabs();
        for terminal in 1..=3 {
            snapshot(&mut desk, terminal);
        }
        let mut fonts = desk_in_fonts(&mut desk);
        answer_all(&mut desk);
        // What the window does each frame.
        let frame = |desk: &mut Desk, fonts: &mut crate::fonts::Fonts| {
            if desk.use_font(fonts) {
                #[allow(clippy::cast_precision_loss)]
                let cell = [fonts.cell.width as f32, fonts.cell.height as f32];
                desk.set_area(Some(Area {
                    rect: crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 810.0,
                        height: 480.0,
                    },
                    cell,
                    gap: 10.0,
                    inset: 5.0,
                }));
            }
            let resized: Vec<TerminalId> = (answer_all(desk).into_iter())
                .filter_map(|request| match request {
                    Request::Resize { terminal, .. } => Some(terminal),
                    _ => None,
                })
                .collect();
            resized
        };
        normal_mode(&mut desk);
        for size in [14, 16, 18, 20] {
            command(&mut desk, &format!("set fsz={size}"));
        }
        desk.lua_from("client.opt.fsz = 22 client.opt.fsz = 24");
        let mut resized = frame(&mut desk, &mut fonts);
        resized.sort_unstable();
        assert_eq!(resized, [TerminalId(1), TerminalId(2), TerminalId(3)]);
        assert_eq!(frame(&mut desk, &mut fonts), [], "and nothing after");
    }

    #[test]
    fn set_font_changes_the_cell_and_a_font_not_installed_is_refused() {
        use crate::options::Decimal;
        let mut fonts = crate::tests::fonts(1.0);
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        let start = Options::drawing_with(&crate::FontOptions {
            family: fonts.mono_family.clone(),
            size: 12.0,
        });
        desk.start_options(start, Vec::new());
        assert!(!desk.use_font(&mut fonts), "nothing's changed");
        let (cell, generation) = (fonts.cell, fonts.generation);
        let family = fonts.mono_family.clone().unwrap_or_default();
        let set_font = |desk: &mut Desk, family: &str| {
            command(desk, &format!("set font={}", family.replace(' ', "\\ ")));
        };

        normal_mode(&mut desk);
        command(&mut desk, "set fontsize=24");
        assert!(desk.use_font(&mut fonts));
        assert!(fonts.cell.height > cell.height && fonts.cell.width > cell.width);
        assert_eq!(fonts.generation, generation, "the same family");
        assert!(!desk.use_font(&mut fonts), "once");
        let big = fonts.cell;

        // A family not installed is put back, and the size still changes.
        command(&mut desk, "set font=No\\ Such\\ Font fsz=12");
        assert!(desk.use_font(&mut fonts));
        assert_eq!(fonts.cell, cell);
        assert_eq!(desk.options.font, family);
        assert_eq!(desk.options.fontsize, Decimal::whole(12));
        assert_eq!(
            desk.message.as_deref(),
            Some("there's no font called `No Such Font`, so the font stays as it was")
        );
        command(&mut desk, "set font=No\\ Such\\ Font");
        assert!(!desk.use_font(&mut fonts), "nothing to change");
        assert_eq!((fonts.cell, fonts.generation), (cell, generation));

        // Another family that is installed is drawn in: the system's
        // sans-serif, which is surely there.
        let collection = &mut fonts.font_cx.collection;
        let sans = (collection.generic_families(fontique::GenericFamily::SansSerif)).next();
        let other = (sans.and_then(|id| collection.family_name(id)))
            .expect("a sans-serif font")
            .to_owned();
        assert_ne!(other, family);
        command(&mut desk, "set fsz=24");
        set_font(&mut desk, &other);
        assert!(desk.use_font(&mut fonts));
        assert_eq!(fonts.mono_family.as_deref(), Some(other.as_str()));
        assert_eq!(fonts.generation, generation + 1);
        // And back, without loading its faces again.
        let faces = fonts.faces();
        set_font(&mut desk, &family);
        assert!(desk.use_font(&mut fonts));
        assert_eq!(fonts.cell, big);
        assert_eq!(fonts.faces(), faces);
    }

    #[test]
    fn ctrl_b_w_picks_a_workspace_or_makes_one() {
        let mut desk = desk(workspaces(vec![
            workspace(1, "clackatoo", vec![tab(1, pane(1))]),
            workspace(2, "peios", vec![tab(2, pane(2))]),
            workspace(3, "homelab", vec![tab(3, pane(3))]),
        ]));
        ctrl_b(&mut desk, "w");
        let choices = |desk: &Desk| match desk.status() {
            Status::Typing { choices, .. } => choices,
            Status::Tabs { .. } => panic!("the picker closed"),
        };
        assert_eq!(choices(&desk).len(), 3);
        // Typing narrows them; Down picks the next.
        type_keys(&mut desk, "o");
        let names: Vec<String> = choices(&desk).into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["clackatoo", "peios", "homelab"]);
        type_keys(&mut desk, "s");
        assert_eq!(choices(&desk), [("peios".to_owned(), true)]);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
        // A name nothing has makes a workspace by it.
        ctrl_b(&mut desk, "wnas");
        assert_eq!(choices(&desk), [("New workspace “nas”".to_owned(), true)]);
        answer_all(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::NewWorkspace { name: Some(name), .. }] if name == "nas"
        ));
        // Down and Up move through the choices.
        ctrl_b(&mut desk, "w");
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowUp),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
    }

    #[test]
    fn ctrl_o_and_ctrl_i_go_back_and_forth_within_the_tab_or_anywhere() {
        let mut desk = three_tabs();
        normal_mode(&mut desk);
        // Pane 2, tab 2's pane 4, back to tab 1 (at pane 1), then pane 3.
        ctrl_b(&mut desk, "p2");
        ctrl_b(&mut desk, "2");
        ctrl_b(&mut desk, "1");
        ctrl_b(&mut desk, "p3");
        // Within the tab: back to pane 1, then past tab 2's pane to pane 2,
        // and forward again past it to pane 1.
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(2));
        ctrl(&mut desk, "i");
        assert_eq!(shown_pane(&desk), PaneId(1));
        // Anywhere, in insert mode too: back to tab 2's pane, and forward.
        type_keys(&mut desk, "i");
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        press(&mut desk, Key::Character("O".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(4));
        press(&mut desk, Key::Character("I".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::SendInput { .. })),
            "the program heard nothing"
        );
    }

    #[test]
    fn numbers_move_between_tabs() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1)), tab(2, pane(2))],
        )]));
        normal_mode(&mut desk);
        command(&mut desk, "2");
        assert_eq!(desk.shown().unwrap().tab, TabId(2));
        command(&mut desk, "3");
        assert_eq!(desk.shown().unwrap().tab, TabId(2));
        assert_eq!(
            desk.message.as_deref(),
            Some("there's no tab 3; this workspace has 2")
        );
    }

    #[test]
    fn workspaces_and_tabs_are_renamed_in_the_palette() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        desk.rename_workspace(WorkspaceId(1));
        assert!(matches!(
            desk.status(),
            Status::Typing { label: Some(label), text, choices, .. }
                if label == "Rename workspace" && text == "work▏" && choices.is_empty()
        ));
        for _ in 0..4 {
            press(
                &mut desk,
                Key::Named(NamedKey::Backspace),
                None,
                Modifiers::default(),
            );
        }
        press(
            &mut desk,
            Key::Character("x".into()),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            answer_all(&mut desk),
            [Request::RenameWorkspace {
                workspace: WorkspaceId(1),
                name: "x".into()
            }]
        );
        // Backspace past the start of a name leaves the palette open.
        desk.rename_tab(TabId(1));
        for _ in 0..3 {
            press(
                &mut desk,
                Key::Named(NamedKey::Backspace),
                None,
                Modifiers::default(),
            );
        }
        assert!(matches!(desk.status(), Status::Typing { .. }));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(matches!(desk.status(), Status::Tabs { .. }));
    }

    #[test]
    fn a_new_name_starts_as_the_old_one_selected_and_the_first_key_replaces_it() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        desk.rename_workspace(WorkspaceId(1));
        let Status::Typing { text, selected, .. } = desk.status() else {
            panic!("the palette");
        };
        assert_eq!(&text[selected.unwrap()], "work");
        type_keys(&mut desk, "home");
        assert!(matches!(
            desk.status(),
            Status::Typing { text, selected: None, .. } if text == "home▏"
        ));
        escape(&mut desk);
        // An arrow keeps it, to add to.
        desk.rename_workspace(WorkspaceId(1));
        press(
            &mut desk,
            Key::Named(NamedKey::End),
            None,
            Modifiers::default(),
        );
        type_keys(&mut desk, "s");
        assert!(matches!(
            desk.status(),
            Status::Typing { text, selected: None, .. } if text == "works▏"
        ));
    }

    #[test]
    fn answers_go_to_whoever_asked() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        normal_mode(&mut desk);
        command(&mut desk, "rename taken");
        desk.take_outbox();
        desk.answered(Response::Error(clackatoo_protocol::Error::NameTaken(
            "taken".into(),
        )));
        assert_eq!(
            desk.message.as_deref(),
            Some("there's already a workspace called \"taken\"")
        );
        // The next key clears it.
        press(
            &mut desk,
            Key::Character("j".into()),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.message.as_deref(), None);
    }

    #[test]
    fn a_drag_stays_with_the_pane_it_started_in() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )]));
        // Both panes' programs ask for drags, in SGR.
        let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        screen.advance(b"\x1b[?1002h\x1b[?1006h");
        for terminal in [1, 2] {
            desk.view_changed(
                TerminalId(terminal),
                &ViewUpdate::Snapshot(View::capture(&screen)),
            );
        }
        desk.take_outbox();
        let mouse = |desk: &mut Desk, point, action| {
            desk.mouse(point, action, 1, Modifiers::default());
        };
        let left = MouseButton::Left;
        // The left tile's cells start 5 pixels in, and are 10 × 20.
        mouse(&mut desk, [50.0, 25.0], MouseAction::Press(left));
        mouse(&mut desk, [60.0, 25.0], MouseAction::Motion(None));
        // Still in the same cell: nothing more.
        mouse(&mut desk, [62.0, 30.0], MouseAction::Motion(None));
        // Over the right pane, but the drag is the left one's, at its edge.
        mouse(&mut desk, [500.0, 25.0], MouseAction::Motion(None));
        mouse(&mut desk, [500.0, 25.0], MouseAction::Release(left));

        let sent: Vec<(u32, String)> = desk
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { terminal, bytes }) => {
                    Some((terminal.0, String::from_utf8(bytes).unwrap()))
                }
                _ => None,
            })
            .collect();
        let expected = [
            "\x1b[<0;5;2M",
            "\x1b[<32;6;2M",
            "\x1b[<32;40;2M",
            "\x1b[<0;40;2m",
        ];
        assert_eq!(sent, expected.map(|bytes| (1, bytes.to_owned())).to_vec());
    }

    /// The width of `pane`'s tile.
    fn width(desk: &Desk, pane: u32) -> f32 {
        desk.placement()
            .unwrap()
            .tile(PaneId(pane))
            .unwrap()
            .rect
            .width
    }

    /// The shares of every split resize asked of the daemon, oldest first.
    fn resizes(desk: &mut Desk) -> Vec<Vec<u32>> {
        answer_all(desk)
            .into_iter()
            .filter_map(|request| match request {
                Request::ResizeSplit { shares, .. } => Some(shares),
                _ => None,
            })
            .collect()
    }

    fn two_side_by_side() -> Workspaces {
        workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )])
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn dragging_a_divider_resizes_the_panes_at_once_and_tells_the_daemon() {
        let mut desk = desk(two_side_by_side());
        // 810 pixels: 400, a gap at 400 to 410, and 400.
        assert_eq!(desk.divider_at([405.0, 100.0]), Some(Axis::LeftToRight));
        assert_eq!(desk.divider_at([300.0, 100.0]), None);
        assert!(desk.grab_divider([405.0, 100.0], 1));
        assert!(desk.drag_divider([303.0, 100.0]));
        assert_eq!(width(&desk, 1), 300.0);
        assert!(!desk.drag_divider([304.0, 100.0]), "within the same cell");
        desk.drop_divider();

        // The daemon's workspaces, from before it heard, don't move it back.
        desk.workspaces_changed(two_side_by_side());
        assert_eq!(width(&desk, 1), 300.0);
        assert_eq!(resizes(&mut desk), [vec![300 * 16, 500 * 16]]);
        // Once answered, the daemon's shares rule, as they would be by now.
        let mut resized = two_side_by_side();
        if let Layout::Split { shares, .. } = &mut resized.list[0].tabs[0].layout {
            *shares = vec![3, 5];
        }
        desk.workspaces_changed(resized);
        assert_eq!(width(&desk, 1), 300.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn alt_moves_the_focused_panes_edges_and_a_double_click_evens_them() {
        let mut desk = desk(two_side_by_side());
        normal_mode(&mut desk);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        // The left pane has its right edge only: Alt+L grows it, and Alt+H
        // shrinks it, both moving that edge a column.
        press(&mut desk, Key::Character("l".into()), None, alt);
        assert_eq!(width(&desk, 1), 410.0);
        press(&mut desk, Key::Character("h".into()), None, alt);
        press(&mut desk, Key::Character("h".into()), None, alt);
        assert_eq!(width(&desk, 1), 390.0);
        // Up and down have no edge to move here.
        press(&mut desk, Key::Character("j".into()), None, alt);
        assert_eq!(resizes(&mut desk).len(), 3);

        // Answered, with this test's daemon still sharing equally.
        assert!(desk.grab_divider([405.0, 100.0], 2));
        desk.drop_divider();
        assert_eq!(resizes(&mut desk), [vec![1, 1]]);
    }

    #[test]
    fn ctrl_shift_c_copies_a_mouse_selection_typed_a_key_at_a_time() {
        let mut desk = desk(two_side_by_side());
        let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        screen.advance(b"hello world");
        desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&screen)));
        answer_all(&mut desk);
        let left = MouseButton::Left;
        // Cells start 5 pixels in, and are 10 × 20: "hello" is 7 to 47.
        desk.mouse(
            [7.0, 10.0],
            MouseAction::Press(left),
            1,
            Modifiers::default(),
        );
        desk.mouse(
            [47.0, 10.0],
            MouseAction::Motion(None),
            1,
            Modifiers::default(),
        );
        desk.mouse(
            [47.0, 10.0],
            MouseAction::Release(left),
            1,
            Modifiers::default(),
        );
        // As a keyboard sends it: Shift, then Ctrl, then C, each a key.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let both = Modifiers {
            ctrl: true,
            ..shift
        };
        press(&mut desk, Key::Named(NamedKey::Shift), None, shift);
        press(&mut desk, Key::Named(NamedKey::Control), None, both);
        press(&mut desk, Key::Character("C".into()), None, both);
        let copies: Vec<_> = answer_all(&mut desk)
            .into_iter()
            .filter(|request| matches!(request, Request::Text { .. }))
            .collect();
        assert_eq!(
            copies.len(),
            2,
            "to the primary selection, then the clipboard"
        );
    }

    /// The changes `buffer_changes` found, without their numbers, which
    /// are counted for the whole window, across tests too.
    fn changes_only(
        sent: Vec<(u64, clackatoo_protocol::Change)>,
    ) -> Vec<clackatoo_protocol::Change> {
        sent.into_iter().map(|(_, change)| change).collect()
    }

    /// A buffer's first snapshot: `text`, from the file at `path`.
    fn snapshot_of(path: &str, text: &str) -> clackatoo_protocol::BufferUpdate {
        clackatoo_protocol::BufferUpdate::Snapshot(clackatoo_protocol::BufferSnapshot {
            path: path.into(),
            text: text.into(),
            version: 0,
            modified: false,
            applied: None,
            landed: None,
            daemon: 0,
        })
    }

    /// Types `text`, a character at a time.
    fn type_text(desk: &mut Desk, text: &str) {
        for ch in text.chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
    }

    /// The changes to buffers waiting to go to the daemon.
    fn buffer_changes(desk: &mut Desk) -> Vec<(u64, clackatoo_protocol::Change)> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Edit { seq, change, .. } => Some((seq, change)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn what_an_editor_types_while_disconnected_goes_once_connected_again() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate, Change};

        let buffer = BufferId(7);
        let layout = side_by_side(vec![
            Layout::Pane(LayoutPane {
                id: PaneId(1),
                content: Content::Buffer(buffer),
                home: None,
                key: None,
            }),
            pane(2),
        ]);
        let snapshot = |text: &str, version, applied| {
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: text.into(),
                version,
                modified: false,
                applied,
                landed: None,
                daemon: 5,
            })
        };
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot("x\n", 0, None));
        assert!(desk.takes_keys_offline(), "an editor edits its copy");
        type_text(&mut desk, "Aa");
        let sent = buffer_changes(&mut desk);
        assert_eq!(sent.len(), 1, "on its way when the connection goes");

        desk.lost();
        type_text(&mut desk, "b");
        assert_eq!(editor_text(&desk), "xab\n");
        assert!(buffer_changes(&mut desk).is_empty(), "nowhere to send it");
        desk.connected(list(), false);
        let window = desk.editors.window.0;
        let resume = Some(clackatoo_protocol::Resume {
            daemon: 5,
            version: 0,
        });
        assert!(answer_all(&mut desk).contains(&Request::AttachBuffer {
            buffer,
            window,
            resume
        }));
        // The first change never arrived; someone else's did.
        let resumed = BufferUpdate::Resumed {
            daemon: 5,
            path: "/notes".into(),
            edits: vec![clackatoo_protocol::Edit::replace(2, 0, "", "Y")],
            applied: None,
            landed: None,
            modified: true,
        };
        desk.buffer_changed(buffer, resumed);
        assert_eq!(editor_text(&desk), "Yxab\n", "both, once");
        let resent: Vec<Change> = buffer_changes(&mut desk)
            .into_iter()
            .map(|(_, change)| change)
            .collect();
        assert!(
            matches!(&resent[..], [Change::Edit { base: 1, .. }]),
            "{resent:?}"
        );
    }

    #[test]
    fn a_view_update_that_cant_be_read_is_made_up_for_by_attaching_again() {
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, pane(3))])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.missed_view(TerminalId(3), false);
        assert_eq!(
            answer_all(&mut desk),
            [Request::Attach {
                terminal: TerminalId(3)
            }]
        );
        // One it doesn't follow is none of its business.
        desk.missed_view(TerminalId(9), false);
        assert!(answer_all(&mut desk).is_empty());
        desk.lost();
        desk.missed_view(TerminalId(3), false);
        assert!(desk.take_outbox().is_empty());
    }

    #[test]
    fn missing_a_view_attaches_once_till_its_snapshot_and_not_without_end() {
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, pane(3))])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        let terminal = TerminalId(3);
        let attach = Request::Attach { terminal };
        let snapshot = ViewUpdate::Snapshot(View::capture(&Terminal::new(
            Size::new(10, 2).unwrap(),
            &Config::default(),
        )));
        // Missed twice before the snapshot asked for comes: asked once.
        desk.missed_view(terminal, false);
        desk.missed_view(terminal, false);
        assert_eq!(answer_all(&mut desk), std::slice::from_ref(&attach));
        desk.view_changed(terminal, &snapshot);
        // Missed again and again, each miss counting: after a few in a
        // minute, the window stops asking, and says why.
        for _ in 2..MOST_MISSED {
            desk.missed_view(terminal, false);
            assert_eq!(answer_all(&mut desk), std::slice::from_ref(&attach));
            desk.view_changed(terminal, &snapshot);
        }
        desk.missed_view(terminal, false);
        assert!(answer_all(&mut desk).is_empty());
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|said| said.contains("says about terminal 3")),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn missing_the_workspaces_asks_for_them_again() {
        let list = |name: &str| workspaces(vec![workspace(1, name, vec![tab(1, pane(3))])]);
        let mut desk = Desk::default();
        desk.connected(list("w"), false);
        answer_all(&mut desk);
        desk.missed_workspaces();
        let sent = desk.take_outbox();
        assert_eq!(requests(&sent), [&Request::WatchWorkspaces]);
        assert!(desk.answered(Response::Workspaces(list("renamed"))));
        assert_eq!(desk.workspaces.list[0].name, "renamed");
    }

    #[test]
    fn missing_a_buffer_again_and_again_stops_asking() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let snapshot = BufferUpdate::Snapshot(BufferSnapshot {
            path: "/notes".into(),
            text: "x\n".into(),
            version: 0,
            modified: false,
            applied: None,
            landed: None,
            daemon: 5,
        });
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot.clone());
        let attaches = |desk: &mut Desk| {
            (answer_all(desk).into_iter())
                .filter(|request| matches!(request, Request::AttachBuffer { .. }))
                .count()
        };
        for _ in 0..MOST_MISSED {
            desk.missed_buffer(buffer, false);
            assert_eq!(attaches(&mut desk), 1);
            desk.buffer_changed(buffer, snapshot.clone());
        }
        desk.missed_buffer(buffer, false);
        assert_eq!(attaches(&mut desk), 0);
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|said| said.contains("says about a file being edited")),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn a_view_attached_again_isnt_left_waiting_for_a_snapshot_that_wont_come() {
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, pane(3))])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        let terminal = TerminalId(3);
        let attaches = |desk: &mut Desk| {
            let sent = desk.take_outbox();
            requests(&sent)
                .into_iter()
                .filter(|request| matches!(request, Request::Attach { .. }))
                .count()
        };
        // Refused: the next miss attaches again.
        desk.missed_view(terminal, false);
        assert_eq!(attaches(&mut desk), 1);
        desk.answered(Response::Error(clackatoo_protocol::Error::NoSuchTerminal(
            terminal,
        )));
        desk.missed_view(terminal, false);
        assert_eq!(attaches(&mut desk), 1);
        desk.answered(Response::Done);
        // The snapshot it waits for, unreadable too: it asks again.
        desk.missed_view(terminal, true);
        assert_eq!(attaches(&mut desk), 1);
    }

    #[test]
    fn a_buffer_copy_whose_answer_never_comes_readable_says_so() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: "x\n".into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon: 5,
            }),
        );
        let attaches = |desk: &mut Desk| {
            (answer_all(desk).into_iter())
                .filter(|request| matches!(request, Request::AttachBuffer { .. }))
                .count()
        };
        desk.missed_buffer(buffer, false);
        assert_eq!(attaches(&mut desk), 1);
        // Its answer, unreadable: it attaches again.
        desk.missed_buffer(buffer, true);
        assert_eq!(attaches(&mut desk), 1);
        // What follows, unreadable too, counts while it waits, and once
        // too many have, the bar says so.
        for _ in 2..MOST_MISSED {
            desk.missed_buffer(buffer, false);
            assert_eq!(attaches(&mut desk), 0);
        }
        assert_eq!(desk.message, None);
        desk.missed_buffer(buffer, false);
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|said| said.contains("says about a file being edited")),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn buffer_updates_missed_while_attaching_again_attach_only_once() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let snapshot = |text: &str, version, applied| {
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: text.into(),
                version,
                modified: true,
                applied,
                landed: applied.map(|_| version),
                daemon: 5,
            })
        };
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot("x\n", 0, None));
        type_text(&mut desk, "Aa");
        let [(seq, _)] = buffer_changes(&mut desk)[..] else {
            panic!("one change on its way");
        };
        // Two in one round trip: one attaching again takes in both.
        desk.missed_buffer(buffer, false);
        desk.missed_buffer(buffer, false);
        let attaches = answer_all(&mut desk)
            .into_iter()
            .filter(|request| matches!(request, Request::AttachBuffer { .. }))
            .count();
        assert_eq!(attaches, 1);
        // Its answer, then a key typed after: no second, older snapshot
        // comes to undo it.
        desk.buffer_changed(buffer, snapshot("xa\n", 1, Some(seq)));
        type_text(&mut desk, "b");
        assert_eq!(buffer_changes(&mut desk).len(), 1);
        assert_eq!(editor_text(&desk), "xab\n");
    }

    #[test]
    fn a_daemon_relaying_another_machines_buffer_has_the_window_attach_again_keeping_its_change() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: "x\n".into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon: 5,
            }),
        );
        type_text(&mut desk, "Aa");
        let sent = buffer_changes(&mut desk);
        let [_] = sent[..] else {
            panic!("one change on its way: {sent:?}");
        };

        // A daemon relaying it can't bring the window up to date itself:
        // it attaches again, from what it knew, as after a lost connection.
        desk.buffer_changed(buffer, BufferUpdate::Reattach);
        let window = desk.editors.window.0;
        let resume = Some(clackatoo_protocol::Resume {
            daemon: 5,
            version: 0,
        });
        assert_eq!(
            answer_all(&mut desk),
            [Request::AttachBuffer {
                buffer,
                window,
                resume
            }]
        );
        assert_eq!(editor_text(&desk), "xa\n", "what it typed is kept");
    }

    #[test]
    fn a_daemon_relaying_another_machines_buffer_has_the_window_attach_again_however_often_it_asks()
    {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let snapshot = |version| {
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: "x\n".into(),
                version,
                modified: false,
                applied: None,
                landed: None,
                daemon: 5,
            })
        };
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot(0));
        // Past how often a window asks again of itself, in a minute: the
        // daemon only asks when the copy would drift from the text there.
        for version in 1..=MOST_MISSED as u64 + 1 {
            desk.buffer_changed(buffer, BufferUpdate::Reattach);
            let asked = answer_all(&mut desk);
            assert!(
                matches!(&asked[..], [Request::AttachBuffer { .. }]),
                "asked again the {version}th time: {asked:?}"
            );
            // Nothing it says is taken in until what that brings comes.
            assert!(
                desk.buffer_copy(buffer)
                    .is_some_and(crate::editor::BufferCopy::is_missing)
            );
            desk.buffer_changed(buffer, snapshot(version));
            assert!(
                !desk
                    .buffer_copy(buffer)
                    .is_some_and(crate::editor::BufferCopy::is_missing)
            );
        }
        // Told again while still attaching again, it waits for what's
        // coming, which brings everything.
        desk.buffer_changed(buffer, BufferUpdate::Reattach);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, BufferUpdate::Reattach);
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn a_buffer_update_that_cant_be_read_brings_the_copy_up_to_date_once() {
        use clackatoo_protocol::{Author, BufferSnapshot, BufferUpdate, Edit};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/notes".into(),
                text: "x\n".into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon: 5,
            }),
        );
        type_text(&mut desk, "Aa");
        let sent = buffer_changes(&mut desk);
        let [(seq, _)] = sent[..] else {
            panic!("one change on its way: {sent:?}");
        };

        // Someone else's edit came, from a newer daemon, as something this
        // window couldn't read: it attaches again, from what it knew.
        desk.missed_buffer(buffer, false);
        let window = desk.editors.window.0;
        let resume = Some(clackatoo_protocol::Resume {
            daemon: 5,
            version: 0,
        });
        assert_eq!(
            answer_all(&mut desk),
            [Request::AttachBuffer {
                buffer,
                window,
                resume
            }]
        );
        // What came after it, before the answer, follows what was missed:
        // it's passed over.
        desk.buffer_changed(buffer, BufferUpdate::Acked { version: 2, seq });
        desk.buffer_changed(
            buffer,
            BufferUpdate::Edited {
                version: 3,
                edit: Edit::replace(4, 4, "", "!"),
                author: Author::User,
                answers: None,
            },
        );
        assert_eq!(editor_text(&desk), "xa\n");
        // The answer: the edit missed, then the window's own, which landed.
        let resumed = BufferUpdate::Resumed {
            daemon: 5,
            path: "/notes".into(),
            edits: vec![
                Edit::replace(2, 0, "", "Y"),
                Edit::replace(3, 2, "", "a"),
                Edit::replace(4, 4, "", "!"),
            ],
            applied: Some(seq),
            landed: Some(2),
            modified: true,
        };
        desk.buffer_changed(buffer, resumed);
        assert_eq!(editor_text(&desk), "Yxa\n!", "each, once");
        assert!(buffer_changes(&mut desk).is_empty(), "nothing goes twice");
        // And it goes on as before.
        type_text(&mut desk, "b");
        assert_eq!(buffer_changes(&mut desk).len(), 1);
    }

    /// A window with buffer 7 open at "x\n", from daemon 5, which typed
    /// `typed` and lost its connection; then daemon 6, restarted, which
    /// has `text` for it.
    fn typed_across_a_restart(typed: &str, text: &str) -> Desk {
        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let snapshot = |text: &str, daemon| {
            clackatoo_protocol::BufferUpdate::Snapshot(clackatoo_protocol::BufferSnapshot {
                path: "/notes".into(),
                text: text.into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon,
            })
        };
        let list = || workspaces(vec![workspace(1, "w", vec![tab(1, layout.clone())])]);
        let mut desk = Desk::default();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot("x\n", 5));
        type_text(&mut desk, typed);
        normal_mode(&mut desk);
        desk.lost();
        desk.connected(list(), false);
        answer_all(&mut desk);
        desk.buffer_changed(buffer, snapshot(text, 6));
        desk
    }

    /// What a window sent of its changes to buffers, and whether it asked
    /// to save, and if so, forced.
    fn edits_and_save(desk: &mut Desk) -> (Vec<clackatoo_protocol::Change>, Option<bool>) {
        let mut save = None;
        let mut edits = Vec::new();
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Edit { change, .. } => {
                    assert!(save.is_none(), "an edit after the save");
                    edits.push(change);
                }
                ClientMessage::Request(Request::Save { force, .. }) => save = Some(force),
                _ => {}
            }
        }
        (edits, save)
    }

    #[test]
    fn after_a_daemon_restarts_an_editors_changes_go_again_if_the_file_is_as_it_was() {
        let mut desk = typed_across_a_restart("Aa", "x\n");
        assert_eq!(editor_text(&desk), "xa\n");
        assert_eq!(
            desk.message.as_deref(),
            Some("reconnected; your unsaved changes to /notes were kept")
        );
        let (edits, save) = edits_and_save(&mut desk);
        assert_eq!(edits.len(), 1, "sent, with no :w");
        assert_eq!(save, None);
    }

    #[test]
    fn two_windows_keeping_their_text_after_a_restart_never_write_over_each_other() {
        use clackatoo_protocol::{Author, BufferUpdate, Change};

        let mut first = typed_across_a_restart("Aa", "other\n");
        let mut second = typed_across_a_restart("Ab", "other\n");
        assert_eq!(editor_text(&first), "xa\n", "kept");
        assert_eq!(
            first.message.as_deref(),
            Some(
                "the file changed since the daemon restarted; your changes to /notes are kept \
                 here: :w! writes them, :e! takes the daemon's"
            )
        );
        // It's a warning, in the warning colour.
        assert!(matches!(
            first.status(),
            Status::Tabs {
                kind: NoteKind::Warning { .. },
                ..
            }
        ));
        assert_eq!(
            edits_and_save(&mut first),
            (vec![], None),
            "nothing unasked"
        );
        let refused = "the file changed since the daemon restarted: :w! writes this window's \
                       text, :e! takes the daemon's";
        command(&mut first, "w");
        assert_eq!(first.message.as_deref(), Some(refused));
        assert_eq!(edits_and_save(&mut first), (vec![], None));
        // Asked to, the first replaces the daemon's text with its own.
        command(&mut first, "w!");
        let (edits, save) = edits_and_save(&mut first);
        assert_eq!(save, Some(true));
        let [Change::Edit { edit, .. }] = &edits[..] else {
            panic!("one edit: {edits:?}");
        };
        let edited = BufferUpdate::Edited {
            version: 1,
            edit: edit.clone(),
            author: Author::User,
            answers: None,
        };
        second.buffer_changed(BufferId(7), edited);
        // The second's `:w` can't put the first's back as it was.
        command(&mut second, "w");
        assert_eq!(second.message.as_deref(), Some(refused));
        assert_eq!(edits_and_save(&mut second), (vec![], None));
        assert_eq!(editor_text(&second), "xb\n");
        // `:e!` takes the daemon's, the first's.
        command(&mut second, "e!");
        assert_eq!(editor_text(&second), "xa\n");
        assert_eq!(edits_and_save(&mut second), (vec![], None));
    }

    #[test]
    fn a_terminal_turns_down_keys_it_cant_send_and_says_so() {
        let mut desk = Desk::default();
        desk.connected(
            workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
            false,
        );
        answer_all(&mut desk);
        assert!(!desk.takes_keys_offline());
        desk.refuse_offline();
        assert_eq!(
            desk.message.as_deref(),
            Some("not connected: keys aren't sent")
        );
        assert!(desk.flash_ends().is_some(), "the pane lights up");
        assert!(desk.take_outbox().is_empty());
    }

    #[test]
    fn an_editor_asks_its_language_server_once_typing_pauses() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate, Query, ServerFeatures, Trigger};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let mut desk = Desk::default();
        desk.connected(
            workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
            false,
        );
        answer_all(&mut desk);
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/src/a.rs".into(),
                text: "x\n".into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon: 0,
            }),
        );
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(buffer, BufferUpdate::Features(features));
        type_text(&mut desk, "Aab");
        assert!(desk.editor_wakes().is_some(), "the window wakes to ask");
        // Not yet: the edits go, answered as the daemon would.
        let mut version = 0;
        let mut queries = Vec::new();
        let mut ticked = false;
        for _ in 0..10 {
            for message in desk.take_outbox() {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(buffer, BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query { query, at, .. } => queries.push((at, query)),
                    _ => {}
                }
            }
            if !ticked && queries.is_empty() {
                let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
                assert!(desk.tick_editors(later));
                ticked = true;
            }
        }
        assert_eq!(queries, [(3, Query::Completion(Trigger::Invoked))]);
        assert_eq!(desk.editor_wakes(), None);
    }

    #[test]
    fn e_opens_a_file_in_place_of_the_one_shown() {
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(BufferId(7)),
            home: None,
            key: None,
        });
        let mut desk = Desk::default();
        desk.connected(
            workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
            false,
        );
        answer_all(&mut desk);
        command(&mut desk, "e src/other.rs");
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: clackatoo_protocol::OpenFile {
                    path: "src/other.rs".into(),
                    cwd: None,
                },
            }]
        );
    }

    #[test]
    fn devices_asking_to_pair_are_asked_about_until_a_window_answers() {
        let mut desk = desk(workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]));
        let ask = clackatoo_protocol::PairingAsk {
            name: "iPhone".into(),
            from: "jack@github".into(),
            until: 0,
        };
        let asking =
            |asks: Vec<clackatoo_protocol::PairingAsk>, locked| clackatoo_protocol::PairingState {
                asking: asks,
                locked,
            };
        assert!(desk.pairing_asked(asking(vec![ask.clone()], false)));
        assert!(
            !desk.pairing_asked(asking(vec![ask], false)),
            "the same again changes nothing"
        );
        let asked = |desk: &Desk| {
            matches!(desk.status(), Status::Typing { text, .. }
                if text.starts_with("iPhone (jack@github) asks to control clackatoo"))
        };
        assert!(asked(&desk));
        // No key answers it: typing meant for the terminal stays the
        // terminal's.
        type_keys(&mut desk, "y\r");
        assert!(asked(&desk));
        desk.devices_command(crate::command::DevicesCommand::Allow("0042".into()));
        assert_eq!(
            answer_all(&mut desk),
            [Request::AnswerPairing {
                code: Some("0042".into())
            }]
        );
        desk.pairing_asked(asking(Vec::new(), false));
        assert!(!asked(&desk));
        // Locked after too many wrong passwords: the bar says so, and how
        // to unlock it.
        desk.pairing_asked(asking(Vec::new(), true));
        assert!(matches!(desk.status(), Status::Typing { text, .. }
            if text.contains(":Devices unlock")));
        desk.devices_command(crate::command::DevicesCommand::Unlock);
        assert_eq!(answer_all(&mut desk), [Request::UnlockPairing]);
    }

    #[test]
    fn the_daemon_hears_whether_the_windows_user_is_there() {
        let mut desk = desk(workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]));
        let present = |desk: &mut Desk| -> Vec<bool> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Present { here } => Some(here),
                    _ => None,
                })
                .collect()
        };
        desk.window_focus(false);
        assert_eq!(present(&mut desk), [false]);
        // Used while it hasn't the focus, as by a click through: not said.
        desk.user_active();
        assert_eq!(present(&mut desk), [] as [bool; 0]);
        desk.window_focus(true);
        assert_eq!(present(&mut desk), [true]);
        // Said at most once a minute while it's used.
        desk.user_active();
        assert_eq!(present(&mut desk), [] as [bool; 0]);
    }

    #[test]
    fn a_language_offered_is_asked_about_as_languageinstall_says() {
        use clackatoo_protocol::BufferUpdate;

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let mut desk = Desk::default();
        desk.connected(
            workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
            false,
        );
        answer_all(&mut desk);
        let offer = || BufferUpdate::Offer {
            language: "toml".into(),
            grammar: true,
            server: Some("taplo".into()),
        };
        let install = Request::Install {
            language: "toml".into(),
            grammar: true,
            server: Some("taplo".into()),
            // Which machine to install it on, as the buffer says.
            buffer: Some(buffer),
        };
        let asked = |desk: &Desk| {
            matches!(desk.status(), Status::Typing { text, .. }
                if text == "Install toml's grammar and taplo? [Y/n]")
        };

        // Asked, and y installs.
        assert!(desk.buffer_changed(buffer, offer()));
        assert!(asked(&desk));
        type_keys(&mut desk, "y");
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), std::slice::from_ref(&install));
        desk.installed(Ok("toml's grammar and taplo".into()));
        assert!(
            matches!(desk.status(), Status::Tabs { note: Some(note), .. }
            if note == "Installed toml's grammar and taplo")
        );

        // n declines for good.
        desk.buffer_changed(buffer, offer());
        type_keys(&mut desk, "n");
        assert_eq!(
            answer_all(&mut desk),
            [Request::Decline {
                language: "toml".into(),
                buffer: Some(buffer),
            }]
        );

        // Not asked while the command line is open: n is typed there.
        type_keys(&mut desk, ":");
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        type_keys(&mut desk, "n");
        assert!(matches!(desk.status(), Status::Typing { text, .. } if text == ":n▏"));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(asked(&desk), "asked once it's closed");

        // Any other key leaves it unanswered, and goes on to the editor.
        type_keys(&mut desk, "j");
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), []);

        // Nor while the finder is open: y and Enter are its.
        command(&mut desk, "Buffers");
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        type_keys(&mut desk, "y");
        assert_eq!(desk.finder_view((10, 60), None).unwrap().query, "y▏");
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(asked(&desk), "asked once it's closed");
        assert!(!answer_all(&mut desk).contains(&install));
        type_keys(&mut desk, "j");

        // Or it's installed without asking, or never.
        command(&mut desk, "set languageinstall=always");
        answer_all(&mut desk);
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), [install]);
        command(&mut desk, "set li=never");
        answer_all(&mut desk);
        assert!(!desk.buffer_changed(buffer, offer()));
        assert_eq!(answer_all(&mut desk), []);
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one story, told in order")]
    fn a_file_is_edited_written_and_kept_open_while_it_has_changes() {
        use clackatoo_protocol::{BufferUpdate, Change, Edit};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let mut desk = Desk::default();
        desk.set_area(Some(Area {
            rect: crate::layout::Rect {
                x: 0.0,
                y: 0.0,
                width: 810.0,
                height: 480.0,
            },
            cell: [10.0, 20.0],
            gap: 10.0,
            inset: 5.0,
        }));
        desk.connected(
            workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
            false,
        );
        let window = desk.editors.window.0;
        let attach = Request::AttachBuffer {
            buffer,
            window,
            resume: None,
        };
        let asked = answer_all(&mut desk);
        assert!(asked.contains(&attach), "{asked:?}");
        desk.buffer_changed(buffer, snapshot_of("/src/notes.txt", "notes\n"));
        assert_eq!(desk.title(), Some("notes.txt"));

        type_text(&mut desk, "A!");
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Pane));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        let sent = buffer_changes(&mut desk);
        let edit = Edit::replace(6, 5, "", "!");
        let [(seq, change)] = &sent[..] else {
            panic!("one change: {sent:?}");
        };
        assert_eq!(
            *change,
            Change::Edit {
                edit,
                base: 0,
                step: true
            }
        );
        desk.buffer_changed(
            buffer,
            BufferUpdate::Acked {
                version: 1,
                seq: *seq,
            },
        );
        desk.buffer_changed(buffer, BufferUpdate::Modified(true));
        let Status::Tabs { mode, note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(mode, Some(ModeName::Normal));
        assert_eq!(note.as_deref(), Some("notes.txt [+]  1:6"));

        // :q won't throw the change away; :w writes it.
        command(&mut desk, "q");
        assert!(answer_all(&mut desk).is_empty());
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains(":q!"))
        );
        command(&mut desk, "w");
        let asked = desk.take_outbox();
        assert!(matches!(
            asked.as_slice(),
            [ClientMessage::Request(Request::Save { force: false, .. })]
        ));
        desk.answered(Response::Done);
        assert_eq!(desk.message.as_deref(), Some("notes.txt written"));
        desk.buffer_changed(buffer, BufferUpdate::Modified(false));
        // What the daemon says of the file goes in the bar.
        let notice = "agent changed notes.txt where you have unsaved edits";
        assert!(desk.buffer_changed(buffer, BufferUpdate::Notice(notice.into())));
        assert_eq!(desk.message.as_deref(), Some(notice));
        // A change from outside is read again, as `autoread` says...
        assert!(!desk.buffer_changed(buffer, BufferUpdate::ChangedOnDisk));
        assert_eq!(requests(&desk.take_outbox()), [&Request::Reload { buffer }]);
        desk.answered(Response::Done);
        // ...or said.
        command(&mut desk, "set noautoread");
        assert!(desk.buffer_changed(buffer, BufferUpdate::ChangedOnDisk));
        assert!(requests(&desk.take_outbox()).is_empty());
        assert_eq!(
            desk.message.as_deref(),
            Some("notes.txt changed on disk: :e! takes it, :w! keeps yours")
        );
        command(&mut desk, "q");
        assert_eq!(
            answer_all(&mut desk),
            [Request::ClosePane { pane: PaneId(1) }]
        );
    }

    // --- The jump list, in editors ------------------------------------------

    fn file_pane(pane: u32, buffer: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(pane),
            content: Content::Buffer(BufferId(buffer)),
            home: None,
            key: None,
        })
    }

    /// One workspace of one tab laid out as `layout`.
    fn one_tab(layout: Layout) -> Workspaces {
        workspaces(vec![workspace(1, "w", vec![tab(1, layout)])])
    }

    /// Gives `buffer` its text, as attaching does.
    fn load(desk: &mut Desk, buffer: u32, path: &str, text: &str) {
        let snapshot = clackatoo_protocol::BufferSnapshot {
            path: path.into(),
            text: text.into(),
            version: 0,
            modified: false,
            applied: None,
            landed: None,
            daemon: 0,
        };
        desk.buffer_changed(
            BufferId(buffer),
            clackatoo_protocol::BufferUpdate::Snapshot(snapshot),
        );
    }

    /// What the bar says of the focused pane: for an editor, the file and
    /// where its cursor is.
    fn note(desk: &Desk) -> String {
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        note.unwrap_or_default()
    }

    /// Answers what was asked, which must include opening the file at
    /// `path` in pane 1, and opens it, as the daemon would, as `buffer`,
    /// with `text`.
    fn open_asked(desk: &mut Desk, buffer: u32, path: &str, text: &str) {
        let asked = answer_all(desk);
        let opened: Vec<(PaneId, &str)> = (asked.iter())
            .filter_map(|request| match request {
                Request::OpenHere { pane, file } => Some((*pane, file.path.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(opened, [(PaneId(1), path)], "{asked:?}");
        desk.workspaces_changed(one_tab(file_pane(1, buffer)));
        answer_all(desk);
        load(desk, buffer, path, text);
    }

    /// The files asked to open in panes in place of theirs, not answered
    /// yet.
    fn opens_asked(desk: &mut Desk) -> Vec<String> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::OpenHere { file, .. }) => Some(file.path),
                _ => None,
            })
            .collect()
    }

    /// The daemon opens the oldest file asked for in pane 1, as `buffer`:
    /// it shows the pane with it, then answers.
    fn opened(desk: &mut Desk, buffer: u32) {
        desk.workspaces_changed(one_tab(file_pane(1, buffer)));
        desk.answered(Response::Done);
    }

    fn b_rs_line_3() -> clackatoo_protocol::FileLocation {
        clackatoo_protocol::FileLocation {
            path: "/src/b.rs".into(),
            line: 2,
            col: 1,
            offset: None,
            text: String::new(),
        }
    }

    const A_RS: &str = "fn a() {}\nfn b() {}\nfn c() {}\n";
    const B_RS: &str = "x\ny\nzzz\n";

    #[test]
    fn a_second_jump_while_a_file_opens_goes_where_it_points() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/c.rs", TEN_LINES);
        type_text(&mut desk, "5G");
        command(&mut desk, "e /b.rs");
        open_asked(&mut desk, 8, "/b.rs", B_RS);
        command(&mut desk, "e /c.rs");
        open_asked(&mut desk, 9, "/c.rs", TEN_LINES);
        type_text(&mut desk, "8j");
        // Back to b.rs, and before it's open, back again to c.rs's line 5.
        ctrl(&mut desk, "o");
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/b.rs", "/c.rs"]);
        opened(&mut desk, 10);
        load(&mut desk, 10, "/b.rs", B_RS);
        opened(&mut desk, 11);
        answer_all(&mut desk);
        load(&mut desk, 11, "/c.rs", TEN_LINES);
        assert_eq!(note(&desk), "c.rs  5:1");
    }

    #[test]
    fn ctrl_o_while_gd_opens_a_file_comes_back_and_ctrl_i_goes_there() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        // Before b.rs is open: you're where gd goes, as far as the jump
        // list knows, so there's somewhere to come back to.
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/a.rs"]);
        opened(&mut desk, 8);
        load(&mut desk, 8, "/src/b.rs", B_RS);
        opened(&mut desk, 9);
        answer_all(&mut desk);
        load(&mut desk, 9, "/src/a.rs", A_RS);
        assert_eq!(note(&desk), "a.rs  2:4");
        ctrl(&mut desk, "i");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  3:2");
    }

    #[test]
    fn a_file_that_fails_to_open_keeps_a_later_jumps_place() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        type_text(&mut desk, "j");
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/a.rs"]);
        // b.rs can't be opened; a.rs is shown already.
        desk.answered(Response::Error(clackatoo_protocol::Error::NotAFile(
            PaneId(1),
        )));
        assert!(desk.message.take().is_some());
        desk.answered(Response::Done);
        assert_eq!(note(&desk), "a.rs  2:4");
    }

    #[test]
    fn opening_a_file_forgets_where_the_cursor_was_to_go_before() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        // Another file, before b.rs is open.
        command(&mut desk, "e /src/c.rs");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/c.rs"]);
        opened(&mut desk, 8);
        load(&mut desk, 8, "/src/b.rs", B_RS);
        opened(&mut desk, 9);
        answer_all(&mut desk);
        load(&mut desk, 9, "/src/c.rs", "c\n");
        // b.rs opened later isn't where gd went.
        command(&mut desk, "e /src/b.rs");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  1:1");
    }

    #[test]
    fn going_back_in_insert_mode_keeps_typing_where_it_was_as_a_new_step() {
        use clackatoo_protocol::{BufferUpdate, Change};

        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/t.txt", "abc\nxyz\n");
        type_text(&mut desk, "Ad");
        let sent = buffer_changes(&mut desk);
        assert_eq!(sent.len(), 1);
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Acked {
                version: 1,
                seq: sent[0].0,
            },
        );
        // To the terminal and back, still in insert mode.
        ctrl_b(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2));
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        press(&mut desk, Key::Character("O".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(note(&desk), "t.txt  1:5", "past the line's end");
        type_text(&mut desk, "e");
        let sent = buffer_changes(&mut desk);
        let [(_, Change::Edit { edit, step, .. })] = sent.as_slice() else {
            panic!("one edit: {sent:?}");
        };
        assert_eq!(*edit, clackatoo_text::Edit::replace(9, 4, "", "e"));
        assert!(*step, "a new undo step");
    }

    /// Answers the language server query just asked with `found`.
    fn answer_query(desk: &mut Desk, buffer: u32, found: Vec<clackatoo_protocol::FileLocation>) {
        let mut id = None;
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Query { id: asked, .. } => id = Some(asked),
                ClientMessage::Request(_) => {
                    desk.answered(Response::Done);
                }
                _ => {}
            }
        }
        let update = clackatoo_protocol::BufferUpdate::Answer {
            id: id.expect("a query"),
            version: 0,
            answer: clackatoo_protocol::Answer::Locations(found),
        };
        desk.buffer_changed(BufferId(buffer), update);
    }

    fn escape(desk: &mut Desk) {
        press(
            desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
    }

    const TEN_LINES: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";

    #[test]
    fn ctrl_o_comes_back_from_gd_to_another_file_and_ctrl_i_goes_again() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", "fn a() {}\nfn b() {}\n");
        type_text(&mut desk, "jw");
        assert_eq!(note(&desk), "a.rs  2:4");
        type_text(&mut desk, "gd");
        let there = clackatoo_protocol::FileLocation {
            path: "/src/b.rs".into(),
            line: 2,
            col: 1,
            offset: None,
            text: String::new(),
        };
        answer_query(&mut desk, 7, vec![there]);
        // Each file opens in the pane in turn, as the daemon would, with a
        // buffer of its own, and the cursor lands once its text is there.
        open_asked(&mut desk, 8, "/src/b.rs", "x\ny\nzzz\n");
        assert_eq!(note(&desk), "b.rs  3:2");
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/src/a.rs", "fn a() {}\nfn b() {}\n");
        assert_eq!(note(&desk), "a.rs  2:4", "back where gd left");
        ctrl(&mut desk, "i");
        open_asked(&mut desk, 10, "/src/b.rs", "x\ny\nzzz\n");
        assert_eq!(note(&desk), "b.rs  3:2", "forward again");
        // Nothing further forward.
        ctrl(&mut desk, "i");
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn going_back_then_jumping_elsewhere_forgets_the_way_forward() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "G");
        assert_eq!(note(&desk), "t.txt  10:1");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1");
        type_text(&mut desk, "5G");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1", "line 10 is forgotten");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1", "nothing further back");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1");
        assert!(
            answer_all(&mut desk).is_empty(),
            "nothing opened: it's the file shown"
        );
    }

    #[test]
    fn a_count_goes_that_many_places_back_or_forward() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "3G5G7G9G");
        type_text(&mut desk, "3");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  3:1");
        // More than there are: nowhere, as in Vim.
        type_text(&mut desk, "9");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  3:1");
        type_text(&mut desk, "2");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  7:1");
        // The count was the binding's, not the editor's next motion's.
        type_text(&mut desk, "j");
        assert_eq!(note(&desk), "t.txt  8:1");
    }

    #[test]
    fn jumps_from_one_line_one_after_another_are_remembered_once() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", "a foo foo\nb\nfoo\n");
        // Two jumps along line 1, then off it.
        type_text(&mut desk, "/foo");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(note(&desk), "t.txt  1:3");
        type_text(&mut desk, "n");
        assert_eq!(note(&desk), "t.txt  1:7");
        type_text(&mut desk, "n");
        assert_eq!(note(&desk), "t.txt  3:1");
        // Back to where line 1 was left last, and no further, even from
        // another line.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:7");
        type_text(&mut desk, "j");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  2:1");
        type_text(&mut desk, "k$");
        // A search that finds nothing isn't a jump.
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  3:1");
        type_text(&mut desk, "/nowhere");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:7");
    }

    #[test]
    fn places_in_panes_closed_since_are_skipped() {
        let layout = side_by_side(vec![pane(1), file_pane(2, 8), pane(3)]);
        let mut desk = desk(one_tab(layout));
        load(&mut desk, 8, "/t.txt", TEN_LINES);
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        type_text(&mut desk, "G");
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        // The editor closes, its jumps with it.
        desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), pane(3)])));
        answer_all(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "i");
        assert_eq!(shown_pane(&desk), PaneId(3));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::OpenHere { .. })),
            "nothing opened in place of what's shown"
        );
    }

    #[test]
    fn places_move_with_edits_made_since() {
        use clackatoo_protocol::{Author, BufferUpdate};
        use clackatoo_text::Edit;

        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "5Gl");
        type_text(&mut desk, "G");
        // Someone else puts a line in at the top...
        let edit = Edit::replace(TEN_LINES.len(), 0, "", "new\n");
        let update = BufferUpdate::Edited {
            version: 1,
            edit,
            author: Author::Agent(TerminalId(9)),
            answers: None,
        };
        desk.buffer_changed(BufferId(7), update);
        assert_eq!(note(&desk), "t.txt  11:1");
        // ...and so does this window, after going to the top.
        type_text(&mut desk, "ggOtop");
        escape(&mut desk);
        assert_eq!(note(&desk), "t.txt  1:3");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  12:1", "where gg left");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  7:2", "where G left");
        // The start of the file, where both lines went in, went down with
        // its line.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  3:1", "where 5G left");
        // As it does with a line opened above it from there.
        type_text(&mut desk, "Oabove");
        escape(&mut desk);
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  8:2", "where G left, moved down");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  4:1", "where 5G left, moved down");
    }

    #[test]
    fn a_file_opened_again_keeps_its_places_within_its_text() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/a.txt", TEN_LINES);
        type_text(&mut desk, "3G9G");
        command(&mut desk, "e /b.txt");
        open_asked(&mut desk, 8, "/b.txt", "b\n");
        // a.txt was cut short while it wasn't open here: line 9 is gone.
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/a.txt", "a\nb\nc\nd\ne\nf\n");
        assert_eq!(note(&desk), "a.txt  6:1");
        // Its places are in the new text, and move with edits to it.
        type_text(&mut desk, "5kOx");
        escape(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "a.txt  4:1", "line 3, moved down by one");
    }

    /// The finder's messages for the daemon, answering every request with
    /// "done" on the way.
    fn finds(desk: &mut Desk) -> Vec<ClientMessage> {
        let messages = desk.take_outbox();
        for message in &messages {
            if let ClientMessage::Request(_) = message {
                desk.answered(Response::Done);
            }
        }
        (messages.into_iter())
            .filter(|message| {
                matches!(
                    message,
                    ClientMessage::Find { .. } | ClientMessage::StopFinding { .. }
                )
            })
            .collect()
    }

    /// The number and what's asked of the one search in `messages`, besides
    /// previews.
    fn search_in(messages: &[ClientMessage]) -> (u64, clackatoo_protocol::Search) {
        use clackatoo_protocol::Search;
        let found: Vec<(u64, Search)> = (messages.iter())
            .filter_map(|message| match message {
                ClientMessage::Find { id, search, .. }
                    if !matches!(search, Search::Preview { .. }) =>
                {
                    Some((*id, search.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "{messages:?}");
        found[0].clone()
    }

    /// A desk with one pane, editing `path`, whose text is `text`.
    fn editing(path: &str, text: &str) -> Desk {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};
        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
            home: None,
            key: None,
        });
        let mut desk = desk(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: path.into(),
                text: text.into(),
                version: 0,
                modified: false,
                applied: None,
                landed: None,
                daemon: 0,
            }),
        );
        answer_all(&mut desk);
        desk
    }

    /// Gives the search numbered `id` the root `/p` and `files`.
    fn found_files(desk: &mut Desk, id: u64, files: &[&str]) {
        use clackatoo_protocol::Found;
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        desk.found(
            id,
            Found::Files(files.iter().map(|&file| file.to_owned()).collect()),
        );
    }

    /// The text of each row the finder shows.
    fn finder_rows(desk: &mut Desk) -> Vec<String> {
        let view = desk
            .finder_view((10, 60), Some((10, 60)))
            .expect("the finder");
        view.rows.into_iter().map(|row| row.text).collect()
    }

    #[test]
    fn a_new_lines_indent_is_put_right_when_the_grammar_answers() {
        use clackatoo_protocol::{Answer, BufferUpdate, Change, LineIndent, Query};
        let mut desk = editing("/p/a.rs", "{\n}\n");
        type_keys(&mut desk, "ox");
        // Its edit first, which the question waits behind.
        let changes = buffer_changes(&mut desk);
        assert_eq!(changes.len(), 1, "{changes:?}");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Acked {
                version: 1,
                seq: changes[0].0,
            },
        );
        let outbox = desk.take_outbox();
        // The x's edit goes on, after the first.
        let x = (outbox.iter())
            .find_map(|message| match message {
                ClientMessage::Edit { seq, .. } => Some(*seq),
                _ => None,
            })
            .expect("the x's edit");
        let asked: Vec<(u64, usize, Query)> = (outbox.into_iter())
            .filter_map(|message| match message {
                ClientMessage::Query { id, at, query, .. } => Some((id, at, query)),
                _ => None,
            })
            .collect();
        let [(id, 2, Query::Indent { end: 2, .. })] = asked[..] else {
            panic!("asked {asked:?}");
        };
        // The x typed meanwhile wasn't held.
        assert_eq!(editor_text(&desk), "{\nx\n}\n");
        let answer = BufferUpdate::Answer {
            id,
            version: 1,
            answer: Answer::Indents(vec![LineIndent {
                line: 2,
                width: Some(4),
            }]),
        };
        assert!(desk.buffer_changed(BufferId(7), answer));
        assert_eq!(editor_text(&desk), "{\n    x\n}\n");
        // Sent once the x is, as part of the same change.
        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version: 2, seq: x });
        let changes = buffer_changes(&mut desk);
        let [(_, Change::Edit { step: false, .. })] = changes[..] else {
            panic!("sent {changes:?}");
        };
    }

    #[test]
    fn an_object_from_the_grammar_is_copied_to_the_clipboard_once_it_answers() {
        use clackatoo_protocol::{Answer, BufferUpdate, Construct, Piece, Query};
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        // Into `"+`, so the x's delete doesn't go to the clipboard after.
        command(&mut desk, "set clipboard=none");
        type_keys(&mut desk, "j\"+yafx");
        let asked: Vec<(u64, usize, Query)> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Query { id, at, query, .. } => Some((id, at, query)),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1);
        let (id, at, query) = asked[0];
        assert_eq!((at, query), (10, Query::Pieces(Construct::Function)));
        assert!(desk.yanked.is_empty());
        let piece = Piece {
            start: 10,
            end: 19,
            inner: None,
        };
        let answer = BufferUpdate::Answer {
            id,
            version: 0,
            answer: Answer::Pieces(vec![piece]),
        };
        assert!(desk.buffer_changed(BufferId(7), answer));
        assert_eq!(desk.yanked, [("fn b() {}".to_owned(), Copy::Clipboard)]);
        // The x held meanwhile is read after it.
        let changes = buffer_changes(&mut desk);
        assert_eq!(changes.len(), 1, "{changes:?}");
    }

    /// The grammar question the desk's editor asked last: its number and
    /// place.
    fn grammar_question(desk: &mut Desk) -> (u64, usize) {
        let asked = (desk.take_outbox().into_iter())
            .rev()
            .find_map(|message| match message {
                ClientMessage::Query { id, at, .. } => Some((id, at)),
                _ => None,
            });
        asked.expect("a question")
    }

    /// Answers the question numbered `id` of the editor of `desk`'s
    /// buffer with `pieces`.
    fn answer_pieces(desk: &mut Desk, id: u64, pieces: Vec<clackatoo_protocol::Piece>) {
        use clackatoo_protocol::{Answer, BufferUpdate};
        let answer = BufferUpdate::Answer {
            id,
            version: 0,
            answer: Answer::Pieces(pieces),
        };
        desk.buffer_changed(BufferId(7), answer);
    }

    /// The text of `desk`'s focused editor.
    fn editor_text(desk: &Desk) -> String {
        desk.focused_editor()
            .expect("an editor")
            .1
            .text()
            .to_string()
    }

    const FUNCTIONS: &str = "fn a() {}\nfn b() {}\n";

    fn function_b() -> clackatoo_protocol::Piece {
        clackatoo_protocol::Piece {
            start: 10,
            end: 19,
            inner: None,
        }
    }

    #[test]
    fn the_windows_keys_and_pastes_wait_for_the_grammar_too() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        // `:` would open the command line at once, and the paste go in.
        type_keys(&mut desk, "jdaf:");
        desk.paste("zz");
        assert!(desk.input.is_none(), "the : is held");
        assert_eq!(editor_text(&desk), FUNCTIONS, "and the paste");
        let (id, _) = grammar_question(&mut desk);
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert!(desk.input.is_some(), "the command line opened after");
        // The paste went into the command line, as it would have.
        assert_eq!(desk.input.as_ref().unwrap().line.text(), "zz");
        assert_eq!(editor_text(&desk), "fn a() {}\n\n");
    }

    /// What the focused editor's bar says.
    fn editor_label(desk: &Desk) -> String {
        let (editor, copy) = desk.focused_editor().expect("an editor");
        editor.label(copy)
    }

    #[test]
    fn a_command_waiting_too_long_is_given_up_on_by_the_clock() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jcifabc");
        let (id, _) = grammar_question(&mut desk);
        let deadline = desk.editor_wakes().expect("woken to give up");
        assert!(desk.tick_editors(deadline));
        assert_eq!(
            editor_label(&desk),
            "the grammar took too long: nothing was done"
        );
        // Neither the command nor the keys held, nor the answer, when it
        // comes; and the next key isn't eaten.
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS);
        assert!(buffer_changes(&mut desk).is_empty());
        type_keys(&mut desk, "daf");
        assert_eq!(grammar_question(&mut desk).1, 10, "the d was had");
    }

    #[test]
    fn a_key_just_as_the_wait_runs_out_is_had_and_the_bar_says_why() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        desk.age_grammar_questions(std::time::Duration::from_secs(2));
        type_keys(&mut desk, "x");
        assert_eq!(editor_text(&desk), "fn a() {}\nn b() {}\n", "the x");
        assert_eq!(
            editor_label(&desk),
            "the grammar took too long: nothing was done"
        );
        type_keys(&mut desk, "l");
        assert!(editor_label(&desk).starts_with("a.rs"));
    }

    #[test]
    fn esc_cancels_a_command_waiting_for_the_grammar() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        let (id, _) = grammar_question(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert_eq!(editor_label(&desk), "cancelled: nothing was done");
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS, "nor the x");
        type_keys(&mut desk, "x");
        assert_eq!(editor_text(&desk), "fn a() {}\nn b() {}\n");
    }

    #[test]
    fn keys_a_held_key_sets_off_come_before_the_rest_held() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        desk.lua_from(r#"client.keys.map("n", "-", "ggdafox<Esc>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "jdaf-");
        type_keys(&mut desk, "Oz");
        let (first, _) = grammar_question(&mut desk);
        answer_pieces(&mut desk, first, vec![function_b()]);
        // The binding's daf asks in turn, once the daemon has the first's
        // edit; its o, x and Esc wait ahead of O and z.
        let mut second = None;
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Edit { seq, .. } => {
                    let acked = clackatoo_protocol::BufferUpdate::Acked { version: 1, seq };
                    desk.buffer_changed(BufferId(7), acked);
                }
                ClientMessage::Query { id, .. } => second = Some(id),
                _ => {}
            }
        }
        let second = second.unwrap_or_else(|| grammar_question(&mut desk).0);
        let a = clackatoo_protocol::Piece {
            start: 0,
            end: 9,
            inner: None,
        };
        let answer = clackatoo_protocol::BufferUpdate::Answer {
            id: second,
            version: 1,
            answer: clackatoo_protocol::Answer::Pieces(vec![a]),
        };
        desk.buffer_changed(BufferId(7), answer);
        assert_eq!(editor_text(&desk), "\nz\nx\n\n");
    }

    #[test]
    fn a_focus_moved_or_an_editor_gone_while_waiting_gives_up() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        let (id, _) = grammar_question(&mut desk);
        // As a click on another pane moves it.
        let shown = desk.shown.unwrap();
        desk.shown = Some(Shown {
            pane: PaneId(2),
            ..shown
        });
        assert!(desk.tick_editors(std::time::Instant::now()));
        let (label, held) = desk.editor_label_and_held(PaneId(1));
        assert_eq!(label.as_deref(), Some("the focus moved: nothing was done"));
        assert_eq!(held, 0, "the x is dropped");
        desk.shown = Some(shown);
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS, "nor the x");

        // The editor gone, as its pane shows another file.
        type_keys(&mut desk, "dafx");
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(BufferId(8)),
            home: None,
            key: None,
        });
        desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        assert!(
            (desk.message.as_deref()).is_some_and(|message| message.contains("nothing was done")),
            "{:?}",
            desk.message
        );
        assert_eq!(desk.editor_label_and_held(PaneId(1)).1, 0);
    }

    #[test]
    fn the_finders_preview_of_a_file_not_open_is_coloured_by_the_daemon() {
        use clackatoo_protocol::{Found, Search, Span, wire};
        let preview_of = |desk: &mut Desk, version| {
            desk.daemon_is(version, None);
            type_keys(desk, " pf");
            let (id, _) = search_in(&finds(desk));
            found_files(desk, id, &["src/lib.rs"]);
            finder_rows(desk);
            let preview = (finds(desk).into_iter())
                .find_map(|message| match message {
                    ClientMessage::Find {
                        id,
                        search: Search::Preview { .. },
                        ..
                    } => Some(id),
                    _ => None,
                })
                .expect("a preview asked for");
            desk.found(
                preview,
                Found::Preview {
                    first: 0,
                    lines: vec!["pub fn x() {}".into()],
                    spans: Vec::new(),
                },
            );
        };
        let colour_asks = |desk: &mut Desk| -> Vec<(u64, Vec<clackatoo_protocol::Code>)> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Colour { id, code } => Some((id, code)),
                    _ => None,
                })
                .collect()
        };
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        preview_of(&mut desk, clackatoo_protocol::PROTOCOL_VERSION);
        assert!(desk.ask_colours());
        let asks = colour_asks(&mut desk);
        let [(id, code)] = &asks[..] else {
            panic!("one ask: {asks:?}")
        };
        assert_eq!(code.len(), 1);
        assert_eq!(code[0].language, "rs");
        assert_eq!(code[0].text, "pub fn x() {}\n");
        assert!(!desk.ask_colours(), "asked once");
        // The answer colours the preview, and it's drawn again.
        let keyword = Span {
            start: 0,
            end: 3,
            kind: clackatoo_protocol::Kind::Keyword,
        };
        assert!(desk.coloured(*id, vec![vec![keyword]]));
        let view = desk.finder_view((10, 60), Some((9, 40))).unwrap();
        let line = &view.preview.expect("a preview").lines[0];
        let ink = crate::colors::syntax_ink(clackatoo_protocol::Kind::Keyword);
        assert!(line.runs.iter().any(|&(_, run)| run == ink), "{line:?}");
        // A daemon too old to colour code isn't asked.
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        preview_of(&mut desk, wire::COLOURS - 1);
        assert!(!desk.ask_colours());
        assert!(colour_asks(&mut desk).is_empty());
    }

    #[test]
    fn space_p_f_finds_the_projects_files_and_opens_the_one_picked() {
        use clackatoo_protocol::{OpenFile, Search};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        type_keys(&mut desk, " pf");
        let asked = finds(&mut desk);
        assert!(matches!(
            asked[..],
            [ClientMessage::Find {
                pane: Some(PaneId(1)),
                search: Search::Files,
                ..
            }]
        ));
        let (id, _) = search_in(&asked);
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Bar));
        found_files(&mut desk, id, &["src/main.rs", "src/other.rs", "README"]);
        assert_eq!(
            finder_rows(&mut desk),
            ["README", "src/main.rs", "src/other.rs"]
        );
        // The one picked is previewed as it's drawn, and the next once
        // it's picked.
        let preview = |messages: &[ClientMessage]| {
            (messages.iter())
                .find_map(|message| match message {
                    ClientMessage::Find {
                        id,
                        search: Search::Preview { path, line, .. },
                        ..
                    } => Some((*id, path.clone(), *line)),
                    _ => None,
                })
                .expect("a preview asked for")
        };
        let (first, path, line) = preview(&finds(&mut desk));
        assert_eq!((path.as_str(), line), ("/p/README", 0));
        finder_rows(&mut desk);
        assert!(finds(&mut desk).is_empty(), "asked for once");
        type_keys(&mut desk, "oth");
        assert_eq!(finder_rows(&mut desk), ["src/other.rs"]);
        let asked = finds(&mut desk);
        assert!(asked.contains(&ClientMessage::StopFinding { id: first }));
        assert_eq!(preview(&asked).1, "/p/src/other.rs");

        // Enter opens it in place of the file shown, as :e does.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(desk.finder_view((10, 60), None).is_none(), "closed");
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: OpenFile {
                    path: "/p/src/other.rs".into(),
                    cwd: None
                },
            }]
        );

        // Ctrl+V beside it, and Esc closes, stopping the search.
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        found_files(&mut desk, id, &["README"]);
        ctrl(&mut desk, "v");
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::File(OpenFile { path, .. }),
                ..
            }] if path == "/p/README"
        ));
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert_eq!(finds(&mut desk), [ClientMessage::StopFinding { id }]);
        assert!(!desk.found(id, clackatoo_protocol::Found::Files(vec!["a".into()])));
    }

    #[test]
    fn a_grep_searches_again_as_typed_and_goes_to_the_line_picked() {
        use clackatoo_protocol::{Found, FoundLine, Search};
        let mut desk = editing("/p/src/main.rs", "use a;\nfn main() {}\n");
        command(&mut desk, "Grep fn");
        let (first, search) = search_in(&finds(&mut desk));
        assert_eq!(
            search,
            Search::Grep {
                pattern: "fn".into(),
                exact: false
            }
        );
        // A capital matches case, as `smartcase` says. Typing searches once
        // it pauses.
        type_keys(&mut desk, "M");
        assert!(finds(&mut desk).is_empty(), "not yet");
        assert!(desk.grep_at().is_some(), "the window wakes for it");
        desk.grep_if_due(std::time::Instant::now() + finding::GREP_WAIT);
        let asked = finds(&mut desk);
        assert!(asked.contains(&ClientMessage::StopFinding { id: first }));
        let (id, search) = search_in(&asked);
        assert_eq!(
            search,
            Search::Grep {
                pattern: "fnM".into(),
                exact: true
            }
        );
        let line = FoundLine {
            path: "src/main.rs".into(),
            line: 1,
            col: 3,
            text: "fn main() {}".into(),
            matches: vec![(0, 2)],
        };
        assert!(
            !desk.found(first, Found::Lines(vec![line.clone()])),
            "too late"
        );
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        desk.found(id, Found::Lines(vec![line]));
        assert_eq!(finder_rows(&mut desk), ["src/main.rs:2: fn main() {}"]);
        // In the file shown, the cursor goes there with nothing opened.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(answer_all(&mut desk).is_empty());
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(note.as_deref(), Some("main.rs  2:4"));
    }

    #[test]
    fn space_ds_lists_whats_wrong_in_the_project() {
        use clackatoo_protocol::{Found, FoundLine, Search};
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        type_keys(&mut desk, " ds");
        let asked = finds(&mut desk);
        let (id, search) = search_in(&asked);
        assert_eq!(search, Search::Diagnostics);
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let line = |path: &str, line, text: &str| FoundLine {
            path: path.into(),
            line,
            col: 1,
            text: text.into(),
            matches: Vec::new(),
        };
        desk.found(
            id,
            Found::Lines(vec![
                line("a.rs", 2, "error: wrong"),
                line("b.rs", 0, "warning: unused"),
            ]),
        );
        assert_eq!(
            finder_rows(&mut desk),
            ["a.rs:3: error: wrong", "b.rs:1: warning: unused"]
        );
        assert_eq!(
            desk.finder_view((10, 60), None).unwrap().title,
            "Diagnostics"
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert!(note.unwrap().starts_with("a.rs  3:2"));
    }

    #[test]
    fn format_formats_and_formatonsave_formats_before_writing() {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut desk = editing("/p/a.rs", "one\n");
        // The query formatting asks, and none else is sent.
        let format_asked = |desk: &mut Desk| {
            let messages = desk.take_outbox();
            let saves = (messages.iter())
                .filter(|message| matches!(message, ClientMessage::Request(Request::Save { .. })))
                .count();
            let id = (messages.into_iter()).find_map(|message| match message {
                ClientMessage::Query {
                    id,
                    query: Query::Format { tab_size, spaces },
                    ..
                } => {
                    assert_eq!((tab_size, spaces), (4, true), "as >> indents");
                    Some(id)
                }
                _ => None,
            });
            (id.expect("formatting asked"), saves)
        };
        let answer = |desk: &mut Desk, id, answer| {
            desk.buffer_changed(
                BufferId(7),
                BufferUpdate::Answer {
                    id,
                    version: 0,
                    answer,
                },
            )
        };
        let note = |desk: &Desk| match desk.status() {
            Status::Tabs { note, .. } => note.unwrap_or_default(),
            Status::Typing { .. } => panic!("the tabs"),
        };
        command(&mut desk, "Format");
        let (id, saves) = format_asked(&mut desk);
        assert_eq!(saves, 0);
        assert!(answer(&mut desk, id, Answer::Formatted { changed: true }));
        assert_eq!(note(&desk), "formatted");

        // With formatonsave, :w formats, then writes once it's answered,
        // whatever the answer.
        command(&mut desk, "set fos");
        command(&mut desk, "w");
        let (id, saves) = format_asked(&mut desk);
        assert_eq!(saves, 0, "not yet");
        answer(
            &mut desk,
            id,
            Answer::Failed("its language server doesn't format files".into()),
        );
        assert!(matches!(
            answer_all(&mut desk)[..],
            [Request::Save { force: false, .. }]
        ));
        // A file being formatted isn't asked again: a :w then writes it
        // once the answer comes.
        command(&mut desk, "Format");
        let (id, _) = format_asked(&mut desk);
        command(&mut desk, "w");
        assert!(desk.take_outbox().is_empty(), "not asked twice");
        answer(&mut desk, id, Answer::Formatted { changed: true });
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));

        // Or after a moment, if the server is slow.
        command(&mut desk, "w");
        format_asked(&mut desk);
        assert!(desk.format_at().is_some(), "the window wakes for it");
        assert!(!desk.format_if_due(std::time::Instant::now()));
        assert!(desk.format_if_due(std::time::Instant::now() + std::time::Duration::from_secs(4)));
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));
        assert!(note(&desk).ends_with("written, not formatted: its language server took too long"));
        assert!(desk.format_at().is_none());
    }

    #[test]
    fn a_server_that_cant_format_says_why_once_and_a_write_still_writes() {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut desk = editing("/p/a.rs", "one\n");
        let format_asked = |desk: &mut Desk| {
            (desk.take_outbox().into_iter())
                .find_map(|message| match message {
                    ClientMessage::Query {
                        id,
                        query: Query::Format { .. },
                        ..
                    } => Some(id),
                    _ => None,
                })
                .expect("formatting asked")
        };
        let answer = |desk: &mut Desk, id, answer| {
            desk.buffer_changed(
                BufferId(7),
                BufferUpdate::Answer {
                    id,
                    version: 0,
                    answer,
                },
            )
        };
        let note = |desk: &Desk| match desk.status() {
            Status::Tabs { note, .. } => note.unwrap_or_default(),
            Status::Typing { .. } => panic!("the tabs"),
        };
        // As gcsl says of a file with a syntax error: "not formatted" once,
        // never "already formatted".
        let why = "not formatted: it has a syntax error (2:8)";
        command(&mut desk, "Format");
        let id = format_asked(&mut desk);
        answer(&mut desk, id, Answer::Failed(why.into()));
        assert_eq!(note(&desk), why);
        // Said plainly, as other servers say it, it's said after.
        command(&mut desk, "Format");
        let id = format_asked(&mut desk);
        answer(&mut desk, id, Answer::Failed("it can't be parsed".into()));
        assert_eq!(note(&desk), "not formatted: it can't be parsed");
        // Whatever case the server writes it in, it's said once.
        command(&mut desk, "Format");
        let id = format_asked(&mut desk);
        let shouted = "Not Formatted: it can't be parsed";
        answer(&mut desk, id, Answer::Failed(shouted.into()));
        assert_eq!(note(&desk), "not formatted: it can't be parsed");
        // And a server that gives no reason is said to have given none.
        command(&mut desk, "Format");
        let id = format_asked(&mut desk);
        answer(&mut desk, id, Answer::Failed(String::new()));
        assert_eq!(
            note(&desk),
            "not formatted: its language server didn't say why"
        );

        // Formatting on save still writes the file, saying why it's not
        // formatted.
        command(&mut desk, "set fos");
        command(&mut desk, "w");
        let id = format_asked(&mut desk);
        answer(&mut desk, id, Answer::Failed(why.into()));
        assert!(matches!(
            answer_all(&mut desk)[..],
            [Request::Save { force: false, .. }]
        ));
        assert_eq!(
            note(&desk),
            "a.rs written, not formatted: it has a syntax error (2:8)"
        );
    }

    #[test]
    fn a_selection_is_formatted_by_its_language_server_with_gq_or_format() {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut desk = editing("/p/a.rs", "one\n  two\nthree\n");
        // What's asked: part of the file, indented as `>>` does.
        let format_asked = |desk: &mut Desk| {
            let asked: Vec<(u64, usize, Query)> = (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Query { id, at, query, .. } => Some((id, at, query)),
                    _ => None,
                })
                .collect();
            asked
        };
        let range = |end| Query::FormatRange {
            end,
            tab_size: 4,
            spaces: true,
        };
        let note = |desk: &Desk| match desk.status() {
            Status::Tabs { note, .. } => note.unwrap_or_default(),
            Status::Typing { .. } => panic!("the tabs"),
        };
        // `:'<,'>Format`, typed from a selection of lines: from the first
        // one's start to the last one's end, one question.
        type_keys(&mut desk, "jVj");
        command(&mut desk, "Format");
        let asked = format_asked(&mut desk);
        assert_eq!(asked.len(), 1, "{asked:?}");
        let (id, at, query) = asked[0];
        assert_eq!((at, query), (4, range(15)));
        // Asked again before it's answered, it's being formatted already.
        command(&mut desk, "2Format");
        assert!(format_asked(&mut desk).is_empty(), "not asked twice");
        assert_eq!(note(&desk), "it's being formatted already");
        // The daemon makes its edits, as the user's, one undo step, then
        // answers, as for `:Format`.
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 1,
                answer: Answer::Formatted { changed: true },
            },
        );
        assert_eq!(note(&desk), "formatted");

        // `gq` on a selection of characters, exactly: "th" of the last line,
        // where the cursor was left.
        type_keys(&mut desk, "vlgq");
        let asked = format_asked(&mut desk);
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!((asked[0].1, asked[0].2), (10, range(12)));
        let why = "its language server can't format a selection";
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id: asked[0].0,
                version: 1,
                answer: Answer::Failed(why.into()),
            },
        );
        assert_eq!(note(&desk), format!("not formatted: {why}"));

        // A daemon too old to format part of a file says so, and how to
        // update it, asking nothing.
        desk.daemon_is(clackatoo_protocol::wire::USES - 1, None);
        command(&mut desk, "%Format");
        assert!(format_asked(&mut desk).is_empty());
        assert!(
            (desk.message.as_deref()).is_some_and(
                |message| message.starts_with("formatting a selection needs a newer daemon")
            ),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn the_focused_editor_lights_the_uses_of_the_name_its_cursor_rests_on() {
        use crate::render::HighlightKind;
        use clackatoo_protocol::{Answer, BufferUpdate, Query, ServerFeatures, Use, UseKind};
        let mut desk = editing("/p/a.rs", "let x = 1;\nx + x\n");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Features(ServerFeatures {
                uses: true,
                ..ServerFeatures::default()
            }),
        );
        let uses_asked = |desk: &mut Desk| -> Vec<(u64, usize)> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Query {
                        id,
                        at,
                        query: Query::Uses,
                        ..
                    } => Some((id, at)),
                    _ => None,
                })
                .collect()
        };
        let lit = |desk: &mut Desk| -> Vec<(usize, HighlightKind)> {
            desk.settle_editors();
            let editor = desk.editor(PaneId(1)).expect("the editor");
            (editor.highlights().into_iter())
                .filter(|highlight| {
                    matches!(highlight.kind, HighlightKind::Used | HighlightKind::Written)
                })
                .map(|highlight| (highlight.line, highlight.kind))
                .collect()
        };
        let rest = |desk: &mut Desk| {
            let now = std::time::Instant::now();
            desk.tick_editors(now);
            let due = desk.editor_wakes().expect("the window wakes to ask");
            assert!(due >= now + crate::editor::USES_WAIT);
            desk.tick_editors(due);
        };
        type_keys(&mut desk, "w");
        rest(&mut desk);
        let asked = uses_asked(&mut desk);
        assert_eq!(asked.iter().map(|&(_, at)| at).collect::<Vec<_>>(), [4]);
        let used = |start, kind| Use {
            start,
            end: start + 1,
            kind,
        };
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id: asked[0].0,
                version: 0,
                answer: Answer::Uses(vec![
                    used(4, UseKind::Write),
                    used(11, UseKind::Read),
                    used(15, UseKind::Read),
                ]),
            },
        );
        assert_eq!(
            lit(&mut desk),
            [
                (0, HighlightKind::Written),
                (1, HighlightKind::Used),
                (1, HighlightKind::Used)
            ]
        );
        // Off the name, they go.
        type_keys(&mut desk, "e");
        assert_eq!(lit(&mut desk), []);
        // A daemon too old to say is never asked.
        type_keys(&mut desk, "b");
        desk.daemon_is(clackatoo_protocol::wire::USES - 1, None);
        let now = std::time::Instant::now();
        desk.tick_editors(now);
        desk.tick_editors(now + crate::editor::USES_WAIT);
        assert!(uses_asked(&mut desk).is_empty());
    }

    #[test]
    fn shown_editors_ask_for_inlay_hints_and_the_same_hints_again_draw_nothing() {
        use clackatoo_protocol::{
            Answer, BufferUpdate, InlayHint, InlayHintKind, Query, ServerFeatures,
        };
        let mut desk = editing("/p/a.rs", "let x = 1;\n");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Features(ServerFeatures {
                inlay_hints: true,
                ..ServerFeatures::default()
            }),
        );
        let hints_asked = |desk: &mut Desk| -> Vec<(u64, usize, usize)> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Query {
                        id,
                        at,
                        query: Query::InlayHints { end },
                        ..
                    } => Some((id, at, end)),
                    _ => None,
                })
                .collect()
        };
        let rest = |desk: &mut Desk| {
            desk.settle_editors();
            let now = std::time::Instant::now();
            desk.tick_editors(now);
            let due = desk.editor_wakes().expect("the window wakes to ask");
            assert!(due >= now + crate::editor::HINTS_WAIT);
            desk.tick_editors(due);
        };
        rest(&mut desk);
        let asked = hints_asked(&mut desk);
        assert_eq!(
            asked
                .iter()
                .map(|&(_, at, end)| (at, end))
                .collect::<Vec<_>>(),
            [(0, 10)]
        );
        let answer = || BufferUpdate::Answer {
            id: asked[0].0,
            version: 0,
            answer: Answer::InlayHints {
                start: 0,
                end: 10,
                hints: vec![InlayHint {
                    at: 5,
                    label: ": i32".into(),
                    kind: InlayHintKind::Type,
                    pad_left: false,
                    pad_right: false,
                }],
            },
        };
        assert!(desk.buffer_changed(BufferId(7), answer()), "drawn");
        assert!(
            !desk.buffer_changed(BufferId(7), answer()),
            "the same hints draw nothing"
        );
        // `<Space>th` turns them off, and says so; on again, they're asked
        // for afresh.
        type_keys(&mut desk, " th");
        assert!(!desk.options.inlayhints);
        assert_eq!(desk.message.as_deref(), Some("inlay hints off"));
        let idle = |desk: &mut Desk| {
            desk.settle_editors();
            let now = std::time::Instant::now();
            desk.tick_editors(now);
            desk.tick_editors(now + crate::editor::HINTS_WAIT);
        };
        idle(&mut desk);
        assert!(hints_asked(&mut desk).is_empty());
        type_keys(&mut desk, " th");
        assert_eq!(desk.message.as_deref(), Some("inlay hints on"));
        rest(&mut desk);
        assert_eq!(hints_asked(&mut desk).len(), 1);
        // A daemon too old to say is never asked.
        type_keys(&mut desk, "x");
        desk.daemon_is(clackatoo_protocol::wire::INLAY_HINTS - 1, None);
        idle(&mut desk);
        assert!(hints_asked(&mut desk).is_empty());
    }

    #[test]
    fn several_places_an_answer_gives_are_picked_from_in_the_finder() {
        use clackatoo_protocol::{Answer, BufferUpdate, FileLocation, OpenFile};
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        type_keys(&mut desk, "gd");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        let place = |path: &str, line, text: &str| FileLocation {
            path: path.into(),
            line,
            col: 1,
            offset: None,
            text: text.into(),
        };
        let found = vec![place("/p/a.rs", 2, "three"), place("/p/lib/b.rs", 0, "b")];
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Locations(found),
            },
        );
        assert_eq!(finder_rows(&mut desk), ["a.rs:3: three", "lib/b.rs:1: b"]);
        assert_eq!(desk.finder_view((10, 60), None).unwrap().title, "Places");
        // One here goes there.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(note.as_deref(), Some("a.rs  3:2"));

        // One elsewhere opens there.
        type_keys(&mut desk, "gd");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        let found = vec![place("/p/a.rs", 2, "three"), place("/p/lib/b.rs", 0, "b")];
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Locations(found),
            },
        );
        ctrl(&mut desk, "n");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: OpenFile {
                    path: "/p/lib/b.rs".into(),
                    cwd: None
                },
            }]
        );
    }

    /// A symbol for the finder, at the start of `line`, `col` bytes in.
    fn symbol(
        name: &str,
        kind: u8,
        container: &str,
        (line, col): (u64, usize),
    ) -> clackatoo_protocol::Symbol {
        clackatoo_protocol::Symbol {
            name: name.into(),
            kind,
            depth: u8::from(!container.is_empty()),
            container: container.into(),
            line,
            col,
            offset: None,
        }
    }

    #[test]
    fn space_b_s_lists_the_files_symbols_and_goes_to_the_one_picked() {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let text = "use a;\ntype Engine {\n    fn start() {}\n}\nfn main() {}\n";
        let mut desk = editing("/p/a.rs", text);
        type_keys(&mut desk, " bs");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query {
                    id,
                    query: Query::Symbols,
                    ..
                } => Some(id),
                _ => None,
            })
            .expect("the symbols asked for");
        // Open at once, waiting for them.
        let view = desk.finder_view((10, 60), None).expect("the finder");
        assert_eq!((view.title, view.count.as_str()), ("Symbols", "0 / 0…"));
        let symbols = vec![
            symbol("Engine", 23, "", (1, 5)),
            symbol("start", 6, "Engine", (2, 7)),
            symbol("main", 12, "", (4, 3)),
        ];
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Symbols {
                    symbols,
                    more: false,
                },
            },
        );
        // In the file's outline, indented as deep as each is.
        assert_eq!(
            finder_rows(&mut desk),
            [
                "Engine  struct",
                "  start  method · Engine",
                "main  function"
            ]
        );
        // Matched as other lists are, by what each says.
        type_keys(&mut desk, "eng");
        assert_eq!(
            finder_rows(&mut desk),
            ["Engine  struct", "  start  method · Engine"]
        );
        ctrl(&mut desk, "n");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(desk.finder_view((10, 60), None).is_none(), "closed");
        let note = |desk: &mut Desk| {
            let Status::Tabs { note, .. } = desk.status() else {
                panic!("the tabs");
            };
            note.unwrap_or_default()
        };
        assert!(
            note(&mut desk).starts_with("a.rs  3:8"),
            "{}",
            note(&mut desk)
        );
        // A jump, so Ctrl+O comes back.
        ctrl(&mut desk, "o");
        assert!(
            note(&mut desk).starts_with("a.rs  1:1"),
            "{}",
            note(&mut desk)
        );
    }

    #[test]
    fn space_b_s_says_why_a_file_has_no_symbols_listed() {
        use clackatoo_protocol::{Answer, BufferUpdate};
        let mut desk = editing("/p/a.rs", "use a;\n");
        // What went wrong is said where the count goes.
        type_keys(&mut desk, " bs");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("the symbols asked for");
        let why = "its language server doesn't list symbols";
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Failed(why.into()),
            },
        );
        assert_eq!(desk.finder_view((10, 60), None).unwrap().count, why);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );

        // A daemon too old to list them says so, and how to update it.
        desk.daemon_is(clackatoo_protocol::wire::SYMBOLS - 1, None);
        type_keys(&mut desk, " bs");
        assert!(desk.finder_view((10, 60), None).is_none());
        assert!(
            (desk.message.as_deref()).is_some_and(
                |message| message.starts_with("listing a file's symbols needs a newer daemon")
            ),
            "{:?}",
            desk.message
        );
        assert!(desk.take_outbox().is_empty(), "nothing asked");

        // And a pane with no file has none.
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        type_keys(&mut desk, " bs");
        assert_eq!(
            desk.message.as_deref(),
            Some("this pane has no file to list the symbols of")
        );
    }

    #[test]
    fn space_p_s_finds_the_projects_symbols_as_typed_and_opens_the_one_picked() {
        use clackatoo_protocol::{Done, Found, FoundSymbol, OpenFile, Search};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        type_keys(&mut desk, " ps");
        let (first, search) = search_in(&finds(&mut desk));
        // Asked at once, for what a server lists for nothing typed.
        assert_eq!(
            search,
            Search::Symbols {
                query: String::new()
            }
        );
        // Typing asks again, once it pauses, stopping the search before.
        type_keys(&mut desk, "eng");
        assert!(finds(&mut desk).is_empty(), "not yet");
        desk.grep_if_due(std::time::Instant::now() + finding::GREP_WAIT);
        let asked = finds(&mut desk);
        assert!(asked.contains(&ClientMessage::StopFinding { id: first }));
        let (id, search) = search_in(&asked);
        assert_eq!(
            search,
            Search::Symbols {
                query: "eng".into()
            }
        );
        let found_symbol = |path: &str, symbol| FoundSymbol {
            path: path.into(),
            symbol,
        };
        for found in [
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
            Found::Symbols(vec![
                found_symbol("src/engine.rs", symbol("Engine", 23, "", (3, 11))),
                found_symbol("src/engine.rs", symbol("start", 6, "Engine", (9, 7))),
            ]),
            Found::Done(Done::All),
        ] {
            desk.found(id, found);
        }
        // As the server ranks them, which matched them as it likes: not
        // narrowed down again.
        assert_eq!(
            finder_rows(&mut desk),
            [
                "Engine  struct  src/engine.rs:4",
                "start  method · Engine  src/engine.rs:10"
            ]
        );
        assert_eq!(
            desk.finder_view((10, 60), None).unwrap().title,
            "Project symbols"
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: OpenFile {
                    path: "/p/src/engine.rs".into(),
                    cwd: None
                },
            }]
        );
    }

    #[test]
    fn keys_a_binding_types_reach_the_windows_keys_and_a_key_ending_a_prefix_is_looked_up() {
        // `:w<CR>` typed by a binding opens the command line and writes.
        let mut desk = editing("/p/a.rs", "one\n");
        desk.lua_from(r#"client.keys.map("n", "<Space>w", ":w<CR>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, " w");
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));

        // In a terminal's insert mode, with `jk` bound: `j` then Ctrl+B
        // types `j`, and Ctrl+B starts its own keys, not ^B.
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(r#"client.keys.map("i", "jk", "<C-;>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "j");
        assert!(answer_all(&mut desk).is_empty(), "j waits for k");
        ctrl(&mut desk, "b");
        let sent: Vec<Vec<u8>> = (answer_all(&mut desk).into_iter())
            .filter_map(|request| match request {
                Request::SendInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [b"j".to_vec()], "not ^B");
        assert!(
            desk.which_keys()
                .is_some_and(|which| which.title == "Ctrl+B")
        );
        assert!(desk.cancel_keys());
        // And `jk` types Ctrl+;, found by where it is on the keyboard,
        // which leaves insert mode.
        type_keys(&mut desk, "jk");
        assert!(matches!(
            desk.status(),
            Status::Tabs {
                mode: Some(ModeName::Normal),
                ..
            }
        ));
    }

    #[test]
    fn the_config_maps_keys_to_keys_and_a_prefix_that_goes_nowhere_is_typed() {
        use clackatoo_protocol::Query;
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        desk.lua_from(r#"client.keys.map("n", "gi", "gri")"#);
        answer_all(&mut desk);
        let queries = |desk: &mut Desk| -> Vec<Query> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Query { query, .. } => Some(query),
                    _ => None,
                })
                .collect()
        };
        // `gi` types `gri`, as the config says.
        type_keys(&mut desk, "gi");
        assert_eq!(queries(&mut desk), [Query::Implementation]);
        // `g` waits for `i`; with another key, both are typed, as if no
        // binding had them: `G` then `gg` still go where they went.
        type_keys(&mut desk, "Ggg");
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert!(note.unwrap().starts_with("a.rs  1:1"));
        type_keys(&mut desk, "gd");
        assert_eq!(queries(&mut desk), [Query::Definition]);
    }

    #[test]
    fn the_config_binds_keys_to_commands_such_as_the_finders() {
        use clackatoo_protocol::Search;
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.keys.unmap("nv", "<Space>pf")
            client.keys.map("n", "<Space>ff", function() client.cmd("Files") end)
            client.keys.map("n", "<Space>x", function() client.cmd("Nonsense") end)
            "#,
        );
        answer_all(&mut desk);
        normal_mode(&mut desk);
        type_keys(&mut desk, " pf");
        assert!(finds(&mut desk).is_empty(), "unbound");
        type_keys(&mut desk, " ff");
        let asked = finds(&mut desk);
        assert_eq!(search_in(&asked).1, Search::Files);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        type_keys(&mut desk, " x");
        assert!(
            (desk.message.as_deref()).is_some_and(|message| message.contains("Nonsense")),
            "{:?}",
            desk.message
        );
        // The other finders' keys, as jack's Telescope keys are.
        type_keys(&mut desk, " pgf");
        assert!(
            desk.finder_view((10, 60), None)
                .is_some_and(|view| view.title == "Grep")
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        type_keys(&mut desk, " ?");
        assert_eq!(search_in(&finds(&mut desk)).1, Search::Buffers);
    }

    #[test]
    fn ctrl_o_comes_back_from_a_file_or_line_the_finder_opened() {
        use clackatoo_protocol::{Found, FoundLine};
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/p/a.rs", "fn a() {}\nfn b() {}\n");
        type_text(&mut desk, "jw");
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        found_files(&mut desk, id, &["b.rs"]);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        open_asked(&mut desk, 8, "/p/b.rs", "x\ny\n");
        assert_eq!(note(&desk), "b.rs  1:1");
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/p/a.rs", "fn a() {}\nfn b() {}\n");
        assert_eq!(note(&desk), "a.rs  2:4", "back where the finder left");

        // A line in the file shown is a jump too.
        command(&mut desk, "Grep fn");
        let (id, _) = search_in(&finds(&mut desk));
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let line = FoundLine {
            path: "a.rs".into(),
            line: 0,
            col: 3,
            text: "fn a() {}".into(),
            matches: vec![(0, 2)],
        };
        desk.found(id, Found::Lines(vec![line]));
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(note(&desk), "a.rs  1:4");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "a.rs  2:4");
    }

    #[test]
    fn space_h_keys_act_on_the_hunk_at_the_cursor() {
        use clackatoo_protocol::{BufferUpdate, Change, Edit, Hunk};
        let mut desk = editing("/p/a.txt", "a\nB\n");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Hunks {
                version: 0,
                hunks: vec![Hunk {
                    start: 2,
                    end: 4,
                    base: "b\n".into(),
                }],
            },
        );
        type_keys(&mut desk, "j hs");
        let stages: Vec<usize> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Stage { at, .. } => Some(at),
                _ => None,
            })
            .collect();
        assert_eq!(stages, [2]);
        type_keys(&mut desk, " hr");
        let edit = Edit::replace(4, 2, "B\n", "b\n");
        assert_eq!(
            changes_only(buffer_changes(&mut desk)),
            [Change::Edit {
                edit,
                base: 0,
                step: true
            }]
        );
        // And from the command line, as gitsigns.nvim's command.
        command(&mut desk, "Gitsigns preview_hunk");
        assert_eq!(note_of(&desk), "nothing's changed here since git's index");
    }

    #[test]
    fn a_key_a_half_typed_command_waits_for_is_never_a_bindings() {
        use clackatoo_protocol::{Change, Edit};
        let mut desk = editing("/p/a.txt", "abc\ndef\nghi\n");
        // `d<Space>` deletes a character, as in Vim, though Space starts
        // the finder's keys, and `j` and `k` only move after it.
        type_keys(&mut desk, "d jk");
        assert!(desk.finder_view((10, 60), None).is_none());
        let edit = Edit::replace(12, 0, "a", "");
        assert_eq!(
            changes_only(buffer_changes(&mut desk)),
            [Change::Edit {
                edit,
                base: 0,
                step: true
            }]
        );
        // And so `f:` finds a colon rather than opening the command line.
        let mut desk = editing("/p/b.txt", "a:b\n");
        type_keys(&mut desk, "f:");
        assert!(matches!(desk.status(), Status::Tabs { .. }), "no palette");
        assert_eq!(note_of(&desk), "b.txt  1:2");
    }

    fn note_of(desk: &Desk) -> String {
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        note.unwrap_or_default()
    }

    #[test]
    fn the_hover_panel_closes_as_the_finder_opens() {
        use clackatoo_protocol::{Answer, BufferUpdate};
        let mut desk = editing("/p/a.rs", "fn a() {}\n");
        type_keys(&mut desk, "K");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Hover {
                    text: "fn a()".into(),
                    markdown: false,
                    range: None,
                },
            },
        );
        let panel = |desk: &Desk| {
            (desk.editor(PaneId(1)))
                .and_then(crate::editor::Editor::panel)
                .is_some()
        };
        desk.settle_editors();
        assert!(panel(&desk));
        type_keys(&mut desk, " pf");
        desk.settle_editors();
        assert!(desk.finder_view((10, 60), None).is_some());
        assert!(!panel(&desk));
    }

    // --- Links in terminals ---------------------------------------------------

    /// Gives `terminal` a screen `cols` wide showing `output`, as attaching
    /// does.
    fn screen_showing(desk: &mut Desk, terminal: u32, cols: u16, output: &str) {
        let mut screen = Terminal::new(Size::new(cols, 24).unwrap(), &Config::default());
        screen.advance(output.as_bytes());
        desk.view_changed(
            TerminalId(terminal),
            &ViewUpdate::Snapshot(View::capture(&screen)),
        );
    }

    /// The point in the middle of the cell at `line` and `col` of the
    /// first pane, whose cells start 5 pixels in and are 10 × 20.
    fn cell_point(line: u16, col: u16) -> [f32; 2] {
        [10.0 + f32::from(col) * 10.0, 15.0 + f32::from(line) * 20.0]
    }

    fn click(desk: &mut Desk, point: [f32; 2], modifiers: Modifiers) {
        let left = MouseButton::Left;
        desk.mouse(point, MouseAction::Press(left), 1, modifiers);
        desk.mouse(point, MouseAction::Release(left), 1, modifiers);
    }

    const LINKS: &str = "\x1b]7;file:///p\x07see https://x.example/a now\r\n--> src/a.rs:3:2\r\n";

    #[test]
    fn ctrl_click_hands_urls_to_the_desktop_and_a_plain_click_doesnt() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 8), Modifiers::default());
        assert!(desk.take_opened().is_empty());
        assert!(desk.link_at(cell_point(0, 8), CTRL));
        assert!(!desk.link_at(cell_point(0, 2), CTRL));
        assert!(!desk.link_at(cell_point(0, 8), Modifiers::default()));
        click(&mut desk, cell_point(0, 8), CTRL);
        assert_eq!(desk.take_opened(), ["https://x.example/a"]);
        // The desktop opens it: nothing goes to the daemon.
        assert_eq!(answer_all(&mut desk), []);
    }

    #[test]
    fn connect_and_ctrl_b_r_ask_the_window_for_the_picker_of_a_machine() {
        let mut desk = desk(one_tab(pane(1)));
        answer_all(&mut desk);
        assert!(!desk.take_connect_asked());
        normal_mode(&mut desk);
        command(&mut desk, "connect");
        assert!(desk.take_connect_asked());
        assert!(!desk.take_connect_asked(), "once");
        ctrl_b(&mut desk, "r");
        assert!(desk.take_connect_asked());
        // The window opens it: nothing goes to the daemon.
        assert_eq!(answer_all(&mut desk), []);
        // With a machine, the daemon connects to it, to bring its
        // workspaces in.
        command(&mut desk, "connect server");
        assert!(!desk.take_connect_asked());
        assert_eq!(
            answer_all(&mut desk),
            [Request::Connect {
                host: "server".into(),
                command: None,
                share: None,
            }]
        );
        assert!(note(&desk).starts_with("connecting to ‘server’"));
    }

    #[test]
    fn port_forward_is_left_to_the_window() {
        use crate::command::Forwarding;
        let mut desk = desk(one_tab(pane(1)));
        answer_all(&mut desk);
        normal_mode(&mut desk);
        command(&mut desk, "PortForward 8080");
        command(&mut desk, "PortForward! 8080");
        let asked = desk.take_forwards_asked();
        assert!(matches!(
            asked[..],
            [Forwarding::Add(_), Forwarding::Stop(Some(8080))]
        ));
        assert!(desk.take_forwards_asked().is_empty(), "once");
        // The window does it: nothing goes to the daemon.
        assert_eq!(answer_all(&mut desk), []);
        // From the config, it's the config's, which a window on this
        // machine passes over without a word.
        desk.lua_from(r#"client.forward(8080) client.forward("3000:box:80")"#);
        let asked = desk.take_forwards_asked();
        assert!(
            matches!(&asked[..], [Forwarding::Config(a), Forwarding::Config(b)]
                if a.local == 8080 && b.host == "box"),
            "{asked:?}"
        );
        desk.lua_from(r#"client.forward("eighty")"#);
        assert!(desk.take_forwards_asked().is_empty());
        assert!(
            (desk.message.as_deref()).is_some_and(|message| message.contains("client.forward")),
            "{:?}",
            desk.message
        );
    }

    #[test]
    fn update_is_left_to_the_window_and_a_notice_waits_for_a_quiet_bar() {
        let mut desk = desk(one_tab(pane(1)));
        answer_all(&mut desk);
        desk.set_notice(Some("clackatoo 2026.10.3 is out: :update".into()));
        assert_eq!(note(&desk), "clackatoo 2026.10.3 is out: :update");
        // A message has the bar first, until the next key.
        normal_mode(&mut desk);
        command(&mut desk, "update");
        assert_eq!(desk.take_update_asked(), Some(UpdateAsked::Here));
        assert_eq!(desk.take_update_asked(), None);
        assert_eq!(note(&desk), "updating clackatoo…");
        // Nothing goes to the daemon: the window runs it.
        assert_eq!(answer_all(&mut desk), []);
        command(&mut desk, "update desk");
        assert_eq!(
            desk.take_update_asked(),
            Some(UpdateAsked::On("desk".into()))
        );

        // On the daemon's machine, it runs in a tab of its own.
        desk.open_tab_running(vec!["sh".into(), "-c".into(), "clackatoo update".into()]);
        let asked = answer_all(&mut desk);
        assert!(
            matches!(&asked[..], [Request::NewTab { terminal: PaneTerminal::New(new), .. }]
                if new.command.as_deref() == Some(&["sh".to_owned(), "-c".into(), "clackatoo update".into()][..])),
            "{asked:?}"
        );
    }

    #[test]
    fn holding_ctrl_over_a_link_underlines_it_until_ctrl_or_the_mouse_goes() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        let underlined = |desk: &Desk| {
            let pane = desk.pane(TerminalId(1)).unwrap();
            (pane.highlights(24).into_iter())
                .filter(|highlight| highlight.kind == crate::render::HighlightKind::Link)
                .map(|highlight| (highlight.line, highlight.cols))
                .collect::<Vec<_>>()
        };
        let motion = MouseAction::Motion(None);
        desk.mouse(cell_point(1, 6), motion, 1, Modifiers::default());
        assert_eq!(underlined(&desk), []);
        assert!(desk.modifiers_changed(CTRL));
        assert_eq!(underlined(&desk), [(1, 4..16)]);
        // Moving along the link changes nothing; off it, it goes.
        assert!(!desk.mouse(cell_point(1, 8), motion, 1, CTRL).redraw);
        assert!(desk.mouse(cell_point(1, 30), motion, 1, CTRL).redraw);
        assert_eq!(underlined(&desk), []);
        desk.mouse(cell_point(1, 8), motion, 1, CTRL);
        assert!(desk.modifiers_changed(Modifiers::default()));
        assert_eq!(underlined(&desk), []);
        desk.modifiers_changed(CTRL);
        assert!(desk.mouse_left());
        assert_eq!(underlined(&desk), []);
    }

    /// Where the requests asked for files to open.
    fn files_opened(requests: Vec<Request>) -> Vec<String> {
        (requests.into_iter())
            .filter_map(|request| match request {
                Request::NewTab {
                    terminal: PaneTerminal::File(file),
                    ..
                } => Some(format!("tab {}", file.path)),
                Request::Split {
                    pane,
                    side,
                    terminal: PaneTerminal::File(file),
                    ..
                } => Some(format!("{side:?} of {} {}", pane.0, file.path)),
                Request::OpenHere { pane, file } => Some(format!("in {} {}", pane.0, file.path)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_file_from_a_narrow_terminal_opens_in_a_tab_of_its_own() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["tab /p/src/a.rs"]);
    }

    #[test]
    fn a_file_from_a_wide_terminal_opens_beside_it() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 200, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(
            files_opened(answer_all(&mut desk)),
            ["Right of 1 /p/src/a.rs"]
        );
    }

    #[test]
    fn a_file_opens_in_the_tabs_editor_at_its_line_and_ctrl_o_comes_back() {
        let mut desk = desk(one_tab(side_by_side(vec![pane(1), file_pane(2, 7)])));
        screen_showing(&mut desk, 1, 40, LINKS);
        load(&mut desk, 7, "/p/b.rs", "b\n");
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["in 2 /p/src/a.rs"]);
        assert_eq!(shown_pane(&desk), PaneId(2));
        desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), file_pane(2, 8)])));
        answer_all(&mut desk);
        load(&mut desk, 8, "/p/src/a.rs", "one\ntwo\nthree\n");
        assert_eq!(note(&desk), "a.rs  3:2");
        normal_mode(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
    }

    #[test]
    fn a_relative_path_with_no_working_directory_is_left_to_the_daemon() {
        let mut desk = desk(one_tab(side_by_side(vec![pane(1), file_pane(2, 7)])));
        screen_showing(&mut desk, 1, 40, "src/a.rs:3\r\n");
        load(&mut desk, 7, "/p/b.rs", "b\n");
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 2), CTRL);
        // Not in the editor, whose directory it isn't from, but in a tab
        // the daemon opens from the terminal's.
        let asked = answer_all(&mut desk);
        assert!(
            matches!(&asked[..], [Request::NewTab { terminal: PaneTerminal::File(file), .. }]
                if file.path == "src/a.rs" && file.cwd.is_none()),
            "{asked:?}"
        );
    }

    #[test]
    fn a_file_link_on_another_machine_says_so_and_opens_nothing() {
        let mut desk = desk(one_tab(pane(1)));
        let output = "\x1b]8;;file://laptop/home/j/a.rs#0\x07there\x1b]8;;\x07";
        screen_showing(&mut desk, 1, 40, output);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 2), CTRL);
        assert!(desk.take_opened().is_empty());
        assert!(files_opened(answer_all(&mut desk)).is_empty());
        assert_eq!(
            note(&desk),
            "file://laptop/home/j/a.rs#0 is on laptop, not this machine"
        );
    }

    #[test]
    fn a_file_link_to_line_0_opens_the_file_at_its_top() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(
            &mut desk,
            1,
            40,
            "\x1b]8;;file:///p/a.rs#0\x07a\x1b]8;;\x07",
        );
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 0), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["tab /p/a.rs"]);
    }

    #[test]
    fn gx_in_normal_mode_opens_the_link_under_the_cursor() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, "https://x.example/b\r\n");
        answer_all(&mut desk);
        normal_mode(&mut desk);
        type_keys(&mut desk, "kgx");
        assert_eq!(desk.take_opened(), ["https://x.example/b"]);
    }

    #[test]
    fn ctrl_click_goes_to_a_program_that_takes_the_mouse_and_ctrl_shift_click_opens() {
        let mut desk = desk(one_tab(pane(1)));
        let output = format!("\x1b[?1000h\x1b[?1006h{LINKS}");
        screen_showing(&mut desk, 1, 40, &output);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 8), CTRL);
        assert!(desk.take_opened().is_empty());
        let sent = answer_all(&mut desk);
        assert!(
            matches!(&sent[..], [Request::SendInput { bytes, .. }, ..] if bytes == b"\x1b[<16;9;1M"),
            "{sent:?}"
        );
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert!(desk.link_at(cell_point(0, 8), ctrl_shift));
        click(&mut desk, cell_point(0, 8), ctrl_shift);
        assert_eq!(desk.take_opened(), ["https://x.example/a"]);
    }

    // --- Registers, macros and marks, in editors ------------------------------

    /// A function of `start`, through `end`, its insides `inner`.
    fn function(
        start: usize,
        end: usize,
        inner: Option<(usize, usize)>,
    ) -> clackatoo_protocol::Piece {
        clackatoo_protocol::Piece { start, end, inner }
    }

    /// Answers the edits sent, as the daemon would, each taking `version`
    /// on by one, until none are left, and returns the last grammar
    /// question asked meanwhile: its number and place, if there was one.
    /// A question waits for the edits before it.
    fn serve_edits(desk: &mut Desk, version: &mut u64) -> Option<(u64, usize)> {
        let mut asked = None;
        loop {
            let mut sent_more = false;
            for message in desk.take_outbox() {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        *version += 1;
                        let acked_update = clackatoo_protocol::BufferUpdate::Acked {
                            version: *version,
                            seq,
                        };
                        desk.buffer_changed(BufferId(7), acked_update);
                        sent_more = true;
                    }
                    ClientMessage::Query { id, at, .. } => asked = Some((id, at)),
                    _ => {}
                }
            }
            if !sent_more {
                return asked;
            }
        }
    }

    /// Answers the edits sent to buffer 7, and its questions how lines are
    /// indented, as the daemon does for a file with no grammar, until
    /// none are left.
    fn serve_without_grammar(desk: &mut Desk) {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut version = 0;
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                return;
            }
            for message in messages {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query {
                        id,
                        query: Query::Indent { .. },
                        ..
                    } => {
                        let answer = Answer::Nothing;
                        let answer = BufferUpdate::Answer {
                            id,
                            version,
                            answer,
                        };
                        desk.buffer_changed(BufferId(7), answer);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Answers the grammar question `id` with `pieces`, counted in the
    /// daemon's text at `version`.
    fn answer_at(desk: &mut Desk, id: u64, version: u64, pieces: Vec<clackatoo_protocol::Piece>) {
        let answer = clackatoo_protocol::BufferUpdate::Answer {
            id,
            version,
            answer: clackatoo_protocol::Answer::Pieces(pieces),
        };
        desk.buffer_changed(BufferId(7), answer);
    }

    #[test]
    fn a_macro_waits_for_the_grammar_at_each_key_that_asks_in_turn() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        type_keys(&mut desk, "qadaf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\nfn d() {}\n");
        // Each daf asks once the keys before it are done, and the keys
        // after it wait for its answer, as the x typed meanwhile does.
        type_keys(&mut desk, "2@ax");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 1, "on b's line");
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\nfn d() {}\n");
        answer_at(&mut desk, id, version, vec![function(1, 10, None)]);
        assert_eq!(editor_text(&desk), "\n\nfn c() {}\nfn d() {}\n");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 2, "the second go, on c's line, after the j");
        answer_at(&mut desk, id, version, vec![function(2, 11, None)]);
        assert_eq!(
            editor_text(&desk),
            "\n\n\nn d() {}\n",
            "then the j, and the x typed after"
        );
    }

    #[test]
    fn keys_typed_while_recording_waits_for_the_grammar_are_recorded_once() {
        let text = "fn a() {1}\nfn b() {2}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // The zz comes while cif waits, and is held, then read.
        type_keys(&mut desk, "qacifzz");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 10, Some((8, 9)))]);
        escape(&mut desk);
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "fn a() {zz}\nfn b() {2}\n");
        type_keys(&mut desk, "@a");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 21);
        let b = function(12, 22, Some((20, 21)));
        answer_at(&mut desk, id, version, vec![b]);
        assert_eq!(editor_text(&desk), "fn a() {zz}\nfn b() {zz}\n");
        // What was recorded, once.
        type_keys(&mut desk, "o");
        escape(&mut desk);
        type_keys(&mut desk, "\"ap");
        assert_eq!(
            editor_text(&desk),
            "fn a() {zz}\nfn b() {zz}\ncifzz<Esc>j\n"
        );
    }

    #[test]
    fn a_macro_waiting_for_the_grammar_stops_if_it_is_given_up_on() {
        let mut desk = editing("/p/a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
        let mut version = 0;
        type_keys(&mut desk, "qadaf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        type_keys(&mut desk, "jq2@a");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        // Esc gives up on it, and the rest of the macro goes too: the next
        // key types no more of it, and the answer, when it comes, nothing.
        escape(&mut desk);
        type_keys(&mut desk, "k");
        answer_at(&mut desk, id, version, vec![function(1, 10, None)]);
        assert_eq!(
            serve_edits(&mut desk, &mut version),
            None,
            "no daf asks again"
        );
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\n");
    }

    #[test]
    fn marks_move_with_edits_made_here_and_by_others() {
        use clackatoo_protocol::{Author, BufferUpdate};
        use clackatoo_text::Edit;

        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "5Glma");
        // Someone else puts a line in at the top...
        let edit = Edit::replace(TEN_LINES.len(), 0, "", "new\n");
        let update = BufferUpdate::Edited {
            version: 1,
            edit,
            author: Author::Agent(TerminalId(9)),
            answers: None,
        };
        desk.buffer_changed(BufferId(7), update);
        // ...and so does this window.
        type_text(&mut desk, "ggOtop");
        escape(&mut desk);
        type_text(&mut desk, "G`a");
        assert_eq!(
            note(&desk),
            "t.txt  7:2",
            "down two lines, on its character"
        );
        // `.` is where the last change was.
        type_text(&mut desk, "'.");
        assert_eq!(note(&desk), "t.txt  1:1");
        type_text(&mut desk, "`.");
        assert_eq!(note(&desk), "t.txt  1:3");
        // A mark on a line deleted goes to where the line was.
        type_text(&mut desk, "7Gdd'a");
        assert_eq!(note(&desk), "t.txt  7:1");
    }

    #[test]
    fn a_mark_in_another_file_opens_it_in_place() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwmA");
        command(&mut desk, "e /src/b.rs");
        open_asked(&mut desk, 8, "/src/b.rs", B_RS);
        // A file's own marks are its own, `'` among them.
        type_text(&mut desk, "`a");
        let said = note(&desk);
        assert!(said.starts_with("mark a isn't set"), "{said}");
        type_text(&mut desk, "''");
        let said = note(&desk);
        assert!(said.starts_with("mark ' isn't set"), "{said}");
        assert!(answer_all(&mut desk).is_empty(), "a.rs isn't opened");
        type_text(&mut desk, "jjma`A");
        open_asked(&mut desk, 9, "/src/a.rs", A_RS);
        assert_eq!(note(&desk), "a.rs  2:4");
        // It was a jump, which Ctrl+O comes back from.
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  3:1");
        type_text(&mut desk, "gg`a");
        assert_eq!(note(&desk), "b.rs  3:1", "b.rs's a is still there");
    }

    #[test]
    fn a_file_on_another_machine_keeps_marks_of_its_own_and_isnt_opened_here() {
        // The same path here, in pane 1, and on desk, in pane 2.
        let desk_id = clackatoo_protocol::MachineId([7; 16]);
        let mut there = workspace(2, "w", vec![tab(2, file_pane(2, 8))]);
        there.home = Some(desk_id);
        if let Layout::Pane(pane) = &mut there.tabs[0].layout {
            pane.home = Some(desk_id);
        }
        let mut desk = desk(workspaces(vec![
            workspace(1, "w", vec![tab(1, file_pane(1, 7))]),
            there,
        ]));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwma");
        desk.show_pane(PaneId(2));
        load(&mut desk, 8, "/src/a.rs", A_RS);
        // Its own `a` isn't this machine's file's.
        type_text(&mut desk, "`a");
        let said = note(&desk);
        assert!(said.starts_with("mark a isn't set"), "{said}");
        // Nor is a mark set there gone to from here.
        type_text(&mut desk, "jjmB");
        desk.show_pane(PaneId(1));
        answer_all(&mut desk);
        type_text(&mut desk, "`B");
        let said = note(&desk);
        assert!(said.contains("another machine"), "{said}");
        assert!(answer_all(&mut desk).is_empty(), "nothing is opened here");
    }

    #[test]
    fn gv_and_the_selections_marks_are_each_files_own_and_kept_while_away() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwvy");
        command(&mut desk, "e /src/b.rs");
        open_asked(&mut desk, 8, "/src/b.rs", B_RS);
        // b.rs has had no selection.
        type_text(&mut desk, "gv");
        let mode = |desk: &Desk| desk.focused_editor().expect("an editor").0.mode;
        assert_eq!(mode(&desk), Mode::Normal);
        command(&mut desk, "e /src/a.rs");
        open_asked(&mut desk, 9, "/src/a.rs", A_RS);
        type_text(&mut desk, "gvd");
        assert_eq!(editor_text(&desk), "fn a() {}\nfn () {}\nfn c() {}\n");
        assert_eq!(mode(&desk), Mode::Normal);
    }

    #[test]
    fn quote_quote_goes_back_and_forth_from_the_last_jump() {
        let mut desk = editing("/t.txt", "one two\nthree\nfour\nfive five\n");
        type_text(&mut desk, "wG");
        assert_eq!(note(&desk), "t.txt  4:1");
        type_text(&mut desk, "``");
        assert_eq!(note(&desk), "t.txt  1:5");
        type_text(&mut desk, "``");
        assert_eq!(note(&desk), "t.txt  4:1");
        type_text(&mut desk, "w''");
        assert_eq!(note(&desk), "t.txt  1:1", "to the line's start");
        // Ctrl+O knows of the jumps `''` made.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  4:6");
        // `m'` makes the place a jump to come back to.
        type_text(&mut desk, "kkm'G''");
        assert_eq!(note(&desk), "t.txt  2:1");
        // And it's a motion too.
        type_text(&mut desk, "Gd''");
        assert_eq!(editor_text(&desk), "one two\n");
    }

    /// What the register `name` holds.
    fn register(desk: &mut Desk, name: char) -> Option<String> {
        let registers = &desk.shared_mut().registers;
        registers
            .get(Some(name))
            .map(|register| register.text.clone())
    }

    #[test]
    fn macros_type_keys_through_the_bindings_as_they_are_bound_now() {
        let mut desk = editing("/t.txt", "a\nb\nc\nd\ne\n");
        desk.lua_from(
            r#"
            client.keys.map("n", "<Space>x", "A!<Esc>")
            client.keys.map("n", "<Space>m", "@qx")
            "#,
        );
        answer_all(&mut desk);
        // The keys pressed are recorded, not what the binding typed...
        type_keys(&mut desk, "qq x");
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "a!\nb\nc\nd\ne\n");
        assert_eq!(register(&mut desk, 'q').as_deref(), Some(" xj"));
        // ...so they're typed through the bindings, as pressing them would
        // be. A binding that types `@qx` types q's keys, and then the x,
        // as in Vim.
        type_keys(&mut desk, " m");
        assert_eq!(editor_text(&desk), "a!\nb!\n\nd\ne\n");
        // Bound to something else, the macro does what that does now.
        desk.lua_from(r#"client.keys.map("n", "<Space>x", "dd")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "@q");
        assert_eq!(editor_text(&desk), "a!\nb!\nd\ne\n");
        assert_eq!(note(&desk), "t.txt  4:1");
    }

    #[test]
    fn a_macro_sets_marks_and_jumps_as_it_goes() {
        let mut desk = editing("/t.txt", TEN_LINES);
        // Each line it puts in at the top takes the mark it set down.
        type_text(&mut desk, "qajmaggOx");
        escape(&mut desk);
        type_text(&mut desk, "`aq");
        assert_eq!(note(&desk), "t.txt  3:1");
        type_text(&mut desk, "@a");
        // It waits at the O for the grammar to say how the line's
        // indented: there's none for a .txt.
        assert_eq!(note(&desk), "t.txt  1:1");
        serve_without_grammar(&mut desk);
        assert_eq!(note(&desk), "t.txt  5:1");
        assert_eq!(&editor_text(&desk)[..10], "x\nx\nl1\nl2\n");
        // Its jumps are in the jump list, in order.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1", "where `a left");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  5:1", "where gg left");
    }

    #[test]
    fn recording_is_shown_in_the_bar_until_q() {
        let mut desk = editing("/t.txt", "x\n");
        type_text(&mut desk, "qz");
        assert_eq!(note(&desk), "recording @z  t.txt  1:1");
        type_text(&mut desk, "ihi");
        assert_eq!(note(&desk), "recording @z  t.txt  1:3");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(note(&desk), "t.txt  1:2");
        // `q` in insert mode is typed, and so is a macro's.
        type_text(&mut desk, "@z");
        assert_eq!(editor_text(&desk), "hhiix\n");
        // Keys the window takes, such as `:`, and Esc on the command line,
        // are recorded too, and recording goes on.
        type_text(&mut desk, "qy:");
        escape(&mut desk);
        type_text(&mut desk, "xq");
        assert_eq!(register(&mut desk, 'y').as_deref(), Some(":<Esc>x"));
        assert_eq!(editor_text(&desk), "hhix\n");
        type_text(&mut desk, "@y");
        assert_eq!(editor_text(&desk), "hhx\n");
    }

    #[test]
    fn yanks_go_to_the_clipboard_and_what_was_copied_elsewhere_is_put() {
        let mut desk = editing("/t.txt", "a\nb\n");
        type_text(&mut desk, "yy");
        assert_eq!(desk.take_yanked(), [("a\n".to_owned(), Copy::Clipboard)]);
        // Copied in another program, while the window hadn't the focus.
        desk.clipboard_asked();
        desk.clipboard_heard(Copy::Clipboard, "z\n".into());
        type_text(&mut desk, "p");
        assert_eq!(editor_text(&desk), "a\nz\nb\n");
        // Copied in a terminal: `"*` holds it at once.
        desk.copied("sel".into(), Copy::Primary);
        type_text(&mut desk, "\"*P");
        assert_eq!(editor_text(&desk), "a\nselz\nb\n");
        assert_eq!(desk.take_yanked(), [("sel".to_owned(), Copy::Primary)]);
        // A macro that copies again and again leaves the clipboard one
        // copy to make.
        type_text(&mut desk, "qmyyjq3@m");
        assert_eq!(desk.take_yanked(), [("b\n".to_owned(), Copy::Clipboard)]);
        // Not, once `clipboard` says neither.
        command(&mut desk, "set clipboard=none");
        type_text(&mut desk, "dd");
        assert!(desk.take_yanked().is_empty());
    }

    #[test]
    fn what_a_chat_copies_goes_to_the_clipboard_and_plus_holds_it() {
        use clackatoo_protocol::{ChatNode, ChatState, Item, Node, Outcome, Turn, TurnEnd};

        let mut desk = desk(two_side_by_side());
        let node = ChatNode {
            state: ChatState::Ready,
            turns: vec![Turn {
                from: None,
                prompt: "how?".into(),
                sent: 0,
                items: vec![Item::Words("Like this:\n\n```sh\ncargo test\n```".into())],
                end: Some(TurnEnd {
                    at: 1,
                    outcome: Outcome::Done,
                }),
                files: None,
                switch: None,
            }],
            ..ChatNode::default()
        };
        desk.view_changed(
            TerminalId(1),
            &ViewUpdate::Snapshot(View {
                title: "chat".into(),
                cwd: None,
                root: Node::Chat(Box::new(node)),
            }),
        );
        let mut fonts = crate::chat::fixtures::fonts();
        let paint = |desk: &mut Desk, fonts: &mut crate::fonts::Fonts| {
            let pane = desk.pane_mut(TerminalId(1)).expect("the chat's pane");
            pane.paint_chat(fonts, [0.0; 2], [900.0, 1000.0], true);
        };
        paint(&mut desk, &mut fonts);
        // In the box: what's typed, selected and copied.
        desk.paste("draft");
        press(&mut desk, Key::Character("a".into()), None, CTRL);
        press(&mut desk, Key::Character("c".into()), None, CTRL);
        desk.settle_chats(&mut fonts);
        assert_eq!(register(&mut desk, '+').as_deref(), Some("draft"));
        assert_eq!(desk.take_yanked(), [("draft".to_owned(), Copy::Clipboard)]);
        // In normal mode, `y` on a block of code: the cursor starts on the
        // last line in view, which is the code's.
        normal_mode(&mut desk);
        paint(&mut desk, &mut fonts);
        type_text(&mut desk, "y");
        desk.settle_chats(&mut fonts);
        assert_eq!(register(&mut desk, '+').as_deref(), Some("cargo test"));
        assert_eq!(
            desk.take_yanked(),
            [("cargo test".to_owned(), Copy::Clipboard)]
        );
    }

    #[test]
    fn colon_registers_and_marks_list_them() {
        let mut desk = editing("/p/t.txt", "one\ntwo\n");
        command(&mut desk, "registers");
        assert_eq!(note(&desk), "the registers are empty");
        type_text(&mut desk, "\"ayyjmb");
        command(&mut desk, "reg");
        desk.settle_editors();
        let editor = desk.editor(PaneId(1)).expect("an editor");
        let lines: Vec<String> = (editor.panel().expect("a panel").lines.iter())
            .map(crate::editor::PanelLine::text)
            .collect();
        assert_eq!(
            lines,
            ["type name content", "l  \"\"   one^J", "l  \"a   one^J"]
        );
        command(&mut desk, "marks");
        assert_eq!(finder_rows(&mut desk), ["t.txt:2: b  two"]);
    }

    // --- Macros, through the desk --------------------------------------------

    /// The daemon's side of each buffer, by buffer.
    type Daemons = HashMap<BufferId, crate::editor::Daemon>;

    /// The daemon's side of buffer 7, whose text is `text`.
    fn daemon_of_7(text: &str) -> Daemons {
        HashMap::from([(BufferId(7), crate::editor::Daemon::new(text))])
    }

    /// Makes the changes to buffers sent, with `daemons`, as the daemon
    /// does, and answers them, until none are left. Returns whether each
    /// edit sent started an undo step, with its buffer, and the other
    /// messages sent meanwhile, unanswered.
    fn serve(
        desk: &mut Desk,
        daemons: &mut Daemons,
    ) -> (Vec<(BufferId, bool)>, Vec<ClientMessage>) {
        let (mut steps, mut others) = (Vec::new(), Vec::new());
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                return (steps, others);
            }
            for message in messages {
                let ClientMessage::Edit { buffer, change, .. } = &message else {
                    others.push(message);
                    continue;
                };
                let buffer = *buffer;
                if let clackatoo_protocol::Change::Edit { step, .. } = change {
                    steps.push((buffer, *step));
                }
                let daemon = daemons.get_mut(&buffer).expect("a daemon for the buffer");
                let (answer, _) = daemon.take(message);
                desk.buffer_changed(buffer, answer);
            }
        }
    }

    /// The requests among `messages`.
    fn requests(messages: &[ClientMessage]) -> Vec<&Request> {
        (messages.iter())
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// The text the daemon has for `buffer`.
    fn daemon_text(daemons: &Daemons, buffer: u32) -> String {
        daemons[&BufferId(buffer)].text().to_string()
    }

    fn enter(desk: &mut Desk) {
        press(
            desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
    }

    /// The byte the focused editor's cursor is on.
    fn cursor_byte(desk: &Desk) -> usize {
        let (editor, copy) = desk.focused_editor().expect("an editor");
        editor.cursor_byte(copy)
    }

    /// The line the focused editor's cursor is on, from 0.
    fn cursor_line(desk: &Desk) -> usize {
        let (_, copy) = desk.focused_editor().expect("an editor");
        copy.text().byte_to_line(cursor_byte(desk))
    }

    /// The number of the finder's search, which must have been asked for.
    fn finder_search(desk: &mut Desk) -> u64 {
        let asked = (desk.take_outbox().into_iter()).find_map(|message| match message {
            ClientMessage::Find { id, .. } => Some(id),
            _ => None,
        });
        asked.expect("a search")
    }

    /// Gives the finder's search `id` the files `a.rs` and `b.rs`, in `/p`,
    /// and says that's all.
    fn found_a_and_b(desk: &mut Desk, id: u64) {
        found_files(desk, id, &["a.rs", "b.rs"]);
        let done = clackatoo_protocol::Found::Done(clackatoo_protocol::Done::All);
        desk.found(id, done);
    }

    #[test]
    fn a_macro_types_through_insert_mode_and_a_count_types_it_again() {
        let text = "a\nb\nc\nd\ne\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qaA!");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("A!<Esc>j"));
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "2@a");
        // Its edits are one undo step, as in Vim, which `u` undoes and
        // Ctrl+R redoes.
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(steps, [(BufferId(7), true), (BufferId(7), false)]);
        assert_eq!(daemon_text(&daemons, 7), "a!\nb!\nc!\nd\ne\n");
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb\nc\nd\ne\n");
        ctrl(&mut desk, "r");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd\ne\n");
        type_text(&mut desk, "4G@@");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd!\ne\n");
        // `"ap` puts the keys, which can be changed and typed again.
        type_text(&mut desk, "G\"ap");
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd!\neA!<Esc>j\n");
        type_text(&mut desk, "0f!r?0l\"by$gg@b");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!?\nb!\nc!\nd!\neA?<Esc>j\n");
    }

    #[test]
    fn a_macro_makes_a_change_made_in_visual_mode_again_in_one_undo_step() {
        let text = "one two\nthree four\nfive six\nseven\neight\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "vlcX");
        escape(&mut desk);
        type_text(&mut desk, "qaj0.q");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("j0."));
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "2@a");
        serve(&mut desk, &mut daemons);
        assert_eq!(
            daemon_text(&daemons, 7),
            "Xe two\nXree four\nXve six\nXven\neight\n"
        );
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(
            editor_text(&desk),
            "Xe two\nXree four\nfive six\nseven\neight\n"
        );
    }

    #[test]
    fn a_macro_runs_commands_and_waits_for_the_daemon_to_answer_them() {
        let text = "a\nb\nc\nd\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qaA!");
        escape(&mut desk);
        command(&mut desk, "w");
        type_text(&mut desk, "jq");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("A!<Esc>:w<CR>j"));
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert_eq!(requests(&sent), [&save]);
        desk.answered(Response::Done);
        // Typed again, it writes the file, and goes on once the daemon
        // has, with an x pressed meanwhile waiting behind it.
        type_text(&mut desk, "@ax");
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert_eq!(requests(&sent), [&save]);
        assert_eq!(editor_text(&desk), "a!\nb!\nc\nd\n");
        assert_eq!(cursor_line(&desk), 1, "waiting after the :w");
        desk.answered(Response::Done);
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\n\nd\n", "then the j, and the x");
        // A write the daemon refuses stops it, as a key that fails does.
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        let changed = clackatoo_protocol::Error::ChangedOnDisk("/t.txt".into());
        desk.answered(Response::Error(changed));
        serve(&mut desk, &mut daemons);
        assert_eq!(cursor_line(&desk), 2, "no j");
        assert_eq!(daemon_text(&daemons, 7), "a!\nb!\n!\nd\n");
    }

    #[test]
    fn keys_written_in_a_register_type_through_the_command_line() {
        let text = "x\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        // A line of keys, yanked into a register, types as it reads.
        type_text(&mut desk, "O:w<CR>A?<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        assert_eq!(requests(&sent), [&save]);
        desk.answered(Response::Done);
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "x?\n");
        // A command that isn't one stops it, saying so.
        type_text(&mut desk, "O:nosuch<CR>A!<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"by$dd");
        desk.message = None;
        type_text(&mut desk, "@b");
        serve(&mut desk, &mut daemons);
        assert!(desk.message.is_some(), "why it stopped");
        assert_eq!(editor_text(&desk), "x?\n");
    }

    /// Whether each edit sent to `buffer`, of those `steps` lists, started
    /// an undo step.
    fn starts_in(steps: &[(BufferId, bool)], buffer: u32) -> Vec<bool> {
        (steps.iter())
            .filter(|&&(of, _)| of == BufferId(buffer))
            .map(|&(_, step)| step)
            .collect()
    }

    #[test]
    fn a_macro_moves_between_panes_and_is_one_undo_step_in_each_file() {
        let mut desk = desk(one_tab(side_by_side(vec![
            file_pane(1, 7),
            file_pane(2, 8),
        ])));
        let (left, right) = ("abc1\nabc2\nabc3\n", "xyz1\nxyz2\nxyz3\n");
        load(&mut desk, 7, "/a.txt", left);
        load(&mut desk, 8, "/b.txt", right);
        let mut daemons = HashMap::from([
            (BufferId(7), crate::editor::Daemon::new(left)),
            (BufferId(8), crate::editor::Daemon::new(right)),
        ]);
        // With the window's own keys for moving between panes.
        type_text(&mut desk, "qaxxj");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "xxj");
        ctrl(&mut desk, "h");
        type_text(&mut desk, "q");
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some("xxj<C-l>xxj<C-h>")
        );
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "@a");
        assert_eq!(shown_pane(&desk), PaneId(1));
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "c1\nc2\nabc3\n");
        assert_eq!(daemon_text(&daemons, 8), "z1\nz2\nxyz3\n");
        // One undo step in each file, which its first edit starts.
        assert_eq!(starts_in(&steps, 7), [true, false]);
        assert_eq!(starts_in(&steps, 8), [true, false]);
        // So `u` in each undoes all the macro did there, and no more.
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "c1\nabc2\nabc3\n");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 8), "z1\nxyz2\nxyz3\n");
        // And the next macro starts steps of its own.
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(starts_in(&steps, 7), [true, false]);
        assert_eq!(starts_in(&steps, 8), [true, false]);
    }

    #[test]
    fn a_key_that_fails_in_any_pane_stops_the_macro() {
        let mut desk = desk(one_tab(side_by_side(vec![
            file_pane(1, 7),
            file_pane(2, 8),
        ])));
        let (left, right) = ("a\nb\n", "x\ny\n");
        load(&mut desk, 7, "/a.txt", left);
        load(&mut desk, 8, "/b.txt", right);
        let mut daemons = HashMap::from([
            (BufferId(7), crate::editor::Daemon::new(left)),
            (BufferId(8), crate::editor::Daemon::new(right)),
        ]);
        type_text(&mut desk, "qa");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "jA!");
        escape(&mut desk);
        ctrl(&mut desk, "h");
        type_text(&mut desk, "A?");
        escape(&mut desk);
        type_text(&mut desk, "q");
        serve(&mut desk, &mut daemons);
        // The j on the right's last line fails: nothing after it is typed.
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(daemon_text(&daemons, 8), "x\ny!\n");
        assert_eq!(daemon_text(&daemons, 7), "a?\nb\n");
    }

    #[test]
    fn a_macro_that_types_itself_stops_where_a_key_fails_or_after_too_many() {
        let mut desk = editing("/t.txt", "1\n2\n3\n4\n");
        // Cleared first, so recording `@a` types nothing.
        type_text(&mut desk, "qaqqaI- ");
        escape(&mut desk);
        type_text(&mut desk, "j@aq");
        assert_eq!(editor_text(&desk), "- 1\n2\n3\n4\n");
        type_text(&mut desk, "@a");
        assert_eq!(
            editor_text(&desk),
            "- 1\n- 2\n- 3\n- 4\n",
            "j on the last line stops it"
        );
        // A count stops too, where it fails, rather than going on with
        // what's left of it.
        let mut desk = editing("/t.txt", "a\nb\n");
        type_text(&mut desk, "qbA.");
        escape(&mut desk);
        type_text(&mut desk, "jq5@b");
        assert_eq!(editor_text(&desk), "a.\nb.\n");
        // Keys that make nothing stop it, as unknown keys do in Vim.
        let mut desk = editing("/t.txt", "x\n");
        type_text(&mut desk, "qcA1");
        escape(&mut desk);
        type_text(&mut desk, "Q");
        ctrl(&mut desk, "k");
        type_text(&mut desk, "A2");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(editor_text(&desk), "x12\n", "recording, it goes on");
        type_text(&mut desk, "@c");
        assert_eq!(editor_text(&desk), "x121\n", "replaying, it stops at Q");
        // And one that never fails is stopped after so many keys.
        type_text(&mut desk, "qdqqd0@dq@d");
        let said = desk.message.clone().unwrap_or_default();
        assert!(said.contains("stopped after"), "{said}");
        type_text(&mut desk, "x");
        assert_eq!(editor_text(&desk), "121\n", "it's done with");
    }

    #[test]
    fn gj_g0_and_g_dollar_are_typed_by_macros() {
        let text = format!("{}\nshort\n", "a".repeat(400));
        let mut desk = editing("/t.txt", &text);
        type_text(&mut desk, "qagjq");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("gj"));
        let row = cursor_byte(&desk);
        assert!(row > 1, "a row down the wrapped line: {row}");
        type_text(&mut desk, "@a");
        assert_eq!(cursor_byte(&desk), 2 * row);
        type_text(&mut desk, "2@a");
        assert_eq!(cursor_byte(&desk), 4 * row);
        // gj that can't go on, on the last row, fails, stopping the macro;
        // g$ and g0 never do.
        type_text(&mut desk, "Gqbg$g0gjxq");
        assert_eq!(editor_text(&desk).lines().nth(1), Some("hort"));
        type_text(&mut desk, "@b");
        assert_eq!(editor_text(&desk).lines().nth(1), Some("hort"), "no x");
    }

    #[test]
    fn a_search_that_finds_nothing_stops_a_macro() {
        let mut desk = editing("/t.txt", "x x x\n");
        type_text(&mut desk, "qa/x");
        enter(&mut desk);
        type_text(&mut desk, "r-A!");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("/x<CR>r-A!<Esc>"));
        assert_eq!(editor_text(&desk), "x - x!\n");
        type_text(&mut desk, "5@a");
        assert_eq!(editor_text(&desk), "- - -!!!\n", "two found, then none");
    }

    #[test]
    fn a_macro_types_into_the_finder_and_waits_for_what_it_finds() {
        let mut desk = editing("/p/a.rs", "a\n");
        type_text(&mut desk, "qa pfb.rs");
        let id = finder_search(&mut desk);
        found_a_and_b(&mut desk, id);
        enter(&mut desk);
        open_asked(&mut desk, 8, "/p/b.rs", "b\n");
        type_text(&mut desk, "q");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some(" pfb.rs<CR>"));
        command(&mut desk, "e /p/a.rs");
        open_asked(&mut desk, 9, "/p/a.rs", "a\n");
        // Typed again, it types into the finder at once, and Enter waits
        // for what it finds.
        type_text(&mut desk, "@a");
        let id = finder_search(&mut desk);
        assert_eq!(desk.finder.as_ref().map(Finder::query), Some("b.rs"));
        found_a_and_b(&mut desk, id);
        open_asked(&mut desk, 10, "/p/b.rs", "b\n");
        assert!(!desk.finding());
        assert_eq!(note(&desk), "b.rs  1:1");
        // With nothing it finds picked, Enter stops the macro.
        type_text(&mut desk, "O pfzzz<CR>A!<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"by$dd@b");
        let id = finder_search(&mut desk);
        found_a_and_b(&mut desk, id);
        assert_eq!(
            desk.message.as_deref(),
            Some("nothing in the finder is picked")
        );
        escape(&mut desk);
        assert_eq!(editor_text(&desk), "b\n", "no A!");
    }

    #[test]
    fn a_macro_waits_for_the_language_server_and_gives_up_on_it() {
        let mut desk = editing("/src/a.rs", A_RS);
        let mut daemons = daemon_of_7(A_RS);
        // Answers gd, asked once the edits before it are made, with a
        // place on `line`.
        let answer = |desk: &mut Desk, daemons: &mut Daemons, line| {
            let (_, sent) = serve(desk, daemons);
            let id = (sent.iter()).find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(*id),
                _ => None,
            });
            let place = clackatoo_protocol::FileLocation {
                path: "/src/a.rs".into(),
                line,
                col: 3,
                offset: None,
                text: String::new(),
            };
            let update = clackatoo_protocol::BufferUpdate::Answer {
                id: id.expect("a query"),
                version: daemons[&BufferId(7)].version(),
                answer: clackatoo_protocol::Answer::Locations(vec![place]),
            };
            desk.buffer_changed(BufferId(7), update);
            serve(desk, daemons);
        };
        type_text(&mut desk, "qagd");
        answer(&mut desk, &mut daemons, 2);
        type_text(&mut desk, "xq");
        assert_eq!(editor_text(&desk), "fn a() {}\nfn b() {}\nfn () {}\n");
        // The x waits for gd's answer, and goes where it went.
        type_text(&mut desk, "gg@a");
        assert_eq!(editor_text(&desk), "fn a() {}\nfn b() {}\nfn () {}\n");
        answer(&mut desk, &mut daemons, 1);
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
        // Esc stops a macro waiting, with the keys pressed meanwhile.
        type_text(&mut desk, "gg@al");
        escape(&mut desk);
        assert_eq!(desk.message.as_deref(), Some("the macro was stopped"));
        answer(&mut desk, &mut daemons, 0);
        assert_eq!(cursor_byte(&desk), 3, "gd went, and no more");
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
        // And it gives up after a while, if no answer comes.
        type_text(&mut desk, "gg@a");
        let later = std::time::Instant::now() + std::time::Duration::from_secs(6);
        assert!(desk.editor_wakes().is_some_and(|wake| wake < later));
        desk.tick_editors(later);
        let said = desk.message.clone().unwrap_or_default();
        assert_eq!(
            said,
            "the macro was stopped: it waited too long for the language server"
        );
        answer(&mut desk, &mut daemons, 0);
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
    }

    #[test]
    fn q_in_a_terminal_stops_recording_as_in_an_editor() {
        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/t.txt", "abc\n");
        snapshot(&mut desk, 2);
        type_text(&mut desk, "qa");
        ctrl(&mut desk, "l");
        assert_eq!(mode(&desk, 2), Mode::Normal);
        type_text(&mut desk, "q");
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("<C-l>"));
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        assert_eq!(shown_pane(&desk), PaneId(2));
    }

    #[test]
    fn a_macro_types_what_a_completion_put_in_without_asking() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        let mut desk = editing("/src/a.rs", "\n\n");
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaial");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        let call = CompletionItem {
            label: "call".into(),
            kind: 3,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(0),
            text: "call(${1:a})$0".into(),
            snippet: true,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![call],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "y");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "call(a)\n\n");
        // Not the key that accepted it, but what it put in, with the cursor
        // where the snippet left it.
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some("ial<BS><BS>call(a)<Left><Esc>j")
        );
        serve_edits(&mut desk, &mut version);
        type_text(&mut desk, "@a");
        desk.tick_editors(later);
        assert_eq!(editor_text(&desk), "call(a)\ncall(a)\n");
        let asked = (desk.take_outbox().into_iter())
            .any(|message| matches!(message, ClientMessage::Query { .. }));
        assert!(!asked, "the server isn't asked");
    }

    #[test]
    fn a_macro_types_what_a_snippet_left_rather_than_the_keys_filling_it_in() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        // Filled in on a line indented four...
        let mut desk = editing("/src/a.rs", "    \n\n");
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaAif");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        // A mirror, lines indented more and less, and a place left with
        // the cursor in it.
        let snippet = CompletionItem {
            label: "if".into(),
            kind: 15,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: None,
            text: "if ${1:x} {\n\t${2:y}($1)\n}$0".into(),
            snippet: true,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![snippet],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "y");
        type_text(&mut desk, "a");
        let tab = |desk: &mut Desk| {
            press(desk, Key::Named(NamedKey::Tab), None, Modifiers::default());
        };
        tab(&mut desk);
        type_text(&mut desk, "b");
        escape(&mut desk);
        type_text(&mut desk, "jjq");
        let filled = "    if a {\n        b(a)\n    }\n";
        assert_eq!(editor_text(&desk), format!("{filled}\n"));
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some(
                "Aif<BS><BS>if a {<CR>    b(a)<CR><BS><BS><BS><BS>}<Up><End><Left><Left><Left><Esc>jj"
            )
        );
        // ...and typed again on a line indented none, by a file with no
        // grammar to indent it.
        serve_edits(&mut desk, &mut version);
        type_text(&mut desk, "@a");
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                break;
            }
            for message in messages {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query {
                        id,
                        query: clackatoo_protocol::Query::Indent { .. },
                        ..
                    } => {
                        let answer = BufferUpdate::Answer {
                            id,
                            version,
                            answer: Answer::Nothing,
                        };
                        desk.buffer_changed(BufferId(7), answer);
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(
            editor_text(&desk),
            format!("{filled}if a {{\n    b(a)\n}}\n")
        );
    }

    #[test]
    fn a_macro_waiting_for_the_grammar_is_still_one_undo_step() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qadaf");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let question = |sent: &[ClientMessage]| {
            (sent.iter())
                .find_map(|message| match message {
                    ClientMessage::Query { id, .. } => Some(*id),
                    _ => None,
                })
                .expect("a question")
        };
        let id = question(&sent);
        answer_at(&mut desk, id, 0, vec![function(0, 9, None)]);
        type_text(&mut desk, "ix");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "x\nfn b() {}\n");
        // The x waits for the answer, and goes in after, in the same step.
        type_text(&mut desk, "@a");
        let (steps, sent) = serve(&mut desk, &mut daemons);
        assert!(steps.is_empty());
        let version = daemons[&BufferId(7)].version();
        answer_at(
            &mut desk,
            question(&sent),
            version,
            vec![function(2, 11, None)],
        );
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "x\nx\n");
        assert_eq!(steps, [(BufferId(7), true), (BufferId(7), false)]);
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(
            daemon_text(&daemons, 7),
            "x\nfn b() {}\n",
            "one u undoes it all"
        );
    }

    /// The number of the last query among `sent`, which must hold one.
    fn query_in(sent: &[ClientMessage]) -> u64 {
        let asked = (sent.iter().rev()).find_map(|message| match message {
            ClientMessage::Query { id, .. } => Some(*id),
            _ => None,
        });
        asked.expect("a query")
    }

    /// Answers the query `id` about buffer 7 with a place on `line`, at
    /// its fourth column, as gd's answer.
    fn answer_gd(desk: &mut Desk, daemons: &Daemons, id: u64, path: &str, line: u64) {
        let place = clackatoo_protocol::FileLocation {
            path: path.into(),
            line,
            col: 3,
            offset: None,
            text: String::new(),
        };
        let update = clackatoo_protocol::BufferUpdate::Answer {
            id,
            version: daemons[&BufferId(7)].version(),
            answer: clackatoo_protocol::Answer::Locations(vec![place]),
        };
        desk.buffer_changed(BufferId(7), update);
    }

    #[test]
    fn a_binding_a_macros_last_key_sets_off_is_part_of_the_macro() {
        let text = "abc\ndef\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        desk.lua_from(r#"client.keys.map("n", "<Space>m", ":w<CR>j")"#);
        answer_all(&mut desk);
        type_text(&mut desk, "Ox m");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd");
        serve(&mut desk, &mut daemons);
        // The binding the macro's last key sets off waits, as the macro's
        // own keys do: the j waits for the write.
        type_text(&mut desk, "@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        assert_eq!(requests(&sent), [&save]);
        assert_eq!(
            (editor_text(&desk).as_str(), cursor_line(&desk)),
            ("bc\ndef\n", 0)
        );
        desk.answered(Response::Done);
        assert_eq!(cursor_line(&desk), 1);
    }

    #[test]
    fn a_macro_waiting_stops_if_the_focus_moves_but_goes_where_it_asks() {
        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        snapshot(&mut desk, 2);
        let mut daemons = daemon_of_7(A_RS);
        type_text(&mut desk, "Ogdx");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd");
        serve(&mut desk, &mut daemons);
        // Clicked away from while it waits for gd: it stops, and the x
        // doesn't go to the terminal, nor anywhere.
        type_text(&mut desk, "@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        desk.show_pane(PaneId(2));
        answer_gd(&mut desk, &daemons, query_in(&sent), "/src/a.rs", 1);
        let (_, sent) = serve(&mut desk, &mut daemons);
        let typed = (requests(&sent).into_iter())
            .any(|request| matches!(request, Request::SendInput { .. }));
        assert!(!typed, "nothing for the terminal: {sent:?}");
        assert_eq!(
            desk.message.as_deref(),
            Some("the macro was stopped: the focus moved")
        );
        assert_eq!(daemon_text(&daemons, 7), A_RS);
        // So too if another window closes its pane meanwhile.
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        desk.workspaces_changed(one_tab(pane(2)));
        desk.tick_editors(std::time::Instant::now());
        assert_eq!(
            desk.message.as_deref(),
            Some("the macro was stopped: the focus moved")
        );
        assert_eq!(shown_pane(&desk), PaneId(2));
        // But a pane the macro makes, the focus going to it, is where it
        // goes on.
        let mut desk = editing("/t.txt", "abc\n");
        let mut daemons = daemon_of_7("abc\n");
        type_text(&mut desk, "O<C-b>|<C-b>p1x");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert!(matches!(requests(&sent)[..], [Request::Split { .. }]));
        desk.workspaces_changed(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        desk.answered(Response::PaneCreated(PaneId(2)));
        serve(&mut desk, &mut daemons);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(daemon_text(&daemons, 7), "bc\n");
    }

    #[test]
    fn keys_held_behind_a_wait_that_fail_do_as_they_would_typed() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // Held while daf waits: a $ and an l, which fails at the line's
        // end, then the finder's keys, which open it as typed.
        type_keys(&mut desk, "jdaf$l pf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(10, 19, None)]);
        assert_eq!(editor_text(&desk), "fn a() {}\n\n");
        assert!(desk.finding(), "the keys after the l");
    }

    #[test]
    fn a_q_held_while_recording_stops_it_where_it_was_pressed() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // The q and y come while daf waits, and are held.
        type_keys(&mut desk, "qadafqy");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("daf"));
    }

    #[test]
    fn a_key_a_binding_types_that_a_menu_takes_is_recorded_as_pressed() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        let mut desk = editing("/src/a.rs", "\n\n");
        desk.lua_from(r#"client.keys.map("i", "<C-j>", "<C-y>")"#);
        answer_all(&mut desk);
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaial");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        let call = CompletionItem {
            label: "call".into(),
            kind: 3,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(0),
            text: "call".into(),
            snippet: false,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![call],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "j");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(editor_text(&desk), "call\n\n");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("ial<C-j><Esc>"));
    }

    #[test]
    fn a_macro_waits_only_for_what_it_asked() {
        let mut desk = editing("/src/a.rs", A_RS);
        let mut daemons = daemon_of_7(A_RS);
        type_text(&mut desk, "qaxq");
        serve(&mut desk, &mut daemons);
        // A gd from before the macro, never answered, isn't waited for.
        type_text(&mut desk, "gd");
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "@ax");
        assert_eq!(editor_text(&desk), "a() {}\nfn b() {}\nfn c() {}\n");
        // Nor, once the connection's lost, is anything asked on it.
        let waiting = |desk: &Desk| {
            let (editor, copy) = desk.focused_editor().expect("an editor");
            editor.waiting_for_answer(copy, 0)
        };
        assert!(waiting(&desk));
        desk.lost();
        desk.connected(one_tab(file_pane(1, 7)), false);
        assert!(!waiting(&desk));
    }

    #[test]
    #[ignore = "a measurement, to run optimised: cargo test --release -- --ignored replaying"]
    fn replaying_a_20_key_macro_1000_times_on_10000_lines() {
        let lines: Vec<String> = (0..10_000)
            .map(|line| format!("line {line} of the file\n"))
            .collect();
        let text = lines.concat();
        let mut desk = editing("/big.txt", &text);
        // Twenty keys: motions, a change, an insert at the end, a delete.
        type_text(&mut desk, "qa0wcwnew");
        escape(&mut desk);
        type_text(&mut desk, "A end");
        escape(&mut desk);
        type_text(&mut desk, "bxj^llq");
        let mut times = Vec::new();
        for _ in 0..5 {
            let started = std::time::Instant::now();
            type_text(&mut desk, "1000@a");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            // What the daemon would be sent, so the next go starts afresh.
            desk.take_outbox();
        }
        let line = |n: usize| editor_text(&desk).lines().nth(n).unwrap().to_owned();
        assert_eq!(line(5000), "line new of the file nd");
        times.sort_by(f64::total_cmp);
        eprintln!(
            "1000@a of 20 keys on 10,000 lines: best {:.1} ms, median {:.1} ms, worst {:.1} ms \
             ({:.2} µs a key at best)",
            times[0],
            times[2],
            times[4],
            times[0] * 1000.0 / 20_000.0
        );
    }

    #[test]
    fn editor_commands_run_from_the_command_line_and_stop_a_macro_when_refused() {
        let mut desk = editing("/t.txt", "x\nx\ny\nx\n");
        type_text(&mut desk, "qa:s/x/-/");
        enter(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "-\nx\ny\nx\n");
        // The third line has no x: the macro stops there, as in Vim.
        type_text(&mut desk, "5@a");
        assert_eq!(editor_text(&desk), "-\n-\ny\nx\n");
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains("isn't in"))
        );
        // A mistake is said in the bar.
        type_text(&mut desk, ":1,2frob");
        enter(&mut desk);
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains("frob"))
        );
    }

    #[test]
    fn the_command_being_typed_is_previewed_in_the_focused_editor_until_esc() {
        let rows = |desk: &mut Desk| -> Vec<String> {
            desk.settle_editors();
            let editor = desk.editor(PaneId(1)).expect("an editor");
            editor
                .scene()
                .0
                .iter()
                .map(|row| row.text().trim_end().to_owned())
                .collect()
        };
        let mut desk = editing("/t.txt", "one two\n");
        let before = rows(&mut desk);
        type_text(&mut desk, ":%s/two/2/");
        assert!(rows(&mut desk)[0].ends_with("one 2"));
        assert_eq!(editor_text(&desk), "one two\n");
        escape(&mut desk);
        assert_eq!(rows(&mut desk), before);
        // From a selection, the command line starts with its lines.
        type_text(&mut desk, "V:");
        assert!(matches!(desk.status(), Status::Typing { text, .. } if text == ":'<,'>▏"));
        type_text(&mut desk, "s/one/1/");
        enter(&mut desk);
        assert_eq!(editor_text(&desk), "1 two\n");
    }

    #[test]
    fn a_number_is_a_line_in_an_editor() {
        let mut desk = editing("/t.txt", "1\n2\n3\n4\n");
        type_text(&mut desk, ":3");
        enter(&mut desk);
        let (editor, copy) = desk.focused_editor().expect("an editor");
        assert_eq!(editor.cursor_byte(copy), 4);
        // Ctrl+O comes back, as from a jump.
        ctrl(&mut desk, "o");
        let (editor, copy) = desk.focused_editor().expect("an editor");
        assert_eq!(editor.cursor_byte(copy), 0);
    }

    // --- File explorers ------------------------------------------------------

    fn files_pane(pane: u32, files: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(pane),
            content: Content::Files(clackatoo_protocol::FilesId(files)),
            home: None,
            key: None,
        })
    }

    fn entry(name: &str, dir: bool) -> clackatoo_protocol::Entry {
        clackatoo_protocol::Entry {
            name: name.into(),
            kind: if dir {
                clackatoo_protocol::EntryKind::Dir
            } else {
                clackatoo_protocol::EntryKind::File { executable: false }
            },
            ignored: false,
            stamp: None,
        }
    }

    /// Gives explorer 1 the root `/p`, holding `src/` and `a.txt`, with
    /// `src` holding `main.rs` if it's asked for, and answers what's
    /// asked.
    fn explore_p(desk: &mut Desk) {
        use clackatoo_protocol::{FilesId, FilesUpdate};
        desk.files_changed(
            FilesId(1),
            FilesUpdate::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let listed = |dir: &str, entries| FilesUpdate::Listed {
            dir: dir.into(),
            entries,
            cut_short: false,
        };
        desk.files_changed(
            FilesId(1),
            listed("/p", vec![entry("src", true), entry("a.txt", false)]),
        );
        let src_asked = (desk.take_outbox().iter()).any(|message| {
            matches!(message, ClientMessage::Files { action: clackatoo_protocol::FilesAction::Expand(dir), .. } if dir == "/p/src")
        });
        if src_asked {
            desk.files_changed(FilesId(1), listed("/p/src", vec![entry("main.rs", false)]));
        }
    }

    /// The file operations asked for, by their numbers.
    fn file_ops(desk: &mut Desk) -> Vec<(u64, clackatoo_protocol::FileOp)> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::FileOp { id, op, .. } => Some((id, op)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn space_e_opens_an_explorer_down_the_tabs_left_at_the_editors_file_then_closes_it() {
        use clackatoo_protocol::{FilesId, OpenFiles};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        type_keys(&mut desk, " e");
        let asked: Vec<Request> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(
            asked,
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Left,
                terminal: PaneTerminal::Files(OpenFiles { dir: None }),
                share: Some(250),
                on: None,
            }],
            "20 cells, a quarter of the tab's 80 columns, as that's fewer than 30"
        );
        // The daemon makes it, telling of the pane first.
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
        desk.workspaces_changed(one_tab(layout));
        desk.answered(Response::PaneCreated(PaneId(2)));
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert!(
            answer_all(&mut desk).contains(&Request::AttachFiles { files: FilesId(1) }),
            "attached"
        );
        explore_p(&mut desk);
        assert_eq!(
            note(&desk),
            "~/p  src/main.rs  g? keys",
            "at the editor's file"
        );
        assert_eq!(desk.title(), Some("~/p"));
        type_keys(&mut desk, " e");
        assert_eq!(
            answer_all(&mut desk),
            [Request::ClosePane { pane: PaneId(2) }]
        );
        // Set, it's as many cells as it says.
        let layout = one_tab(file_pane(1, 7));
        desk.workspaces_changed(layout);
        normal_mode(&mut desk);
        command(&mut desk, "set explorerwidth=30");
        type_keys(&mut desk, " e");
        assert!(
            (desk.take_outbox().iter()).any(|message| matches!(
                message,
                ClientMessage::Request(Request::SplitTab {
                    share: Some(375),
                    ..
                })
            )),
            "30 of the tab's 80 columns"
        );
    }

    #[test]
    fn with_two_explorers_in_a_tab_space_e_closes_the_focused_one() {
        let layout = side_by_side(vec![files_pane(2, 1), files_pane(3, 2)]);
        let mut desk = super::tests::desk(one_tab(layout));
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        answer_all(&mut desk);
        type_keys(&mut desk, " e");
        assert_eq!(
            answer_all(&mut desk),
            [Request::ClosePane { pane: PaneId(3) }],
            "not the first"
        );
    }

    #[test]
    fn a_split_mark_opens_an_explorer_or_a_review_beside_the_focused_pane() {
        use clackatoo_protocol::{NewReview, OpenFiles, ReviewScope};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        let asked = |desk: &mut Desk| -> Vec<Request> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Request(request) => Some(request),
                    _ => None,
                })
                .collect()
        };
        command(&mut desk, "|Explore src");
        assert_eq!(
            asked(&mut desk),
            [Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::Files(OpenFiles {
                    dir: Some("src".into())
                }),
                on: None,
            }]
        );
        command(&mut desk, "-Review base");
        assert_eq!(
            asked(&mut desk),
            [Request::Split {
                pane: PaneId(1),
                side: Side::Below,
                terminal: PaneTerminal::Review(NewReview {
                    chat: None,
                    scope: ReviewScope::Base,
                    cwd: Some("/p/src".into()),
                }),
                on: None,
            }]
        );
    }

    #[test]
    fn a_file_picked_opens_in_the_editor_last_focused_or_a_split_or_a_tab() {
        use clackatoo_protocol::OpenFile;
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7), pane(3)]);
        let mut desk = desk(one_tab(layout));
        load(&mut desk, 7, "/p/b.txt", "b\n");
        assert_eq!(shown_pane(&desk), PaneId(2));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(2));
        answer_all(&mut desk);
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        let file = |path: &str| OpenFile {
            path: path.into(),
            cwd: None,
        };
        let mut asked = answer_all(&mut desk);
        asked.retain(|request| !matches!(request, Request::Resize { .. }));
        assert_eq!(
            asked,
            [
                Request::Focus { pane: PaneId(1) },
                Request::OpenHere {
                    pane: PaneId(1),
                    file: file("/p/a.txt"),
                }
            ]
        );
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "h");
        answer_all(&mut desk);
        ctrl(&mut desk, "v");
        ctrl(&mut desk, "t");
        assert_eq!(
            answer_all(&mut desk),
            [
                Request::Split {
                    pane: PaneId(1),
                    side: Side::Right,
                    terminal: PaneTerminal::File(file("/p/a.txt")),
                    on: None,
                },
                Request::NewTab {
                    workspace: WorkspaceId(1),
                    terminal: PaneTerminal::File(file("/p/a.txt")),
                    on: None,
                }
            ]
        );
    }

    #[test]
    fn with_no_editor_a_file_picked_opens_beside_the_explorer() {
        use clackatoo_protocol::OpenFile;
        let file = PaneTerminal::File(OpenFile {
            path: "/p/a.txt".into(),
            cwd: None,
        });
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        assert_eq!(
            answer_all(&mut desk),
            [Request::Split {
                pane: PaneId(3),
                side: Side::Left,
                terminal: file.clone(),
                on: None,
            }],
            "between the explorer and the terminal"
        );
        // Alone, the explorer keeps its width.
        let mut desk = super::tests::desk(one_tab(files_pane(2, 1)));
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        assert_eq!(
            answer_all(&mut desk),
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Right,
                terminal: file,
                share: Some(750),
                on: None,
            }]
        );
    }

    #[test]
    fn file_operations_are_asked_in_the_bar_and_what_comes_of_them_said() {
        use clackatoo_protocol::{FileOp, FileOpError};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "jd");
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Bar));
        assert!(matches!(
            desk.status(),
            Status::Typing { text, .. } if text == "delete ~/p/a.txt, to the trash? (y/n)"
        ));
        type_keys(&mut desk, "y");
        let trashed = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: true,
            expect: None,
        };
        assert_eq!(file_ops(&mut desk), [(0, trashed)]);
        desk.file_op_done(0, Err(FileOpError::NoTrash("no trash there".into())));
        assert!(matches!(
            desk.status(),
            Status::Typing { text, .. } if text == "no trash there: delete ~/p/a.txt for good? (y/n)"
        ));
        type_keys(&mut desk, "y");
        let deleted = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: false,
            expect: None,
        };
        assert_eq!(file_ops(&mut desk), [(1, deleted)]);
        desk.file_op_done(1, Ok(()));
        assert_eq!(note(&desk), "deleted ~/p/a.txt for good");
        // Cut here, put there: a move, which places in files follow.
        type_keys(&mut desk, "xkp");
        let moved = FileOp::Move {
            from: "/p/a.txt".into(),
            to: "/p/src/a.txt".into(),
            overwrite: None,
            expect: None,
        };
        assert_eq!(file_ops(&mut desk), [(2, moved)]);
        desk.file_op_done(2, Ok(()));
        assert_eq!(note(&desk), "moved to ~/p/src/a.txt");
        type_keys(&mut desk, "p");
        assert_eq!(note(&desk), "nothing's cut or copied: x or y does");
    }

    #[test]
    fn what_was_cut_on_one_machine_isnt_pasted_on_another() {
        use clackatoo_protocol::{FilesId, FilesUpdate, MachineId};
        // Desk's explorer, of a folder with the same path as this one's.
        let desk_id = MachineId([7; 16]);
        let mut there = workspace(2, "w", vec![tab(2, files_pane(4, 5))]);
        there.home = Some(desk_id);
        if let Layout::Pane(pane) = &mut there.tabs[0].layout {
            pane.home = Some(desk_id);
        }
        let mut desk = desk(workspaces(vec![
            workspace(
                1,
                "w",
                vec![tab(1, side_by_side(vec![files_pane(2, 1), pane(3)]))],
            ),
            there,
        ]));
        explore_p(&mut desk);
        type_keys(&mut desk, "jx");
        desk.show_pane(PaneId(4));
        let root = FilesUpdate::Root {
            path: "/p".into(),
            name: "~/p".into(),
        };
        desk.files_changed(FilesId(5), root);
        let listed = FilesUpdate::Listed {
            dir: "/p".into(),
            entries: vec![entry("src", true), entry("a.txt", false)],
            cut_short: false,
        };
        desk.files_changed(FilesId(5), listed);
        file_ops(&mut desk);
        type_keys(&mut desk, "p");
        assert!(file_ops(&mut desk).is_empty(), "nothing is moved on desk");
        assert_eq!(note(&desk), "cut on this machine: paste it there");
    }

    #[test]
    fn dot_dot_in_an_explorer_goes_up_on_a_double_click_and_is_kept_as_it_was_left() {
        use clackatoo_protocol::{FilesAction, FilesId, FilesUpdate};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        desk.take_outbox();
        // A double click on `..`, the first line under the root's.
        assert!(desk.explorer_click(PaneId(2), [1, 6], 2));
        assert!(
            desk.take_outbox().contains(&ClientMessage::Files {
                files: FilesId(1),
                action: FilesAction::Root("/".into()),
            }),
            "gone up"
        );
        // Closed on `..`, or on a file, and opened again on its root in
        // this window, it's as it was left: never a line off.
        let reopened = |desk: &mut Desk, files: u32| {
            desk.workspaces_changed(one_tab(pane(3)));
            let id = 10 + files;
            let layout = side_by_side(vec![files_pane(id, files), pane(3)]);
            desk.workspaces_changed(one_tab(layout));
            let root = FilesUpdate::Root {
                path: "/p".into(),
                name: "~/p".into(),
            };
            desk.files_changed(FilesId(files), root);
            let listed = FilesUpdate::Listed {
                dir: "/p".into(),
                entries: vec![entry("src", true), entry("a.txt", false)],
                cut_short: false,
            };
            desk.files_changed(FilesId(files), listed);
            desk.explorer(PaneId(id)).unwrap().label()
        };
        type_keys(&mut desk, "gg");
        assert_eq!(note(&desk), "~/p  ..  g? keys");
        assert_eq!(reopened(&mut desk, 2), "~/p  ..  g? keys");
        let mut desk = super::tests::desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "G");
        assert_eq!(note(&desk), "~/p  a.txt  g? keys");
        assert_eq!(reopened(&mut desk, 2), "~/p  a.txt  g? keys");
    }

    #[test]
    fn colon_explore_opens_an_explorer_at_a_directory_or_goes_to_the_one_there() {
        use clackatoo_protocol::OpenFiles;
        let mut desk = desk(one_tab(pane(3)));
        normal_mode(&mut desk);
        command(&mut desk, "Explore ~/src");
        assert_eq!(
            answer_all(&mut desk),
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Left,
                terminal: PaneTerminal::Files(OpenFiles {
                    dir: Some("~/src".into())
                }),
                share: Some(250),
                on: None,
            }]
        );
        let mut desk = super::tests::desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        command(&mut desk, "Ex");
        assert_eq!(shown_pane(&desk), PaneId(2));
        // Keys in it are its own: q closes it.
        type_keys(&mut desk, "q");
        assert!(answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(2) }));
    }

    // --- Browsers --------------------------------------------------------

    #[test]
    fn a_browser_pane_watches_its_chats_page_only_while_its_tab_is_shown() {
        use clackatoo_protocol::{BrowserStatus, BrowserUpdate, Event};
        let browser = |pane: u32| {
            Layout::Pane(LayoutPane {
                id: PaneId(pane),
                content: Content::Browser(TerminalId(5)),
                home: None,
                key: None,
            })
        };
        let mut desk = desk(one_tab(pane(3)));
        normal_mode(&mut desk);
        // Only a chat has a page.
        command(&mut desk, "|Browser");
        assert!(answer_all(&mut desk).is_empty());
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|said| said.contains("focus the chat"))
        );

        // Shown, it's attached, at a size its tile gives, in steps.
        let both = |second: Layout| {
            workspaces(vec![workspace(
                1,
                "w",
                vec![
                    tab(1, side_by_side(vec![pane(3), browser(4)])),
                    tab(2, second),
                ],
            )])
        };
        desk.workspaces_changed(both(pane(6)));
        let attached: Vec<Request> = (answer_all(&mut desk).into_iter())
            .filter(|request| matches!(request, Request::AttachBrowser { .. }))
            .collect();
        let [
            Request::AttachBrowser {
                chat,
                width,
                height,
            },
        ] = attached.as_slice()
        else {
            panic!("{attached:?}");
        };
        assert_eq!(*chat, TerminalId(5));
        assert!(width % 160 == 0 && height % 160 == 0 && *width > 0);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(4));
        let status = BrowserStatus {
            url: "https://example.com/".into(),
            title: "Example".into(),
            action: Some("Opened https://example.com/".into()),
            running: true,
            failure: None,
        };
        let event = Event::Browser {
            chat: TerminalId(5),
            update: BrowserUpdate::Status(status),
        };
        assert!(desk.browser_event(event), "shown, so drawn again");
        assert_eq!(desk.title(), Some("Example"));
        // From a browser pane, another pane on the same chat's page.
        command(&mut desk, "-Browser");
        assert!(answer_all(&mut desk).contains(&Request::Split {
            pane: PaneId(4),
            side: Side::Below,
            terminal: PaneTerminal::Browser(TerminalId(5)),
            on: None,
        }));

        // In a tab not shown, it's let go: no pictures nobody sees.
        command(&mut desk, "2");
        assert!(answer_all(&mut desk).contains(&Request::DetachBrowser {
            chat: TerminalId(5)
        }));
        desk.show_tab(TabId(1));
        assert!(
            (answer_all(&mut desk).iter())
                .any(|request| matches!(request, Request::AttachBrowser { .. }))
        );
        // q closes it.
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(4));
        type_keys(&mut desk, "q");
        assert!(answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(4) }));
    }

    // --- Boards ----------------------------------------------------------

    fn board_pane(pane: u32, board: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(pane),
            content: Content::Board(clackatoo_protocol::BoardId(board)),
            home: None,
            key: None,
        })
    }

    #[test]
    fn a_board_pane_shows_its_board_live_and_posts_typed_in_the_bar() {
        use clackatoo_protocol::{
            BoardAction, BoardEntry, BoardId, BoardScope, BoardSummary, BoardUpdate, OpenBoard,
            PostKind,
        };
        let mut desk = desk(one_tab(pane(3)));
        normal_mode(&mut desk);
        // A kind of pane: in a tab, or with a mark, beside.
        command(&mut desk, "Board plan");
        command(&mut desk, "|Board plan");
        let opened = answer_all(&mut desk);
        assert_eq!(
            opened,
            [
                Request::NewTab {
                    workspace: WorkspaceId(1),
                    terminal: PaneTerminal::Board(OpenBoard::Named("plan".into())),
                    on: None,
                },
                Request::Split {
                    pane: PaneId(3),
                    side: Side::Right,
                    terminal: PaneTerminal::Board(OpenBoard::Named("plan".into())),
                    on: None,
                },
            ]
        );

        // Shown, it's attached.
        desk.workspaces_changed(one_tab(side_by_side(vec![pane(3), board_pane(4, 9)])));
        assert!(answer_all(&mut desk).contains(&Request::AttachBoard { board: BoardId(9) }));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(4));
        let summary = |entries| BoardSummary {
            id: BoardId(9),
            name: "plan".into(),
            purpose: String::new(),
            scope: BoardScope::Global,
            archived: false,
            created: 0,
            entries,
        };
        let entry = BoardEntry {
            id: 1,
            kind: PostKind::Note,
            by: clackatoo_protocol::BoardAuthor::User,
            at: 0,
            text: "first".into(),
            links: Vec::new(),
            subject: None,
            expires: None,
            released: None,
            edits: Vec::new(),
        };
        let snapshot = BoardUpdate::Snapshot {
            summary: summary(1),
            entries: vec![entry],
        };
        let event = clackatoo_protocol::Event::Board {
            board: BoardId(9),
            update: snapshot,
        };
        assert!(desk.board_event(event), "shown, so drawn again");
        assert_eq!(desk.title(), Some("Board: plan"));
        // Typed in the bar, posted as the user.
        type_keys(&mut desk, "i");
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Bar));
        type_keys(&mut desk, "on it");
        enter(&mut desk);
        let posted: Vec<ClientMessage> = (desk.take_outbox().into_iter())
            .filter(|message| matches!(message, ClientMessage::Board { .. }))
            .collect();
        assert_eq!(
            posted,
            [ClientMessage::Board {
                board: BoardId(9),
                action: BoardAction::Post {
                    kind: PostKind::Note,
                    text: "on it".into(),
                },
            }]
        );
        // q closes the pane; the board is the daemon's to keep.
        type_keys(&mut desk, "q");
        assert!(answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(4) }));
    }

    #[test]
    fn colon_boards_picks_a_board_in_the_finder_and_opens_it_beside() {
        use clackatoo_protocol::{BoardId, BoardScope, BoardSummary, OpenBoard, Search};
        let mut desk = desk(one_tab(pane(3)));
        normal_mode(&mut desk);
        command(&mut desk, "Boards");
        let asked = (desk.take_outbox().into_iter()).find_map(|message| match message {
            ClientMessage::Find {
                id,
                search: Search::Boards,
                ..
            } => Some(id),
            _ => None,
        });
        let id = asked.expect("the boards asked for");
        let board = |id, name: &str, archived| BoardSummary {
            id: BoardId(id),
            name: name.into(),
            purpose: "for it".into(),
            scope: BoardScope::Global,
            archived,
            created: 0,
            entries: 0,
        };
        let boards = vec![board(2, "plan", false), board(5, "old", true)];
        assert!(desk.found(id, clackatoo_protocol::Found::Boards(boards)));
        type_keys(&mut desk, "old");
        enter(&mut desk);
        assert_eq!(
            answer_all(&mut desk),
            [Request::Split {
                pane: PaneId(3),
                side: Side::Right,
                terminal: PaneTerminal::Board(OpenBoard::Id(BoardId(5))),
                on: None,
            }]
        );
    }

    #[test]
    fn q_in_an_explorer_stops_recording_rather_than_closing_it() {
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
        let mut desk = super::tests::desk(one_tab(layout));
        load(&mut desk, 7, "/p/b.txt", "b\n");
        ctrl(&mut desk, "l");
        type_keys(&mut desk, "qa");
        assert_eq!(desk.recording(), Some('a'));
        ctrl(&mut desk, "h");
        answer_all(&mut desk);
        type_keys(&mut desk, "q");
        assert_eq!(desk.recording(), None);
        assert!(!answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(2) }));
    }

    // --- Folders edited as text --------------------------------------------

    /// What a step on a file listed with no stamp expects.
    fn file_expected() -> clackatoo_protocol::Expect {
        clackatoo_protocol::Expect {
            kind: clackatoo_protocol::EntryKind::File { executable: false },
            stamp: None,
        }
    }

    /// Gives explorer 1 the root `/p`, holding `entries`.
    fn explore_with(desk: &mut Desk, entries: Vec<clackatoo_protocol::Entry>) {
        use clackatoo_protocol::{FilesId, FilesUpdate};
        desk.files_changed(
            FilesId(1),
            FilesUpdate::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let listed = FilesUpdate::Listed {
            dir: "/p".into(),
            entries,
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
        answer_all(desk);
    }

    /// A file's stamp, told apart by `len`.
    fn stamped(name: &str, len: u64) -> clackatoo_protocol::Entry {
        clackatoo_protocol::Entry {
            stamp: Some(clackatoo_protocol::FileStamp {
                device: 1,
                inode: 2,
                len,
                seconds: 3,
                nanoseconds: 4,
            }),
            ..entry(name, false)
        }
    }

    #[test]
    fn a_folder_changed_since_its_listing_was_made_is_refused_and_steps_expect_what_was_listed() {
        use clackatoo_protocol::{EntryKind, Expect, FileOp};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_with(&mut desk, vec![stamped("a.txt", 1), stamped("b.txt", 1)]);
        type_keys(&mut desk, "edd");
        command(&mut desk, "w");
        assert_eq!(note(&desk), "1 change to make");
        // a.txt is written meanwhile.
        explore_with(&mut desk, vec![stamped("a.txt", 2), stamped("b.txt", 1)]);
        type_keys(&mut desk, "y");
        assert_eq!(file_ops(&mut desk), [], "nothing done");
        assert_eq!(
            note(&desk),
            "nothing was done: a folder changed since it was listed. :q! then e shows it afresh"
        );
        assert_eq!(listing_shown(&desk), ["..", "b.txt"], "back to the text");
        command(&mut desk, "q!");
        type_keys(&mut desk, "edd");
        command(&mut desk, "w");
        type_keys(&mut desk, "y");
        let expected = Expect {
            kind: EntryKind::File { executable: false },
            stamp: stamped("a.txt", 2).stamp,
        };
        let trashed = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: true,
            expect: Some(expected),
        };
        assert_eq!(file_ops(&mut desk), [(0, trashed)], "as it is now");
    }

    #[test]
    fn a_folder_made_again_under_its_name_is_refused_and_a_folders_steps_expect_it() {
        use clackatoo_protocol::{EntryKind, Expect, FileOp, FileStamp};
        let folder = |inode| clackatoo_protocol::Entry {
            stamp: Some(FileStamp {
                device: 1,
                inode,
                len: 0,
                seconds: 0,
                nanoseconds: 0,
            }),
            ..entry("src", true)
        };
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_with(&mut desk, vec![folder(7), entry("a.txt", false)]);
        type_keys(&mut desk, "edd");
        command(&mut desk, "w");
        // Deleted and made again meanwhile: another folder.
        explore_with(&mut desk, vec![folder(8), entry("a.txt", false)]);
        type_keys(&mut desk, "y");
        assert_eq!(file_ops(&mut desk), [], "nothing done");
        command(&mut desk, "q!");
        type_keys(&mut desk, "edd");
        command(&mut desk, "w");
        type_keys(&mut desk, "y");
        let trashed = FileOp::Delete {
            path: "/p/src".into(),
            trash: true,
            expect: Some(Expect {
                kind: EntryKind::Dir,
                stamp: folder(8).stamp,
            }),
        };
        assert_eq!(file_ops(&mut desk), [(0, trashed)]);
    }

    #[test]
    fn names_no_line_can_say_are_left_out_and_left_alone() {
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        let entries = vec![
            entry("ok.txt", false),
            entry("two\nlines", false),
            entry("\u{10FFFD}marked", false),
        ];
        explore_with(&mut desk, entries);
        type_keys(&mut desk, "e");
        assert_eq!(listing_shown(&desk), ["..", "ok.txt"]);
        assert_eq!(
            note(&desk),
            "2 names with line breaks or stray marks are left out, untouched"
        );
        type_keys(&mut desk, "dd");
        command(&mut desk, "w");
        assert_eq!(
            explorer_rows(&mut desk),
            [
                "Changes to ~/p (2 names left out)",
                "  delete ok.txt, to the trash",
                "Apply this change?  y yes · n no"
            ]
        );
    }

    #[test]
    fn an_answer_to_another_operation_doesnt_count_as_a_step() {
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        // A delete from the tree, not yet answered.
        type_keys(&mut desk, "jdy");
        assert_eq!(file_ops(&mut desk).len(), 1);
        type_keys(&mut desk, "eOx");
        escape(&mut desk);
        command(&mut desk, "w");
        type_keys(&mut desk, "y");
        assert_eq!(file_ops(&mut desk).len(), 1);
        desk.file_op_done(0, Ok(()));
        assert_eq!(note(&desk), "making change 1 of 1: Esc stops after it");
        assert!(desk.explorer(PaneId(2)).unwrap().editing());
        desk.file_op_done(1, Ok(()));
        assert_eq!(note(&desk), "made the change");
    }

    #[test]
    fn a_stop_after_setting_something_aside_says_where_it_is() {
        use clackatoo_protocol::FileOpError;
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_with(
            &mut desk,
            vec![entry("a.txt", false), entry("b.txt", false)],
        );
        type_keys(&mut desk, "ecwb");
        escape(&mut desk);
        type_keys(&mut desk, "jcwa");
        escape(&mut desk);
        assert_eq!(listing_shown(&desk), ["..", "b.txt", "a.txt"]);
        command(&mut desk, "w");
        type_keys(&mut desk, "y");
        assert_eq!(file_ops(&mut desk).len(), 1);
        desk.file_op_done(0, Ok(()));
        assert_eq!(file_ops(&mut desk).len(), 1);
        desk.file_op_done(1, Err(FileOpError::Failed("the disk is full".into())));
        assert_eq!(
            note(&desk),
            "stopped after 1 of 3: the disk is full. a.txt is set aside as a.txt.moving: put it \
             back by hand. q goes back to the tree"
        );
    }

    /// What the listing in the focused explorer's editor shows, a line
    /// each, its marks hidden.
    fn listing_shown(desk: &Desk) -> Vec<String> {
        (editor_text(desk).lines())
            .map(|line| line[crate::explorer::listing::mark_len(line)..].to_owned())
            .collect()
    }

    /// The rows explorer 2 shows, without the blank ones.
    fn explorer_rows(desk: &mut Desk) -> Vec<String> {
        desk.settle_explorers();
        let (rows, _) = desk.explorer(PaneId(2)).expect("an explorer").scene();
        (rows.iter())
            .map(|row| row.text().trim_end().to_owned())
            .filter(|row| !row.is_empty())
            .collect()
    }

    /// What's sent to the daemon, but for how panes are sized and drawn.
    fn sent(desk: &mut Desk) -> Vec<ClientMessage> {
        (desk.take_outbox().into_iter())
            .filter(|message| {
                !matches!(
                    message,
                    ClientMessage::FrameDone { .. }
                        | ClientMessage::Request(Request::Resize { .. })
                )
            })
            .collect()
    }

    /// The line of its listing the focused editor's cursor is on.
    fn listing_line(desk: &Desk) -> usize {
        let (editor, copy) = desk.focused_editor().expect("an editor");
        copy.text().byte_to_line(editor.cursor_byte(copy))
    }

    #[test]
    fn e_edits_a_folder_as_text_and_w_shows_each_step_then_does_them_through_file_ops() {
        use clackatoo_protocol::FileOp;
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "je");
        // In an editor in the explorer's pane, on the line it was on.
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        assert_eq!(listing_line(&desk), 2);
        type_keys(&mut desk, "cwb");
        escape(&mut desk);
        type_keys(&mut desk, "onew/");
        escape(&mut desk);
        type_keys(&mut desk, "u");
        assert_eq!(
            listing_shown(&desk),
            ["..", "src/", "b.txt"],
            "undone, as in any editor"
        );
        ctrl(&mut desk, "r");
        assert_eq!(listing_shown(&desk), ["..", "src/", "b.txt", "new/"]);
        // The text is the window's alone: nothing of it is sent.
        assert_eq!(sent(&mut desk), []);
        command(&mut desk, "w");
        assert_eq!(note(&desk), "2 changes to make");
        assert_eq!(
            explorer_rows(&mut desk),
            [
                "Changes to ~/p",
                "  make   new/",
                "  rename a.txt to b.txt",
                "Apply 2 changes?  y yes · n no"
            ]
        );
        assert_eq!(file_ops(&mut desk), [], "not until it's said yes to");
        type_keys(&mut desk, "y");
        let made = FileOp::Create {
            path: "/p/new".into(),
            dir: true,
        };
        assert_eq!(file_ops(&mut desk), [(0, made)]);
        assert_eq!(note(&desk), "making change 1 of 2: Esc stops after it");
        desk.file_op_done(0, Ok(()));
        let moved = FileOp::Move {
            from: "/p/a.txt".into(),
            to: "/p/b.txt".into(),
            overwrite: None,
            expect: Some(file_expected()),
        };
        assert_eq!(file_ops(&mut desk), [(1, moved)], "one at a time");
        assert_eq!(explorer_rows(&mut desk)[1], "✓ make   new/");
        desk.file_op_done(1, Ok(()));
        assert_eq!(note(&desk), "made all 2 changes");
        assert!(desk.focused_editor().is_none(), "back to the tree");
        assert!(!desk.explorer(PaneId(2)).unwrap().editing());
    }

    #[test]
    fn the_plans_question_sits_under_its_steps_in_view_with_its_keys_in_bold() {
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "jedk");
        desk.fit_explorer(PaneId(2), clackatoo_protocol::Size::new(60, 20).unwrap());
        command(&mut desk, "w");
        desk.settle_explorers();
        let rows = |desk: &Desk| -> Vec<String> {
            let (rows, _) = desk.explorer(PaneId(2)).expect("an explorer").scene();
            (rows.iter())
                .map(|row| row.text().trim_end().to_owned())
                .collect()
        };
        // Under the steps, after a blank row, not at the pane's foot.
        let shown = rows(&desk);
        assert_eq!(
            shown[..5],
            [
                "Changes to ~/p",
                "  delete src and everything in it, to the trash",
                "  delete a.txt, to the trash",
                "",
                "Apply 2 changes?  y yes · n no"
            ]
        );
        let (scene, _) = desk.explorer(PaneId(2)).unwrap().scene();
        let row = scene[4];
        let bold = |col: usize| {
            (row.style(row.cells()[col]).attributes).contains(clackatoo_protocol::Attributes::BOLD)
        };
        let col = |text: &str| shown[4][..shown[4].find(text).unwrap()].chars().count();
        let (y, n) = (col("y yes"), col("n no"));
        assert!(bold(y) && bold(n) && !bold(y + 2) && !bold(0));
        // Asked there, it isn't asked in the bar too.
        assert_eq!(note(&desk), "2 changes to make");
        // In a pane too short for every step, the question keeps its place
        // in view, and the steps scroll above it.
        let short = clackatoo_protocol::Size::new(60, 4).unwrap();
        desk.fit_explorer(PaneId(2), short);
        desk.settle_explorers();
        assert_eq!(
            rows(&desk),
            [
                "Changes to ~/p",
                "  delete src and everything in it, to the trash",
                "",
                "Apply 2 changes?  y yes · n no"
            ]
        );
        type_keys(&mut desk, "j");
        desk.settle_explorers();
        assert_eq!(rows(&desk)[1], "  delete a.txt, to the trash");
        assert_eq!(rows(&desk)[3], "Apply 2 changes?  y yes · n no");
        // With no room for the question in the pane, the bar asks it.
        desk.fit_explorer(PaneId(2), clackatoo_protocol::Size::new(60, 3).unwrap());
        desk.settle_explorers();
        assert_eq!(note(&desk), "apply 2 changes? (y/n)");
        // Its keys are as they were: n goes back, having done nothing.
        type_keys(&mut desk, "n");
        assert_eq!(file_ops(&mut desk), []);
        assert_eq!(note(&desk), "nothing was done: back to the listing");
    }

    #[test]
    fn the_plans_question_names_its_keys_only_while_its_pane_has_the_focus() {
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "jedk");
        desk.fit_explorer(PaneId(2), clackatoo_protocol::Size::new(60, 20).unwrap());
        command(&mut desk, "w");
        let question = |desk: &mut Desk| -> (String, bool) {
            desk.settle_explorers();
            let (rows, _) = desk.explorer(PaneId(2)).expect("an explorer").scene();
            let row = rows[4];
            let bold = (0..row.cells().len()).any(|col| {
                (row.style(row.cells()[col]).attributes)
                    .contains(clackatoo_protocol::Attributes::BOLD)
            });
            (row.text().trim_end().to_owned(), bold)
        };
        assert_eq!(
            question(&mut desk),
            ("Apply 2 changes?  y yes · n no".into(), true)
        );
        // Away in the terminal, `y` goes there: the question says so.
        ctrl_b(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        assert_eq!(
            question(&mut desk),
            ("Apply 2 changes? (focus to answer)".into(), false)
        );
        type_keys(&mut desk, "y");
        assert_eq!(file_ops(&mut desk), [], "nothing done from elsewhere");
        escape(&mut desk);
        // Back, it asks with its keys again.
        ctrl_b(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(
            question(&mut desk),
            ("Apply 2 changes?  y yes · n no".into(), true)
        );
    }

    #[test]
    fn a_step_that_fails_stops_the_rest_and_says_what_was_done() {
        use clackatoo_protocol::{FileOp, FileOpError};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "jeddox");
        escape(&mut desk);
        command(&mut desk, "w");
        assert_eq!(note(&desk), "2 changes to make");
        // `n` goes back to the text, as it was left.
        type_keys(&mut desk, "n");
        assert_eq!(listing_shown(&desk), ["..", "src/", "x"]);
        command(&mut desk, "q");
        assert_eq!(
            note(&desk),
            "the listings have changes: :w makes them, :q! drops them"
        );
        command(&mut desk, "w");
        type_keys(&mut desk, "y");
        let ops = file_ops(&mut desk);
        assert_eq!(ops.len(), 1);
        desk.file_op_done(ops[0].0, Ok(()));
        let trashed = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: true,
            expect: Some(file_expected()),
        };
        let ops = file_ops(&mut desk);
        assert_eq!(ops[0].1, trashed);
        // No trash: it stops, and never deletes for good unasked.
        desk.file_op_done(ops[0].0, Err(FileOpError::NoTrash("no trash there".into())));
        assert_eq!(file_ops(&mut desk), []);
        assert_eq!(
            note(&desk),
            "stopped after 1 of 2: no trash there, so nothing was deleted: d in the tree \
             deletes for good. q goes back to the tree"
        );
        assert_eq!(
            explorer_rows(&mut desk),
            [
                "Changes to ~/p: stopped",
                "✓ make   x",
                "✗ delete a.txt, to the trash"
            ]
        );
        type_keys(&mut desk, "q");
        assert!(!desk.explorer(PaneId(2)).unwrap().editing());
        assert!(desk.focused_editor().is_none(), "the tree again");
    }

    #[test]
    fn what_cant_be_done_is_refused_at_its_line_and_q_bang_leaves() {
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "jeyyp");
        type_keys(&mut desk, "gg");
        command(&mut desk, "w");
        assert_eq!(
            note(&desk),
            "can't: two lines are both a.txt: a copy needs a name of its own"
        );
        assert_eq!(listing_line(&desk), 3, "counting the `..` line");
        assert_eq!(file_ops(&mut desk), []);
        command(&mut desk, "e!");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        type_keys(&mut desk, "dd");
        command(&mut desk, "q!");
        assert!(desk.focused_editor().is_none());
        assert_eq!(file_ops(&mut desk), []);
    }

    #[test]
    fn enter_and_minus_go_between_folders_so_a_line_cut_in_one_and_put_in_another_moves() {
        use clackatoo_protocol::{FileOp, FilesAction, FilesId, FilesUpdate};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "e");
        enter(&mut desk);
        // Listed once it's expanded.
        let expanded = (desk.take_outbox().into_iter()).any(|message| {
            matches!(message, ClientMessage::Files { action: FilesAction::Expand(dir), .. } if dir == "/p/src")
        });
        assert!(expanded);
        let listed = FilesUpdate::Listed {
            dir: "/p/src".into(),
            entries: vec![entry("main.rs", false)],
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
        assert_eq!(listing_shown(&desk), ["..", "main.rs"]);
        type_keys(&mut desk, "dd-");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        assert_eq!(listing_line(&desk), 1, "on the folder come from");
        type_keys(&mut desk, "p");
        assert_eq!(listing_shown(&desk), ["..", "src/", "main.rs", "a.txt"]);
        command(&mut desk, "w");
        assert_eq!(
            explorer_rows(&mut desk),
            [
                "Changes to ~/p",
                "  move   src/main.rs to main.rs",
                "Apply this change?  y yes · n no"
            ]
        );
        type_keys(&mut desk, "y");
        let moved = FileOp::Move {
            from: "/p/src/main.rs".into(),
            to: "/p/main.rs".into(),
            overwrite: None,
            expect: Some(file_expected()),
        };
        assert_eq!(file_ops(&mut desk), [(0, moved)]);
        // Enter on a file's line opens it beside, as in the tree.
        let mut desk = super::tests::desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "jej");
        enter(&mut desk);
        assert!(matches!(
            answer_all(&mut desk).as_slice(),
            [Request::Split { terminal: PaneTerminal::File(file), .. }] if file.path == "/p/a.txt"
        ));
    }

    /// Whether the focused editor's bar says its text is modified.
    fn modified(desk: &Desk) -> bool {
        editor_label(desk).contains("[+]")
    }

    #[test]
    fn dot_dot_tops_a_listing_goes_up_with_enter_and_comes_to_nothing() {
        use clackatoo_protocol::{FilesId, FilesUpdate};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "e");
        enter(&mut desk);
        let listed = FilesUpdate::Listed {
            dir: "/p/src".into(),
            entries: vec![entry("main.rs", false)],
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
        // On the first entry, not `..`.
        assert_eq!(listing_shown(&desk), ["..", "main.rs"]);
        assert_eq!(listing_line(&desk), 1);
        // Enter on `..` goes up, as `-` does, to the folder come from.
        type_keys(&mut desk, "gg");
        enter(&mut desk);
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        assert_eq!(listing_line(&desk), 1, "on src/");
        // Deleted, it's no change, and writing does nothing.
        type_keys(&mut desk, "ggdd");
        assert_eq!(listing_shown(&desk), ["src/", "a.txt"]);
        assert!(!modified(&desk));
        command(&mut desk, "w");
        assert_eq!(note(&desk), "nothing has changed");
        assert_eq!(file_ops(&mut desk), []);
        type_keys(&mut desk, "u");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        assert!(!modified(&desk));
        // An entry's change is a change, and undone, isn't.
        type_keys(&mut desk, "jx");
        assert!(modified(&desk));
        type_keys(&mut desk, "u");
        assert!(!modified(&desk));
        // Put three times more, it's still nothing; `>>` isn't done.
        type_keys(&mut desk, "ggyy3p>>");
        assert_eq!(
            listing_shown(&desk),
            ["..", "..", "..", "..", "src/", "a.txt"]
        );
        assert!(!modified(&desk));
        command(&mut desk, "w");
        assert_eq!(note(&desk), "nothing has changed");
        assert_eq!(file_ops(&mut desk), []);
        command(&mut desk, "e!");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        // Cut here and put in another listing, it moves nothing, and
        // leaving says nothing of changes.
        type_keys(&mut desk, "ggdd");
        enter(&mut desk);
        assert_eq!(listing_shown(&desk), ["..", "main.rs"]);
        type_keys(&mut desk, "p");
        assert_eq!(listing_shown(&desk), ["..", "main.rs", ".."]);
        assert!(!modified(&desk));
        command(&mut desk, "w");
        assert_eq!(note(&desk), "nothing has changed");
        assert_eq!(file_ops(&mut desk), []);
        // Gone back to, the listing it was cut from has it again.
        type_keys(&mut desk, "-");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        command(&mut desk, "q");
        assert!(
            !desk.explorer(PaneId(2)).unwrap().editing(),
            "back to the tree"
        );
        // Gone with others in a count, only the others go.
        type_keys(&mut desk, "e-");
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        type_keys(&mut desk, "ggddp");
        assert_eq!(listing_shown(&desk), ["src/", "..", "a.txt"]);
        type_keys(&mut desk, "2dd");
        command(&mut desk, "w");
        assert_eq!(note(&desk), "1 change to make");
        assert_eq!(
            explorer_rows(&mut desk),
            [
                "Changes to ~/p",
                "  delete a.txt, to the trash",
                "Apply this change?  y yes · n no"
            ]
        );
    }

    #[test]
    fn an_empty_folders_listing_is_dot_dot_alone_and_its_going_is_no_change() {
        use clackatoo_protocol::{FilesId, FilesUpdate};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "e");
        enter(&mut desk);
        let listed = FilesUpdate::Listed {
            dir: "/p/src".into(),
            entries: Vec::new(),
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
        assert_eq!(listing_shown(&desk), [".."]);
        assert_eq!(listing_line(&desk), 0, "on `..`, the only line");
        type_keys(&mut desk, "dd");
        assert!(!modified(&desk), "{:?}", editor_text(&desk));
        command(&mut desk, "w");
        assert_eq!(note(&desk), "nothing has changed");
        assert_eq!(file_ops(&mut desk), []);
        command(&mut desk, "q");
        assert!(
            !desk.explorer(PaneId(2)).unwrap().editing(),
            "back to the tree"
        );
    }

    #[test]
    fn indenting_a_listings_lines_is_refused_and_writing_does_nothing() {
        use crate::explorer::listing::mark_len;
        use clackatoo_protocol::{FilesId, FilesUpdate};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        answer_all(&mut desk);
        type_keys(&mut desk, "e");
        enter(&mut desk);
        // A name that starts with a space, which an indent after its mark
        // would make another, and `<` would take as an indent.
        let listed = FilesUpdate::Listed {
            dir: "/p/src".into(),
            entries: vec![
                entry(" foo", false),
                entry("lib", true),
                entry("main.rs", false),
            ],
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
        assert_eq!(listing_shown(&desk), ["..", " foo", "lib/", "main.rs"]);
        let text = editor_text(&desk);
        let mode = |desk: &Desk| desk.focused_editor().expect("an editor").0.mode;
        // `.` after each has no change to make again: none was made.
        for (keys, refused) in [
            ("gg>>", true),
            ("j>>", true),
            ("j.", false),
            ("gg<<", true),
            ("j<<", true),
            ("gg0Vj>", true),
            ("jVj<", true),
            ("j.", false),
            ("gg3>>", true),
        ] {
            type_keys(&mut desk, keys);
            assert_eq!(editor_text(&desk), text, "{keys}");
            if refused {
                assert_eq!(
                    note(&desk),
                    "a listing's lines can't be indented: a name may start with a space",
                    "{keys}"
                );
            }
            assert_eq!(mode(&desk), Mode::Normal, "{keys}");
        }
        // Each entry's mark still starts its line.
        let marked: Vec<bool> = text.lines().map(|line| mark_len(line) > 0).collect();
        assert_eq!(marked, [false, true, true, true]);
        assert!(!modified(&desk));
        command(&mut desk, "w");
        assert_eq!(note(&desk), "nothing has changed");
        assert_eq!(file_ops(&mut desk), []);
    }

    #[test]
    fn colon_explore_bang_edits_the_folder_of_the_editors_file() {
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        command(&mut desk, "Explore!");
        let asked = (desk.take_outbox().into_iter()).any(|message| {
            matches!(
                message,
                ClientMessage::Request(Request::SplitTab {
                    terminal: PaneTerminal::Files(_),
                    ..
                })
            )
        });
        assert!(asked);
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
        desk.workspaces_changed(one_tab(layout));
        desk.answered(Response::PaneCreated(PaneId(2)));
        answer_all(&mut desk);
        explore_p(&mut desk);
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(listing_shown(&desk), ["..", "main.rs"]);
        let (editor, _) = desk.focused_editor().expect("an editor");
        assert_eq!(editor.mode, Mode::Normal);
    }

    #[test]
    fn a_new_chat_keeps_claude_codes_task_tool_as_claudetask_says() {
        let size = Size::new(80, 24).unwrap();
        let task = |options: &Options| match new_pane(Kind::Agent, None, size, options) {
            PaneTerminal::Agent(new) => new.task,
            other => panic!("{other:?}"),
        };
        // Off unless it's turned on: one way to hand work on, for every
        // agent.
        let mut options = Options::default();
        assert!(!task(&options));
        options.claudetask = true;
        assert!(task(&options));
    }

    // --- Reviews of what agents changed ---------------------------------------

    /// The requests waiting, unanswered.
    fn asked_of(desk: &mut Desk) -> Vec<Request> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// An editor of `/p/a.rs`, whose second line chat 3's agent changed
    /// in its first turn, as agent hunk 5.
    fn editing_agents_change() -> Desk {
        use clackatoo_protocol::{AgentHunk, BufferUpdate, ChatName};
        let mut desk = editing("/p/a.rs", "a\nB\nc\n");
        let hunk = AgentHunk {
            id: 5,
            start: 2,
            end: 4,
            base: "b\n".into(),
            chat: 3,
            turn: 0,
            reviewed: false,
        };
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Agents {
                version: 0,
                hunks: vec![hunk],
                chats: vec![ChatName {
                    chat: TerminalId(3),
                    name: "Claude Code".into(),
                }],
            },
        );
        desk
    }

    #[test]
    fn space_g_keeps_or_rejects_the_agents_change_at_the_cursor() {
        use clackatoo_protocol::{Hunks, ReviewAction};
        let mut desk = editing_agents_change();
        // Not on it, there's nothing to keep.
        type_keys(&mut desk, " ga");
        assert!(asked_of(&mut desk).is_empty());
        assert_eq!(note(&desk), "there's no change of an agent's here");
        type_keys(&mut desk, "j ga");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Keep(Hunks::Ids(vec![5])))]
        );
        desk.answered(Response::Reviewed("Kept 1 hunk.".into()));
        assert_eq!(note(&desk), "Kept 1 hunk.");
        type_keys(&mut desk, " gx");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Reject {
                hunks: Hunks::Ids(vec![5]),
                force: false
            })]
        );
        desk.answered(Response::Error(clackatoo_protocol::Error::Review(
            "Couldn't reject, so nothing was: in a.rs, line 2 has changed since.".into(),
        )));
        assert_eq!(
            note(&desk),
            "Couldn't reject, so nothing was: in a.rs, line 2 has changed since."
        );
        // A whole turn, of the chat that made the line at the cursor.
        command(&mut desk, "ReviewReject! turn");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Reject {
                hunks: Hunks::Turn {
                    chat: TerminalId(3),
                    turn: 0
                },
                force: true
            })]
        );
    }

    #[test]
    fn space_c_comments_on_the_line_for_the_agent_and_review_send_sends_them() {
        use clackatoo_protocol::{CommentPlace, ReviewAction};
        let mut desk = editing_agents_change();
        type_keys(&mut desk, "j c");
        assert!(asked_of(&mut desk).is_empty(), "the palette asks first");
        let Status::Typing { label, .. } = desk.status() else {
            panic!("the palette");
        };
        assert_eq!(label.as_deref(), Some("Comment on a.rs:2"));
        type_keys(&mut desk, "why capitals?");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Comment {
                chat: TerminalId(3),
                place: CommentPlace {
                    path: "/p/a.rs".into(),
                    line: 2,
                    at: Some((BufferId(7), 0, 2)),
                },
                text: "why capitals?".into(),
            })]
        );
        command(&mut desk, "ReviewSend");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Send(TerminalId(3)))]
        );
    }

    #[test]
    fn colon_review_opens_a_review_beside_the_editor_whose_keys_ask_the_daemon() {
        use clackatoo_protocol::{
            Hunks, Made, NewReview, ReviewAction, ReviewFile, ReviewHunk, ReviewId, ReviewList,
            ReviewScope,
        };
        let mut desk = editing_agents_change();
        type_keys(&mut desk, "j");
        command(&mut desk, "Review turn 1");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::Review(NewReview {
                    chat: Some(TerminalId(3)),
                    scope: ReviewScope::Turn(Some(0)),
                    cwd: Some("/p".into()),
                }),
                on: None,
            }]
        );
        let layout = side_by_side(vec![
            file_pane(1, 7),
            Layout::Pane(LayoutPane {
                id: PaneId(2),
                content: Content::Review(ReviewId(1)),
                home: None,
                key: None,
            }),
        ]);
        desk.workspaces_changed(one_tab(layout));
        desk.answered(Response::PaneCreated(PaneId(2)));
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert!(
            answer_all(&mut desk).contains(&Request::AttachReview {
                review: ReviewId(1)
            }),
            "attached"
        );
        let list = ReviewList {
            chat: Some(TerminalId(3)),
            chat_name: "Claude Code".into(),
            scope: Some(ReviewScope::Turn(Some(0))),
            files: vec![ReviewFile {
                path: "/p/a.rs".into(),
                shown: "a.rs".into(),
                buffer: Some(BufferId(7)),
                hunks: vec![ReviewHunk {
                    line: 2,
                    before: vec!["a".into()],
                    base: "b\n".into(),
                    text: "B\n".into(),
                    after: vec!["c".into()],
                    agents: vec![5],
                    by: Some(Made {
                        chat: TerminalId(3),
                        turn: 0,
                    }),
                    reviewed: false,
                }],
            }],
            comments: Vec::new(),
            note: None,
            chats: Vec::new(),
        };
        assert!(desk.review_event(clackatoo_protocol::Event::Review {
            review: ReviewId(1),
            list,
        }));
        assert_eq!(desk.title(), Some("Review of turn 1"));
        assert_eq!(note(&desk), "a.rs  hunk 1 of 1");
        type_keys(&mut desk, "a");
        assert_eq!(
            asked_of(&mut desk),
            [Request::Review(ReviewAction::Keep(Hunks::Ids(vec![5])))]
        );
        // Enter goes to the line in the editor beside it.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(shown_pane(&desk), PaneId(1));
        let (editor, copy) = desk.focused_editor().unwrap();
        assert_eq!(editor.cursor_byte(copy), 2, "on the agent's line");
        // `<Space>gr` goes back to it, and from it, closes it.
        type_keys(&mut desk, " gr");
        assert_eq!(shown_pane(&desk), PaneId(2));
        type_keys(&mut desk, " gr");
        assert!(answer_all(&mut desk).ends_with(&[Request::ClosePane { pane: PaneId(2) }]));
    }
}
