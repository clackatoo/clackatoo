//! The protocol clackatoo clients use to talk to the daemon.
//!
//! A connection is a byte stream carrying [frames](frame): a length prefix, then one message
//! serialized with `postcard`. The client sends [`ClientMessage`]s and the
//! daemon sends [`ServerMessage`]s.
//!
//! The client's first message must be [`ClientMessage::Hello`]. The daemon
//! answers with [`ServerMessage::Hello`], giving its own
//! [`PROTOCOL_VERSION`], and closes the connection if the versions differ.
//! Both `Hello`s are variant 0 of their enums and must stay that way. That
//! keeps them decodable across versions, so a version mismatch is always
//! reported as one, not as a garbled message.
//!
//! A client that finds an older daemon (protocol 11 or later) can ask it to
//! upgrade in place, by opening a connection with
//! [`ClientMessage::Upgrade`] instead. Both it and
//! [`ServerMessage::NotUpgrading`] are variant 1 of their enums and must
//! stay that way too, for the same reason.
//!
//! After that, the daemon answers each [`Request`], in order, with exactly
//! one [`Response`]. In between, it sends [`Event`]s: changes to the views
//! the client has attached to. A client acknowledges each view update with
//! [`ClientMessage::FrameDone`] once it has drawn it, and the daemon sends
//! no more updates for that view until then. Changes in the meantime are
//! merged into the next update, so a client gets updates as fast as it can
//! draw them and no faster, and a flood of output costs it one update per
//! frame.
//!
//! On this machine, that byte stream is a Unix socket. A client on another
//! machine reaches the daemon over SSH, and the stream is a QUIC stream:
//! its connection to the daemon's socket turns into a carrier of QUIC
//! packets with [`Request::CarryQuic`], and the client then opens a QUIC
//! connection through it, whose first stream it talks over as usual.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod buffer;
mod chat;
pub mod frame;
mod view;
mod workspace;

use std::fmt;

pub use clackatoo_term::{
    Attributes, Block, Color, CursorShape, InputModes, Match, Position, Size, Style, Underline,
    Which,
};
use serde::{Deserialize, Serialize};

pub use buffer::{Author, BufferId, BufferSnapshot, BufferUpdate, Change, OpenFile};
pub use chat::{
    ChatAction, ChatNode, ChatPatch, ChatState, Choice, ChoiceKind, Diff, Item, Location, Outcome,
    Question, Setting, SettingChoice, SettingKind, Tool, ToolContent, ToolKind, ToolStatus, Turn,
    TurnEnd, Usage,
};
pub use clackatoo_text::{Edit, Kind, Op, Span};
pub use view::{
    CursorState, GridNode, GridState, HistoryLine, HistoryLines, Line, Node, NodeId, Patch, Run,
    View, ViewUpdate,
};
pub use workspace::{
    Axis, Content, Layout, Pane, PaneId, PaneTerminal, Side, Tab, TabId, Workspace, WorkspaceId,
    Workspaces,
};

/// The protocol version this crate speaks. Bump it on any incompatible
/// change to a message.
pub const PROTOCOL_VERSION: u32 = 22;

/// The first protocol version whose daemons can upgrade in place: see
/// [`ClientMessage::Upgrade`].
pub const UPGRADABLE_SINCE: u32 = 11;

