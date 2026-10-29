//! Versions on the wire: how a client and a daemon of different releases
//! still talk.
//!
//! Each connection runs at one protocol version, its *wire* version: the
//! older of the two sides', agreed in the `Hello`s (see the crate docs).
//! Everything sent on it is encoded as a build of that version would, and
//! read as one would write it. [`frame::encode`](crate::frame::encode) and
//! [`FrameDecoder::decode`](crate::frame::FrameDecoder::decode) take the
//! wire version for that, and hold it in a thread-local while they run,
//! for the serde helpers here to read.
//!
//! The messages are postcard: positional, with no field names and no
//! lengths inside a message, so a peer can skip nothing it doesn't know.
//! Changes are kept readable both ways by these rules:
//!
//! - **New messages, variants and fields go at the end.** Nothing is
//!   reordered, removed or reshaped: a reshape is a new variant, and the
//!   old one stays handled while any peer that sends it can connect.
//! - **A new field is gated** with [`before`] and [`since`], at a named
//!   gate below, so it's left out for an older peer and filled in with its
//!   `Default` from one:
//!
//!   ```ignore
//!   #[serde(
//!       skip_serializing_if = "wire::before::<{ wire::REVIEW_CHATS }, _>",
//!       deserialize_with = "wire::since::<{ wire::REVIEW_CHATS }, _, _>"
//!   )]
//!   pub chats: Vec<ReviewChat>,
//!   ```
//!
//!   The default must mean "an older peer", never something it didn't say:
//!   an `Option` with `None`, or a value the daemon fills in for the
//!   connection. `#[serde(default)]` does nothing under postcard.
//! - **A field that makes a request safer** isn't defaulted: the daemon
//!   refuses the request from older peers with `Error::Unsupported`, and a
//!   newer client asks [`daemon_has`] before sending it, as an older daemon
//!   would read past the field and do without it.
//! - **An answer is never refused** ([`Downgraded::Refused`]) to a peer
//!   that has the fallbacks: refusing a reply drops the client, which
//!   connects again into the same refusal, for ever. Give it an arm that
//!   turns what's newer into what it can read.
//! - **A new variant is never sent to a peer older than it.** Each variant
//!   of the enums the daemon pushes inside its state says when it arrived
//!   ([`Since`], which doesn't compile until it does), and what's pushed
//!   is turned, for an older peer, into what it can read, by [`Downgrade`]:
//!   a new pane kind into [`Content::Other`], a new error into
//!   [`Error::Other`]. A peer from before [`FALLBACKS`] has no `Other`:
//!   what's for it is dropped, or refused, never put in an `Other`. A new
//!   request is only sent once [`daemon_has`] says the daemon reads it.
//! - **The serde types of `clackatoo_term` and `clackatoo_text` are
//!   protocol types** too: a variant added to one needs a downgrade, or
//!   closes the window.
//! - **Nothing per cell or per line is gated** (`Line`, `Run`,
//!   `HistoryLine`, `Style`, `Color`, `Underline`, `Edit`'s ops): a change
//!   there closes the window, by raising the floors to the new version.
//! - **A change of meaning on the same bytes** is gated, the daemon
//!   keeping the old meaning for older peers, or closes the window.
//! - **Every new type, field or variant gets a sample** in
//!   [`corpus`](crate::corpus) that reaches it, and the last variant of
//!   every enum is sampled, so the release fixtures pin it.
//!
//! [`Content::Other`]: crate::Content::Other
//! [`Error::Other`]: crate::Error::Other

use std::cell::Cell;

use serde::{Deserialize, Deserializer};

use crate::{
    Answer, BufferUpdate, ChatPatch, ChoiceKind, ClientMessage, ConnectionState, Content, Done,
    Error, Event, FilesUpdate, Found, Item, Node, PROTOCOL_VERSION, Patch, Response, ServerMessage,
    Setting, SettingKind, ToolContent, Turn, ViewUpdate, Workspaces,
};

/// The oldest client a daemon of this build talks to.
///
/// Moved only when a release is made, and never past the previous
/// release's protocol.
pub const MIN_CLIENT: u32 = 72;

/// The oldest daemon a client of this build talks to.
///
/// Moved only when a release is made, and never past the previous
/// release's protocol. **Never below [`FALLBACKS`]:** a daemon older than
/// that refuses a client by answering `Hello` with its own version and
/// hanging up, which a client that took that version would read as
/// agreement. It would then find the connection closed, and a remote
/// window would connect again into the same refusal for ever, never
/// saying the daemon needs updating.
pub const MIN_DAEMON: u32 = 73;

const _: () = assert!(MIN_DAEMON >= FALLBACKS, "see MIN_DAEMON");
const _: () = assert!(
    FALLBACKS > crate::corpus::FIRST,
    "the corpus began at the last release without the fallbacks"
);
const _: () = assert!(MIN_CLIENT <= PROTOCOL_VERSION && MIN_DAEMON <= PROTOCOL_VERSION);

