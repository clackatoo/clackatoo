//! The protocol clackatoo clients use to talk to the daemon.
//!
//! A connection is a byte stream carrying [frames](frame): a length prefix, then one message
//! serialized with `postcard`. The client sends [`ClientMessage`]s and the
//! daemon sends [`ServerMessage`]s.
//!
//! The client's first message must be [`ClientMessage::Hello`], giving its
//! [`PROTOCOL_VERSION`]. A daemon that talks to clients of that version
//! (see [`wire::MIN_CLIENT`]) answers with [`ServerMessage::Hello`] giving
//! the older of the two versions, which the connection then runs at both
//! ways (see [`wire`]). One that doesn't answers with its own version and
//! closes the connection, as daemons older than protocol 73 do whenever
//! the versions differ. A client takes the answer only if it talks to
//! daemons of that version too (see [`wire::MIN_DAEMON`]). A client that
//! says a version past the daemon's is told the daemon's own, which is how
//! one asks which version a daemon speaks.
//!
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
//! A phone, or any browser, reaches the daemon through its web app, which
//! the daemon serves: the byte stream is then a WebSocket, each of whose
//! binary messages carries one or more whole frames.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is, as the web app does.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod board;
mod browser;
mod buffer;
mod chat;
pub mod corpus;
mod devices;
mod files;
mod find;
pub mod frame;
mod review;
mod view;
pub mod wire;
mod workspace;

use std::fmt;

pub use clackatoo_term::{
    Attributes, Block, Color, CursorShape, InputModes, LinkId, MAX_CLIPBOARD_TEXT, Match, Position,
    Selection, ShellState, Size, Style, Underline, Which,
};
use serde::{Deserialize, Serialize};

pub use board::{
    BoardAction, BoardAuthor, BoardEntry, BoardId, BoardLink, BoardScope, BoardSummary,
    BoardUpdate, OpenBoard, PostKind,
};
pub use browser::{BrowserFrame, BrowserStatus, BrowserUpdate};
pub use buffer::{
    Answer, Author, BufferId, BufferSnapshot, BufferUpdate, Change, CodeAction, CompletionItem,
    FileLocation, OpenFile, Query, Resume, ServerFeatures, Symbol, TextChange, Trigger,
};
pub use chat::{
    AgentSession, ChatAction, ChatChoices, ChatNode, ChatPatch, ChatState, ChatSummary, Choice,
    ChoiceKind, Diff, Item, Location, Outcome, Provider, ProviderModels, Question, ResumeSession,
    Sender, Setting, SettingChoice, SettingKind, Subagent, SubagentStatus, Switch, Tool,
    ToolContent, ToolKind, ToolStatus, Turn, TurnEnd, TurnFiles, TurnFilesState, Usage,
    turn_number, turn_numbered,
};
pub use clackatoo_text::{
    AgentHunk, Construct, Diagnostic, Edit, Hunk, Kind, LineIndent, Op, Piece, Severity, Span,
};
pub use corpus::corpus;
pub use devices::{Device, PairingAsk, PairingState};
pub use files::{
    Entry, EntryKind, Expect, FileOp, FileOpError, FileStamp, FilesAction, FilesId, FilesUpdate,
    GitMark, OpenFiles,
};
pub use find::{Done, Found, FoundBuffer, FoundLine, FoundSymbol, Search};
pub use review::{
    ChatName, CommentPlace, Hunks, Made, NewReview, ReviewAction, ReviewChat, ReviewComment,
    ReviewFile, ReviewHunk, ReviewId, ReviewList, ReviewScope,
};
pub use view::{
    CursorState, GridNode, GridState, HistoryLine, HistoryLines, Line, LinkTarget, Links, Node,
    NodeId, Patch, Run, View, ViewUpdate,
};
pub use workspace::{
    Axis, Content, Layout, MAX_NESTING, Pane, PaneId, PaneKind, PaneTerminal, Side, Tab, TabId,
    Workspace, WorkspaceId, Workspaces,
};