/// Everything a client sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Opens the conversation. Must be the first message, and must stay
    /// variant 0 (see the crate docs).
    Hello {
        /// The client's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
    /// Instead of [`Hello`](Self::Hello), on a connection from this
    /// machine: asks the daemon to become `executable`, a newer build,
    /// keeping its terminals. It replaces its own process with the new
    /// build's, which picks up where it left off, and the connection
    /// closes as it does. Connect again to talk to the new build. It
    /// answers [`ServerMessage::NotUpgrading`] instead if it won't.
    ///
    /// Must stay variant 1 (see the crate docs).
    Upgrade {
        /// The client's [`PROTOCOL_VERSION`]. A daemon won't upgrade to an
        /// older one.
        protocol_version: u32,
        /// The path of the program to run, as bytes.
        executable: Vec<u8>,
    },
    /// A request, which the daemon answers with one [`Response`].
    Request(Request),
    /// The client has drawn the last view update for `terminal`, and is
    /// ready for the next. Not answered.
    FrameDone {
        /// The terminal whose view was drawn.
        terminal: TerminalId,
    },
    /// A change to a buffer the client attached to. Not answered as a
    /// request is: the daemon tells the client how it went with a
    /// [`BufferUpdate`]. Send the next change to a buffer once it has.
    Edit {
        /// The buffer.
        buffer: BufferId,
        /// The client's number for the change, counting up from 0 for each
        /// buffer, by which the daemon says it's been made.
        seq: u64,
        /// The change.
        change: Change,
    },
}

/// Everything the daemon sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Answers [`ClientMessage::Hello`]. Must stay variant 0 (see the crate
    /// docs).
    Hello {
        /// The daemon's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
    /// Answers [`ClientMessage::Upgrade`]: the daemon won't. Must stay
    /// variant 1 (see the crate docs).
    NotUpgrading {
        /// Why not.
        reason: String,
    },
    /// The answer to the oldest unanswered request.
    Response(Response),
    /// Something the client attached to changed.
    Event(Event),
}

/// Something the daemon tells a client without being asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// The view of a terminal the client attached to changed.
    View {
        /// The terminal.
        terminal: TerminalId,
        /// The change.
        update: ViewUpdate,
    },
    /// The workspaces changed, for a client watching them. Sent to the
    /// client whose request changed them before that request's answer.
    Workspaces(Workspaces),
    /// A buffer the client attached to changed.
    Buffer {
        /// The buffer.
        buffer: BufferId,
        /// The change.
        update: BufferUpdate,
    },
}