/// Declares the gates: the protocol versions things arrived in, by name,
/// and [`GATES`], the list of them.
macro_rules! gates {
    ($($(#[$doc:meta])* $name:ident = $version:literal,)*) => {
        $($(#[$doc])* pub const $name: u32 = $version;)*

        /// Every gate, by name.
        pub const GATES: &[(&str, u32)] = &[$((stringify!($name), $name),)*];
    };
}

gates! {
    /// The fallback variants a daemon turns what's newer into
    /// (`Content::Other`, `Node::Other`, `Item::Other`,
    /// `ToolContent::Other`, `Error::Unsupported`, `Error::Other`), and
    /// reading past frames that can't be read rather than hanging up.
    /// Renumbered with the protocol version it lands in, if that changes
    /// on a rebase, and `MIN_DAEMON` with it: never below it.
    FALLBACKS = 73,
    /// `Event::Exited`: a terminal's program ended, said before its pane
    /// closes, so a window can tell that from a pane closed elsewhere.
    EXITED = 74,
    /// Terminals from a phone: `Request::Fit`, and `TerminalInfo`'s
    /// `cwd` and `command`.
    PHONE_TERMINALS = 75,
    /// `Answer::Hover`'s `range`, the text the server says it's about, and
    /// `BufferUpdate::Coloured`, how the code in its fenced blocks is
    /// coloured, sent after it.
    HOVER = 76,
    /// Symbols, and checking a rename first: `Query::Symbols` and
    /// `Answer::Symbols`, a file's; `Search::Symbols` and
    /// `Found::Symbols`, a project's; `Query::PrepareRename`,
    /// `Answer::Renamable` and `ServerFeatures`' `prepare_rename`.
    SYMBOLS = 77,
    /// `BufferUpdate::Language`: the characters a buffer's words are made
    /// of besides letters, digits and `_`, as its language says.
    WORDS = 78,
    /// The other uses of a name, and formatting part of a file:
    /// `Query::Uses` and `Answer::Uses`; `Query::FormatRange`; and
    /// `ServerFeatures`' `uses`.
    USES = 79,
    /// Inlay hints: `Query::InlayHints` and `Answer::InlayHints`
    /// (`InlayHint`, `InlayHintKind`), and `ServerFeatures`'
    /// `inlay_hints`.
    INLAY_HINTS = 80,
    /// An account's limits, as its agent says them: `Usage`'s `five_hour`
    /// and `weekly` (`Limit`).
    LIMITS = 81,
    /// Forwarding a port from a window's machine to the daemon's:
    /// `Request::Forward` (`Forward`), on a QUIC stream of its own, and
    /// `Error::Forward` (`ForwardFailure`).
    FORWARDS = 82,
    /// Forwarded connections whose window asks for them to go as fast as
    /// they can, rather than as background traffic: `Forward`'s
    /// `unpaced`.
    FORWARD_PACE = 83,
    /// Colouring the code in agents' words: `ClientMessage::Colour`
    /// (`Code`), answered with `Event::Coloured`.
    COLOURS = 84,
    /// A preview's colours, as the file's buffer is highlighted:
    /// `Found::Preview`'s `spans`.
    PREVIEW_COLOURS = 85,
    /// Where shells' blocks start and how they ended, for windows to mark
    /// failed commands by: `GridNode`'s, `Patch::Grid`'s and
    /// `HistoryLines`' `marks` (`BlockMarks`); and the daemon's home
    /// directory, for windows to write paths from `~` by: `Workspaces`'
    /// `home`.
    BLOCK_MARKS = 86,
    /// Copying a block of a terminal's columns: `Request::BlockText`.
    BLOCKS = 87,
    /// Buffers with no file yet: `PaneTerminal::Blank` (`OpenBlank`),
    /// `Request::BlankHere`, and `Request::SaveAs`, which names one.
    BLANK = 88,
    /// Buffers' changes not saved that came back from recovery files:
    /// `Request::Recover`.
    RECOVER = 89,
    /// Workspaces kept from one run of the daemon to the next:
    /// `Workspaces`' `restored` (`Restored`, `Place`), and
    /// `ClientMessage::Place` and `ClientMessage::Dismiss`.
    RESTORED = 90,
    /// Which daemon is which, and daemons connecting to each other:
    /// `Workspaces`' `machine` (`Machine`), `Workspace`'s and `Pane`'s
    /// `home` (`MachineId`); `Request::Connect`, `Request::Disconnect`
    /// and `Request::WatchConnections`, answered with
    /// `Response::Connections` and followed by `Event::Connections`
    /// (`Connection`, `ConnectionState`); and the questions ssh asks as
    /// it logs in, `Event::Login`, answered with `ClientMessage::Login`
    /// (`Secret`).
    FEDERATION = 91,
    /// Everything of another machine's working through the daemon:
    /// `Request::Relay`, which a daemon relaying for its own clients
    /// sends; the pane `ClientMessage::FileOp` is asked from, and the
    /// buffer `Request::Install` and `Request::Decline` answer an offer
    /// of, which say on which machine; `Request::Forwards` (`Forwarding`),
    /// with `Event::Forwarded`; and panes other than terminals from
    /// another machine in the workspaces, which an older peer is sent as
    /// `Content::Other`, as it would do what's asked of them on the
    /// daemon's own machine; and `BufferUpdate::Reattach`.
    RELAY = 92,
    /// Panes on another machine than their workspace's, and new ones made
    /// on a machine named: `Request::NewWorkspace`'s, `Request::NewTab`'s,
    /// `Request::Split`'s and `Request::SplitTab`'s `on`; the panes a
    /// daemon lends a daemon connected to it, `Request::Lend`, and the
    /// workspaces holding them, `Workspace`'s `lent` (`Lent`), which only
    /// the daemon that asked for them is shown, once it says which machine
    /// it is, `Request::RelayAs`; giving one back, `Request::Unlend`, and
    /// listing them, `Request::Lent`; and `Event::Said`, for the bar.
    LEND = 93,
    /// Layouts holding panes from machines their daemon can't reach: a
    /// pane another machine puts in one, `PaneTerminal::Lent`, which the
    /// workspace lending it says it did, `Lent`'s `placed`; the number a
    /// pane has on its machine, `Pane`'s `key`, by which any daemon that
    /// reaches that machine shows it; and what the machines a daemon isn't
    /// connected to are called, `Workspaces`' `others` (`OtherMachine`).
    MIXED = 94,
    /// Machines passed on through the daemons connected to them: what a
    /// daemon passes on, `Workspaces`' `passed` (`Passed`), with the key it
    /// knows each by; a stream carrying a daemon's connection on to one of
    /// them, `Request::Reach`; the QUIC certificate signed by the
    /// machine's identity key, `Response::Carrying`'s `signed` (`Signed`);
    /// forgetting a machine's key, `Request::Forget`; a connection not to
    /// pass on, `Request::Connect`'s `share`; and how each connection goes,
    /// `Connection`'s `through` and `unshared`.
    CHAIN = 95,
    /// Background tabs: tabs a workspace keeps running out of its tab
    /// bar, `Workspace`'s `background` (`Background`, `BackgroundState`);
    /// what's asked of them, `Request::Background` (`BackgroundAction`);
    /// and the ports their programs listen on, `Event::Listening`
    /// (`Listening`, `ListeningPort`). And terminals kept running,
    /// `Workspaces`' `kept` (`Kept`), and marking them, `Request::Keep`
    /// (`Keeping`).
    BACKGROUND = 96,
    /// Clipboard images sent with a chat message.
    CHAT_IMAGES = 97,
    /// Small image previews in drafts and retained turns.
    CHAT_PREVIEWS = 98,
    /// Image thumbnails returned by tools, inside tool content.
    TOOL_IMAGES = 99,
    /// Original chat images fetched on demand by opaque handle.
    CHAT_ORIGINALS = 100,
    /// Atomic provider handoff with its first prompt.
    CHAT_HANDOFF = 101,
    /// Daemon-owned pending user messages and explicit steering.
    CHAT_QUEUE = 102,
    /// Cross-provider subscription usage snapshots and read-only refresh.
    PROVIDER_USAGE = 103,
    /// Opt-in recent chat snapshots and older history pages.
    CHAT_HISTORY = 104,
    /// Managed agent activities and explicit window access.
    HARNESS = 105,
    /// Undo steps that keep where the cursor was and the last selection,
    /// and steps that change nothing, as Vim's undo header keeps them:
    /// `Change::Step`, `Change::Empty` and `Change::Reverse`
    /// (`StepState`, `StepPlace`, `StepVisual`, `StepSelection`), and
    /// `BufferUpdate::Reversed`. And `Change::Undo` and `Change::Redo`
    /// pass over steps that change nothing.
    UNDO_STATE = 106,
    /// What's of another machine the daemon is connected to: listing its
    /// agent's past sessions, for the start page of a pane there,
    /// `ClientMessage::ListSessions`' `on`; and background tabs holding a
    /// pane borrowed from it, a shell it starts with a command,
    /// `Request::Lend`'s `command`, and reports on, `Lent`'s `background`
    /// (`LentBackground`), and brings back or says was seen,
    /// `Request::LentBackground` (`LentBackgroundAction`).
    ELSEWHERE = 107,
    /// A terminal's search for the match a count says, in one answer:
    /// `Request::Search`'s `count`, answered with `Response::Searched`.
    SEARCH_COUNT = 108,
    /// The profile a chat an agent started was started as, for windows to
    /// mark its pane by: `ChatSummary`'s `profile`.
    SUBAGENT_PROFILE = 109,
    /// What an agent runs in the background of a chat: `ChatNode`'s and
    /// `Subagent`'s `tasks` (`AgentTask`, `AgentTaskKind`,
    /// `AgentTaskState`), `ChatPatch::Tasks`, and stopping one,
    /// `ChatAction::StopTask`.
    AGENT_TASKS = 110,
    /// Turns an agent takes by itself, with no message: `Turn`'s `woke`.
    AGENT_TURNS = 111,
    /// The agent's plan, its to-do list: `ChatNode`'s `plan` (`PlanStep`,
    /// `PlanState`) and `ChatPatch::Plan`.
    AGENT_PLAN = 112,
    /// What an agent asks the user to fill in or open (ACP's elicitation):
    /// `Question`'s `form` (`Form`, `FormAsk`, `Field`, `FieldInput`,
    /// `FieldOption`) and answering it, `ChatAction::Fill` (`FormReply`,
    /// `FieldValue`); and the plan a question about leaving planning
    /// carries, `Question`'s `plan`.
    ELICITATION = 113,
    /// The slash commands the agent offers: `ChatNode`'s `commands`
    /// (`AgentCommand`) and `ChatPatch::Commands`.
    AGENT_COMMANDS = 114,
    /// What an agent says of a turn beyond its words: that it's trying
    /// again, `ChatNode`'s `retrying` and `ChatPatch::Retrying`; that it
    /// compacted its context, `Item::Compacted`; a command's exit code,
    /// `Tool`'s `exit`; a failure in the agent's own words, `TurnEnd`'s
    /// `raw`, and what a turn spent, its `spent` (`Spent`, `Cost`); what
    /// a session cost, `Usage`'s `cost`; an answer of a kind the daemon
    /// doesn't know, `ChoiceKind::Other`; and sending a failed message
    /// again, `ChatAction::Resend`.
    CHAT_FACTS = 115,
    /// Whether a chat's last turn went wrong, as the list of chats says
    /// it: `ChatSummary`'s `failed`. (116 and 117 are taken by work in
    /// flight; renumber on a rebase.)
    TURN_FAILED = 118,
}

/// The oldest protocol version any peer of this build can speak: older
/// gates and fixtures are of no more use.
pub const OLDEST: u32 = if MIN_CLIENT < MIN_DAEMON {
    MIN_CLIENT
} else {
    MIN_DAEMON
};

thread_local! {
    /// The wire version of what's being encoded or decoded on this thread.
    static WIRE: Cell<u32> = const { Cell::new(PROTOCOL_VERSION) };
}

/// The wire version of what's being encoded or decoded on this thread, or
/// this build's own outside of that.
#[must_use]
pub fn current() -> u32 {
    WIRE.get()
}

/// Runs `f` with [`current`] at `wire`, putting it back after, even if `f`
/// panics.
pub(crate) fn speaking<R>(wire: u32, f: impl FnOnce() -> R) -> R {
    /// Puts the version back when dropped.
    struct Restore(u32);
    impl Drop for Restore {
        fn drop(&mut self) {
            WIRE.set(self.0);
        }
    }
    let _restore = Restore(WIRE.replace(wire));
    f()
}

/// For `skip_serializing_if`: whether what's encoded is for a peer older
/// than version `V`, which hasn't the field.
#[must_use]
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
pub fn before<const V: u32, T: ?Sized>(_field: &T) -> bool {
    current() < V
}

/// For `deserialize_with`: reads the field if what's read is from a peer
/// of version `V` or later, and otherwise, reading nothing, takes its
/// default.
///
/// # Errors
///
/// If the field is there and can't be read.
pub fn since<'de, const V: u32, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    if current() >= V {
        T::deserialize(deserializer)
    } else {
        Ok(T::default())
    }
}

/// The wire version a daemon of this build runs a connection at, for a
/// client that said it speaks `client`, or `None` if it's too old.
#[must_use]
pub fn for_client(client: u32) -> Option<u32> {
    (client >= MIN_CLIENT).then(|| client.min(PROTOCOL_VERSION))
}

/// Whether a client of this build can run a connection at `reply`, the
/// version a daemon answered its `Hello` with. A daemon newer than
/// [`MIN_CLIENT`] allows answers with its own, newer version when it
/// refuses the client; a daemon older than [`MIN_DAEMON`] can't be talked
/// to.
#[must_use]
pub fn takes_reply(reply: u32) -> bool {
    (MIN_DAEMON..=PROTOCOL_VERSION).contains(&reply)
}

/// Whether a connection at `wire` has what arrived at `gate`: whether
/// the daemon on the other end reads a request new then, say, before a
/// window offers what needs it.
#[must_use]
pub fn daemon_has(wire: u32, gate: u32) -> bool {
    wire >= gate
}

/// Older than any peer of this build can be: what every one has.
pub const EARLIER: u32 = 0;

/// The protocol version a variant arrived in, of the enums whose variants
/// are pushed to a client inside the daemon's state, where a peer older
/// than the variant can't be sent it: [`EARLIER`] for what every peer has.
///
/// Each implementation matches every variant, so a variant added to one
/// of these enums doesn't compile until it says when it arrived; and then
/// [`newest`] finds it, and [`Downgrade`] keeps it from older peers.
pub trait Since {
    /// The protocol version it arrived in.
    fn since(&self) -> u32;
}

impl Since for Content {
    fn since(&self) -> u32 {
        match self {
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Folder(_) => EARLIER,
            Self::Other { .. } => FALLBACKS,
        }
    }
}

impl Since for Node {
    fn since(&self) -> u32 {
        match self {
            Self::Grid(_) | Self::Chat(_) => EARLIER,
            Self::Other => FALLBACKS,
        }
    }
}

impl Since for Item {
    fn since(&self) -> u32 {
        match self {
            Self::Words(_) | Self::Thought(_) | Self::Tool(_) => EARLIER,
            Self::Other(_) => FALLBACKS,
            Self::Compacted(_) => CHAT_FACTS,
        }
    }
}

impl Since for ToolContent {
    fn since(&self) -> u32 {
        match self {
            Self::Text(_) | Self::Diff(_) | Self::Output(_) => EARLIER,
            Self::Other(_) => FALLBACKS,
            Self::Image(_) => TOOL_IMAGES,
        }
    }
}

impl Since for ChatPatch {
    fn since(&self) -> u32 {
        match self {
            Self::Turn { .. }
            | Self::Item { .. }
            | Self::Append { .. }
            | Self::Output { .. }
            | Self::State(_)
            | Self::Question(_)
            | Self::Notice(_)
            | Self::Settings(_)
            | Self::Usage(_)
            | Self::Files { .. }
            | Self::Agent(_)
            | Self::Providers(_)
            | Self::Subagents(_) => EARLIER,
            Self::Delivery(_) => CHAT_QUEUE,
            Self::ProviderUsage(_) => PROVIDER_USAGE,
            Self::History { .. } => CHAT_HISTORY,
            Self::Activities(_) => HARNESS,
            Self::Tasks(_) => AGENT_TASKS,
            Self::Plan(_) => AGENT_PLAN,
            Self::Commands(_) => AGENT_COMMANDS,
            Self::Retrying(_) => CHAT_FACTS,
        }
    }
}

impl Since for ChoiceKind {
    fn since(&self) -> u32 {
        match self {
            Self::AllowOnce | Self::AllowAlways | Self::RejectOnce | Self::RejectAlways => EARLIER,
            Self::Other => CHAT_FACTS,
        }
    }
}

impl Since for BufferUpdate {
    fn since(&self) -> u32 {
        match self {
            Self::Snapshot(_)
            | Self::Resumed { .. }
            | Self::Acked { .. }
            | Self::Edited { .. }
            | Self::NoChange(_)
            | Self::Modified(_)
            | Self::Highlights { .. }
            | Self::Diagnostics { .. }
            | Self::Offer { .. }
            | Self::Answer { .. }
            | Self::Hunks { .. }
            | Self::Features(_)
            | Self::Moved(_)
            | Self::Notice(_)
            | Self::ChangedOnDisk
            | Self::Agents { .. } => EARLIER,
            Self::Coloured { .. } => HOVER,
            Self::Language { .. } => WORDS,
            Self::Reattach => RELAY,
            Self::Reversed { .. } => UNDO_STATE,
        }
    }
}

impl Since for Answer {
    fn since(&self) -> u32 {
        match self {
            Self::Locations(_)
            | Self::Hover { .. }
            | Self::Completions { .. }
            | Self::Resolved { .. }
            | Self::Signature { .. }
            | Self::CodeActions(_)
            | Self::Acted { .. }
            | Self::Renamed { .. }
            | Self::Nothing
            | Self::Formatted { .. }
            | Self::Failed(_)
            | Self::Pieces(_)
            | Self::Indents(_)
            | Self::Staged => EARLIER,
            Self::Symbols { .. } | Self::Renamable { .. } => SYMBOLS,
            Self::Uses(_) => USES,
            Self::InlayHints { .. } => INLAY_HINTS,
        }
    }
}

impl Since for Found {
    fn since(&self) -> u32 {
        match self {
            Self::Root { .. }
            | Self::Files(_)
            | Self::Lines(_)
            | Self::Buffers(_)
            | Self::Boards(_)
            | Self::Preview { .. }
            | Self::Done(_) => EARLIER,
            Self::Symbols(_) => SYMBOLS,
        }
    }
}

impl Since for FilesUpdate {
    fn since(&self) -> u32 {
        match self {
            Self::Root { .. }
            | Self::Listed { .. }
            | Self::Unlisted { .. }
            | Self::Git(_)
            | Self::NoGit(_) => EARLIER,
        }
    }
}

impl Since for SettingKind {
    fn since(&self) -> u32 {
        match self {
            Self::Mode | Self::Model | Self::Effort | Self::Tools | Self::Other | Self::Browser => {
                EARLIER
            }
        }
    }
}

impl Since for ConnectionState {
    fn since(&self) -> u32 {
        match self {
            Self::Connecting
            | Self::Asking
            | Self::NeedsPassword
            | Self::Connected { .. }
            | Self::Down { .. }
            | Self::Stopped { .. }
            | Self::Other => FEDERATION,
        }
    }
}

impl Since for Event {
    fn since(&self) -> u32 {
        match self {
            Self::View { .. }
            | Self::Workspaces(_)
            | Self::Buffer { .. }
            | Self::Installed { .. }
            | Self::Found { .. }
            | Self::Clipboard { .. }
            | Self::ClipboardAsked { .. }
            | Self::Files { .. }
            | Self::FileOp { .. }
            | Self::Show { .. }
            | Self::Board { .. }
            | Self::Review { .. }
            | Self::Browser { .. }
            | Self::Chats(_)
            | Self::Pairing(_)
            | Self::Sessions { .. } => EARLIER,
            Self::Exited { .. } => EXITED,
            Self::Coloured { .. } => COLOURS,
            Self::Connections(_) | Self::Login { .. } => FEDERATION,
            Self::Forwarded(_) => RELAY,
            Self::Said(_) => LEND,
            Self::Listening(_) => BACKGROUND,
            Self::NativeRequest { .. } | Self::NativeGrantAck { .. } => HARNESS,
        }
    }
}

impl Since for Error {
    fn since(&self) -> u32 {
        match self {
            Self::HelloRequired
            | Self::NoSuchTerminal(_)
            | Self::TerminalExited(_)
            | Self::InputBacklogFull(_)
            | Self::SpawnFailed(_)
            | Self::RecordingFailed { .. }
            | Self::NoSuchPane(_)
            | Self::NoSuchWorkspace(_)
            | Self::NoSuchTab(_)
            | Self::NoSuchSplit(_)
            | Self::EmptyShare
            | Self::IsAChat(_)
            | Self::NotAChat(_)
            | Self::TerminalInPane(_)
            | Self::NameTaken(_)
            | Self::BadName { .. }
            | Self::CantCarry(_)
            | Self::NoSuchBuffer(_)
            | Self::OpenFailed { .. }
            | Self::SaveFailed { .. }
            | Self::ChangedOnDisk(_)
            | Self::NotAFile(_)
            | Self::NoSuchFiles(_)
            | Self::NoTools(_)
            | Self::NoSuchBoard(_)
            | Self::Boards(_)
            | Self::NoSuchReview(_)
            | Self::Review(_)
            | Self::NoSuchDevice(_)
            | Self::Pairing(_)
            | Self::SessionOpen(_) => EARLIER,
            Self::Unsupported | Self::Other(_) => FALLBACKS,
            Self::NotFollowing(_) | Self::NotFitted(_) => PHONE_TERMINALS,
            Self::Forward(_) => FORWARDS,
        }
    }
}

/// The newest protocol version any variant in `message` arrived in, of
/// those [`Since`] tags, and so the oldest peer that can be sent it as it
/// is.
#[must_use]
pub fn newest(message: &ServerMessage) -> u32 {
    match message {
        ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. } => EARLIER,
        ServerMessage::Response(response) => match response {
            Response::Error(err) => err.since(),
            Response::Workspaces(workspaces) => newest_in_workspaces(workspaces),
            Response::View(view) => newest_in_node(&view.root),
            Response::Searched { .. } => SEARCH_COUNT,
            _ => EARLIER,
        },
        ServerMessage::Event(event) => {
            let inside = match event {
                Event::View { update, .. } => match update {
                    ViewUpdate::Snapshot(view) => newest_in_node(&view.root),
                    ViewUpdate::Patch(patches) => (patches.iter())
                        .map(|patch| match patch {
                            Patch::Chat(patch) => newest_in_patch(patch),
                            Patch::Title(_) | Patch::Cwd(_) | Patch::Grid { .. } => EARLIER,
                        })
                        .max()
                        .unwrap_or(EARLIER),
                },
                Event::Workspaces(workspaces) => newest_in_workspaces(workspaces),
                Event::Buffer { update, .. } => match update {
                    BufferUpdate::Answer { answer, .. } => update.since().max(answer.since()),
                    update => update.since(),
                },
                Event::Files { update, .. } => update.since(),
                Event::Found { found, .. } => found.since(),
                Event::Connections(connections) => (connections.iter())
                    .map(|connection| connection.state.since())
                    .max()
                    .unwrap_or(EARLIER),
                _ => EARLIER,
            };
            event.since().max(inside)
        }
    }
}

fn newest_in_workspaces(workspaces: &Workspaces) -> u32 {
    let background = (workspaces.list.iter()).flat_map(|workspace| &workspace.background);
    let contents = (workspaces.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .chain(background.map(|background| &background.tab))
        .flat_map(|tab| tab.layout.panes())
        .map(|pane| pane.content.since())
        .max()
        .unwrap_or(EARLIER);
    // A workspace from another machine, which a peer from before
    // federation would take for one of the daemon's own.
    let homes = if workspaces
        .list
        .iter()
        .any(|workspace| workspace.home.is_some())
    {
        FEDERATION
    } else {
        EARLIER
    };
    // A pane from another machine that isn't a terminal, which a peer
    // from before the relay would act on as the daemon's own.
    let relayed = if (workspaces.list.iter())
        .flat_map(|workspace| &workspace.tabs)
        .flat_map(|tab| tab.layout.panes())
        .any(|pane| relayed_elsewhere(&pane))
    {
        RELAY
    } else {
        EARLIER
    };
    // A workspace lent to another daemon, which only one of protocol 93
    // or later asks for, and an older peer would show as one of the
    // daemon's own.
    let lent = if (workspaces.list.iter()).any(|workspace| workspace.lent.is_some()) {
        LEND
    } else {
        EARLIER
    };
    // A pane of the daemon's own in a workspace from another machine, or
    // one on a machine the daemon can't reach, whose home an older peer
    // would misread.
    let own = if own_elsewhere(workspaces).next().is_some() {
        MIXED
    } else {
        EARLIER
    };
    contents.max(homes).max(relayed).max(lent).max(own)
}

/// The panes, by their IDs, whose homes an older peer would misread:
/// those of the daemon's own in `workspaces`' workspaces from other
/// machines, lent to them, which have a key; and those on a machine the
/// daemon isn't connected to (`others`), which it would wait for as one
/// it's connecting to.
fn own_elsewhere(workspaces: &Workspaces) -> impl Iterator<Item = crate::PaneId> + '_ {
    let own = workspaces.machine.as_ref().map(|machine| machine.id);
    let other = |home: Option<crate::MachineId>| {
        home.is_some_and(|home| workspaces.others.iter().any(|other| other.id == home))
    };
    (workspaces.list.iter())
        .flat_map(move |workspace| {
            let elsewhere = workspace.home.is_some();
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter(move |pane| {
                    (elsewhere && own.is_some() && pane.home == own && pane.key.is_some())
                        || other(pane.home)
                })
        })
        .map(|pane| pane.id)
}

/// `message`, if it's the workspaces and some panes' homes are ones an
/// older peer would misread (see [`own_elsewhere`]), with those said to be
/// on their workspace's machine: what a peer from before mixed layouts is
/// sent. It would take a home of the daemon's own for yet another
/// machine's, and wait for one the daemon can't reach. Asked about
/// through the daemon, as all a window asks is, such a pane still works,
/// or says it can't.
fn with_own_homes_unsaid(message: &ServerMessage) -> Option<ServerMessage> {
    let (ServerMessage::Event(Event::Workspaces(workspaces))
    | ServerMessage::Response(Response::Workspaces(workspaces))) = message
    else {
        return None;
    };
    let panes: Vec<crate::PaneId> = own_elsewhere(workspaces).collect();
    if panes.is_empty() {
        return None;
    }
    let mut shown = workspaces.clone();
    for tab in shown
        .list
        .iter_mut()
        .flat_map(|workspace| &mut workspace.tabs)
    {
        tab.layout.for_each_pane_mut(&mut |pane| {
            if panes.contains(&pane.id) {
                pane.home = None;
            }
        });
    }
    Some(match message {
        ServerMessage::Event(_) => ServerMessage::Event(Event::Workspaces(shown)),
        _ => ServerMessage::Response(Response::Workspaces(shown)),
    })
}

/// `message`, if it's the workspaces and some are lent to another daemon,
/// without those: what a peer from before lending is sent, should one be,
/// though none ever asks for them.
fn without_lent(message: &ServerMessage) -> Option<ServerMessage> {
    let (ServerMessage::Event(Event::Workspaces(workspaces))
    | ServerMessage::Response(Response::Workspaces(workspaces))) = message
    else {
        return None;
    };
    if workspaces
        .list
        .iter()
        .all(|workspace| workspace.lent.is_none())
    {
        return None;
    }
    let mut own = workspaces.clone();
    own.list.retain(|workspace| workspace.lent.is_none());
    if (own.active).is_some_and(|active| own.get(active).is_none()) {
        own.active = None;
    }
    Some(match message {
        ServerMessage::Event(_) => ServerMessage::Event(Event::Workspaces(own)),
        _ => ServerMessage::Response(Response::Workspaces(own)),
    })
}

/// Whether `pane` is on another machine than the daemon's, and shows what
/// only a peer of the relay's version or later asks about as it should:
/// anything but a terminal, or a pane already shown as one to come.
fn relayed_elsewhere(pane: &crate::Pane) -> bool {
    pane.home.is_some() && !matches!(pane.content, Content::Terminal(_) | Content::Other { .. })
}

/// `message`, if it's the workspaces and some of their panes from other
/// machines aren't terminals, with those shown as panes to come
/// ([`Content::Other`], naming their kind): what a peer from before the
/// relay is sent. It would otherwise ask, of a file explorer there, to
/// move or delete files with no word of the pane, which the daemon would
/// do on its own machine, and answer offers to install a language there
/// on it too.
fn with_placeholders(message: &ServerMessage) -> Option<ServerMessage> {
    let (ServerMessage::Event(Event::Workspaces(workspaces))
    | ServerMessage::Response(Response::Workspaces(workspaces))) = message
    else {
        return None;
    };
    let mut shown = workspaces.clone();
    let mut changed = false;
    for tab in shown
        .list
        .iter_mut()
        .flat_map(|workspace| &mut workspace.tabs)
    {
        tab.layout.for_each_pane_mut(&mut |pane| {
            if relayed_elsewhere(pane) {
                pane.content = Content::Other {
                    kind: crate::PaneKind::new(pane.content.kind_name()),
                };
                changed = true;
            }
        });
    }
    changed.then_some(match message {
        ServerMessage::Event(_) => ServerMessage::Event(Event::Workspaces(shown)),
        _ => ServerMessage::Response(Response::Workspaces(shown)),
    })
}

/// `message`, if it's the workspaces and some are from other machines,
/// without those: what a peer from before federation is sent, which can't
/// tell them from the daemon's own, as names may be the same, nor has any
/// way to say where they are. Its active one, if it was one of those, is
/// none.
fn without_others(message: &ServerMessage) -> Option<ServerMessage> {
    let (ServerMessage::Event(Event::Workspaces(workspaces))
    | ServerMessage::Response(Response::Workspaces(workspaces))) = message
    else {
        return None;
    };
    if workspaces
        .list
        .iter()
        .all(|workspace| workspace.home.is_none())
    {
        return None;
    }
    let mut own = workspaces.clone();
    own.list.retain(|workspace| workspace.home.is_none());
    if (own.active).is_some_and(|active| own.get(active).is_none()) {
        own.active = None;
    }
    Some(match message {
        ServerMessage::Event(_) => ServerMessage::Event(Event::Workspaces(own)),
        _ => ServerMessage::Response(Response::Workspaces(own)),
    })
}

fn newest_in_node(node: &Node) -> u32 {
    match node {
        Node::Chat(chat) => (chat.turns.iter())
            .map(newest_in_turn)
            .chain([newest_in_settings(&chat.settings)])
            .chain([newest_in_question(chat.question.as_ref())])
            .max()
            .unwrap_or(EARLIER),
        Node::Grid(_) | Node::Other => node.since(),
    }
}

fn newest_in_settings(settings: &[Setting]) -> u32 {
    (settings.iter())
        .map(|setting| setting.kind.since())
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_question(question: Option<&crate::Question>) -> u32 {
    (question.into_iter())
        .flat_map(|question| &question.choices)
        .map(|choice| choice.kind.since())
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_turn(turn: &Turn) -> u32 {
    turn.items
        .iter()
        .map(newest_in_item)
        .max()
        .unwrap_or(EARLIER)
}

fn newest_in_item(item: &Item) -> u32 {
    match item {
        Item::Tool(tool) => (tool.content.iter())
            .map(Since::since)
            .max()
            .unwrap_or(EARLIER),
        Item::Words(_) | Item::Thought(_) | Item::Other(_) | Item::Compacted(_) => item.since(),
    }
}

fn newest_in_patch(patch: &ChatPatch) -> u32 {
    let inside = match patch {
        ChatPatch::Turn { turn, .. } => newest_in_turn(turn),
        ChatPatch::Item { value, .. } => newest_in_item(value),
        ChatPatch::Settings(settings) => newest_in_settings(settings),
        ChatPatch::Question(question) => newest_in_question(question.as_ref()),
        ChatPatch::History { turns, .. } => {
            turns.iter().map(newest_in_turn).max().unwrap_or(EARLIER)
        }
        _ => EARLIER,
    };
    patch.since().max(inside)
}

/// What an older peer is told in place of an answer too new for it.
const TOO_NEW: &str = "the daemon answered with what this window is too old to read";

/// What a message becomes for a peer older than this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Downgraded<T> {
    /// It can read the message as it is.
    AsIs,
    /// It reads this in its place.
    To(T),
    /// It's sent nothing: what it would say is only for a newer peer.
    Dropped,
    /// It can't be sent anything in its place, and it must be: an answer
    /// to a request. Encoding it fails, which drops the client, and it
    /// connects again into the same refusal: only for a peer from before
    /// the fallbacks, which never asks for what's newer.
    Refused,
}

/// What a peer older than this build can read in place of a message that
/// may hold something newer than it.
///
/// [`frame::encode`](crate::frame::encode) asks, for a connection at a
/// version older than [`PROTOCOL_VERSION`], before encoding: so what's
/// sent to a peer of this build's own version never pays for it.
pub trait Downgrade: Sized {
    /// The message as a peer speaking `wire` can read it, never holding a
    /// variant newer than that.
    fn downgrade(&self, wire: u32) -> Downgraded<Self> {
        let _ = wire;
        Downgraded::AsIs
    }
}

/// Windows check capabilities before sending. A relay may reach an older
/// daemon still: refuse image messages rather than dropping attachments.
impl Downgrade for ClientMessage {
    fn downgrade(&self, wire: u32) -> Downgraded<Self> {
        if wire < UNDO_STATE
            && let Some(older) = change_as_older(self)
        {
            return older;
        }
        // A command an older daemon would drop, lending a plain shell.
        if wire < ELSEWHERE
            && matches!(
                self,
                Self::Request(
                    crate::Request::LentBackground { .. }
                        | crate::Request::Lend {
                            command: Some(_),
                            ..
                        }
                )
            )
        {
            return Downgraded::Refused;
        }
        if wire < HARNESS
            && matches!(
                self,
                Self::NativeGrant { .. }
                    | Self::NativeReply { .. }
                    | Self::Request(
                        crate::Request::HarnessCapabilities { .. }
                            | crate::Request::CancelActivity { .. }
                    )
            )
        {
            return Downgraded::Refused;
        }
        if wire < CHAT_HISTORY {
            match self {
                Self::Request(crate::Request::AttachRecent { terminal, .. }) => {
                    return Downgraded::To(Self::Request(crate::Request::Attach {
                        terminal: *terminal,
                    }));
                }
                Self::Request(crate::Request::ChatHistory { .. }) => return Downgraded::Refused,
                _ => {}
            }
        }
        if let Self::Request(crate::Request::Chat { action, .. }) = self
            && wire < chat_action_since(action)
        {
            return Downgraded::Refused;
        }
        if wire < CHAT_ORIGINALS
            && matches!(self, Self::Request(crate::Request::ChatOriginal { .. }))
        {
            Downgraded::Refused
        } else {
            Downgraded::AsIs
        }
    }
}

/// The oldest protocol whose daemons take `action`: an older one would
/// drop it, or what it carries, so it's refused instead.
fn chat_action_since(action: &crate::ChatAction) -> u32 {
    use crate::ChatAction;
    match action {
        ChatAction::Fill { .. } => ELICITATION,
        ChatAction::Resend => CHAT_FACTS,
        ChatAction::StopTask { .. } => AGENT_TASKS,
        ChatAction::RefreshUsage { .. } => PROVIDER_USAGE,
        ChatAction::Submit { .. }
        | ChatAction::RemoveQueued { .. }
        | ChatAction::SendQueued { .. } => CHAT_QUEUE,
        ChatAction::SendTo { .. } => CHAT_HANDOFF,
        ChatAction::SendImages { .. } => CHAT_IMAGES,
        ChatAction::Send(_)
        | ChatAction::Stop
        | ChatAction::Answer { .. }
        | ChatAction::Set { .. }
        | ChatAction::Undo { .. }
        | ChatAction::Redo { .. }
        | ChatAction::Switch { .. }
        | ChatAction::LearnModels
        | ChatAction::Open { .. } => EARLIER,
    }
}

/// Replace image pixels with a readable label for older clients, in both
/// snapshots and incremental tool/turn updates.
fn without_tool_images(message: &ServerMessage) -> ServerMessage {
    fn item(item: &mut Item) {
        if let Item::Tool(tool) = item {
            for content in &mut tool.content {
                if matches!(content, ToolContent::Image(_)) {
                    *content =
                        ToolContent::Text("Image preview (update the client to view)".into());
                }
            }
        }
    }
    fn turn(turn: &mut Turn) {
        for value in &mut turn.items {
            item(value);
        }
    }
    fn node(node: &mut Node) {
        if let Node::Chat(chat) = node {
            for value in &mut chat.turns {
                turn(value);
            }
        }
    }
    let mut message = message.clone();
    match &mut message {
        ServerMessage::Response(Response::View(view)) => node(&mut view.root),
        ServerMessage::Event(Event::View { update, .. }) => match update {
            ViewUpdate::Snapshot(view) => node(&mut view.root),
            ViewUpdate::Patch(patches) => {
                for patch in patches {
                    match patch {
                        Patch::Chat(ChatPatch::Turn { turn: value, .. }) => turn(value),
                        Patch::Chat(ChatPatch::Item { value, .. }) => item(value),
                        _ => {}
                    }
                }
            }
        },
        _ => {}
    }
    message
}

/// `message` as a peer from before `CHAT_FACTS` reads it: a line across a
/// turn where the agent compacted its context as words saying so, and
/// answers of a kind the daemon doesn't know left out of a question, so
/// that no older window takes one for yes or for no.
fn without_chat_facts(message: &ServerMessage) -> ServerMessage {
    fn item(item: &mut Item) {
        if let Item::Compacted(details) = item {
            *item = Item::Other(format!("*{}*", Item::compacted_label(details)));
        }
    }
    fn turn(turn: &mut Turn) {
        turn.items.iter_mut().for_each(item);
    }
    fn question(question: &mut Option<crate::Question>) {
        if let Some(question) = question {
            let asked = !question.choices.is_empty();
            (question.choices).retain(|choice| choice.kind != ChoiceKind::Other);
            // With none left, it can still be said no to.
            if asked && question.choices.is_empty() {
                question.choices.push(crate::Choice {
                    id: crate::chat::DECLINE.to_owned(),
                    name: "Decline".to_owned(),
                    kind: ChoiceKind::RejectOnce,
                });
            }
        }
    }
    fn node(node: &mut Node) {
        if let Node::Chat(chat) = node {
            chat.turns.iter_mut().for_each(turn);
            question(&mut chat.question);
        }
    }
    let mut message = message.clone();
    match &mut message {
        ServerMessage::Response(Response::View(view)) => node(&mut view.root),
        ServerMessage::Event(Event::View { update, .. }) => match update {
            ViewUpdate::Snapshot(view) => node(&mut view.root),
            ViewUpdate::Patch(patches) => {
                for patch in patches {
                    match patch {
                        Patch::Chat(ChatPatch::Turn { turn: value, .. }) => turn(value),
                        Patch::Chat(ChatPatch::Item { value, .. }) => item(value),
                        Patch::Chat(ChatPatch::History { turns, .. }) => {
                            turns.iter_mut().for_each(turn);
                        }
                        Patch::Chat(ChatPatch::Question(value)) => question(value),
                        _ => {}
                    }
                }
            }
        },
        _ => {}
    }
    message
}

/// `message` as a peer of `wire`, older than `CHAT_FACTS`, reads a chat:
/// without the chat updates it has no controls for, or else without what
/// `CHAT_FACTS` brought. `None` if it reads it as it is.
fn older_chat(message: &ServerMessage, wire: u32) -> Option<ServerMessage> {
    if wire >= CHAT_FACTS {
        return None;
    }
    without_optional_chat_updates(message, wire)
        .or_else(|| (newest(message) == CHAT_FACTS).then(|| without_chat_facts(message)))
}

/// Informative chat updates can be omitted for clients without their controls.
fn without_optional_chat_updates(message: &ServerMessage, wire: u32) -> Option<ServerMessage> {
    let ServerMessage::Event(Event::View {
        terminal,
        update: ViewUpdate::Patch(patches),
    }) = message
    else {
        return None;
    };
    let unsupported = |patch: &Patch| {
        matches!(patch, Patch::Chat(ChatPatch::Retrying(_)) if wire < CHAT_FACTS)
            || matches!(patch, Patch::Chat(ChatPatch::Commands(_)) if wire < AGENT_COMMANDS)
            || matches!(patch, Patch::Chat(ChatPatch::Plan(_)) if wire < AGENT_PLAN)
            || matches!(patch, Patch::Chat(ChatPatch::Tasks(_)) if wire < AGENT_TASKS)
            || matches!(patch, Patch::Chat(ChatPatch::Activities(_)) if wire < HARNESS)
            || matches!(patch, Patch::Chat(ChatPatch::History { .. }) if wire < CHAT_HISTORY)
            || matches!(patch, Patch::Chat(ChatPatch::Delivery(_)) if wire < CHAT_QUEUE)
            || matches!(patch, Patch::Chat(ChatPatch::ProviderUsage(_)) if wire < PROVIDER_USAGE)
    };
    if !patches.iter().any(unsupported) {
        return None;
    }
    Some(ServerMessage::Event(Event::View {
        terminal: *terminal,
        update: ViewUpdate::Patch(
            patches
                .iter()
                .filter(|patch| !unsupported(patch))
                .cloned()
                .collect(),
        ),
    }))
}

impl Downgrade for ServerMessage {
    /// A peer from before the fallbacks has nothing to read in place of
    /// what's newer: an event holding it is dropped, and an answer refused.
    /// A later peer gets the fallbacks: as the daemon's state grows a
    /// variant, its arm goes here, for `wire` older than the variant. A
    /// pane of a new kind becomes `Content::Other`, naming it; a new node,
    /// chat item or tool content becomes its `Other`; a new chat patch
    /// becomes a fresh snapshot of the chat; something only informative,
    /// such as a new buffer update, is dropped; a new error becomes
    /// `Error::Other(err.to_string())`, as `Error::NotFollowing` and
    /// `Error::NotFitted` do.
    /// `Event::Exited` is dropped: a window that never hears a program
    /// ended takes its pane closing as someone else's doing, and goes on
    /// in normal mode, the safe side. So is `BufferUpdate::Coloured`: a
    /// hover's code stays plain. And `BufferUpdate::Language`: words are
    /// read as in plain text, as they were before. And `Event::Coloured`,
    /// which only answers what a newer window asked: its code stays
    /// plain. And `Event::Connections` and `Event::Login`, which go only
    /// to a client that asked for them, which an older one never does.
    /// The workspaces leave out those from other machines for a peer from
    /// before federation, which would take them for the daemon's own; and
    /// for a peer from before the relay, show their panes that aren't
    /// terminals as panes to come, as it would act on them as the daemon's
    /// own. `Event::Forwarded` is dropped, as it goes only to a client
    /// that asked for forwards, which an older one never does; and so is
    /// `BufferUpdate::Reattach`, as only another machine's buffers are
    /// attached again so, which an older peer isn't shown; and so is
    /// `Event::Said`, which is only ever about a pane lent, which an
    /// older peer is never shown; and `Event::Listening`, which goes only
    /// to a client that asked for it, which an older one never does.
    /// `BufferUpdate::Reversed` only answers a newer window's undo or redo,
    /// and becomes the `Edited` an older one's is answered with.
    /// A new answer, or a new kind of thing found, only ever answers what
    /// a newer window asked; should one reach an older peer all the same,
    /// it's told its question failed, rather than wait for ever.
    fn downgrade(&self, wire: u32) -> Downgraded<Self> {
        if newest(self) <= wire {
            return Downgraded::AsIs;
        }
        if let Some(edited) = reversed_as_edited(self) {
            return Downgraded::To(edited);
        }
        if let Some(older) = older_chat(self, wire) {
            return match older.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(older),
                other => other,
            };
        }
        if wire < TOOL_IMAGES && newest(self) == TOOL_IMAGES {
            let older = without_tool_images(self);
            return match older.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(older),
                other => other,
            };
        }
        if wire < MIXED
            && let Some(shown) = with_own_homes_unsaid(self)
        {
            return match shown.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(shown),
                downgraded => downgraded,
            };
        }
        if wire < LEND
            && let Some(own) = without_lent(self)
        {
            return match own.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(own),
                downgraded => downgraded,
            };
        }
        if wire < FEDERATION
            && let Some(own) = without_others(self)
        {
            return match own.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(own),
                downgraded => downgraded,
            };
        }
        if wire < RELAY
            && let Some(shown) = with_placeholders(self)
        {
            return match shown.downgrade(wire) {
                Downgraded::AsIs => Downgraded::To(shown),
                downgraded => downgraded,
            };
        }
        match self {
            Self::Event(
                Event::NativeGrantAck { .. }
                | Event::NativeRequest { .. }
                | Event::Exited { .. }
                | Event::Coloured { .. }
                | Event::Connections(_)
                | Event::Login { .. }
                | Event::Forwarded(_)
                | Event::Said(_)
                | Event::Listening(_)
                | Event::Buffer {
                    update:
                        BufferUpdate::Coloured { .. }
                        | BufferUpdate::Language { .. }
                        | BufferUpdate::Reattach,
                    ..
                },
            ) => Downgraded::Dropped,
            Self::Event(Event::Buffer {
                buffer,
                update: BufferUpdate::Answer { id, version, .. },
            }) if wire >= FALLBACKS => Downgraded::To(Self::Event(Event::Buffer {
                buffer: *buffer,
                update: BufferUpdate::Answer {
                    id: *id,
                    version: *version,
                    answer: Answer::Failed(TOO_NEW.into()),
                },
            })),
            Self::Event(Event::Found { id, .. }) if wire >= FALLBACKS => {
                Downgraded::To(Self::Event(Event::Found {
                    id: *id,
                    found: Found::Done(Done::Failed(TOO_NEW.into())),
                }))
            }
            Self::Response(Response::Error(err)) if wire >= FALLBACKS => Downgraded::To(
                Self::Response(Response::Error(Error::Other(err.to_string()))),
            ),
            // Only ever asked with a count, which an older peer doesn't
            // send: as the next match, which is what it would have asked.
            Self::Response(Response::Searched { found, .. }) => {
                Downgraded::To(Self::Response(Response::Found(*found)))
            }
            Self::Response(_) => Downgraded::Refused,
            _ if wire < FALLBACKS => Downgraded::Dropped,
            // What's newer than the fallbacks has no arm yet, so nothing is.
            _ => Downgraded::Refused,
        }
    }
}

