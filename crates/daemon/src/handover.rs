//! What one build of the daemon hands the next when it upgrades in place
//! (see [`upgrade`](crate::upgrade)): its terminals, chats, workspaces and
//! listening socket.
//!
//! The state is a file format, written by one build and read by a newer
//! one, so the types here are frozen, like [`clackatoo_term::Snapshot`]:
//! they don't follow the protocol's types, which change with every protocol
//! version. A change that needs the format to change makes a new version
//! alongside this one, and newer builds keep reading this one.
//!
//! File descriptors travel separately (they can't be written down), and
//! the state refers to each by its index in [`Fds`].
//!
//! Versions:
//! 1. The first.
//! 2. Each terminal's shell blocks ([`clackatoo_term::BlocksSnapshot`]).
//! 3. Chats with agents, as the screens they were written into.
//! 4. Chats as their turns.
//! 5. Chats' settings, and the values the user chose for them.
//! 6. What sort of tool each of a chat's tools is, and what it shows: a
//!    command's output, a change to a file.
//! 7. Where in files each tool works: their paths, and lines.
//! 8. Buffers: files being edited, with their history, and panes showing
//!    them.
//! 9. Each terminal's links ([`clackatoo_term::LinksSnapshot`]).
//! 10. File explorers, and panes showing them.
//! 11. Which agent each chat talks to.
//! 12. The edit log: what agents changed in files, by chat and turn, so
//!     that turns can still be undone.
//! 13. Where each chat stands among the chats agents talk in, and panes
//!     showing boards.
//! 14. The last change each window sent each buffer, so a window that lost
//!     its connection as the daemon upgraded knows which arrived.
//! 15. What agents changed and the user hasn't reviewed: each agent hunk,
//!     open or not, the comments waiting to go to chats, and reviews, and
//!     panes showing them.
//! 16. Chats that move between agents: the sessions each had with other
//!     agents, where it moved, how much of it each session has seen, and
//!     the agent's plan.
//! 17. Panes showing a chat's page in clackatoo's browser. The browser
//!     itself isn't handed over: it stops, and starts again with the next
//!     page an agent opens.
//! 18. Panes showing a folder in an editor, as its listing. The folder is
//!     an explorer's root, handed over with the other explorers.
//! 19. The lock on the recovery files, so no other daemon takes them
//!     while the next build starts.
//! 20. Each terminal's shell's secret (see `crate::shell`), the file it's
//!     read from if the shell hasn't yet, and which block's command the
//!     shell's integration marked with it.
//! 21. Which machine the daemon is, by its ID, and its connections to the
//!     daemons on other machines, which the next build makes again.
//! 22. Panes showing what's on another machine, by that machine and the
//!     number this daemon gave the pane there, and the workspaces this
//!     daemon lent another, holding a pane of its own for that daemon to
//!     show.

use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::fd::OwnedFd;

use clackatoo_term::{BlocksSnapshot, LinksSnapshot, Snapshot};
use serde::{Deserialize, Serialize};

/// The start of every handover, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo handover\n";

/// The version written.
pub(crate) const VERSION: u32 = 22;

/// The versions this build reads.
pub(crate) const READABLE: &[u32] = &[
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, VERSION,
];

/// Everything one build hands the next.
#[derive(Debug)]
pub(crate) struct Handover {
    pub(crate) state: State,
    pub(crate) fds: Fds,
}

/// The file descriptors a handover's state refers to, by index.
#[derive(Debug, Default)]
pub(crate) struct Fds(Vec<Option<OwnedFd>>);

impl Fds {
    /// Adds `fd`, returning the index the state refers to it by.
    pub(crate) fn add(&mut self, fd: OwnedFd) -> u32 {
        self.0.push(Some(fd));
        u32::try_from(self.0.len() - 1).expect("far fewer than 4 billion fds")
    }

    /// Takes the fd at `index`. Each can be taken once.
    pub(crate) fn take(&mut self, index: u32) -> Result<OwnedFd, HandoverError> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.0.get_mut(index))
            .and_then(Option::take)
            .ok_or(HandoverError::Damaged("an fd is missing or used twice"))
    }

    /// Every fd, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &OwnedFd> {
        self.0.iter().flatten()
    }

    pub(crate) fn from_received(fds: Vec<OwnedFd>) -> Self {
        Self(fds.into_iter().map(Some).collect())
    }
}

/// The daemon's state, version 15.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct State {
    /// The listening socket.
    pub(crate) listener: u32,
    /// The runtime directory's lock, set by whoever holds it.
    pub(crate) lock: Option<u32>,
    /// The ID the next terminal gets.
    pub(crate) next_terminal: u32,
    pub(crate) terminals: Vec<Terminal>,
    /// Chats with agents, which are terminals too: their IDs are
    /// terminals' IDs.
    pub(crate) chats: Vec<Chat>,
    /// Closed terminals' programs that haven't exited yet, and agents that
    /// were stopped.
    pub(crate) orphans: Vec<Process>,
    pub(crate) workspaces: Workspaces,
    /// The ID the next buffer gets.
    pub(crate) next_buffer: u32,
    /// Files being edited.
    pub(crate) buffers: Vec<Buffer>,
    /// The ID the next file explorer gets.
    pub(crate) next_files: u32,
    /// File explorers.
    pub(crate) explorers: Vec<Files>,
    /// What agents changed in files.
    pub(crate) edits: EditLog,
    /// The last change each window sent each buffer. Since version 14.
    pub(crate) applied: Vec<Applied>,
    /// The daemon's own number, chosen at random when it first started:
    /// see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
    /// Since version 14; 0 before, for the build reading it to choose one.
    pub(crate) daemon: u64,
    /// What agents changed that the user hasn't reviewed, and reviews.
    pub(crate) reviews: Reviews,
    /// The lock on the recovery files, if the daemon keeps them (see
    /// `crate::recovery`). Since version 19.
    pub(crate) recovery_lock: Option<u32>,
    /// The secrets of the terminals' shells that have one. Since version
    /// 20; before, no shell had one.
    pub(crate) shells: Vec<Shell>,
    /// Which machine the daemon is: its ID (see
    /// [`MachineId`](clackatoo_protocol::MachineId)). Since version 21;
    /// `None` before, for the build reading it to take the one its state
    /// directory keeps.
    pub(crate) machine: Option<[u8; 16]>,
    /// The daemon's connections to the daemons on other machines. Since
    /// version 21; before, there were none.
    pub(crate) connections: Vec<Connection>,
    /// The workspaces lent to the daemons on other machines. Since version
    /// 22; before, there were none.
    pub(crate) lent: Vec<Lent>,
    /// The machines they're lent to. Since version 22.
    pub(crate) borrowers: Vec<Borrower>,
}

/// A machine whose daemon this one lends workspaces to. Since version 22.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Borrower {
    /// Its ID.
    pub(crate) machine: [u8; 16],
    /// What it says it's called.
    pub(crate) name: String,
    /// When its daemon was last connected, in seconds since 1970.
    pub(crate) last_seen: u64,
}

/// A workspace lent to the daemon on another machine, holding a pane for
/// it to show (see [`clackatoo_protocol::Lent`]). Since version 22.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Lent {
    /// The workspace, by its ID.
    pub(crate) workspace: u32,
    /// The machine it's lent to, by its ID.
    pub(crate) to: [u8; 16],
    /// The number that machine knows the pane by.
    pub(crate) key: u64,
}

/// A connection to the daemon on another machine. Since version 21.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Connection {
    /// The machine, as ssh names it.
    pub(crate) host: String,
    /// The shell command that runs clackatoo there.
    pub(crate) command: String,
}