/// A message from a client to the daemon that expects an answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Lists every terminal.
    ListTerminals,
    /// Starts a terminal. Answered with [`Response::TerminalCreated`].
    NewTerminal(NewTerminal),
    /// Asks a chat with an agent to do something: send a message, stop
    /// the agent answering, or answer its question.
    Chat {
        /// The chat.
        terminal: TerminalId,
        /// What to do.
        action: ChatAction,
    },
    /// Writes bytes to a terminal, as if typed.
    SendInput {
        /// The terminal to write to.
        terminal: TerminalId,
        /// The bytes, already encoded as a terminal expects them (`\r` for
        /// Enter, `\x1b[A` for Up…).
        bytes: Vec<u8>,
    },
    /// Asks for what a terminal is showing. Answered with [`Response::View`].
    Capture {
        /// The terminal to capture.
        terminal: TerminalId,
    },
    /// Closes a terminal. Its program gets a hangup signal, as when a
    /// terminal window closes.
    CloseTerminal {
        /// The terminal to close.
        terminal: TerminalId,
    },
    /// Changes a terminal's size. Its program is told, and the screen's
    /// lines are rewrapped to the new width.
    Resize {
        /// The terminal to resize.
        terminal: TerminalId,
        /// The new size.
        size: Size,
    },
    /// Starts sending the terminal's view: a snapshot, then a patch
    /// whenever it changes (see the crate docs). Attaching twice is
    /// harmless and sends a fresh snapshot.
    Attach {
        /// The terminal to follow.
        terminal: TerminalId,
    },
    /// Stops sending the terminal's view.
    Detach {
        /// The terminal to stop following.
        terminal: TerminalId,
    },
    /// Asks for up to `count` lines of the terminal's history, ending just
    /// before line number `end`. Answered with [`Response::History`], which
    /// leaves out lines no longer held.
    History {
        /// The terminal.
        terminal: TerminalId,
        /// One past the newest line wanted.
        end: u64,
        /// How many lines, at most.
        count: u16,
    },
    /// Asks for the text between two places in the terminal's document:
    /// its history, then its screen (see [`HistoryLines`] for how lines are
    /// numbered). Answered with [`Response::Text`].
    Text {
        /// The terminal.
        terminal: TerminalId,
        /// One end, included.
        from: Position,
        /// The other end, included.
        to: Position,
        /// Take every line whole, ignoring the columns.
        whole_lines: bool,
    },
    /// Searches the terminal's document for `query`, from just after
    /// `from` (or just before it, if `backward`), wrapping around the
    /// ends. Answered with [`Response::Found`].
    Search {
        /// The terminal.
        terminal: TerminalId,
        /// The text to look for.
        query: String,
        /// Where to start.
        from: Position,
        /// Search towards the start of the document.
        backward: bool,
        /// Match case exactly; otherwise case is ignored. The client
        /// decides, by its options, such as Vim's `smartcase`.
        exact: bool,
    },
    /// Finds the block (the prompt, command and output of one command,
    /// as the shell marked them) at the document's line `line`, or the one
    /// before or after it. Answered with [`Response::Block`].
    Block {
        /// The terminal.
        terminal: TerminalId,
        /// The line, numbered as [`Request::Text`] numbers them.
        line: u64,
        /// The line's own block, or the one before or after.
        which: Which,
    },
    /// Asks for every workspace, and for an [`Event::Workspaces`] whenever
    /// they change. Answered with [`Response::Workspaces`].
    WatchWorkspaces,
    /// Makes a workspace with one tab, showing `terminal`. Without a name,
    /// it's named with the lowest number no workspace has. Answered with
    /// [`Response::PaneCreated`].
    NewWorkspace {
        /// Its name, unique among the daemon's workspaces.
        name: Option<String>,
        /// What its pane shows.
        terminal: PaneTerminal,
    },
    /// Adds a tab to a workspace, after its others, showing `terminal`.
    /// Answered with [`Response::PaneCreated`].
    NewTab {
        /// The workspace.
        workspace: WorkspaceId,
        /// What the tab's pane shows.
        terminal: PaneTerminal,
    },
    /// Splits a pane in two, putting a new pane on `side` of it. Answered
    /// with [`Response::PaneCreated`].
    Split {
        /// The pane to split.
        pane: PaneId,
        /// Where the new pane goes.
        side: Side,
        /// What the new pane shows.
        terminal: PaneTerminal,
    },
    /// Closes a pane and the terminal it shows. A tab goes with its last
    /// pane, and a workspace with its last tab. A pane also closes when its
    /// terminal's program exits.
    ClosePane {
        /// The pane to close.
        pane: PaneId,
    },
    /// Records that a client switched to this pane: its tab becomes its
    /// workspace's active tab, it becomes that tab's focused pane, and the
    /// workspace becomes the active one.
    Focus {
        /// The pane switched to.
        pane: PaneId,
    },
    /// Renames a workspace.
    RenameWorkspace {
        /// The workspace.
        workspace: WorkspaceId,
        /// Its new name, unique among the daemon's workspaces.
        name: String,
    },
    /// Names a tab, or with an empty name, takes its name away, so clients
    /// name it after its focused pane again.
    RenameTab {
        /// The tab.
        tab: TabId,
        /// Its new name.
        name: String,
    },
    /// Closes a tab, and every pane in it and its terminal. A workspace
    /// goes with its last tab.
    CloseTab {
        /// The tab.
        tab: TabId,
    },
    /// Closes a workspace, and every tab and pane in it and their
    /// terminals.
    CloseWorkspace {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// Shares a split's space between its parts anew, as dragging the
    /// dividers between them does.
    ResizeSplit {
        /// The tab the split is in.
        tab: TabId,
        /// Where the split is in the tab's layout: see [`Layout::at`].
        split: Vec<u16>,
        /// A share for each of its parts, none 0. Only their ratios matter.
        shares: Vec<u32>,
    },
    /// Turns this connection into a carrier of QUIC packets, for a client
    /// on another machine reaching the daemon over SSH. Answered with
    /// [`Response::Carrying`], after which each frame either way holds one
    /// QUIC packet, not a message. The client must wait for the answer
    /// before sending any, and should send this only once its `Hello` is
    /// answered: an older daemon may hang up on a request it can't decode
    /// without answering a `Hello` that came with it. Only a connection to
    /// the daemon's Unix socket can carry QUIC.
    CarryQuic,
    /// Starts sending the buffer: a snapshot, then each change to it (see
    /// [`BufferUpdate`]). Attaching twice sends a fresh snapshot.
    AttachBuffer {
        /// The buffer to follow.
        buffer: BufferId,
    },
    /// Stops sending the buffer.
    DetachBuffer {
        /// The buffer to stop following.
        buffer: BufferId,
    },
    /// Writes the buffer to its file. Refused if the file has changed since
    /// it was read or last written, unless `force`.
    Save {
        /// The buffer.
        buffer: BufferId,
        /// Write over changes made to the file since.
        force: bool,
    },
    /// Throws away the buffer's changes that aren't saved, reading its file
    /// again: an edit by the user, which can be undone.
    Discard {
        /// The buffer.
        buffer: BufferId,
    },
}