/// `message`, if it's a change to a buffer, as a daemon before undo steps
/// kept anything takes it: an edit that starts a step as one that starts
/// one, an undo or a redo as one, and a step that changes nothing not at
/// all. `None` for anything else.
fn change_as_older(message: &ClientMessage) -> Option<Downgraded<ClientMessage>> {
    let ClientMessage::Edit {
        buffer,
        seq,
        change,
    } = message
    else {
        return None;
    };
    let older = match change {
        crate::Change::Step { edit, base, .. } => crate::Change::Edit {
            edit: edit.clone(),
            base: *base,
            step: true,
        },
        crate::Change::Reverse { redo: false, .. } => crate::Change::Undo,
        crate::Change::Reverse { redo: true, .. } => crate::Change::Redo,
        crate::Change::Empty { .. } => return Some(Downgraded::Dropped),
        crate::Change::Edit { .. } | crate::Change::Undo | crate::Change::Redo => {
            return Some(Downgraded::AsIs);
        }
    };
    Some(Downgraded::To(ClientMessage::Edit {
        buffer: *buffer,
        seq: *seq,
        change: older,
    }))
}

/// `message`, if it's an undo or a redo that gives back what its step kept,
/// as the edit an older window's undo or redo is answered with: it only
/// ever answers a newer window's.
fn reversed_as_edited(message: &ServerMessage) -> Option<ServerMessage> {
    let ServerMessage::Event(Event::Buffer {
        buffer,
        update: BufferUpdate::Reversed {
            version, seq, edit, ..
        },
    }) = message
    else {
        return None;
    };
    Some(ServerMessage::Event(Event::Buffer {
        buffer: *buffer,
        update: BufferUpdate::Edited {
            version: *version,
            edit: edit.clone(),
            author: crate::Author::User,
            answers: Some(*seq),
        },
    }))
}