/// The protocol version this crate speaks. Bump it on any change to a
/// message, and keep the change readable by older peers (see [`wire`]).
pub const PROTOCOL_VERSION: u32 = 78;

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
        /// The client's number for the change, counting up for each
        /// buffer, by which the daemon says it's been made. A window keeps
        /// counting up across its connections.
        seq: u64,
        /// The change.
        change: Change,
    },
    /// Asks the language server of a buffer the client attached to about
    /// the text at a place, as the text is after every change the client
    /// has sent. Not answered as a request is, since the server may take
    /// its time: the answer comes as [`BufferUpdate::Answer`], in order
    /// with the buffer's edits.
    Query {
        /// The buffer.
        buffer: BufferId,
        /// The client's number for the query, which the answer gives back.
        id: u64,
        /// The version of the text `at` is in: the client's copy's, after
        /// every change it has sent has been made. The daemon moves `at`
        /// along with edits made since.
        version: u64,
        /// The place asked about, in bytes.
        at: usize,
        /// What's asked.
        query: Query,
    },
    /// Asks the language server of a buffer the client attached to to
    /// rename what's at a place, everywhere it's used, as
    /// [`Query`](Self::Query) asks about one. The daemon makes the edits
    /// it says, in buffers open as the user's, one undo step in each, and
    /// in files not open on disk, then answers with
    /// [`Answer::Renamed`] in a [`BufferUpdate::Answer`].
    Rename {
        /// The buffer.
        buffer: BufferId,
        /// The client's number for it, which the answer gives back.
        id: u64,
        /// The version of the text `at` is in, as for a query.
        version: u64,
        /// The place, in bytes.
        at: usize,
        /// The name to give it.
        name: String,
    },
    /// Asks the daemon to find something, such as the files in the project
    /// a pane is in. Not answered as a request is, since it may take a
    /// while: what's found comes in [`Event::Found`]s numbered `id`, until
    /// one says it's done.
    Find {
        /// The client's number for the search.
        id: u64,
        /// The pane whose project to look in: the one its file or its
        /// program's working directory is in. `None` looks in the home
        /// directory.
        pane: Option<PaneId>,
        /// What to find.
        search: Search,
    },
    /// Stops the search numbered `id`. Nothing more comes of it. Not
    /// answered.
    StopFinding {
        /// The search's number.
        id: u64,
    },
    /// Asks the daemon to stage the hunk at a place in a buffer the client
    /// attached to: to put just that hunk's lines in git's index, as `git
    /// add -p` would, as [`Query`](Self::Query) asks about a place. The
    /// answer comes as [`Answer::Staged`](crate::Answer::Staged) in a
    /// [`BufferUpdate::Answer`], after the buffer's new hunks.
    Stage {
        /// The buffer.
        buffer: BufferId,
        /// The client's number for it, which the answer gives back.
        id: u64,
        /// The version of the text `at` is in, as for a query.
        version: u64,
        /// A place on the hunk's lines, in bytes.
        at: usize,
    },
    /// Answers [`Event::ClipboardAsked`]: what's on the clipboard asked
    /// about, or `None` if the client won't say, or can't. Not answered.
    Clipboard {
        /// The terminal whose program asked.
        terminal: TerminalId,
        /// The ask's number.
        id: u64,
        /// What's on the clipboard.
        text: Option<String>,
    },
    /// Asks something of a file explorer the client attached to, such as
    /// to list a directory. Not answered: what comes of it comes in
    /// [`Event::Files`]s.
    Files {
        /// The explorer.
        files: FilesId,
        /// What to do.
        action: FilesAction,
    },
    /// Asks the daemon to make, move, copy or delete files. Not answered
    /// as a request is, since it may take a while: how it went comes as
    /// an [`Event::FileOp`] numbered `id`.
    FileOp {
        /// The client's number for it, which the answer gives back.
        id: u64,
        /// What to do.
        op: FileOp,
    },
    /// Does something to a board the client attached to, as the user:
    /// posts on it, or archives it. Not answered: what comes of it comes
    /// in [`Event::Board`]s.
    Board {
        /// The board.
        board: BoardId,
        /// What to do.
        action: BoardAction,
    },
    /// Whether the client's user is there: a window has the focus and was
    /// used lately, a phone shows the app. A chat followed by a client
    /// whose user is there needs no notification when its turn ends or
    /// it asks something. A client that never says isn't, and `here`
    /// lasts five minutes unless it's said again, as a window says it
    /// while it's used, about once a minute. Not answered.
    Present {
        /// Whether they are.
        here: bool,
    },
    /// Asks an agent for the sessions it kept that work in a directory,
    /// to go on with one (see [`NewAgent::resume`]): clackatoo's chats',
    /// and those started elsewhere. Not answered as a request is, since
    /// the agent may have to start first: they come in an
    /// [`Event::Sessions`] numbered `id`.
    ListSessions {
        /// The client's number for it, which the answer gives back.
        id: u64,
        /// The agent, by its name: `claude`.
        agent: String,
        /// The directory.
        cwd: String,
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
    /// A language the client asked to install with [`Request::Install`]
    /// is installed, as this says, or couldn't be, saying why.
    Installed {
        /// The language.
        language: String,
        /// What was installed, or why it couldn't be.
        outcome: Result<String, String>,
    },
    /// Some of what the client's [`ClientMessage::Find`] numbered `id`
    /// found.
    Found {
        /// The search's number.
        id: u64,
        /// What it found.
        found: Found,
    },
    /// The program in a terminal the client follows put text on a
    /// clipboard, with OSC 52, as Neovim and tmux can. Sent to one client
    /// following the terminal: the one that last typed into it, else the
    /// one that started following it last. A write may be dropped for a
    /// newer one to the same clipboard while the client is catching up.
    Clipboard {
        /// The terminal.
        terminal: TerminalId,
        /// Which clipboard.
        selection: Selection,
        /// The text, at most [`MAX_CLIPBOARD_TEXT`] bytes.
        text: String,
    },
    /// The program in a terminal the client follows asked what's on a
    /// clipboard, with OSC 52, and waits to be told. Asked of the client
    /// [`Clipboard`](Self::Clipboard) would go to: answer with
    /// [`ClientMessage::Clipboard`], with `None` to refuse, within two
    /// seconds, after which the program is told there's nothing.
    ClipboardAsked {
        /// The terminal.
        terminal: TerminalId,
        /// The ask's number, for the answer.
        id: u64,
        /// Which clipboard.
        selection: Selection,
    },
    /// A file explorer the client attached to has something to show.
    Files {
        /// The explorer.
        files: FilesId,
        /// What.
        update: FilesUpdate,
    },
    /// The client's [`ClientMessage::FileOp`] numbered `id` is done, or
    /// wasn't, saying why.
    FileOp {
        /// Its number.
        id: u64,
        /// How it went.
        outcome: Result<(), FileOpError>,
    },
    /// An agent asked to show the client a pane, for a client watching
    /// the workspaces: draw the eye to it, and for a file, put its cursor
    /// on `at`. Never a reason to move the client's keyboard focus: an
    /// agent could have it typing into a terminal it chose. The pane is
    /// in the workspaces sent before this.
    Show {
        /// The pane.
        pane: PaneId,
        /// Where in the pane's file to put its cursor.
        at: Option<FileLocation>,
    },
    /// A board the client attached to changed, or has something to say.
    Board {
        /// The board.
        board: BoardId,
        /// What.
        update: BoardUpdate,
    },
    /// A review the client attached to has a new list: all of it, in
    /// place of the last.
    Review {
        /// The review.
        review: ReviewId,
        /// What it lists.
        list: ReviewList,
    },
    /// A chat's page, which the client attached to, changed, or there's
    /// a new picture of it.
    Browser {
        /// The chat.
        chat: TerminalId,
        /// What.
        update: BrowserUpdate,
    },
    /// The chats changed, for a client watching them
    /// ([`Request::WatchChats`]): every one, in place of the last list.
    Chats(Vec<ChatSummary>),
    /// Pairing is now as this says, for a window watching the workspaces:
    /// the devices asking, waiting to be allowed, the question for its
    /// user, answered with [`Request::AnswerPairing`], and whether pairing
    /// with the password is locked, after too many wrong ones, until
    /// [`Request::UnlockPairing`]. Sent when a window starts watching
    /// too, if any are asking or it's locked. Never sent to a device on
    /// the web app.
    Pairing(PairingState),
    /// The sessions the client's [`ClientMessage::ListSessions`] numbered
    /// `id` asked for, newest first, or why the agent couldn't list them.
    Sessions {
        /// Its number.
        id: u64,
        /// The sessions, or why there are none.
        sessions: Result<Vec<AgentSession>, String>,
    },
    /// The program of a terminal the client is attached to has ended, so
    /// its pane is about to close, in the workspaces sent after this. A
    /// window keeps its mode for the pane it goes on to, as the user's own
    /// doing, as after `exit`; a pane closed by anyone else isn't.
    Exited {
        /// The terminal.
        terminal: TerminalId,
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
    /// Turns this connection into one an agent's tools are served on,
    /// for the chat `chat`, which the agent works for: clackatoo as an MCP
    /// server (the Model Context Protocol), which `clackatoo mcp` relays
    /// to the agent. Answered with [`Response::Done`], after which each
    /// way carries MCP's JSON-RPC messages, one per line, not frames. The
    /// client must wait for the answer before sending any. Only a
    /// connection to the daemon's Unix socket can carry them.
    CarryMcp {
        /// The chat.
        chat: TerminalId,
    },
    /// Starts sending the buffer: a snapshot, then each change to it (see
    /// [`BufferUpdate`]). Attaching twice sends a fresh snapshot.
    AttachBuffer {
        /// The buffer to follow.
        buffer: BufferId,
        /// The window attaching, as a number it chose at random when it
        /// started, the same on every connection it makes. The daemon
        /// remembers the last change each window sent to each buffer, and
        /// says in the snapshot, so a window that lost its connection with
        /// changes waiting knows which arrived (see
        /// [`BufferSnapshot::applied`]). A change's number (`seq` in
        /// [`ClientMessage::Edit`]) only ever goes up, for a window,
        /// across every buffer. Any other connection that attached as the
        /// same window is one it lost: the daemon takes no more changes
        /// from it.
        window: u64,
        /// What the window knew of the buffer before it lost a connection,
        /// to be brought up to date from: see [`BufferUpdate::Resumed`].
        resume: Option<Resume>,
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
    /// Installs a language, as a [`BufferUpdate::Offer`] offered: its
    /// grammar if `grammar`, and its language server `server`. Answered
    /// at once; the install goes on meanwhile, and
    /// [`Event::Installed`] says how it went.
    Install {
        /// The language.
        language: String,
        /// Install its grammar.
        grammar: bool,
        /// Install this language server, by its program's name.
        server: Option<String>,
    },
    /// Don't offer to install a language again: the answer to an offer was
    /// no. Kept on the daemon's machine.
    Decline {
        /// The language.
        language: String,
    },
    /// Shows `file` in `pane`, which shows a file or a folder, in place of
    /// what it shows, as Vim's `:e` does: a folder as its listing
    /// ([`Content::Folder`]). The file it showed closes if nothing else
    /// shows it and it has no changes to save.
    OpenHere {
        /// The pane.
        pane: PaneId,
        /// The file or folder to show: a relative path is from the
        /// directory the file the pane shows was opened from, or the folder
        /// it shows, as Vim's are from its working directory.
        file: OpenFile,
    },
    /// Starts sending a file explorer: its [`FilesUpdate::Root`], then what
    /// the client asks for with [`ClientMessage::Files`]. Attaching twice
    /// sends the root again.
    AttachFiles {
        /// The explorer.
        files: FilesId,
    },
    /// Stops sending a file explorer, collapsing everything the client
    /// expanded in it.
    DetachFiles {
        /// The explorer.
        files: FilesId,
    },
    /// Starts sending a board: the board whole, then each change to it
    /// (see [`BoardUpdate`]). Attaching twice sends it whole again.
    AttachBoard {
        /// The board.
        board: BoardId,
    },
    /// Stops sending a board.
    DetachBoard {
        /// The board.
        board: BoardId,
    },
    /// Puts a new pane along one edge of a tab, as long as the tab, as a
    /// file explorer goes down its left. Answered with
    /// [`Response::PaneCreated`].
    SplitTab {
        /// The tab.
        tab: TabId,
        /// Which edge.
        side: Side,
        /// What the new pane shows.
        terminal: PaneTerminal,
        /// Its share of the tab's width or height, in thousandths, from 1
        /// to 999. `None` gives it as much as each of the parts beside it
        /// has, as a split does.
        share: Option<u16>,
    },
    /// Reads the buffer's file again, as an edit by the user, which can be
    /// undone, if it has changed on disk since it was read or last written
    /// and the buffer has no changes not saved: what a window does with
    /// [`BufferUpdate::ChangedOnDisk`] as its `autoread` says. Otherwise
    /// it does nothing, and a buffer changed meanwhile has its followers
    /// told so in a [`BufferUpdate::Notice`].
    Reload {
        /// The buffer.
        buffer: BufferId,
    },
    /// Asks which release of clackatoo the daemon is, such as `2026.10.0`,
    /// so a window on another machine can say when the two differ.
    /// Answered with [`Response::Text`].
    Version,
    /// Starts sending a review: its list, then the list again whenever it
    /// changes, in [`Event::Review`]s. Attaching twice sends it again.
    AttachReview {
        /// The review.
        review: ReviewId,
    },
    /// Stops sending a review.
    DetachReview {
        /// The review.
        review: ReviewId,
    },
    /// Keeps or rejects what agents changed, or comments on it for the
    /// agent (see [`ReviewAction`]). Answered with [`Response::Reviewed`],
    /// saying how it went, or an [`Error::Review`] saying why nothing was
    /// done.
    Review(ReviewAction),
    /// Starts sending a chat's page: what it is, in a
    /// [`BrowserUpdate::Status`], and again whenever that changes, and
    /// pictures of it, at most `width` by `height` pixels, while it
    /// changes. Attaching again sends the status again, and pictures at
    /// the new size. Answered with [`Response::Done`]: a chat that has
    /// closed, or never was, is said to be in the status.
    AttachBrowser {
        /// The chat.
        chat: TerminalId,
        /// How wide the pictures may be, in pixels.
        width: u32,
        /// How tall the pictures may be, in pixels.
        height: u32,
    },
    /// Stops sending a chat's page.
    DetachBrowser {
        /// The chat.
        chat: TerminalId,
    },
    /// Asks for every chat, summed up, and for an [`Event::Chats`] whenever
    /// that changes. Answered with [`Response::Chats`].
    WatchChats,
    /// Asks for the devices paired with the daemon's web app. Answered
    /// with [`Response::Devices`].
    Devices,
    /// Unpairs a device: it can't connect again, and is cut off if it's
    /// connected. Answered with [`Response::Done`].
    RevokeDevice {
        /// The device's number.
        device: u64,
    },
    /// Answers the devices asking to pair: allows the one showing `code`,
    /// or with none, turns every one down. Refused from a device on the
    /// web app. Answered with [`Response::Text`], saying what was done.
    AnswerPairing {
        /// The code the device shows, as the user typed it.
        code: Option<String>,
    },
    /// Lets devices pair with the password again, after too many wrong
    /// ones locked it. Refused from a device on the web app. Answered with
    /// [`Response::Text`], saying what was done.
    UnlockPairing,
    /// Asks what a new chat beside `pane` could be started with: the
    /// agents and their models, and the directory it would work in; and
    /// for the chats in no pane, which could be shown instead. Answered
    /// with [`Response::ChatChoices`].
    ChatChoices {
        /// The pane the chat would go beside, or `None` for the home
        /// directory.
        pane: Option<PaneId>,
    },
    /// Sizes a terminal to the client for a while, as a phone does to
    /// show it at its own width, or with no size, puts it back. Only a
    /// client following the terminal may fit it ([`Error::NotFollowing`]
    /// otherwise). The size holds until the client puts it back, stops
    /// following the terminal, or goes, or another client types into it,
    /// or a minute goes by without the client asking for it again; the
    /// terminal then takes the size it had, or the latest any client asked
    /// for meanwhile, as [`Request::Resize`]s are kept for then rather
    /// than made. A client that fits a terminal fitted by another takes
    /// the fit over. With `renew`, it only asks for its fit again, to keep
    /// it: never a fit back once it's been put back, which is answered
    /// with [`Error::NotFitted`]. Answered with [`Response::Done`]. New in
    /// protocol 75: a client asks
    /// [`wire::daemon_has`] with [`wire::PHONE_TERMINALS`] before sending
    /// it.
    Fit {
        /// The terminal.
        terminal: TerminalId,
        /// Its size for now, or `None` to put it back.
        size: Option<Size>,
        /// Only keep the fit this client has, asking for it again: the
        /// size is left as it is.
        renew: bool,
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
    /// The agent, by its name: `claude`, `codex`, or one the daemon's
    /// `agents.toml` adds. `None` is the daemon's default, Claude Code.
    pub agent: Option<String>,
    /// What the agent may do in the workspace through clackatoo's own
    /// tools.
    pub tools: AgentTools,
    /// Whether a Claude Code session keeps its own Task tool, which runs
    /// subagents the user doesn't see inside one turn. Without it, the
    /// agent hands work on with clackatoo's `start_chat`, in chats the
    /// user can watch. Other agents have no such tool, and ignore it.
    pub task: bool,
    /// Whether the chat's Browser setting starts on: its agent gets
    /// tools that drive a browser of clackatoo's, in a profile of its own,
    /// never the user's. The user is asked before the first page opens.
    pub browser: bool,
    /// The model to start on, by its value or its name, as the agent
    /// offers it, or a part of the name only one model has. `None`, or
    /// one the agent doesn't offer, leaves it the agent's own choice.
    pub model: Option<String>,
    /// A session the agent kept, to go on with rather than start a new
    /// one, as [`ClientMessage::ListSessions`] lists them. Where the agent
    /// can, it tells the chat the conversation so far, which the chat
    /// shows as its turns; otherwise the chat says the turns before
    /// aren't shown. A session another chat has open already, or had
    /// before it moved to another agent, isn't opened twice: that chat
    /// goes in the pane instead, if it's in no pane and of this workspace
    /// or none; otherwise it's refused, with [`Error::TerminalInPane`] or
    /// [`Error::SessionOpen`].
    pub resume: Option<ResumeSession>,
}

/// What an agent may do through the tools clackatoo gives it, which show
/// the user things: open files, run commands in terminals beside the
/// chat, and read them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentTools {
    /// It gets none.
    Off,
    /// It may use them, but asks before it runs a command, types into a
    /// terminal, or shows what's outside the chat's project.
    #[default]
    Ask,
    /// It runs commands in terminals it started without asking, but still
    /// asks before it types into one it didn't start, or shows what's
    /// outside the chat's project.
    Allow,
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
    /// Answers [`Request::Review`]: what was done, to say in the bar.
    Reviewed(String),
    /// Answers [`Request::WatchChats`].
    Chats(Vec<ChatSummary>),
    /// Answers [`Request::Devices`]: the devices, in the order they paired.
    Devices(Vec<Device>),
    /// Answers [`Request::ChatChoices`].
    ChatChoices(ChatChoices),
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
    /// The pane doesn't show a file.
    #[error("pane {0} shows no file")]
    NotAFile(PaneId),
    /// No file explorer has this ID. It may have been closed.
    #[error("no file explorer {0}")]
    NoSuchFiles(FilesId),
    /// The connection can't serve an agent's tools: the chat has none, or
    /// the connection isn't to the Unix socket.
    #[error("can't serve an agent's tools: {0}")]
    NoTools(String),
    /// No board has this ID.
    #[error("no board {0}")]
    NoSuchBoard(BoardId),
    /// The boards can't be used, as this says: they're still being read,
    /// or another daemon keeps them, or they're turned off.
    #[error("{0}")]
    Boards(String),
    /// No review has this ID. It may have been closed.
    #[error("no review {0}")]
    NoSuchReview(ReviewId),
    /// What was asked of reviewing couldn't be done, and why: nothing was.
    #[error("{0}")]
    Review(String),
    /// No device paired with the web app has this number.
    #[error("no device {0}")]
    NoSuchDevice(u64),
    /// An answer to devices asking to pair couldn't be given, and why.
    #[error("{0}")]
    Pairing(String),
    /// The session asked to be resumed is open in this chat already, in
    /// another workspace: a session is never opened twice.
    #[error("chat {0}, in another workspace, has that session open")]
    SessionOpen(TerminalId),
    /// The daemon couldn't take the request: it came from a newer release
    /// of clackatoo than the daemon's, which it couldn't read, or from an
    /// older one, without something the daemon needs to do it safely.
    #[error("the daemon can't do that: it's from another release of clackatoo than this")]
    Unsupported,
    /// A failure this build doesn't know, from a newer daemon, saying
    /// what went wrong.
    #[error("{0}")]
    Other(String),
    /// The client asked something of a terminal it doesn't follow, which
    /// it must, as to fit it ([`Request::Fit`]). New in protocol 75.
    #[error("terminal {0} isn't followed: attach to it first")]
    NotFollowing(TerminalId),
    /// The client asked to keep a fit it no longer has: it was put back,
    /// as when a window typed into the terminal ([`Request::Fit`]). New in
    /// protocol 75.
    #[error("terminal {0} isn't fitted to this client any more")]
    NotFitted(TerminalId),
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
    /// The working directory its shell last reported (OSC 7), if it has.
    /// `None` from a daemon older than protocol 75.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::PHONE_TERMINALS }, _>",
        deserialize_with = "wire::since::<{ wire::PHONE_TERMINALS }, _, _>"
    )]
    pub cwd: Option<String>,
    /// The command its shell is running, as the shell's marks said, if
    /// one is. `None` from a daemon older than protocol 75.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::PHONE_TERMINALS }, _>",
        deserialize_with = "wire::since::<{ wire::PHONE_TERMINALS }, _, _>"
    )]
    pub command: Option<String>,
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