/// How to start a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTerminal {
    /// The terminal's size.
    pub size: Size,
    /// The program and its arguments. `None` runs the daemon's `$SHELL`.
    pub command: Option<Vec<String>>,
    /// The working directory. `None` uses the daemon's home directory.
    pub cwd: Option<String>,
    /// A file to record the terminal's output to, for tests and benchmarks
    /// (see `clackatoo_term::recording`). The path is on the daemon's machine.
    pub record_to: Option<String>,
}

/// How to start a chat with an agent, such as Claude Code, in a pane.
///
/// A chat has a terminal's ID, and goes in panes as terminals do, but its
/// view is a [`ChatNode`], not a grid, and it's sent messages with
/// [`Request::Chat`], not input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAgent {
    /// The terminal's size.
    pub size: Size,
    /// The directory the agent works in. `None` uses the one the pane
    /// beside it is in, as for a new terminal.
    pub cwd: Option<String>,
}

/// The daemon's answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// The request failed.
    Error(Error),
    /// The request succeeded and has nothing to return.
    Done,
    /// Answers [`Request::ListTerminals`].
    Terminals(Vec<TerminalInfo>),
    /// Answers [`Request::NewTerminal`].
    TerminalCreated(TerminalId),
    /// Answers [`Request::Capture`].
    View(View),
    /// Answers [`Request::History`].
    History(HistoryLines),
    /// Answers [`Request::Text`].
    Text(String),
    /// Answers [`Request::Search`]: where the text is, if anywhere.
    Found(Option<Match>),
    /// Answers [`Request::Block`]: the block, if there is one.
    Block(Option<Block>),
    /// Answers [`Request::WatchWorkspaces`].
    Workspaces(Workspaces),
    /// Answers requests that make a pane.
    PaneCreated(PaneId),
    /// Answers [`Request::CarryQuic`]: the connection now carries QUIC
    /// packets.
    Carrying {
        /// The DER-encoded certificate the daemon's QUIC endpoint presents,
        /// for the client to accept and no other. The client learned it
        /// over SSH, which logged it in to this machine.
        certificate: Vec<u8>,
        /// Where the daemon's QUIC endpoint takes packets over UDP, if it
        /// does. A client that reaches it there may move its connection off
        /// the carrier onto UDP, where it survives the client's network
        /// changing. New connections only ever come through a carrier.
        udp: Option<Udp>,
    },
}

/// Where a daemon takes QUIC packets over UDP, and how a client finds out
/// whether they get through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Udp {
    /// The port, at whatever address SSH reached the daemon's machine.
    pub port: u16,
    /// What a client puts in a probe, a datagram that isn't QUIC, for the
    /// daemon to send it straight back: proof that UDP gets through both
    /// ways before any of the connection's packets go that way. A probe
    /// without it gets no answer, so the port gives nothing away to a scan.
    pub probe_key: [u8; 16],
}