#[cfg(test)]
mod tests {
    use serde::Serialize;

    use super::*;
    use crate::frame::{self, FrameDecoder};

    /// The test messages gained what they gained in this build's version,
    /// as a change made in it does: a peer a version older hasn't it.
    const NEW: u32 = PROTOCOL_VERSION;
    const OLD: u32 = PROTOCOL_VERSION - 1;

    /// `patches` as a peer of `wire` reads them.
    fn patches_at(wire: u32, patches: Vec<Patch>) -> Vec<Patch> {
        let message = ServerMessage::Event(Event::View {
            terminal: crate::TerminalId(1),
            update: ViewUpdate::Patch(patches),
        });
        let read: ServerMessage = decode(wire, &frame::encode(wire, &message).unwrap());
        let ServerMessage::Event(Event::View {
            update: ViewUpdate::Patch(patches),
            ..
        }) = read
        else {
            panic!("not patches: {read:?}")
        };
        patches
    }

    #[test]
    fn a_peer_before_chat_facts_reads_a_compacted_line_as_words() {
        let compacted = Patch::Chat(ChatPatch::Item {
            turn: 0,
            item: 0,
            value: Item::Compacted("automatic · 182k → 31k tokens".into()),
        });
        assert_eq!(patches_at(CHAT_FACTS, vec![compacted.clone()]), [compacted]);
        let older = patches_at(AGENT_COMMANDS, vec![compacted_item()]);
        assert_eq!(
            older,
            [Patch::Chat(ChatPatch::Item {
                turn: 0,
                item: 0,
                value: Item::Other("*Context compacted · automatic · 182k → 31k tokens*".into()),
            })]
        );
        // And is sent no news of the agent trying again.
        let retrying = Patch::Chat(ChatPatch::Retrying(Some("Retrying".into())));
        assert_eq!(
            patches_at(AGENT_COMMANDS, vec![retrying, compacted_item()]).len(),
            1
        );
    }