/// What agents changed in files, hunk by hunk, whether the user has
/// reviewed it, the comments waiting to go to chats, and reviews. Since
/// version 15.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Reviews {
    /// The number the next hunk gets.
    pub(crate) next_hunk: u64,
    /// The agent hunks of each open buffer, by its ID.
    pub(crate) buffers: Vec<(u32, Vec<AgentHunk>)>,
    /// The agent hunks of files not open.
    pub(crate) closed: Vec<ClosedHunks>,
    /// What the agent made each hunk's lines into, by its number.
    pub(crate) made: Vec<(u64, String)>,
    /// The number the next comment gets.
    pub(crate) next_comment: u64,
    pub(crate) comments: Vec<Comment>,
    /// The ID the next review gets.
    pub(crate) next_review: u32,
    pub(crate) list: Vec<Review>,
    /// The chat whose agent last changed a file.
    pub(crate) last_chat: Option<u32>,
}

/// Lines an agent changed: bytes `start` to `end` of the text, which were
/// `base`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AgentHunk {
    pub(crate) id: u64,
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) base: String,
    pub(crate) chat: u32,
    pub(crate) turn: u32,
    pub(crate) reviewed: bool,
}

/// The agent hunks of a file not open, which fit its text as it was when
/// its stamp was `stamp`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ClosedHunks {
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    pub(crate) stamp: Option<DiskStamp>,
    pub(crate) hunks: Vec<AgentHunk>,
}

/// A comment waiting to go to chat `chat`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Comment {
    pub(crate) chat: u32,
    pub(crate) id: u64,
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    /// The line, from 1, as it was when it was made.
    pub(crate) line: u32,
    /// The buffer it's in, and where, while that's open.
    pub(crate) at: Option<(u32, u64)>,
    pub(crate) text: String,
}

/// A review: what it lists, of which chat, from which directory.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Review {
    pub(crate) id: u32,
    pub(crate) chat: Option<u32>,
    pub(crate) scope: ReviewScope,
    /// The project's directory, as bytes.
    pub(crate) root: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ReviewScope {
    Turn(u32),
    Chat,
    Base,
}

/// The number of the last change window `window` sent buffer `buffer`
/// that the daemon dealt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Applied {
    pub(crate) buffer: u32,
    pub(crate) window: u64,
    pub(crate) seq: u64,
    /// The version it made, if it made one.
    pub(crate) landed: Option<u64>,
}

/// What agents changed in files, by chat and turn. Since version 12.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct EditLog {
    pub(crate) chats: Vec<ChatEdits>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatEdits {
    pub(crate) chat: u32,
    /// Turns dropped to keep to the bound, and how many files each changed.
    pub(crate) dropped: Vec<(u64, u32)>,
    pub(crate) turns: Vec<TurnEdits>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TurnEdits {
    pub(crate) turn: u64,
    pub(crate) files: Vec<FileEdits>,
}

/// What a turn changed in one file.
#[derive(Debug, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four facts about a file, each yes or no, and each free of the others"
)]
pub(crate) struct FileEdits {
    /// The file's path, as bytes.
    pub(crate) path: Vec<u8>,
    /// The tool call that last changed it.
    pub(crate) tool: Option<String>,
    /// Whether the file was there before the changes, and after.
    pub(crate) existed: bool,
    pub(crate) exists: bool,
    /// Whether what the turn did to it is known.
    pub(crate) known: bool,
    /// Whether the turn's changes to it are undone.
    pub(crate) undone: bool,
    pub(crate) changes: Vec<FileChange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum FileChange {
    /// The edit that made `version` of buffer `buffer`, keeping `bytes`.
    Buffer {
        buffer: u32,
        version: u64,
        bytes: u64,
    },
    /// A change to the file not open: `edit` made `after`.
    Text { after: String, edit: Vec<EditOp> },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Terminal {
    pub(crate) id: u32,
    /// The program, unless it has been reaped.
    pub(crate) process: Option<Process>,
    /// The program's pid, kept after it is reaped.
    pub(crate) pid: u32,
    /// The PTY's master end, unless it has closed.
    pub(crate) pty: Option<u32>,
    pub(crate) status: Status,
    /// Input the PTY hasn't accepted yet.
    pub(crate) input: Vec<u8>,
    pub(crate) screen: Snapshot,
    pub(crate) blocks: BlocksSnapshot,
    pub(crate) links: LinksSnapshot,
}

/// A terminal's shell's secret, which its integration adds to its marks
/// (see `crate::shell`). Since version 20.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Shell {
    pub(crate) terminal: u32,
    pub(crate) secret: String,
    /// The file the shell reads it from, if it's still there, for the next
    /// build to remove once the shell has read it.
    pub(crate) file: Option<Vec<u8>>,
    /// The latest block, if the integration marked its command with the
    /// secret: the terminal's blocks don't say.
    pub(crate) trusted: Option<u32>,
}

/// A chat with an agent. The agent isn't handed over: the next build
/// opens the session again when it's next needed.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Chat {
    pub(crate) id: u32,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) title: String,
    /// The directory the agent works in.
    pub(crate) cwd: String,
    /// The agent's session, if it had made one.
    pub(crate) session: Option<String>,
    /// Why the agent couldn't start, if it couldn't.
    pub(crate) failure: Option<String>,
    /// The turns. A turn the upgrade cut off is handed over ended.
    pub(crate) turns: Vec<ChatTurn>,
    /// The agent's settings, as it last gave them.
    pub(crate) settings: Vec<ChatSetting>,
    /// The values the user gave settings, by their IDs.
    pub(crate) chosen: Vec<(String, String)>,
    /// The agent, by its name: `claude`, `codex`…
    pub(crate) agent: String,
    /// Where it stands among the chats agents talk in. Since version 13.
    pub(crate) relations: ChatRelations,
    /// How many of the turns the session has seen. Since version 16.
    pub(crate) known: u64,
    /// The sessions it had with other agents. Since version 16.
    pub(crate) parked: Vec<ParkedSession>,
    /// The turns where it moved to another agent. Since version 16.
    pub(crate) switches: Vec<ChatSwitch>,
    /// About the most tokens a digest for the next agent takes. Since
    /// version 16.
    pub(crate) budget: u32,
    /// The agent's plan, as it last gave it, and that agent, as it's
    /// shown. Since version 16.
    pub(crate) plan: Vec<PlanStep>,
    pub(crate) plan_by: Option<String>,
}

impl Chat {
    /// What a chat from before version 16 had of moving between agents:
    /// nothing, its session having seen all of its `turns`.
    fn of_one_agent(turns: &[ChatTurn]) -> (u64, u32) {
        let known = u64::try_from(turns.len()).unwrap_or(u64::MAX);
        let budget = u32::try_from(crate::handoff::BUDGET).unwrap_or(u32::MAX);
        (known, budget)
    }
}

/// A session a chat had with another agent. Since version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ParkedSession {
    /// The agent, by its name.
    pub(crate) agent: String,
    /// The agent, as it's shown.
    pub(crate) title: String,
    pub(crate) session: Option<String>,
    /// How many of the chat's turns it saw.
    pub(crate) known: u64,
    pub(crate) chosen: Vec<(String, String)>,
    pub(crate) settings: Vec<ChatSetting>,
}

/// A turn of a chat that's where it moved to another agent. Since
/// version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSwitch {
    /// The turn's index.
    pub(crate) turn: u64,
    /// The agents it moved from and to, as they're shown.
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) model: Option<String>,
    /// It went back to a session it had before.
    pub(crate) resumed: bool,
    /// The settings that didn't carry over.
    pub(crate) notes: Vec<String>,
}

/// One step of an agent's plan. Since version 16.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PlanStep {
    pub(crate) content: String,
    pub(crate) state: StepState,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum StepState {
    ToDo,
    Doing,
    Done,
}

/// Where a chat stands among the chats agents talk in: which chat's agent
/// started it, and which messages other agents sent it.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ChatRelations {
    /// The chat whose agent started it, if an agent did.
    pub(crate) parent: Option<u32>,
    /// The chat the user started that the chats starting it go back to.
    pub(crate) root: Option<u32>,
    /// How many chats started by agents lead down to it.
    pub(crate) depth: u8,
    /// How many messages between agents led to its last turn.
    pub(crate) hops: u8,
    /// The user's turn its last turn came of: the chat, and the turn's
    /// index, if a message started it.
    pub(crate) cause: Option<(u32, u32)>,
    /// The turns other agents sent, by index, and who sent each.
    pub(crate) senders: Vec<(u32, ChatSender)>,
    /// Messages from other agents waiting for its turn to end.
    pub(crate) inbox: Vec<ChatMessage>,
}

