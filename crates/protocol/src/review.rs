//! Reviewing what agents changed: the lines each agent changed in files,
//! hunk by hunk, kept or rejected, and comments on them sent back to the
//! agent as one message.
//!
//! The daemon keeps every agent hunk (see [`clackatoo_text::AgentHunks`]):
//! those in buffers go to the windows showing them, in a
//! [`BufferUpdate::Agents`](crate::BufferUpdate::Agents), for the colour
//! beside their lines. A review is a pane listing the hunks of one turn of
//! a chat, of the whole chat, or every change since the branch's base, by
//! file: the daemon sends the windows attached to it the list, again
//! whenever it changes, and the window keeps its cursor and scroll.
//!
//! Hunks are named by their numbers, which are never reused, so keeping or
//! rejecting one needs no place in a text: one that's gone by the time the
//! daemon hears is said to be.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{BufferId, TerminalId};

/// Identifies a review within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReviewId(pub u32);

impl fmt::Display for ReviewId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// What a review lists.
///
/// A review of a turn or of a chat takes in the chats under the chat too,
/// subagents started by its agent and theirs, open or closed: their
/// changes are part of its work, though undoing its turn leaves them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewScope {
    /// The hunks not yet reviewed that one turn of the chat changed last,
    /// by its index, with those of the subagents started in that turn; or
    /// with `None`, its latest turn that changed any.
    Turn(Option<u32>),
    /// The hunks not yet reviewed that the chat's agent changed, or its
    /// subagents'.
    Chat,
    /// Everything changed since the branch's base, the commit `git
    /// merge-base` finds with `main`, `master` or the branch's upstream:
    /// the user's changes too, with the agents' marked.
    Base,
}

/// A new review, in a pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewReview {
    /// The chat it's of, which comments go to. `None` takes the chat whose
    /// agent changed a file last.
    pub chat: Option<TerminalId>,
    /// What it lists.
    pub scope: ReviewScope,
    /// The directory whose project `Base` is of. `None` uses the chat's,
    /// or the pane beside it's.
    pub cwd: Option<String>,
}

/// What a review lists, as the daemon sends it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewList {
    /// The chat it's of, if any.
    pub chat: Option<TerminalId>,
    /// What the chat's called: its pane's title, or for a subagent, its
    /// profile and number, as `explore (chat 7)`.
    pub chat_name: String,
    /// What it lists, with a turn's index worked out.
    pub scope: Option<ReviewScope>,
    /// The files with hunks, in order of their paths.
    pub files: Vec<ReviewFile>,
    /// The comments waiting to go to the chat.
    pub comments: Vec<ReviewComment>,
    /// Anything to say instead of, or as well as, the hunks, such as that
    /// the base is still being worked out, or why it can't be.
    pub note: Option<String>,
    /// The chats whose agents made the hunks listed, by their numbers,
    /// for saying who made each.
    pub chats: Vec<ReviewChat>,
}

/// A chat whose agent made hunks a review lists.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewChat {
    /// The chat.
    pub chat: TerminalId,
    /// What it's called: for a subagent, its profile and number, as
    /// `explore (chat 7)`.
    pub name: String,
    /// For a subagent of the review's chat, or one under it: what it was
    /// asked to do, as the card that started it says.
    pub task: Option<String>,
    /// For a subagent of the review's chat, or one under it: the turn of
    /// the review's chat it was started in, by its index.
    pub turn: Option<u32>,
    /// Whether it has closed, its changes left for review.
    pub closed: bool,
}

/// One file's hunks in a review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFile {
    /// Its path on the daemon's machine, made absolute.
    pub path: String,
    /// Its path as the review shows it: from the project's directory, if
    /// it's in it.
    pub shown: String,
    /// Its buffer, if it's open.
    pub buffer: Option<BufferId>,
    /// The hunks, in order.
    pub hunks: Vec<ReviewHunk>,
}

/// One hunk in a review, with the lines around it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewHunk {
    /// The line its text starts on, counting from 1: for lines deleted,
    /// the line after where they were.
    pub line: u32,
    /// The lines before it, each without its line end.
    pub before: Vec<String>,
    /// What its lines were, each with its line end.
    pub base: String,
    /// What they are, each with its line end.
    pub text: String,
    /// The lines after it, each without its line end.
    pub after: Vec<String>,
    /// The agent hunks it is, by their numbers, which keeping or rejecting
    /// it acts on: one, but for a change since the base, which may be
    /// several agents' or none, the user's own.
    pub agents: Vec<u64>,
    /// Who changed it last, if an agent did.
    pub by: Option<Made>,
    /// Whether all of the agents' changes in it have been kept.
    pub reviewed: bool,
}

/// Which chat's agent made a change, and in which turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    /// The chat.
    pub chat: TerminalId,
    /// The turn's index, counting from 0.
    pub turn: u32,
}

/// The name of a chat, for saying who changed a line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatName {
    /// The chat.
    pub chat: TerminalId,
    /// What it's called.
    pub name: String,
}

/// A comment waiting to go to a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    /// Its number, to take it back by.
    pub id: u64,
    /// The file it's on, made absolute.
    pub path: String,
    /// The file as the message names it.
    pub shown: String,
    /// The line it's on, counting from 1.
    pub line: u32,
    /// What it says.
    pub text: String,
    /// Whether the line is in a change of the chat's agent.
    pub in_change: bool,
}

/// Which agent hunks to keep or reject.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hunks {
    /// These, by their numbers.
    Ids(Vec<u64>),
    /// Every one not yet reviewed that this turn of the chat changed last,
    /// or the subagents started in it, as a review of the turn lists them.
    Turn {
        /// The chat.
        chat: TerminalId,
        /// The turn's index.
        turn: u32,
    },
    /// Every one not yet reviewed that the chat's agent changed, or its
    /// subagents', as a review of the chat lists them.
    Chat(TerminalId),
}

/// A place in a file to comment on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentPlace {
    /// The file, made absolute.
    pub path: String,
    /// The line, counting from 1, as the window sees it.
    pub line: u32,
    /// For a file open in a buffer the client attached to: the buffer,
    /// the version of the text the place is in, and the place, in bytes,
    /// which the daemon moves along with edits until the comment's sent.
    pub at: Option<(BufferId, u64, usize)>,
}

/// What a client asks of reviewing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewAction {
    /// Keep these agent hunks: they're marked reviewed, and lose their
    /// colour.
    Keep(Hunks),
    /// Take these agent hunks' changes out of their files, as undoing the
    /// agent's turn would, keeping whatever's changed since. If any can't
    /// be, because a change since touches the same lines, none is, unless
    /// `force`, which does the rest.
    Reject {
        /// Which.
        hunks: Hunks,
        /// Reject what can be, though some can't be.
        force: bool,
    },
    /// Add a comment for the chat, to go with the others when they're
    /// sent.
    Comment {
        /// The chat.
        chat: TerminalId,
        /// Where.
        place: CommentPlace,
        /// What it says.
        text: String,
    },
    /// Take back the comment numbered `id`.
    Uncomment {
        /// The chat it's for.
        chat: TerminalId,
        /// Its number.
        id: u64,
    },
    /// Send the chat its comments, as one message from the user.
    Send(TerminalId),
    /// Work out a review's list afresh, as for a change since the base.
    Refresh(ReviewId),
}