    fn compacted_item() -> Patch {
        Patch::Chat(ChatPatch::Item {
            turn: 0,
            item: 0,
            value: Item::Compacted("automatic · 182k → 31k tokens".into()),
        })
    }

    #[test]
    fn a_peer_before_chat_facts_is_shown_no_answer_of_a_kind_not_known() {
        let choice = |id: &str, kind| crate::Choice {
            id: id.into(),
            name: id.into(),
            kind,
        };
        let asking = |choices| {
            Patch::Chat(ChatPatch::Question(Some(crate::Question {
                id: 1,
                title: "touch probe.txt".into(),
                choices,
                own: false,
                form: None,
                plan: None,
            })))
        };
        let choices = |patches: Vec<Patch>| match &patches[..] {
            [Patch::Chat(ChatPatch::Question(Some(question)))] => (question.choices.iter())
                .map(|choice| (choice.id.clone(), choice.kind))
                .collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        };
        let mixed = asking(vec![
            choice("allow", ChoiceKind::AllowOnce),
            choice("session", ChoiceKind::Other),
            choice("reject", ChoiceKind::RejectOnce),
        ]);
        assert_eq!(
            choices(patches_at(CHAT_FACTS, vec![mixed.clone()])).len(),
            3
        );
        assert_eq!(
            choices(patches_at(AGENT_COMMANDS, vec![mixed])),
            [
                ("allow".to_owned(), ChoiceKind::AllowOnce),
                ("reject".to_owned(), ChoiceKind::RejectOnce)
            ]
        );
        // With every answer unknown, it can still say no.
        let unknown = asking(vec![choice("session", ChoiceKind::Other)]);
        assert_eq!(
            choices(patches_at(AGENT_COMMANDS, vec![unknown])),
            [(crate::chat::DECLINE.to_owned(), ChoiceKind::RejectOnce)]
        );
    }