/// Why the daemon refused or failed a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum Error {
    /// The connection's first message wasn't [`ClientMessage::Hello`].
    #[error("the first message must be Hello")]
    HelloRequired,
    /// No terminal has this ID. It may have been closed.
    #[error("no terminal {0}")]
    NoSuchTerminal(TerminalId),
    /// The terminal's program has exited, so it takes no more input.
    #[error("terminal {0} has exited")]
    TerminalExited(TerminalId),
    /// The terminal's program isn't reading its input, and too much is
    /// already queued for it.
    #[error("terminal {0} is not reading its input; too much is already queued")]
    InputBacklogFull(TerminalId),
    /// The terminal could not be started.
    #[error("could not start the terminal: {0}")]
    SpawnFailed(String),
    /// The output recording could not be set up.
    #[error("could not record to {path}: {reason}")]
    RecordingFailed {
        /// The requested recording path.
        path: String,
        /// What went wrong.
        reason: String,
    },
    /// No pane has this ID. It may have been closed.
    #[error("no pane {0}")]
    NoSuchPane(PaneId),
    /// No workspace has this ID. It may have been closed.
    #[error("no workspace {0}")]
    NoSuchWorkspace(WorkspaceId),
    /// No tab has this ID. It may have been closed.
    #[error("no tab {0}")]
    NoSuchTab(TabId),
    /// The tab has no split there, or not with that many parts.
    #[error("tab {0} has no such split")]
    NoSuchSplit(TabId),
    /// A part's share of a split can't be 0.
    #[error("every part of a split needs some of its space")]
    EmptyShare,
    /// The terminal is a chat with an agent, which has no grid and takes
    /// no input: it's sent messages with [`Request::Chat`].
    #[error("terminal {0} is a chat with an agent")]
    IsAChat(TerminalId),
    /// The terminal isn't a chat with an agent.
    #[error("terminal {0} isn't a chat")]
    NotAChat(TerminalId),
    /// The terminal is already shown in a pane.
    #[error("terminal {0} is already in a pane")]
    TerminalInPane(TerminalId),
    /// Another workspace has this name.
    #[error("there's already a workspace called {0:?}")]
    NameTaken(String),
    /// The name can't be a workspace's.
    #[error("{name:?} can't be a workspace's name: {reason}")]
    BadName {
        /// The name asked for.
        name: String,
        /// What's wrong with it.
        reason: String,
    },
    /// The connection can't carry QUIC: it isn't to the Unix socket, or
    /// the daemon couldn't set up its QUIC endpoint.
    #[error("can't carry QUIC: {0}")]
    CantCarry(String),
    /// No buffer has this ID. It may have been closed.
    #[error("no buffer {0}")]
    NoSuchBuffer(BufferId),
    /// The file couldn't be read, or isn't text.
    #[error("can't open {path}: {reason}")]
    OpenFailed {
        /// The path asked for.
        path: String,
        /// Why not.
        reason: String,
    },
    /// The file couldn't be written.
    #[error("can't write {path}: {reason}")]
    SaveFailed {
        /// The file's path.
        path: String,
        /// Why not.
        reason: String,
    },
    /// The file has changed since the buffer read it or last wrote it.
    #[error("{0} has changed since it was read")]
    ChangedOnDisk(String),
}

/// Identifies a terminal within one daemon. IDs are never reused while
/// the daemon runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TerminalId(pub u32);

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A summary of one terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// The terminal's ID.
    pub id: TerminalId,
    /// The process ID of the program the terminal started. A chat with an
    /// agent has none: the daemon writes it.
    pub pid: Option<u32>,
    /// The terminal's size.
    pub size: Size,
    /// The window title the program set, or empty.
    pub title: String,
    /// Whether the program is still running.
    pub status: TerminalStatus,
}

/// Whether a terminal's program is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStatus {
    /// The program is running.
    Running,
    /// The program exited with this status code.
    Exited(i32),
    /// The program was killed by this signal.
    Killed(i32),
}

impl fmt::Display for TerminalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Exited(code) => write!(f, "exited ({code})"),
            Self::Killed(signal) => write!(f, "killed (signal {signal})"),
        }
    }
}
