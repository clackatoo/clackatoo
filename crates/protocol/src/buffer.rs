//! Buffers: the text of files being edited, kept by the daemon, with a copy
//! in each window showing them.
//!
//! A window attaches to a buffer, and gets a [`BufferUpdate::Snapshot`] of
//! it, then each edit as it's made. It edits its copy as you type, without
//! waiting, and sends each [`Change`] to the daemon in a
//! [`ClientMessage::Edit`](crate::ClientMessage::Edit), with the version it
//! was made to. Changes made at once elsewhere, by an agent or another
//! window, are reconciled by operational transformation (see
//! [`clackatoo_text`]): the daemon brings each edit up to date as it
//! arrives, and the window brings the daemon's edits up to date past its
//! own that the daemon hasn't yet answered.
//!
//! A window has one change on its way at a time: it sends the next once
//! the daemon has answered the last, with [`BufferUpdate::Acked`] for an
//! edit, or with the edit an undo or redo made.
//!
//! A buffer whose language the daemon has a grammar for is parsed after
//! each edit, off the event loop, and its [`BufferUpdate::Highlights`] sent
//! as each parse ends: all of them first, then what each parse changed.
//! The window moves the highlights it has along with each edit until then.
//!
//! A buffer whose language has a language server is kept open in it, and
//! its [`BufferUpdate::Diagnostics`] sent whenever the server says what's
//! wrong; the window moves them along with each edit too.

use std::fmt;

use clackatoo_text::{Diagnostic, Edit, Span};
use serde::{Deserialize, Serialize};

use crate::TerminalId;

/// Identifies a buffer within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BufferId(pub u32);

impl fmt::Display for BufferId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A file to edit in a new pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    /// Its path, on the daemon's machine. A file already being edited is
    /// shown again, not read again, and a file that doesn't exist yet is
    /// made when it's first saved.
    pub path: String,
    /// The directory a relative path is from. `None` uses the one the pane
    /// beside it is in.
    pub cwd: Option<String>,
}

/// Who made an edit. Each undoes their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Author {
    /// The person at the windows: every window's edits are theirs.
    User,
    /// The agent of the chat with this terminal ID.
    Agent(TerminalId),
}

/// A buffer as it is now: what a window attaching to it gets first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferSnapshot {
    /// The file's path on the daemon's machine, made absolute.
    pub path: String,
    /// Its text.
    pub text: String,
    /// The text's version: how many edits have been made to it.
    pub version: u64,
    /// Whether it has changes that aren't saved.
    pub modified: bool,
}

/// A change a window makes to a buffer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// An edit, which the window has already made to its copy.
    Edit {
        /// The edit.
        edit: Edit,
        /// The version of the text it was made to.
        base: u64,
        /// Whether it starts a new undo step, rather than joining the last,
        /// as each key typed in one go of insert mode does.
        step: bool,
    },
    /// Undo the user's last step, wherever it was made.
    Undo,
    /// Redo the step the user last undid.
    Redo,
}

/// What the daemon tells a window about a buffer it attached to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BufferUpdate {
    /// The whole buffer: the first update a window gets, and whenever its
    /// copy can't be brought up to date, such as when an edit it sent
    /// couldn't be made. Changes it sent and hadn't heard back about are
    /// gone.
    Snapshot(BufferSnapshot),
    /// The window's own edit, which it numbered `seq`, has been made, and
    /// made `version`.
    Acked {
        /// The version it made.
        version: u64,
        /// The window's number for the change.
        seq: u64,
    },
    /// An edit was made, making `version`: someone else's, or one this
    /// window's undo or redo made.
    Edited {
        /// The version it made.
        version: u64,
        /// The edit, made to the version before.
        edit: Edit,
        /// Who made it.
        author: Author,
        /// For the window whose undo or redo made it, that change's number.
        answers: Option<u64>,
    },
    /// The window's undo or redo, which it numbered with this, found
    /// nothing to undo or redo.
    NoChange(u64),
    /// Whether the buffer has changes that aren't saved is now this.
    Modified(bool),
    /// What a parse found: bytes `start` to `end` of the text at `version`
    /// are highlighted as `spans` say, and the rest as before. Sent after
    /// the edit that made `version`, and after a snapshot, for the whole
    /// text.
    Highlights {
        /// The version of the text they're for.
        version: u64,
        /// The first byte they're for.
        start: usize,
        /// The byte after the last.
        end: usize,
        /// The spans there, in order.
        spans: Vec<Span>,
    },
    /// What the buffer's language server says about the text at `version`:
    /// every diagnostic, in place of those before. Sent after the edit
    /// that made `version`, and after a snapshot.
    Diagnostics {
        /// The version of the text they're for.
        version: u64,
        /// The diagnostics, in order of where they start.
        diagnostics: Vec<Diagnostic>,
    },
}