    #[test]
    fn tool_images_downgrade_without_losing_the_tool_or_text() {
        let tool = crate::Tool {
            id: "image".into(),
            title: "View Image".into(),
            status: crate::ToolStatus::Done,
            kind: crate::ToolKind::Read,
            locations: Vec::new(),
            content: vec![
                ToolContent::Text("caption".into()),
                ToolContent::Image(crate::ChatPreview {
                    original: 0,
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 255].into(),
                }),
            ],
            exit: None,
        };
        let message = ServerMessage::Event(Event::View {
            terminal: crate::TerminalId(1),
            update: ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Item {
                turn: 0,
                item: 0,
                value: Item::Tool(tool),
            })]),
        });
        let current: ServerMessage =
            decode(TOOL_IMAGES, &frame::encode(TOOL_IMAGES, &message).unwrap());
        assert_eq!(current, message);
        let older: ServerMessage = decode(
            CHAT_PREVIEWS,
            &frame::encode(CHAT_PREVIEWS, &message).unwrap(),
        );
        let ServerMessage::Event(Event::View {
            update: ViewUpdate::Patch(patches),
            ..
        }) = older
        else {
            panic!()
        };
        let Patch::Chat(ChatPatch::Item {
            value: Item::Tool(tool),
            ..
        }) = &patches[0]
        else {
            panic!()
        };
        assert_eq!(tool.id, "image");
        assert_eq!(tool.content[0], ToolContent::Text("caption".into()));
        assert!(
            matches!(&tool.content[1], ToolContent::Text(label) if label.contains("Image preview"))
        );
    }

    /// A message as this build has it.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Before {
        items: Vec<Thing>,
        shape: OldShape,
        last: u8,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Thing {
        name: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum OldShape {
        Round,
        Other,
    }

    /// The same message a version on: with a field gated at it, at its
    /// end and in a struct inside a list, and a variant new then.
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct After {
        items: Vec<NewThing>,
        shape: Shape,
        last: u8,
        #[serde(
            skip_serializing_if = "before::<{ self::NEW }, _>",
            deserialize_with = "since::<{ self::NEW }, _, _>"
        )]
        note: Option<String>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct NewThing {
        name: String,
        #[serde(
            skip_serializing_if = "before::<NEW, _>",
            deserialize_with = "since::<NEW, _, _>"
        )]
        size: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    enum Shape {
        Round,
        Other,
        /// New at `NEW`: older peers get `Other`.
        Square,
    }

    impl Downgrade for After {
        fn downgrade(&self, wire: u32) -> Downgraded<Self> {
            if wire < NEW && self.shape == Shape::Square {
                Downgraded::To(Self {
                    shape: Shape::Other,
                    ..self.clone()
                })
            } else {
                Downgraded::AsIs
            }
        }
    }

    impl Downgrade for Before {}

    fn after(shape: Shape) -> After {
        After {
            items: vec![
                NewThing {
                    name: "a".into(),
                    size: 1,
                },
                NewThing {
                    name: "b".into(),
                    size: 300,
                },
            ],
            shape,
            last: 9,
            note: Some("new".into()),
        }
    }

    fn old(shape: OldShape) -> Before {
        Before {
            items: vec![Thing { name: "a".into() }, Thing { name: "b".into() }],
            shape,
            last: 9,
        }
    }

    fn decode<T: serde::de::DeserializeOwned>(wire: u32, bytes: &[u8]) -> T {
        let mut decoder = FrameDecoder::new();
        decoder.push(bytes);
        decoder.decode(wire).unwrap().unwrap()
    }

    #[test]
    fn pending_handoff_is_never_split_or_downgraded_to_an_old_daemon() {
        let message = crate::ClientMessage::Request(crate::Request::Chat {
            terminal: crate::TerminalId(1),
            action: crate::ChatAction::SendTo {
                agent: "codex".into(),
                model: None,
                budget: 20000,
                text: "continue".into(),
                images: Vec::new(),
            },
        });
        let encoded = frame::encode(CHAT_HANDOFF, &message).unwrap();
        assert_eq!(
            decode::<crate::ClientMessage>(CHAT_HANDOFF, &encoded),
            message
        );
        assert!(frame::encode(CHAT_HANDOFF - 1, &message).is_err());
    }

    #[test]
    fn chat_previews_round_trip_and_are_omitted_for_older_peers() {
        fn encode<T: serde::Serialize>(wire: u32, value: &T) -> Vec<u8> {
            speaking(wire, || postcard::to_stdvec(value).unwrap())
        }
        fn read<T: serde::de::DeserializeOwned>(wire: u32, bytes: &[u8]) -> T {
            speaking(wire, || postcard::from_bytes(bytes).unwrap())
        }
        let preview = crate::ChatPreview {
            original: 0,
            width: 1,
            height: 1,
            rgba: vec![10, 20, 30, 255].into(),
        };
        let mut image = crate::ChatImage {
            data: "cG5n".into(),
            width: 1,
            height: 1,
            preview: Some(preview.clone()),
        };
        let bytes = encode(CHAT_PREVIEWS, &image);
        assert_eq!(read::<crate::ChatImage>(CHAT_PREVIEWS, &bytes), image);
        let bytes = encode(CHAT_IMAGES, &image);
        image.preview = None;
        assert_eq!(read::<crate::ChatImage>(CHAT_IMAGES, &bytes), image);
        let mut turn = crate::Turn {
            woke: None,
            prompt: "[Image]".into(),
            sent: 0,
            items: Vec::new(),
            end: None,
            files: None,
            from: None,
            switch: None,
            previews: vec![preview],
        };
        let bytes = encode(CHAT_PREVIEWS, &turn);
        assert_eq!(read::<crate::Turn>(CHAT_PREVIEWS, &bytes), turn);
        let bytes = encode(CHAT_IMAGES, &turn);
        turn.previews.clear();
        assert_eq!(read::<crate::Turn>(CHAT_IMAGES, &bytes), turn);
    }

    #[test]
    fn images_are_never_sent_to_an_older_daemon_as_text_alone() {
        let message = ClientMessage::Request(crate::Request::Chat {
            terminal: crate::TerminalId(1),
            action: crate::ChatAction::SendImages {
                text: "look".into(),
                images: vec![crate::ChatImage {
                    preview: None,
                    data: "cG5n".into(),
                    width: 1,
                    height: 1,
                }],
            },
        });
        assert!(frame::encode(CHAT_IMAGES, &message).is_ok());
        assert!(matches!(
            frame::encode(CHAT_IMAGES - 1, &message),
            Err(crate::frame::FrameError::TooNew(_))
        ));
    }

    #[test]
    fn gated_fields_are_left_out_for_an_older_peer_byte_for_byte() {
        let sent = frame::encode(OLD, &after(Shape::Round)).unwrap();
        let old_bytes = frame::encode(OLD, &old(OldShape::Round)).unwrap();
        assert_eq!(sent, old_bytes);
        assert_eq!(decode::<Before>(OLD, &sent), old(OldShape::Round));
    }

    #[test]
    fn gated_fields_from_an_older_peer_take_their_defaults() {
        let sent = frame::encode(OLD, &old(OldShape::Round)).unwrap();
        let read: After = decode(OLD, &sent);
        let mut expected = after(Shape::Round);
        expected.note = None;
        expected.items[0].size = 0;
        expected.items[1].size = 0;
        assert_eq!(read, expected);
    }

    #[test]
    fn gated_fields_go_both_ways_between_peers_that_have_them() {
        let sent = frame::encode(NEW, &after(Shape::Square)).unwrap();
        let older = frame::encode(OLD, &after(Shape::Square)).unwrap();
        assert_ne!(sent, older);
        assert_eq!(decode::<After>(NEW, &sent), after(Shape::Square));
    }

    #[test]
    fn a_variant_newer_than_the_peer_is_downgraded_before_it_goes() {
        let sent = frame::encode(OLD, &after(Shape::Square)).unwrap();
        let old_bytes = frame::encode(OLD, &old(OldShape::Other)).unwrap();
        assert_eq!(sent, old_bytes);
        // A peer that has the variant gets it.
        let sent = frame::encode(NEW, &after(Shape::Square)).unwrap();
        assert_eq!(decode::<After>(NEW, &sent).shape, Shape::Square);
    }

    #[test]
    fn undo_steps_that_keep_anything_go_to_an_older_daemon_as_it_takes_them() {
        let edit = |change| ClientMessage::Edit {
            buffer: crate::BufferId(3),
            seq: 9,
            change,
        };
        let step = edit(crate::Change::Step {
            edit: crate::Edit::replace(3, 0, "", "x"),
            base: 4,
            state: crate::StepState::default(),
        });
        let older = edit(crate::Change::Edit {
            edit: crate::Edit::replace(3, 0, "", "x"),
            base: 4,
            step: true,
        });
        let old = UNDO_STATE - 1;
        assert_eq!(step.downgrade(old), Downgraded::To(older));
        let redo = edit(crate::Change::Reverse {
            redo: true,
            base: 4,
            visual: None,
        });
        assert_eq!(
            redo.downgrade(old),
            Downgraded::To(edit(crate::Change::Redo))
        );
        let empty = edit(crate::Change::Empty {
            base: 4,
            state: crate::StepState::default(),
        });
        assert_eq!(empty.downgrade(old), Downgraded::Dropped);
        assert_eq!(step.downgrade(UNDO_STATE), Downgraded::AsIs);
    }

    #[test]
    fn an_undo_that_gives_back_what_its_step_kept_is_an_older_windows_edit() {
        let reversed = ServerMessage::Event(Event::Buffer {
            buffer: crate::BufferId(3),
            update: BufferUpdate::Reversed {
                version: 6,
                seq: 14,
                edit: crate::Edit::replace(3, 0, "abc", ""),
                state: Some(crate::StepState::default()),
            },
        });
        let edited = ServerMessage::Event(Event::Buffer {
            buffer: crate::BufferId(3),
            update: BufferUpdate::Edited {
                version: 6,
                edit: crate::Edit::replace(3, 0, "abc", ""),
                author: crate::Author::User,
                answers: Some(14),
            },
        });
        assert_eq!(reversed.downgrade(UNDO_STATE - 1), Downgraded::To(edited));
        assert_eq!(reversed.downgrade(UNDO_STATE), Downgraded::AsIs);
    }

    #[test]
    fn the_version_is_put_back_after_encoding_and_decoding() {
        assert_eq!(current(), PROTOCOL_VERSION);
        let bytes = frame::encode(MIN_CLIENT, &after(Shape::Round)).unwrap();
        assert_eq!(current(), PROTOCOL_VERSION);
        let _: Before = decode(MIN_CLIENT, &bytes);
        assert_eq!(current(), PROTOCOL_VERSION);
        let caught = std::panic::catch_unwind(|| speaking(1, || panic!("in the middle")));
        assert!(caught.is_err());
        assert_eq!(current(), PROTOCOL_VERSION);
    }

    /// The messages a newer daemon may send a peer that has the fallbacks
    /// nothing in place of, as they're only informative: each added here
    /// on purpose, beside the downgrade arm that drops it.
    fn droppable(message: &ServerMessage) -> bool {
        // Only ever a reason to keep the mode as a pane closes: without
        // it, a window goes on in normal mode, which sends nothing typed.
        // And a hover's colours: without them, its code is plain. And
        // what a buffer's words are made of: without it, they're plain
        // text's. And the colours of a chat's code: without them, it's
        // plain. And the connections, and ssh's questions for them, and
        // what's said of ports forwarded through them, which only a client
        // that asked for them is sent; and what the bar is told of panes
        // lent, which an older peer is never shown; and the ports
        // background tabs listen on, which only a client that asked is
        // sent.
        matches!(
            message,
            ServerMessage::Event(
                Event::NativeRequest { .. }
                    | Event::NativeGrantAck { .. }
                    | Event::Exited { .. }
                    | Event::Coloured { .. }
                    | Event::Connections(_)
                    | Event::Login { .. }
                    | Event::Forwarded(_)
                    | Event::Said(_)
                    | Event::Listening(_)
                    | Event::Buffer {
                        update: BufferUpdate::Coloured { .. }
                            | BufferUpdate::Language { .. }
                            | BufferUpdate::Reattach,
                        ..
                    }
            )
        )
    }

    #[test]
    fn nothing_newer_than_a_peer_reaches_it() {
        use crate::corpus::{Sample, corpus};

        let mut withheld = 0;
        for wire in OLDEST..PROTOCOL_VERSION {
            for sample in corpus(PROTOCOL_VERSION) {
                let Sample::Server(message) = sample else {
                    continue;
                };
                let newer = newest(&message) > wire;
                match message.downgrade(wire) {
                    Downgraded::AsIs => assert!(!newer, "{message:?} goes to {wire} as it is"),
                    // Before the fallbacks there's no Other to read: which
                    // the check that what it gets is no newer than it
                    // covers, as an Other is from the fallbacks.
                    Downgraded::To(older) => {
                        assert!(newer, "{message:?} is changed for nothing");
                        assert!(
                            newest(&older) <= wire,
                            "{older:?} is still newer than {wire}"
                        );
                    }
                    Downgraded::Dropped | Downgraded::Refused if wire < FALLBACKS => {
                        assert!(newer, "{message:?} is withheld from {wire} for nothing");
                        withheld += 1;
                    }
                    // From the fallbacks on, an answer is never refused,
                    // and only what's listed is dropped.
                    Downgraded::Dropped => assert!(
                        newer && droppable(&message),
                        "{message:?} is dropped for {wire}: give it a downgrade arm, or list it \
                         as droppable"
                    ),
                    Downgraded::Refused => panic!(
                        "{message:?} is refused to {wire}, which drops it: give it a downgrade arm"
                    ),
                }
                // And what's encoded for it, read back, is as old as it is.
                match frame::encode(wire, &message) {
                    Ok(bytes) if bytes.is_empty() => assert!(newer),
                    Ok(bytes) => {
                        let sent: ServerMessage = decode(wire, &bytes);
                        assert!(newest(&sent) <= wire, "{sent:?}");
                    }
                    Err(err) => {
                        assert!(newer && matches!(err, crate::frame::FrameError::TooNew(_)));
                    }
                }
            }
        }
        assert!(withheld >= 5, "the corpus samples the fallbacks");
    }

    #[test]
    fn each_fixture_lists_the_gates_its_release_had() {
        // A branch that also took a version, merged as if it hadn't, would
        // have its gates at a version a release already had without them.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("the fixtures are committed") {
            let path = entry.unwrap().path();
            let Some(version) = (path.file_name().and_then(|name| name.to_str()))
                .and_then(|name| name.strip_prefix("protocol-v")?.strip_suffix(".bin"))
                .and_then(|version| version.parse::<u32>().ok())
            else {
                continue;
            };
            let listed = std::fs::read_to_string(path.with_extension("gates"))
                .unwrap_or_else(|_| panic!("protocol-v{version}.gates is missing"));
            for &(name, gate) in GATES.iter().filter(|&&(_, gate)| gate <= version) {
                assert!(
                    listed.lines().any(|line| line == format!("{name} {gate}")),
                    "{name} is at {gate}, but protocol {version}'s release didn't have it: \
                     renumber it, and what it gates, past {version}"
                );
            }
            checked += 1;
        }
        assert!(checked > 0);
    }

    #[test]
    fn every_variant_says_when_it_arrived() {
        assert_eq!(Node::Other.since(), FALLBACKS);
        assert_eq!(Error::Unsupported.since(), FALLBACKS);
        assert_eq!(Error::SessionOpen(crate::TerminalId(1)).since(), EARLIER);
        assert_eq!(Item::Other(String::new()).since(), FALLBACKS);
        assert_eq!(ToolContent::Other(String::new()).since(), FALLBACKS);
        let other = Content::Other {
            kind: crate::PaneKind::new("x"),
        };
        assert_eq!(other.since(), FALLBACKS);
        assert_eq!(BufferUpdate::ChangedOnDisk.since(), EARLIER);
        assert_eq!(FilesUpdate::NoGit(String::new()).since(), EARLIER);
        assert_eq!(SettingKind::Browser.since(), EARLIER);
        let exited = Event::Exited {
            terminal: crate::TerminalId(1),
        };
        assert_eq!(exited.since(), EXITED);
        let code = Event::Coloured {
            id: 1,
            spans: Vec::new(),
        };
        assert_eq!(code.since(), COLOURS);
        let coloured = BufferUpdate::Coloured {
            id: 1,
            spans: Vec::new(),
        };
        assert_eq!(coloured.since(), HOVER);
        assert_eq!(Answer::Staged.since(), EARLIER);
        assert_eq!(Answer::Renamable { name: None }.since(), SYMBOLS);
        assert_eq!(Answer::Uses(Vec::new()).since(), USES);
        let hints = Answer::InlayHints {
            start: 0,
            end: 0,
            hints: Vec::new(),
        };
        assert_eq!(hints.since(), INLAY_HINTS);
        assert_eq!(Found::Done(Done::All).since(), EARLIER);
        assert_eq!(Found::Symbols(Vec::new()).since(), SYMBOLS);
        let language = BufferUpdate::Language { word: "-".into() };
        assert_eq!(language.since(), WORDS);
    }

    #[test]
    fn a_new_answer_reaching_an_older_peer_says_its_question_failed() {
        let answer = ServerMessage::Event(Event::Buffer {
            buffer: crate::BufferId(1),
            update: BufferUpdate::Answer {
                id: 4,
                version: 2,
                answer: Answer::Symbols {
                    symbols: Vec::new(),
                    more: false,
                },
            },
        });
        let Downgraded::To(ServerMessage::Event(Event::Buffer {
            update: BufferUpdate::Answer { id: 4, answer, .. },
            ..
        })) = answer.downgrade(SYMBOLS - 1)
        else {
            panic!("not turned into a failure");
        };
        assert_eq!(answer, Answer::Failed(TOO_NEW.into()));
        assert_eq!(found_symbols_for(SYMBOLS), Downgraded::AsIs);
        // A peer from before the fallbacks has its event dropped.
        assert_eq!(found_symbols_for(FALLBACKS - 1), Downgraded::Dropped);
    }

    /// What a found symbol becomes for a peer speaking `wire`.
    fn found_symbols_for(wire: u32) -> Downgraded<ServerMessage> {
        ServerMessage::Event(Event::Found {
            id: 1,
            found: Found::Symbols(Vec::new()),
        })
        .downgrade(wire)
    }

    #[test]
    fn a_daemon_runs_a_connection_at_the_older_version_or_refuses() {
        assert_eq!(for_client(PROTOCOL_VERSION), Some(PROTOCOL_VERSION));
        assert_eq!(for_client(MIN_CLIENT), Some(MIN_CLIENT));
        assert_eq!(for_client(MIN_CLIENT - 1), None);
        // A newer client runs at this build's version.
        assert_eq!(for_client(PROTOCOL_VERSION + 5), Some(PROTOCOL_VERSION));
        // Asking with a version past every daemon's gets its own back.
        assert_eq!(for_client(u32::MAX), Some(PROTOCOL_VERSION));
    }

    #[test]
    fn a_client_takes_only_a_reply_it_can_talk_at() {
        assert!(takes_reply(PROTOCOL_VERSION));
        assert!(takes_reply(MIN_DAEMON));
        // A newer daemon's own version: it refused this client.
        assert!(!takes_reply(PROTOCOL_VERSION + 1));
        // A daemon older than the fallbacks: its own version, and so
        // perhaps a refusal, is never taken for agreement.
        for reply in 0..FALLBACKS {
            assert!(!takes_reply(reply), "{reply}");
        }
    }

    #[test]
    fn features_are_offered_only_by_daemons_that_have_them() {
        assert!(daemon_has(PROTOCOL_VERSION, FALLBACKS));
        assert!(daemon_has(FALLBACKS, FALLBACKS));
        assert!(!daemon_has(FALLBACKS - 1, FALLBACKS));
    }

    #[test]
    fn the_fallbacks_are_this_builds_version_until_a_release_has_them() {
        // A daemon from before them refuses a client by answering with its
        // own version, which a client takes for agreement if FALLBACKS is
        // below the version they came in: so until a release with them is
        // made, writing its fixture, they're this build's own version, as
        // a rebase onto other protocol changes must keep them.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let released = dir.join(crate::corpus::fixture_name(FALLBACKS)).exists();
        assert!(
            released || FALLBACKS == PROTOCOL_VERSION,
            "FALLBACKS is {FALLBACKS}, but no release has had them yet: it must be \
             PROTOCOL_VERSION ({PROTOCOL_VERSION}), and MIN_DAEMON with it"
        );
    }

    #[test]
    fn a_peer_from_before_lending_is_never_sent_a_workspace_lent_to_another() {
        use crate::{Layout, Lent, Pane, PaneId, Tab, TabId, Workspace, WorkspaceId};
        let workspace = |id: u32, lent: Option<Lent>| Workspace {
            id: WorkspaceId(id),
            name: String::new(),
            tabs: vec![Tab {
                id: TabId(id),
                layout: Layout::Pane(Pane {
                    id: PaneId(id),
                    content: Content::Terminal(crate::TerminalId(id)),
                    home: None,
                    key: None,
                }),
                focused: PaneId(id),
                name: None,
            }],
            active_tab: TabId(id),
            home: None,
            lent,
            background: Vec::new(),
        };
        let lent = Lent {
            to: crate::MachineId([7; 16]),
            key: 9,
            placed: false,
            background: None,
        };
        let workspaces = Workspaces {
            list: vec![workspace(1, None), workspace(2, Some(lent))],
            ..Workspaces::default()
        };
        let message = ServerMessage::Event(Event::Workspaces(workspaces));
        assert_eq!(newest(&message), LEND);
        assert_eq!(message.downgrade(LEND), Downgraded::AsIs);
        let Downgraded::To(ServerMessage::Event(Event::Workspaces(own))) =
            message.downgrade(LEND - 1)
        else {
            panic!("{message:?} wasn't downgraded");
        };
        assert_eq!(own.list, vec![workspace(1, None)]);
    }

    #[test]
    fn a_peer_from_before_mixed_layouts_is_told_a_pane_of_the_daemons_own_is_its_workspaces() {
        use crate::{Layout, Machine, Pane, PaneId, Tab, TabId, Workspace, WorkspaceId};
        let laptop = crate::MachineId([0xa1; 16]);
        let pane = |id: u32, home, key| {
            Layout::Pane(Pane {
                id: PaneId(id),
                content: Content::Terminal(crate::TerminalId(id)),
                home,
                key,
            })
        };
        let desk = |held: Layout| Workspace {
            id: WorkspaceId(1),
            name: "work".into(),
            tabs: vec![Tab {
                id: TabId(1),
                layout: held,
                focused: PaneId(1),
                name: None,
            }],
            active_tab: TabId(1),
            home: Some(crate::MachineId([7; 16])),
            lent: None,
            background: Vec::new(),
        };
        let workspaces = |held| Workspaces {
            list: vec![desk(held)],
            machine: Some(Machine {
                id: laptop,
                name: "laptop".into(),
                run: 1,
            }),
            ..Workspaces::default()
        };
        // Desk's workspace holds a terminal of the daemon's own machine,
        // lent it.
        let message = ServerMessage::Event(Event::Workspaces(workspaces(pane(
            1,
            Some(laptop),
            Some(9),
        ))));
        assert_eq!(newest(&message), MIXED);
        assert_eq!(message.downgrade(MIXED), Downgraded::AsIs);
        let Downgraded::To(ServerMessage::Event(Event::Workspaces(older))) =
            message.downgrade(MIXED - 1)
        else {
            panic!("{message:?} wasn't downgraded");
        };
        assert_eq!(older, workspaces(pane(1, None, Some(9))));
        // One on a machine the daemon isn't connected to, which it names,
        // is told as one of the workspace's.
        let mut away = workspaces(pane(1, Some(crate::MachineId([5; 16])), Some(3)));
        away.others = vec![crate::OtherMachine {
            id: crate::MachineId([5; 16]),
            name: "server".into(),
        }];
        let message = ServerMessage::Event(Event::Workspaces(away.clone()));
        let Downgraded::To(ServerMessage::Event(Event::Workspaces(older))) =
            message.downgrade(MIXED - 1)
        else {
            panic!("{message:?} wasn't downgraded");
        };
        away.list[0].tabs[0].layout = pane(1, None, Some(3));
        assert_eq!(older, away);
        // One of the daemon's machine with no key, as protocol 91's
        // samples have, is as it was.
        let message =
            ServerMessage::Event(Event::Workspaces(workspaces(pane(1, Some(laptop), None))));
        assert!(newest(&message) < MIXED);
    }

    #[test]
    fn a_peer_from_before_federation_sees_only_the_daemons_own_workspaces() {
        use crate::{Layout, Pane, PaneId, Tab, TabId, Workspace, WorkspaceId};
        let workspace = |id: u32, home: Option<crate::MachineId>| Workspace {
            id: WorkspaceId(id),
            name: "main".into(),
            tabs: vec![Tab {
                id: TabId(id),
                layout: Layout::Pane(Pane {
                    id: PaneId(id),
                    content: Content::Terminal(crate::TerminalId(id)),
                    home: None,
                    key: None,
                }),
                focused: PaneId(id),
                name: None,
            }],
            active_tab: TabId(id),
            home,
            lent: None,
            background: Vec::new(),
        };
        let desk = crate::MachineId([7; 16]);
        let workspaces = Workspaces {
            list: vec![workspace(1, None), workspace(2, Some(desk))],
            active: Some(WorkspaceId(2)),
            ..Workspaces::default()
        };
        for message in [
            ServerMessage::Event(Event::Workspaces(workspaces.clone())),
            ServerMessage::Response(Response::Workspaces(workspaces.clone())),
        ] {
            assert_eq!(message.downgrade(FEDERATION), Downgraded::AsIs);
            let Downgraded::To(
                ServerMessage::Event(Event::Workspaces(own))
                | ServerMessage::Response(Response::Workspaces(own)),
            ) = message.downgrade(FEDERATION - 1)
            else {
                panic!("{message:?} wasn't downgraded");
            };
            assert_eq!(own.list, vec![workspace(1, None)]);
            assert_eq!(own.active, None, "the active one was another machine's");
            // And it reads as such a peer would write it.
            let frame = frame::encode(FEDERATION - 1, &message).unwrap();
            let mut decoder = FrameDecoder::new();
            decoder.push(&frame);
            let read: ServerMessage = decoder.decode(FEDERATION - 1).unwrap().unwrap();
            assert!(matches!(
                read,
                ServerMessage::Event(Event::Workspaces(ref read))
                    | ServerMessage::Response(Response::Workspaces(ref read))
                    if read.list.len() == 1
            ));
        }
        // With none from elsewhere, nothing changes.
        let own = ServerMessage::Event(Event::Workspaces(Workspaces {
            list: vec![workspace(1, None)],
            ..Workspaces::default()
        }));
        assert_eq!(own.downgrade(FALLBACKS), Downgraded::AsIs);
    }

    #[test]
    fn no_gate_is_newer_than_the_protocol() {
        for &(name, version) in GATES {
            assert!(version <= PROTOCOL_VERSION, "{name} is {version}");
        }
    }

    #[test]
    fn gates_and_fixtures_every_peer_has_outgrown_are_deleted() {
        // Every peer this build talks to is OLDEST or newer: a gate at or
        // below it is always open, and a fixture below it pins bytes no
        // peer reads. `scripts/release` moves the floors; this says what
        // to delete then.
        // Bar FALLBACKS: the Other variants are for good. Its checks are
        // only always true then, and can go or stay.
        let gates: Vec<&str> = (GATES.iter())
            .filter(|&&(name, version)| version <= OLDEST && name != "FALLBACKS")
            .map(|&(name, _)| name)
            .collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
        let fixtures: Vec<String> = std::fs::read_dir(dir)
            .expect("the fixtures are committed")
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let version: u32 = (name.strip_prefix("protocol-v")?)
                    .strip_suffix(".bin")?
                    .parse()
                    .ok()?;
                (version < OLDEST).then_some(name)
            })
            .collect();
        assert!(
            gates.is_empty() && fixtures.is_empty(),
            "every peer is protocol {OLDEST} or newer now: delete the gates {gates:?} \
             (and what they gate), and the fixtures {fixtures:?}"
        );
    }
}