/// The chat whose agent sent a message, as it was then.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSender {
    pub(crate) chat: u32,
    pub(crate) agent: String,
    pub(crate) title: String,
}

/// A message another chat's agent sent, waiting its turn.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatMessage {
    pub(crate) from: ChatSender,
    pub(crate) text: String,
    pub(crate) hops: u8,
    pub(crate) cause: Option<(u32, u32)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatSetting {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: SettingKind,
    pub(crate) value: String,
    pub(crate) choices: Vec<ChatChoice>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum SettingKind {
    Mode,
    Model,
    Effort,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatChoice {
    pub(crate) value: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatTurn {
    pub(crate) prompt: String,
    /// Milliseconds since the Unix epoch.
    pub(crate) sent: u64,
    pub(crate) items: Vec<ChatItem>,
    pub(crate) end: Option<ChatEnd>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatItem {
    Words(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        status: ToolStatus,
        kind: ToolKind,
        content: Vec<ToolContent>,
        locations: Vec<ToolLocation>,
    },
}

/// A file a tool works in, and the line, if it says.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ToolLocation {
    pub(crate) path: String,
    pub(crate) line: Option<u32>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ToolContent {
    /// Markdown.
    Text(String),
    /// A change to the file at `path`: `old` is `None` for a new file.
    Diff {
        path: String,
        old: Option<String>,
        new: String,
    },
    /// A command's output, as plain text. Since version 11.
    Output(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChatEnd {
    /// Milliseconds since the Unix epoch.
    pub(crate) at: u64,
    pub(crate) outcome: ChatOutcome,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ChatOutcome {
    Done,
    Stopped,
    Failed(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Process {
    pub(crate) pid: i32,
    pub(crate) pidfd: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Status {
    Running,
    Exited(i32),
    Killed(i32),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspaces {
    pub(crate) list: Vec<Workspace>,
    pub(crate) active: Option<u32>,
    pub(crate) next_workspace: u32,
    pub(crate) next_tab: u32,
    pub(crate) next_pane: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Workspace {
    pub(crate) id: u32,
    pub(crate) name: String,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) active_tab: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Tab {
    pub(crate) id: u32,
    pub(crate) layout: Layout,
    pub(crate) focused: u32,
    pub(crate) name: Option<String>,
}

/// Every version reads this, so new kinds of pane only ever go on the end.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Layout {
    /// A pane showing a terminal.
    Pane { id: u32, terminal: u32 },
    /// Panes side by side, left to right or top to bottom, each with its
    /// share of the space.
    Split {
        left_to_right: bool,
        parts: Vec<Layout>,
        shares: Vec<u32>,
    },
    /// A pane showing a buffer. Since version 8.
    Buffer { id: u32, buffer: u32 },
    /// A pane showing a file explorer. Since version 10.
    Files { id: u32, files: u32 },
    /// A pane showing a board, by the board's ID, which is kept on disk
    /// with it. Since version 13.
    Board { id: u32, board: u32 },
    /// A pane showing a review. Since version 15.
    Review { id: u32, review: u32 },
    /// A pane showing a chat's page in the browser, by the chat's ID.
    /// Since version 17.
    Browser { id: u32, chat: u32 },
    /// A pane showing a folder in an editor, as its listing, by the
    /// explorer whose root it is. Since version 18.
    Folder { id: u32, files: u32 },
    /// A pane showing what's on another machine: a pane its daemon lent
    /// this one, by that machine's ID and the number this daemon gave the
    /// pane, and the kind of pane it is, such as `terminal`. Since version
    /// 22.
    Borrowed {
        id: u32,
        machine: [u8; 16],
        key: u64,
        kind: String,
    },
}

/// A file explorer: the directory at its root. What's expanded in it is
/// each window's own, and isn't handed over.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Files {
    pub(crate) id: u32,
    /// The directory, as bytes.
    pub(crate) root: Vec<u8>,
}

/// A file being edited: its text, its history, and the file as it was
/// when last read or written.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Buffer {
    pub(crate) id: u32,
    /// The file's path, as bytes; none for a buffer with no file yet.
    /// Nothing reads a buffer's path as it's resumed, so an older build
    /// taking one over has a buffer it can't write, not a failed handover.
    pub(crate) path: Vec<u8>,
    /// The directory paths opened beside it are from, as bytes.
    pub(crate) dir: Vec<u8>,
    pub(crate) text: String,
    /// The text as last read or written, if it's changed since.
    pub(crate) saved: Option<String>,
    /// The file as last read or written, if it existed.
    pub(crate) disk: Option<DiskStamp>,
    pub(crate) history: BufferHistory,
}

/// What tells one version of a file from another without reading it.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct DiskStamp {
    pub(crate) seconds: i64,
    pub(crate) nanoseconds: i64,
    pub(crate) len: u64,
    pub(crate) inode: u64,
    pub(crate) device: u64,
}

/// A buffer's edits, and each author's undo and redo steps: see
/// [`clackatoo_text::History`].
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BufferHistory {
    pub(crate) first: u64,
    pub(crate) edits: Vec<(Vec<EditOp>, Author)>,
    pub(crate) steps: Vec<AuthorSteps>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum EditOp {
    Retain(u64),
    Insert(String),
    Delete(String),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum Author {
    User,
    Agent(u32),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct AuthorSteps {
    pub(crate) author: Author,
    pub(crate) version: u64,
    pub(crate) undo: Vec<Vec<EditOp>>,
    pub(crate) redo: Vec<Vec<EditOp>>,
}

/// A handover couldn't be read.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HandoverError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("it isn't a handover from clackatoo")]
    NotAHandover,
    #[error("it is version {0}, newer than this build reads")]
    Version(u32),
    #[error("it can't be decoded: {0}")]
    Decode(#[from] postcard::Error),
    #[error("it is damaged: {0}")]
    Damaged(&'static str),
    #[error(transparent)]
    Restore(#[from] clackatoo_term::RestoreError),
}

impl State {
    /// Writes the state to `file`, from its start.
    pub(crate) fn write(&self, file: &mut File) -> io::Result<()> {
        let body = postcard::to_allocvec(self).map_err(io::Error::other)?;
        file.write_all(MAGIC)?;
        file.write_all(&VERSION.to_le_bytes())?;
        file.write_all(&body)?;
        file.rewind()
    }

    /// Reads a state [`write`](Self::write) wrote, from `file`'s start.
    pub(crate) fn read(file: &mut File) -> Result<Self, HandoverError> {
        file.rewind()?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(HandoverError::NotAHandover)?;
        let (version, body) = rest
            .split_first_chunk::<4>()
            .ok_or(HandoverError::NotAHandover)?;
        match u32::from_le_bytes(*version) {
            1 => Ok(v7::State::from(postcard::from_bytes::<v1::State>(body)?).into()),
            2 => Ok(v7::State::from(postcard::from_bytes::<v2::State>(body)?).into()),
            3 => Ok(v7::State::try_from(postcard::from_bytes::<v3::State>(body)?)?.into()),
            4 => Ok(v7::State::from(postcard::from_bytes::<v4::State>(body)?).into()),
            5 => Ok(v7::State::from(postcard::from_bytes::<v5::State>(body)?).into()),
            6 => Ok(v7::State::from(postcard::from_bytes::<v6::State>(body)?).into()),
            7 => Ok(postcard::from_bytes::<v7::State>(body)?.into()),
            8 => Ok(postcard::from_bytes::<v8::State>(body)?.into()),
            9 => Ok(postcard::from_bytes::<v9::State>(body)?.into()),
            10 => Ok(postcard::from_bytes::<v10::State>(body)?.into()),
            11 => Ok(postcard::from_bytes::<v11::State>(body)?.into()),
            12 => Ok(postcard::from_bytes::<v12::State>(body)?.into()),
            13 => Ok(postcard::from_bytes::<v13::State>(body)?.into()),
            14 => Ok(postcard::from_bytes::<v14::State>(body)?.into()),
            15 => Ok(postcard::from_bytes::<v15::State>(body)?.into()),
            // Versions 16 and 17 are version 18 without browser panes,
            // then without folders' panes, which go on the end of
            // `Layout`, so they read as it does.
            16..=18 => Ok(postcard::from_bytes::<v18::State>(body)?.into()),
            19 => Ok(postcard::from_bytes::<v19::State>(body)?.into()),
            20 => Ok(postcard::from_bytes::<v20::State>(body)?.into()),
            // Version 21 is version 22 without lent workspaces; borrowed
            // panes go on the end of `Layout`, so they read as it does.
            21 => Ok(postcard::from_bytes::<v21::State>(body)?.into()),
            VERSION => Ok(postcard::from_bytes(body)?),
            other => Err(HandoverError::Version(other)),
        }
    }
}

/// Version 21, as the builds that wrote it wrote it: no lent workspaces.
mod v21 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, Chat, Connection, EditLog, Files, Process, Reviews, Shell, Terminal,
        Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
        machine: Option<[u8; 16]>,
        connections: Vec<Connection>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: state.machine,
                connections: state.connections,
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 20, as the builds that wrote it wrote it: no machine, and no
/// connections.
mod v20 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Shell, Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
        shells: Vec<Shell>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: state.shells,
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 19, as the builds that wrote it wrote it: no shells' secrets.
mod v19 {
    use serde::Deserialize;

    use super::{Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
        recovery_lock: Option<u32>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: state.recovery_lock,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 18, as the builds that wrote it wrote it: no lock on the
/// recovery files.
mod v18 {
    use serde::Deserialize;

    use super::{Applied, Buffer, Chat, EditLog, Files, Process, Reviews, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 1, as the builds that wrote it wrote it. The types that later
/// versions left as they were are shared: copy one here before changing it.
mod v1 {
    use clackatoo_term::Snapshot;
    use serde::Deserialize;

    use super::{Process, Status, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Terminal {
        id: u32,
        process: Option<Process>,
        pid: u32,
        pty: Option<u32>,
        status: Status,
        input: Vec<u8>,
        screen: Snapshot,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state
                    .terminals
                    .into_iter()
                    .map(|terminal| super::v8::Terminal {
                        id: terminal.id,
                        process: terminal.process,
                        pid: terminal.pid,
                        pty: terminal.pty,
                        status: terminal.status,
                        input: terminal.input,
                        screen: terminal.screen,
                        blocks: clackatoo_term::BlocksSnapshot::default(),
                    })
                    .collect(),
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 2, as the builds that wrote it wrote it, sharing the types
/// later versions left as they were.
mod v2 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: Vec::new(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 3, as the builds that wrote it wrote it: chats as the screens
/// they were written into, which become one turn of their text.
mod v3 {
    use clackatoo_term::{BlocksSnapshot, Position, Snapshot};
    use serde::Deserialize;

    use super::{
        ChatEnd, ChatItem, ChatOutcome, ChatTurn, HandoverError, Process, Workspaces, v8::Terminal,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) answering: bool,
        pub(super) failure: Option<String>,
        pub(super) input: String,
        pub(super) screen: Snapshot,
        pub(super) blocks: BlocksSnapshot,
    }

    impl TryFrom<State> for super::v7::State {
        type Error = HandoverError;

        fn try_from(state: State) -> Result<Self, HandoverError> {
            let chats = (state.chats.into_iter())
                .map(super::v10::Chat::try_from)
                .collect::<Result<_, _>>()?;
            Ok(Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats,
                orphans: state.orphans,
                workspaces: state.workspaces,
            })
        }
    }

    impl TryFrom<Chat> for super::v10::Chat {
        type Error = HandoverError;

        /// The chat's text, as written into its screen and history, in
        /// one turn of its own, as a block of text.
        fn try_from(chat: Chat) -> Result<Self, HandoverError> {
            let screen = clackatoo_term::Terminal::restore(
                &chat.screen,
                &clackatoo_term::Config::default(),
            )?;
            let size = screen.size();
            let text = screen.text(
                Position { line: 0, col: 0 },
                Position {
                    line: u64::MAX,
                    col: usize::MAX,
                },
                true,
            );
            let text = text.trim_end();
            let turns = if text.is_empty() {
                Vec::new()
            } else {
                vec![ChatTurn {
                    prompt: String::new(),
                    sent: 0,
                    items: vec![ChatItem::Words(format!("```text\n{text}\n```"))],
                    end: Some(ChatEnd {
                        at: 0,
                        outcome: ChatOutcome::Done,
                    }),
                }]
            };
            let _ = (chat.answering, chat.input, chat.blocks);
            Ok(Self {
                id: chat.id,
                cols: size.cols(),
                rows: size.rows(),
                title: screen.title().to_owned(),
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns,
                settings: Vec::new(),
                chosen: Vec::new(),
            })
        }
    }
}

/// Version 4, as the builds that wrote it wrote it: chats without their
/// settings, which they get again from the agent.
mod v4 {
    use serde::Deserialize;

    use super::v5::ChatTurn;
    use super::v8::Terminal;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            super::v5::State {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v5::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns,
                        settings: Vec::new(),
                        chosen: Vec::new(),
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
            .into()
        }
    }
}

/// Version 5, as the builds that wrote it wrote it: tools without their
/// kinds or what they show, which come through as tools of no kind,
/// showing nothing.
mod v5 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::{ChatEnd, ChatSetting, Process, ToolStatus, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatTurn {
        prompt: String,
        sent: u64,
        items: Vec<ChatItem>,
        end: Option<ChatEnd>,
    }

    #[derive(Debug, Deserialize)]
    enum ChatItem {
        Words(String),
        Thought(String),
        Tool {
            id: String,
            title: String,
            status: ToolStatus,
        },
    }

    impl From<ChatTurn> for super::v6::ChatTurn {
        fn from(turn: ChatTurn) -> Self {
            Self {
                prompt: turn.prompt,
                sent: turn.sent,
                items: (turn.items.into_iter())
                    .map(|item| match item {
                        ChatItem::Words(words) => super::v6::ChatItem::Words(words),
                        ChatItem::Thought(words) => super::v6::ChatItem::Thought(words),
                        ChatItem::Tool { id, title, status } => super::v6::ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind: super::ToolKind::Other,
                            content: Vec::new(),
                        },
                    })
                    .collect(),
                end: turn.end,
            }
        }
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            super::v6::State {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v6::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns.into_iter().map(Into::into).collect(),
                        settings: chat.settings,
                        chosen: chat.chosen,
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
            .into()
        }
    }
}

/// Version 6, as the builds that wrote it wrote it: tools without where
/// they work, which come through as working nowhere in particular.
mod v6 {
    use serde::Deserialize;

    use super::{
        ChatEnd, ChatSetting, Process, ToolContent, ToolKind, ToolStatus, Workspaces, v8::Terminal,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct ChatTurn {
        pub(super) prompt: String,
        pub(super) sent: u64,
        pub(super) items: Vec<ChatItem>,
        pub(super) end: Option<ChatEnd>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) enum ChatItem {
        Words(String),
        Thought(String),
        Tool {
            id: String,
            title: String,
            status: ToolStatus,
            kind: ToolKind,
            content: Vec<ToolContent>,
        },
    }

    impl From<ChatTurn> for super::ChatTurn {
        fn from(turn: ChatTurn) -> Self {
            Self {
                prompt: turn.prompt,
                sent: turn.sent,
                items: (turn.items.into_iter())
                    .map(|item| match item {
                        ChatItem::Words(words) => super::ChatItem::Words(words),
                        ChatItem::Thought(words) => super::ChatItem::Thought(words),
                        ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind,
                            content,
                        } => super::ChatItem::Tool {
                            id,
                            title,
                            status,
                            kind,
                            content,
                            locations: Vec::new(),
                        },
                    })
                    .collect(),
                end: turn.end,
            }
        }
    }

    impl From<State> for super::v7::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: (state.chats.into_iter())
                    .map(|chat| super::v10::Chat {
                        id: chat.id,
                        cols: chat.cols,
                        rows: chat.rows,
                        title: chat.title,
                        cwd: chat.cwd,
                        session: chat.session,
                        failure: chat.failure,
                        turns: chat.turns.into_iter().map(Into::into).collect(),
                        settings: chat.settings,
                        chosen: chat.chosen,
                    })
                    .collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
            }
        }
    }
}

/// Version 7, as the builds that wrote it wrote it: no buffers. The older
/// versions are read into this.
mod v7 {
    use serde::Deserialize;

    use super::v8::Terminal;
    use super::v10::Chat;
    use super::{Process, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        pub(super) listener: u32,
        pub(super) lock: Option<u32>,
        pub(super) next_terminal: u32,
        pub(super) terminals: Vec<Terminal>,
        pub(super) chats: Vec<Chat>,
        pub(super) orphans: Vec<Process>,
        pub(super) workspaces: Workspaces,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals.into_iter().map(Into::into).collect(),
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: 1,
                buffers: Vec::new(),
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 8, as the builds that wrote it wrote it: terminals without
/// their links. Older versions share its terminals.
mod v8 {
    use clackatoo_term::{BlocksSnapshot, LinksSnapshot, Snapshot};
    use serde::Deserialize;

    use super::v10::Chat;
    use super::{Buffer, Process, Status, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Terminal {
        pub(super) id: u32,
        pub(super) process: Option<Process>,
        pub(super) pid: u32,
        pub(super) pty: Option<u32>,
        pub(super) status: Status,
        pub(super) input: Vec<u8>,
        pub(super) screen: Snapshot,
        pub(super) blocks: BlocksSnapshot,
    }

    impl From<Terminal> for super::Terminal {
        fn from(terminal: Terminal) -> Self {
            Self {
                id: terminal.id,
                process: terminal.process,
                pid: terminal.pid,
                pty: terminal.pty,
                status: terminal.status,
                input: terminal.input,
                screen: terminal.screen,
                blocks: terminal.blocks,
                links: LinksSnapshot::default(),
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals.into_iter().map(Into::into).collect(),
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 9, as the builds that wrote it wrote it: no file explorers.
mod v9 {
    use serde::Deserialize;

    use super::v10::Chat;
    use super::{Buffer, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: 1,
                explorers: Vec::new(),
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 11, as the builds that wrote it wrote it: no edit log.
mod v11 {
    use serde::Deserialize;

    use super::v12::Chat;
    use super::{Buffer, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 15, as the builds that wrote it wrote it: chats with one agent
/// each. Versions 13 and 14 share its chats.
mod v15 {
    use serde::Deserialize;

    use super::{
        Applied, Buffer, ChatRelations, ChatSetting, ChatTurn, EditLog, Files, Process, Reviews,
        Terminal, Workspaces,
    };

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
        reviews: Reviews,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
        settings: Vec<ChatSetting>,
        chosen: Vec<(String, String)>,
        agent: String,
        relations: ChatRelations,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: chat.agent,
                relations: chat.relations,
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: state.reviews,
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 14, as the builds that wrote it wrote it: no reviews.
mod v14 {
    use serde::Deserialize;

    use super::v15::Chat;
    use super::{Applied, Buffer, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
        applied: Vec<Applied>,
        daemon: u64,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: state.applied,
                daemon: state.daemon,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 13, as the builds that wrote it wrote it: no windows' changes.
mod v13 {
    use serde::Deserialize;

    use super::v15::Chat;
    use super::{Buffer, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 12, as the builds that wrote it wrote it: chats with nothing
/// of other chats, and no panes showing boards. Version 11 shares its
/// chats.
mod v12 {
    use serde::Deserialize;

    use super::{Buffer, ChatSetting, ChatTurn, EditLog, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
        edits: EditLog,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        id: u32,
        cols: u16,
        rows: u16,
        title: String,
        cwd: String,
        session: Option<String>,
        failure: Option<String>,
        turns: Vec<ChatTurn>,
        settings: Vec<ChatSetting>,
        chosen: Vec<(String, String)>,
        agent: String,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: chat.agent,
                relations: super::ChatRelations::default(),
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: state.edits,
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

/// Version 10, as the builds that wrote it wrote it: chats without their
/// agent, as there was only ever Claude Code. Older versions share its
/// chats.
mod v10 {
    use serde::Deserialize;

    use super::{Buffer, ChatSetting, ChatTurn, Files, Process, Terminal, Workspaces};

    #[derive(Debug, Deserialize)]
    pub(super) struct State {
        listener: u32,
        lock: Option<u32>,
        next_terminal: u32,
        terminals: Vec<Terminal>,
        chats: Vec<Chat>,
        orphans: Vec<Process>,
        workspaces: Workspaces,
        next_buffer: u32,
        buffers: Vec<Buffer>,
        next_files: u32,
        explorers: Vec<Files>,
    }

    #[derive(Debug, Deserialize)]
    pub(super) struct Chat {
        pub(super) id: u32,
        pub(super) cols: u16,
        pub(super) rows: u16,
        pub(super) title: String,
        pub(super) cwd: String,
        pub(super) session: Option<String>,
        pub(super) failure: Option<String>,
        pub(super) turns: Vec<ChatTurn>,
        pub(super) settings: Vec<ChatSetting>,
        pub(super) chosen: Vec<(String, String)>,
    }

    impl From<Chat> for super::Chat {
        fn from(chat: Chat) -> Self {
            let (known, budget) = Self::of_one_agent(&chat.turns);
            Self {
                id: chat.id,
                cols: chat.cols,
                rows: chat.rows,
                title: chat.title,
                cwd: chat.cwd,
                session: chat.session,
                failure: chat.failure,
                turns: chat.turns,
                settings: chat.settings,
                chosen: chat.chosen,
                agent: crate::agents::DEFAULT.to_owned(),
                relations: super::ChatRelations::default(),
                known,
                parked: Vec::new(),
                switches: Vec::new(),
                budget,
                plan: Vec::new(),
                plan_by: None,
            }
        }
    }

    impl From<State> for super::State {
        fn from(state: State) -> Self {
            Self {
                listener: state.listener,
                lock: state.lock,
                next_terminal: state.next_terminal,
                terminals: state.terminals,
                chats: state.chats.into_iter().map(Into::into).collect(),
                orphans: state.orphans,
                workspaces: state.workspaces,
                next_buffer: state.next_buffer,
                buffers: state.buffers,
                next_files: state.next_files,
                explorers: state.explorers,
                edits: super::EditLog::default(),
                applied: Vec::new(),
                daemon: 0,
                reviews: super::Reviews::default(),
                recovery_lock: None,
                shells: Vec::new(),
                machine: None,
                connections: Vec::new(),
                lent: Vec::new(),
                borrowers: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::too_many_lines,
        reason = "one state, whole, each part of it filled"
    )]
    fn state() -> State {
        State {
            listener: 0,
            lock: Some(1),
            next_terminal: 3,
            terminals: Vec::new(),
            chats: vec![Chat {
                id: 2,
                cols: 40,
                rows: 10,
                title: "fixing the build".to_owned(),
                cwd: "/src".to_owned(),
                session: Some("s".to_owned()),
                failure: None,
                turns: vec![ChatTurn {
                    prompt: "why?".to_owned(),
                    sent: 1,
                    items: vec![
                        ChatItem::Words("Because".to_owned()),
                        ChatItem::Tool {
                            id: "t".to_owned(),
                            title: "Read".to_owned(),
                            status: ToolStatus::Done,
                            kind: ToolKind::Edit,
                            content: vec![
                                ToolContent::Text("for".to_owned()),
                                ToolContent::Output("ran\n".to_owned()),
                                ToolContent::Diff {
                                    path: "/src/a".to_owned(),
                                    old: None,
                                    new: "b".to_owned(),
                                },
                            ],
                            locations: vec![ToolLocation {
                                path: "/src/a".to_owned(),
                                line: Some(4),
                            }],
                        },
                    ],
                    end: Some(ChatEnd {
                        at: 2,
                        outcome: ChatOutcome::Failed("no".to_owned()),
                    }),
                }],
                settings: vec![ChatSetting {
                    id: "mode".to_owned(),
                    name: "Mode".to_owned(),
                    kind: SettingKind::Mode,
                    value: "auto".to_owned(),
                    choices: vec![ChatChoice {
                        value: "auto".to_owned(),
                        name: "Auto".to_owned(),
                        description: None,
                    }],
                }],
                chosen: vec![("model".to_owned(), "haiku".to_owned())],
                agent: "codex".to_owned(),
                relations: ChatRelations {
                    parent: Some(5),
                    root: Some(4),
                    depth: 2,
                    hops: 1,
                    cause: Some((4, 7)),
                    senders: vec![(0, sender())],
                    inbox: vec![ChatMessage {
                        from: sender(),
                        text: "next".to_owned(),
                        hops: 2,
                        cause: Some((4, 7)),
                    }],
                },
                known: 1,
                parked: vec![ParkedSession {
                    agent: "claude".to_owned(),
                    title: "Claude Code".to_owned(),
                    session: Some("c".to_owned()),
                    known: 1,
                    chosen: vec![("mode".to_owned(), "plan".to_owned())],
                    settings: Vec::new(),
                }],
                switches: vec![ChatSwitch {
                    turn: 1,
                    from: "Claude Code".to_owned(),
                    to: "Codex".to_owned(),
                    model: Some("6 Luna".to_owned()),
                    resumed: false,
                    notes: vec!["Mode Plan doesn't carry over".to_owned()],
                }],
                budget: 9000,
                plan: vec![PlanStep {
                    content: "test it".to_owned(),
                    state: StepState::Doing,
                }],
                plan_by: Some("Codex".to_owned()),
            }],
            orphans: vec![Process { pid: 42, pidfd: 2 }],
            workspaces: Workspaces {
                list: vec![
                    Workspace {
                        id: 1,
                        name: "clackatoo".to_owned(),
                        tabs: vec![Tab {
                            id: 1,
                            layout: Layout::Split {
                                left_to_right: true,
                                parts: vec![
                                    Layout::Pane { id: 1, terminal: 1 },
                                    Layout::Pane { id: 2, terminal: 2 },
                                    Layout::Buffer { id: 3, buffer: 1 },
                                    Layout::Files { id: 4, files: 1 },
                                    Layout::Board { id: 5, board: 3 },
                                    Layout::Review { id: 6, review: 1 },
                                    Layout::Browser { id: 7, chat: 2 },
                                    Layout::Folder { id: 8, files: 2 },
                                    Layout::Borrowed {
                                        id: 9,
                                        machine: [0xd5; 16],
                                        key: 0x0123_4567_89ab_cdef,
                                        kind: "terminal".to_owned(),
                                    },
                                ],
                                shares: vec![2, 1, 1, 1, 1, 1, 1, 1, 1],
                            },
                            focused: 2,
                            name: None,
                        }],
                        active_tab: 1,
                    },
                    Workspace {
                        id: 2,
                        name: String::new(),
                        tabs: vec![Tab {
                            id: 2,
                            layout: Layout::Buffer { id: 10, buffer: 1 },
                            focused: 10,
                            name: None,
                        }],
                        active_tab: 2,
                    },
                ],
                active: Some(1),
                next_workspace: 3,
                next_tab: 3,
                next_pane: 11,
            },
            next_buffer: 2,
            buffers: vec![buffer()],
            next_files: 3,
            explorers: vec![
                Files {
                    id: 1,
                    root: b"/src".to_vec(),
                },
                Files {
                    id: 2,
                    root: b"/src/sub".to_vec(),
                },
            ],
            edits: edits(),
            applied: vec![Applied {
                buffer: 1,
                window: 0xfeed,
                seq: 7,
                landed: Some(3),
            }],
            daemon: 0xd43,
            reviews: reviews(),
            recovery_lock: Some(3),
            shells: vec![Shell {
                terminal: 1,
                secret: "0123456789abcdef0123456789abcdef".to_owned(),
                file: Some(b"/run/clackatoo/shell/secret.1".to_vec()),
                trusted: Some(4),
            }],
            machine: Some([0xa7; 16]),
            connections: vec![Connection {
                host: "jack@laptop:2222".to_owned(),
                command: "~/.cargo/bin/clackatoo".to_owned(),
            }],
            lent: vec![Lent {
                workspace: 2,
                to: [0xa1; 16],
                key: 7,
            }],
            borrowers: vec![Borrower {
                machine: [0xa1; 16],
                name: "jack-ubuntu-laptop".to_owned(),
                last_seen: 1_792_000_000,
            }],
        }
    }

    fn reviews() -> Reviews {
        let hunk = |id, reviewed| AgentHunk {
            id,
            start: 0,
            end: 4,
            base: "old\n".to_owned(),
            chat: 2,
            turn: 0,
            reviewed,
        };
        Reviews {
            next_hunk: 3,
            buffers: vec![(1, vec![hunk(1, false)])],
            closed: vec![ClosedHunks {
                path: b"/src/b".to_vec(),
                stamp: Some(DiskStamp {
                    seconds: 6,
                    nanoseconds: 7,
                    len: 8,
                    inode: 9,
                    device: 10,
                }),
                hunks: vec![hunk(2, true)],
            }],
            made: vec![(1, "new\n".to_owned()), (2, "made\n".to_owned())],
            next_comment: 1,
            comments: vec![Comment {
                chat: 2,
                id: 0,
                path: b"/src/a".to_vec(),
                line: 1,
                at: Some((1, 0)),
                text: "why?".to_owned(),
            }],
            next_review: 2,
            list: vec![Review {
                id: 1,
                chat: Some(2),
                scope: ReviewScope::Turn(0),
                root: b"/src".to_vec(),
            }],
            last_chat: Some(2),
        }
    }

    fn edits() -> EditLog {
        EditLog {
            chats: vec![ChatEdits {
                chat: 2,
                dropped: vec![(0, 3)],
                turns: vec![TurnEdits {
                    turn: 1,
                    files: vec![
                        FileEdits {
                            path: b"/src/a".to_vec(),
                            tool: Some("t1".to_owned()),
                            existed: true,
                            exists: true,
                            known: true,
                            undone: false,
                            changes: vec![FileChange::Buffer {
                                buffer: 1,
                                version: 1,
                                bytes: 6,
                            }],
                        },
                        FileEdits {
                            path: b"/src/b".to_vec(),
                            tool: None,
                            existed: false,
                            exists: true,
                            known: true,
                            undone: true,
                            changes: vec![FileChange::Text {
                                after: "made\n".to_owned(),
                                edit: vec![EditOp::Insert("made\n".to_owned())],
                            }],
                        },
                    ],
                }],
            }],
        }
    }

    fn buffer() -> Buffer {
        Buffer {
            id: 1,
            path: b"/src/a".to_vec(),
            dir: b"/src".to_vec(),
            text: "new".to_owned(),
            saved: Some("old".to_owned()),
            disk: Some(DiskStamp {
                seconds: 1,
                nanoseconds: 2,
                len: 3,
                inode: 4,
                device: 5,
            }),
            history: BufferHistory {
                first: 0,
                edits: vec![(
                    vec![
                        EditOp::Delete("old".to_owned()),
                        EditOp::Insert("new".to_owned()),
                    ],
                    Author::User,
                )],
                steps: vec![AuthorSteps {
                    author: Author::User,
                    version: 1,
                    undo: vec![vec![
                        EditOp::Insert("old".to_owned()),
                        EditOp::Delete("new".to_owned()),
                    ]],
                    redo: Vec::new(),
                }],
            },
        }
    }

    #[test]
    fn a_state_reads_back_as_written() {
        let mut file = tempfile::tempfile().unwrap();
        state().write(&mut file).unwrap();
        let read = State::read(&mut file).unwrap();
        assert_eq!(format!("{read:?}"), format!("{:?}", state()));
    }

    fn sender() -> ChatSender {
        ChatSender {
            chat: 5,
            agent: "Claude Code".to_owned(),
            title: "parent".to_owned(),
        }
    }

    /// Written by a build that wrote version 13. It must always be read.
    #[test]
    fn version_13_is_read_with_each_chats_relations_and_panes_showing_boards() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v13.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let relations = &state.chats[0].relations;
        assert_eq!(
            (
                relations.parent,
                relations.root,
                relations.depth,
                relations.hops
            ),
            (Some(5), Some(4), 2, 1)
        );
        assert_eq!(relations.cause, Some((4, 7)));
        assert_eq!(relations.senders[0].1.title, "parent");
        assert_eq!(relations.inbox[0].text, "next");
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[4], Layout::Board { id: 5, board: 3 }));
        assert_eq!(state.edits.chats[0].chat, 2);
    }

    /// Written by a build that wrote version 15. It must always be read.
    #[test]
    fn version_15_is_read_with_the_reviews() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v15.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let reviews = &state.reviews;
        assert_eq!(reviews.next_hunk, 3);
        let (buffer, hunks) = &reviews.buffers[0];
        assert_eq!((*buffer, hunks[0].id, hunks[0].reviewed), (1, 1, false));
        assert_eq!(hunks[0].base, "old\n");
        let closed = &reviews.closed[0];
        assert_eq!(closed.path, b"/src/b");
        assert!(closed.hunks[0].reviewed);
        assert_eq!(reviews.made[1], (2, "made\n".to_owned()));
        assert_eq!(reviews.comments[0].text, "why?");
        assert!(matches!(
            reviews.list[0],
            Review {
                id: 1,
                chat: Some(2),
                scope: ReviewScope::Turn(0),
                ..
            }
        ));
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[5], Layout::Review { id: 6, review: 1 }));
        assert_eq!(state.edits.chats[0].chat, 2);
    }

    #[test]
    fn version_22_is_read_with_borrowed_panes_and_lent_workspaces() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v22.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(
            &parts[8],
            Layout::Borrowed { id: 9, machine: [0xd5, ..], key: 0x0123_4567_89ab_cdef, kind }
                if kind == "terminal"
        ));
        let lent = &state.lent[0];
        assert_eq!((lent.workspace, lent.to, lent.key), (2, [0xa1; 16], 7));
        let borrower = &state.borrowers[0];
        assert_eq!(
            (borrower.machine, borrower.name.as_str(), borrower.last_seen),
            ([0xa1; 16], "jack-ubuntu-laptop", 1_792_000_000)
        );
    }

    #[test]
    fn version_21_is_read_with_the_machine_and_its_connections() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v21.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(state.machine, Some([0xa7; 16]));
        let connection = &state.connections[0];
        assert_eq!(
            (connection.host.as_str(), connection.command.as_str()),
            ("jack@laptop:2222", "~/.cargo/bin/clackatoo")
        );
        assert_eq!(state.shells[0].trusted, Some(4));
        assert!(state.lent.is_empty());
    }

    #[test]
    fn version_20_is_read_with_the_shells_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v20.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let shell = &state.shells[0];
        assert_eq!(
            (shell.terminal, shell.secret.as_str(), shell.trusted),
            (1, "0123456789abcdef0123456789abcdef", Some(4))
        );
        assert_eq!(
            shell.file.as_deref(),
            Some(&b"/run/clackatoo/shell/secret.1"[..])
        );
        assert_eq!(state.recovery_lock, Some(3));
        assert!(state.machine.is_none() && state.connections.is_empty());
    }

    #[test]
    fn version_19_is_read_with_the_lock_on_the_recovery_files_and_no_secrets() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v19.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(state.recovery_lock, Some(3));
        assert_eq!(state.daemon, 0xd43);
        assert!(state.shells.is_empty());
    }

    #[test]
    fn version_18_is_read_with_panes_showing_folders() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v18.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[7], Layout::Folder { id: 8, files: 2 }));
        assert_eq!(state.workspaces.next_pane, 9);
        assert_eq!(state.explorers[1].root, b"/src/sub");
    }

    #[test]
    fn version_17_is_read_with_panes_showing_chats_pages() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v17.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[6], Layout::Browser { id: 7, chat: 2 }));
        assert_eq!(state.workspaces.next_pane, 8);
    }

    #[test]
    fn version_16_is_read_with_chats_that_moved_between_agents() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v16.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(
            (chat.agent.as_str(), chat.known, chat.budget),
            ("codex", 1, 9000)
        );
        let parked = &chat.parked[0];
        assert_eq!(
            (
                parked.agent.as_str(),
                parked.session.as_deref(),
                parked.known
            ),
            ("claude", Some("c"), 1)
        );
        assert_eq!(parked.chosen, [("mode".to_owned(), "plan".to_owned())]);
        let switch = &chat.switches[0];
        assert_eq!(
            (switch.turn, switch.to.as_str(), switch.model.as_deref()),
            (1, "Codex", Some("6 Luna"))
        );
        assert_eq!(switch.notes, ["Mode Plan doesn't carry over"]);
        assert!(matches!(chat.plan[0].state, StepState::Doing));
        assert_eq!(chat.plan_by.as_deref(), Some("Codex"));
        assert_eq!(chat.relations.inbox[0].text, "next");
    }

    /// Before version 16, a chat's session had seen every turn, and it had
    /// no other.
    #[test]
    fn version_15_comes_with_each_session_having_seen_every_turn() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v15.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(chat.known, 1);
        assert!(chat.parked.is_empty() && chat.switches.is_empty());
        assert_eq!(chat.relations.parent, Some(5));
    }

    #[test]
    fn version_14_comes_with_no_reviews() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v14.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(state.reviews.buffers.is_empty() && state.reviews.list.is_empty());
    }

    /// Version 12 comes with chats that know nothing of other chats.
    #[test]
    fn version_12_comes_with_no_relations() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v12.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(
            state
                .chats
                .iter()
                .all(|chat| chat.relations.parent.is_none() && chat.relations.inbox.is_empty())
        );
    }

    /// Writes the fixture for the current version: run once, with
    /// `--ignored`, when a version is made, and never again.
    #[test]
    #[ignore = "writes tests/fixtures/handover-v22.bin"]
    fn write_the_current_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/handover-v22.bin"
        );
        let mut file = File::create(path).unwrap();
        state().write(&mut file).unwrap();
    }

    #[test]
    fn other_files_and_versions_are_refused() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"not a handover").unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::NotAHandover)
        ));

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(MAGIC).unwrap();
        file.write_all(&(VERSION + 1).to_le_bytes()).unwrap();
        assert!(matches!(
            State::read(&mut file),
            Err(HandoverError::Version(version)) if version == VERSION + 1
        ));
    }

    /// Written by a build that wrote version 1. It must always be read.
    #[test]
    fn version_1_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v1.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (8, 99));
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.input.as_slice()), (7, &b"ls\r"[..]));
        assert_eq!(terminal.blocks, BlocksSnapshot::default());
        let screen =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap()
                .screen_text();
        assert_eq!(screen, "two\nthree\nfour");
        assert!(matches!(
            state.workspaces.list[0].tabs[0].layout,
            Layout::Split { ref shares, .. } if *shares == [2, 1]
        ));
    }

    /// Written by a build that wrote version 2. It must always be read.
    #[test]
    fn version_2_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v2.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (6, 98));
        assert!(state.chats.is_empty());
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.pid), (5, 77));
        let mut emulator =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap();
        emulator.restore_blocks(&terminal.blocks);
        let block = emulator.block(0, clackatoo_term::Which::At).unwrap();
        assert_eq!(block.command.as_deref(), Some("ls"));
    }

    /// Written by a build that wrote version 3. It must always be read.
    #[test]
    fn version_3_is_read_with_each_chat_as_its_text() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v3.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (4, 97));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.cols, chat.rows), (3, 30, 6));
        assert_eq!(
            (chat.cwd.as_str(), chat.session.as_deref()),
            ("/src/project", Some("s1"))
        );
        let [ChatTurn { prompt, items, .. }] = chat.turns.as_slice() else {
            panic!("one turn: {:?}", chat.turns);
        };
        assert!(prompt.is_empty());
        let [ChatItem::Words(text)] = items.as_slice() else {
            panic!("{items:?}");
        };
        assert_eq!(text, "```text\n› hi\n\necho: hi\n```");
    }

    /// Written by a build that wrote version 4. It must always be read.
    #[test]
    fn version_4_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v4.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (5, 96));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (4, Some("s4")));
        assert_eq!(chat.turns[0].prompt, "why?");
        assert!(chat.settings.is_empty() && chat.chosen.is_empty());
    }

    /// Written by a build that wrote version 5. It must always be read.
    #[test]
    fn version_5_is_read_with_tools_of_no_kind() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v5.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (6, 95));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (5, Some("s5")));
        assert_eq!(chat.chosen, [("model".to_owned(), "haiku".to_owned())]);
        let [
            _,
            ChatItem::Tool {
                title,
                kind,
                content,
                ..
            },
        ] = chat.turns[0].items.as_slice()
        else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert_eq!(title, "Read");
        assert!(matches!(kind, ToolKind::Other) && content.is_empty());
    }

    /// Written by a build that wrote version 6. It must always be read.
    #[test]
    fn version_6_is_read_with_tools_working_nowhere() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v6.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (7, 94));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (6, Some("s6")));
        let [
            _,
            ChatItem::Tool {
                kind,
                content,
                locations,
                ..
            },
        ] = chat.turns[0].items.as_slice()
        else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert!(matches!(kind, ToolKind::Edit) && content.len() == 2);
        assert!(locations.is_empty());
    }

    /// Written by a build that wrote version 7. It must always be read.
    #[test]
    fn version_7_is_read_with_no_buffers() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v7.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (8, 93));
        let chat = &state.chats[0];
        assert_eq!((chat.id, chat.session.as_deref()), (7, Some("s7")));
        let ChatItem::Tool { locations, .. } = &chat.turns[0].items[1] else {
            panic!("{:?}", chat.turns[0].items);
        };
        assert_eq!(locations[0].line, Some(4));
        assert!(state.buffers.is_empty());
        assert_eq!(state.next_buffer, 1);
    }

    /// Written by a build that wrote version 8. It must always be read.
    #[test]
    fn version_8_is_read() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v8.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (9, 94));
        assert_eq!(state.chats[0].session.as_deref(), Some("s8"));
        // From before there were other agents.
        assert_eq!(state.chats[0].agent, "claude");
        assert_eq!(
            (state.next_buffer, state.buffers[0].text.as_str()),
            (2, "new")
        );
        let terminal = &state.terminals[0];
        assert_eq!((terminal.id, terminal.pid), (1, 88));
        let mut emulator =
            clackatoo_term::Terminal::restore(&terminal.screen, &clackatoo_term::Config::default())
                .unwrap();
        emulator.restore_blocks(&terminal.blocks);
        assert_eq!(emulator.screen_text(), "two\nthree\nfour");
        assert_eq!(emulator.primary().scrollback_len(), 1);
        assert_eq!(emulator.cwd(), Some("/v8"));
    }

    /// Written by a build that wrote version 9. It must always be read.
    #[test]
    fn version_9_is_read_with_each_terminals_links() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v9.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (10, 95));
        assert_eq!((state.next_files, state.explorers.len()), (1, 0));
        let terminal = &state.terminals[0];
        let emulator = clackatoo_term::Terminal::restore_with_links(
            &terminal.screen,
            &terminal.links,
            &clackatoo_term::Config::default(),
        )
        .unwrap();
        assert_eq!(emulator.screen_text(), "two\nthree\nfour");
        let uri = |row: &clackatoo_term::Row| {
            let id = row.style(row.cells()[0]).link?;
            Some(emulator.link(id)?.uri().to_owned())
        };
        let history = emulator.primary().scrollback().next().unwrap();
        assert_eq!(uri(&history).as_deref(), Some("https://a.example/"));
        let three = emulator.grid().row(1);
        assert_eq!(uri(three).as_deref(), Some("file:///v9/three"));
        assert_eq!(uri(emulator.grid().row(2)), None);
    }

    /// Written by a build that wrote version 10. It must always be read.
    #[test]
    fn version_10_is_read_with_its_explorers() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v10.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!((state.next_terminal, state.orphans[0].pid), (11, 96));
        assert_eq!(state.next_files, 2);
        let explorer = &state.explorers[0];
        assert_eq!((explorer.id, explorer.root.as_slice()), (1, &b"/v10"[..]));
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[3], Layout::Files { id: 4, files: 1 }));
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Written by a build that wrote version 11. It must always be read.
    #[test]
    fn version_11_is_read_with_each_chats_agent() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v11.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.chats[0];
        assert_eq!(
            (chat.agent.as_str(), chat.session.as_deref()),
            ("codex", Some("s"))
        );
        let ChatItem::Tool { content, .. } = &chat.turns[0].items[1] else {
            panic!("not a tool");
        };
        assert!(matches!(&content[1], ToolContent::Output(text) if text == "ran\n"));
        assert_eq!(
            (state.next_files, state.buffers[0].text.as_str()),
            (2, "new")
        );
        let Layout::Split { parts, .. } = &state.workspaces.list[0].tabs[0].layout else {
            panic!("not split");
        };
        assert!(matches!(parts[3], Layout::Files { id: 4, files: 1 }));
    }

    /// Written by a build that wrote version 12. It must always be read.
    #[test]
    fn version_12_is_read_with_the_edit_log() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v12.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        let chat = &state.edits.chats[0];
        assert_eq!((chat.chat, chat.dropped.as_slice()), (2, &[(0, 3)][..]));
        let files = &chat.turns[0].files;
        assert_eq!(chat.turns[0].turn, 1);
        assert!(matches!(
            files[0].changes[..],
            [FileChange::Buffer {
                buffer: 1,
                version: 1,
                bytes: 6
            }]
        ));
        assert_eq!(files[0].tool.as_deref(), Some("t1"));
        let FileChange::Text { after, edit } = &files[1].changes[0] else {
            panic!("not a change to a file not open");
        };
        assert_eq!(after, "made\n");
        assert!(matches!(&edit[..], [EditOp::Insert(made)] if made == "made\n"));
        assert!(!files[1].existed && files[1].exists && files[1].undone);
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Versions before 12 come with an empty edit log.
    #[test]
    fn version_11_comes_with_no_edits() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v11.bin"))
            .unwrap();
        assert!(State::read(&mut file).unwrap().edits.chats.is_empty());
    }

    /// Written by a build that wrote version 14. It must always be read.
    #[test]
    fn version_14_is_read_with_each_windows_last_change() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v14.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert_eq!(
            state.applied,
            [Applied {
                buffer: 1,
                window: 0xfeed,
                seq: 7,
                landed: Some(3)
            }]
        );
        assert_eq!(state.daemon, 0xd43);
        assert_eq!(state.buffers[0].text, "new");
    }

    /// Versions before 14 come knowing no window's changes.
    #[test]
    fn version_13_comes_with_no_windows_changes() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(include_bytes!("../tests/fixtures/handover-v13.bin"))
            .unwrap();
        let state = State::read(&mut file).unwrap();
        assert!(state.applied.is_empty());
        assert_eq!(state.daemon, 0, "none: the build reading it chooses one");
    }

    #[test]
    fn each_fd_is_taken_once() {
        let mut fds = Fds::default();
        let index = fds.add(tempfile::tempfile().unwrap().into());
        assert!(fds.take(index).is_ok());
        assert!(fds.take(index).is_err());
        assert!(fds.take(index + 1).is_err());
    }
}
