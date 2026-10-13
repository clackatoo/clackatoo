//! Boards: shared scratchpads agents and the user coordinate on, each a
//! list of entries (notes, claims on work, and results), kept by the
//! daemon on disk, so they outlast the daemon and the machine.
//!
//! A board belongs to a workspace, by its name, or to every workspace.
//! The daemon keeps every board, archived ones too, which can still be
//! read. A pane shows one ([`Content::Board`](crate::Content::Board)): a
//! client attaches to it with
//! [`Request::AttachBoard`](crate::Request::AttachBoard), and is sent the
//! board whole, then each change to it, as [`BoardUpdate`]s. It acts on it
//! with [`ClientMessage::Board`](crate::ClientMessage::Board), as the user.
//! Agents use the same boards through their tools.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::TerminalId;

/// Identifies a board. Unlike other IDs, it's kept on disk with the
/// board, so it's the same from one run of the daemon to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BoardId(pub u32);

impl fmt::Display for BoardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A board to show in a new pane: the one called `name` in the workspace
/// the pane goes in, or else the one in every workspace, made in the
/// workspace if there's neither; or the board with `id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenBoard {
    /// By its name.
    Named(String),
    /// By its ID.
    Id(BoardId),
}

/// Which workspaces see a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardScope {
    /// Every workspace.
    Global,
    /// The workspace with this name.
    Workspace(String),
}

/// Who wrote something on a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardAuthor {
    /// The user.
    User,
    /// A chat's agent, as the chat was then: its ID, which a chat keeps
    /// only while the daemon runs, its agent as it's shown, and its title.
    Chat {
        /// The chat.
        chat: TerminalId,
        /// Its agent: "Claude Code", "Codex"…
        agent: String,
        /// Its title.
        title: String,
    },
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PostKind {
    /// Something to know.
    Note,
    /// A claim on a piece of work, so no one else takes it: held until
    /// released or it expires.
    Claim,
    /// What came of a piece of work.
    Result,
}

/// Something an entry points to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardLink {
    /// A file, on the daemon's machine, and a line in it, from 1.
    File {
        /// Its absolute path.
        path: String,
        /// The line.
        line: Option<u32>,
    },
    /// A turn of a chat, from 1.
    Turn {
        /// The chat.
        chat: TerminalId,
        /// The turn.
        turn: u32,
    },
}

/// An entry on a board, as it is now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardEntry {
    /// Its number on its board, from 1.
    pub id: u32,
    /// What it is.
    pub kind: PostKind,
    /// Who wrote it.
    pub by: BoardAuthor,
    /// When, in milliseconds since the Unix epoch.
    pub at: u64,
    /// What it says, as last edited.
    pub text: String,
    /// What it points to.
    pub links: Vec<BoardLink>,
    /// For a claim: the work claimed, which no one else can claim while
    /// it's held.
    pub subject: Option<String>,
    /// For a claim: when it expires, in milliseconds since the Unix epoch,
    /// unless released first. Renewing a claim moves it on.
    pub expires: Option<u64>,
    /// For a claim: when it was released, and by whom, if it was.
    pub released: Option<(u64, BoardAuthor)>,
    /// What it said before each edit, the oldest first: when, by whom,
    /// and the text it had until then.
    pub edits: Vec<(u64, BoardAuthor, String)>,
}

impl BoardEntry {
    /// Whether it's a claim still held at `now`, in milliseconds since the
    /// Unix epoch: not released, and not expired.
    #[must_use]
    pub fn holds(&self, now: u64) -> bool {
        self.kind == PostKind::Claim
            && self.released.is_none()
            && self.expires.is_some_and(|expires| expires > now)
    }
}

/// What a board is, without its entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSummary {
    /// Its ID.
    pub id: BoardId,
    /// Its name, unique among the boards of its scope.
    pub name: String,
    /// What it's for, if whoever made it said.
    pub purpose: String,
    /// Which workspaces see it.
    pub scope: BoardScope,
    /// It's done with: it can be read, but not written on, until it's
    /// unarchived.
    pub archived: bool,
    /// When it was made, in milliseconds since the Unix epoch.
    pub created: u64,
    /// How many entries it has.
    pub entries: u32,
}

/// What a client asks of a board it attached to, as the user. Not
/// answered: what comes of it comes as [`BoardUpdate`]s, or a
/// [`BoardUpdate::Refused`] saying why not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardAction {
    /// Adds an entry: a note or a result. Claims are agents' to make.
    Post {
        /// What it is.
        kind: PostKind,
        /// What it says.
        text: String,
    },
    /// Changes what an entry says, keeping what it said before.
    Edit {
        /// The entry.
        entry: u32,
        /// What it says now.
        text: String,
    },
    /// Releases a claim, whoever holds it.
    Release {
        /// The claim's entry.
        entry: u32,
    },
    /// Archives the board, or with `false`, unarchives it.
    Archive(bool),
}

/// What the daemon tells a client about a board it attached to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoardUpdate {
    /// The board whole: sent first.
    Snapshot {
        /// What it is.
        summary: BoardSummary,
        /// Its entries, in order.
        entries: Vec<BoardEntry>,
    },
    /// It changed: what it is now, and the entries added or changed, in
    /// place of what the client had of them.
    Changed {
        /// What it is now.
        summary: BoardSummary,
        /// The entries added or changed.
        entries: Vec<BoardEntry>,
    },
    /// What the client asked of it wasn't done, as this says.
    Refused(String),
    /// It can't be shown, as this says: it's gone, or the boards couldn't
    /// be read.
    Gone(String),
}