#[cfg(test)]
mod original_tests {
    use super::*;
    #[test]
    fn history_requests_fall_back_and_old_clients_keep_known_patches() {
        use crate::{Request, TerminalId};
        let terminal = TerminalId(1);
        let attach = ClientMessage::Request(Request::AttachRecent {
            terminal,
            first: Some(8),
        });
        assert_eq!(
            attach.downgrade(CHAT_HISTORY - 1),
            Downgraded::To(ClientMessage::Request(Request::Attach { terminal }))
        );
        let older = ClientMessage::Request(Request::ChatHistory {
            terminal,
            before: 8,
        });
        assert_eq!(older.downgrade(CHAT_HISTORY - 1), Downgraded::Refused);
        let message = ServerMessage::Event(Event::View {
            terminal,
            update: ViewUpdate::Patch(vec![
                Patch::Chat(ChatPatch::History {
                    first: 0,
                    before: 0,
                    messages_before: 0,
                    turns: vec![],
                }),
                Patch::Title("current".into()),
            ]),
        });
        let Downgraded::To(ServerMessage::Event(Event::View {
            update: ViewUpdate::Patch(patches),
            ..
        })) = message.downgrade(CHAT_HISTORY - 1)
        else {
            panic!("compatible update");
        };
        assert_eq!(patches, [Patch::Title("current".into())]);
        let node = crate::ChatNode {
            first_turn: 8,
            first_message: 7,
            ..crate::ChatNode::default()
        };
        let bytes = speaking(CHAT_HISTORY - 1, || postcard::to_allocvec(&node).unwrap());
        let old: crate::ChatNode =
            speaking(CHAT_HISTORY - 1, || postcard::from_bytes(&bytes).unwrap());
        assert_eq!((old.first_turn, old.first_message), (0, 0));
    }

    #[test]
    fn original_handles_are_gated_without_changing_older_thumbnail_bytes() {
        let mut preview = crate::ChatPreview {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255].into(),
            original: 42,
        };
        let bytes = speaking(CHAT_ORIGINALS, || postcard::to_allocvec(&preview).unwrap());
        assert_eq!(
            speaking(CHAT_ORIGINALS, || {
                postcard::from_bytes::<crate::ChatPreview>(&bytes).unwrap()
            }),
            preview
        );
        let bytes = speaking(TOOL_IMAGES, || postcard::to_allocvec(&preview).unwrap());
        preview.original = 0;
        assert_eq!(
            speaking(TOOL_IMAGES, || postcard::from_bytes::<crate::ChatPreview>(
                &bytes
            )
            .unwrap()),
            preview
        );
        assert_eq!(
            crate::ClientMessage::Request(crate::Request::ChatOriginal {
                terminal: crate::TerminalId(1),
                original: 42
            })
            .downgrade(TOOL_IMAGES),
            Downgraded::Refused
        );
    }
}

#[cfg(test)]
mod lend_background_tests {
    use super::*;

    /// An older daemon would lend a plain shell, dropping the command, and
    /// can't be asked about one lent as a background tab: both refused.
    #[test]
    fn a_background_shell_is_never_asked_of_an_older_daemon() {
        let lend = |command: Option<&str>| {
            ClientMessage::Request(crate::Request::Lend {
                key: 1,
                beside: None,
                terminal: crate::PaneTerminal::New(crate::NewTerminal {
                    size: crate::Size::new(80, 24).unwrap(),
                    command: None,
                    cwd: None,
                    record_to: None,
                }),
                command: command.map(str::to_owned),
            })
        };
        assert_eq!(
            lend(Some("x")).downgrade(ELSEWHERE - 1),
            Downgraded::Refused
        );
        assert_eq!(lend(Some("x")).downgrade(ELSEWHERE), Downgraded::AsIs);
        assert_eq!(lend(None).downgrade(ELSEWHERE - 1), Downgraded::AsIs);
        let seen = ClientMessage::Request(crate::Request::LentBackground {
            key: 1,
            action: crate::LentBackgroundAction::Seen,
        });
        assert_eq!(seen.downgrade(ELSEWHERE - 1), Downgraded::Refused);
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;
    #[test]
    fn usage_refresh_is_refused_and_quotas_are_omitted_for_older_peers() {
        let request = ClientMessage::Request(crate::Request::Chat {
            terminal: crate::TerminalId(1),
            action: crate::ChatAction::RefreshUsage { force: true },
        });
        assert_eq!(request.downgrade(PROVIDER_USAGE - 1), Downgraded::Refused);
        assert_eq!(request.downgrade(PROVIDER_USAGE), Downgraded::AsIs);
        let reports = vec![crate::ProviderUsage {
            agent: "codex".into(),
            ..Default::default()
        }];
        let node = crate::ChatNode {
            provider_usage: reports.clone(),
            ..Default::default()
        };
        let bytes = speaking(PROVIDER_USAGE - 1, || postcard::to_allocvec(&node).unwrap());
        let old: crate::ChatNode =
            speaking(PROVIDER_USAGE - 1, || postcard::from_bytes(&bytes).unwrap());
        assert!(old.provider_usage.is_empty());
        let message = ServerMessage::Event(Event::View {
            terminal: crate::TerminalId(1),
            update: ViewUpdate::Patch(vec![
                Patch::Chat(ChatPatch::ProviderUsage(reports)),
                Patch::Chat(ChatPatch::Delivery(crate::ChatDelivery::default())),
            ]),
        });
        let Downgraded::To(ServerMessage::Event(Event::View {
            update: ViewUpdate::Patch(patches),
            ..
        })) = message.downgrade(PROVIDER_USAGE - 1)
        else {
            panic!("older peer gets known patches");
        };
        assert!(matches!(
            patches.as_slice(),
            [Patch::Chat(ChatPatch::Delivery(_))]
        ));
    }
}

#[cfg(test)]
mod search_count_tests {
    use super::*;

    /// A counted search's answer reaches an older peer as the next match,
    /// which a relay's older peer asking with no count takes it for.
    #[test]
    fn a_counted_answer_goes_to_an_older_peer_as_the_next_match() {
        let found = Some(crate::Match {
            start: crate::Position { line: 1, col: 2 },
            cells: 3,
        });
        let searched = ServerMessage::Response(Response::Searched {
            found,
            wrapped: true,
        });
        assert_eq!(searched.downgrade(SEARCH_COUNT), Downgraded::AsIs);
        assert_eq!(
            searched.downgrade(SEARCH_COUNT - 1),
            Downgraded::To(ServerMessage::Response(Response::Found(found)))
        );
    }
}
