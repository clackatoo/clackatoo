//! File explorers: panes showing a project's files as a tree, and the
//! operations on files they offer, which agents and other clients can use
//! too.
//!
//! The daemon owns each explorer, which is little more than the directory
//! at its root, and lists directories for it, off its event loop, only as
//! they're asked for: a client attaches to an explorer with
//! [`Request::AttachFiles`](crate::Request::AttachFiles), and is told its
//! [`FilesUpdate::Root`]; then it expands directories with
//! [`FilesAction::Expand`], and is sent each one's
//! [`FilesUpdate::Listed`] as it's listed, and again whenever it changes,
//! until it's collapsed. The daemon watches the directories expanded, so
//! files a terminal or an agent makes show at once. Nothing is ever walked
//! whole: a tree with a hundred thousand files costs what's expanded.
//!
//! Which directories are expanded, where the cursor is and what's scrolled
//! to are the client's own, as an editor's cursor is: two windows can show
//! one explorer each their own way.
//!
//! What git says of the files comes in [`FilesUpdate::Git`], for the
//! repository the root is in: every path git has something to say about,
//! whenever that changes. A client rolls a directory's mark up from the
//! paths under it. What `.gitignore` leaves out comes with each listing.
//!
//! Files are made, moved, copied and deleted with
//! [`ClientMessage::FileOp`](crate::ClientMessage::FileOp), answered by
//! an [`Event::FileOp`](crate::Event::FileOp) once done, since it may
//! take a while, off the event loop. The daemon never writes over a file
//! unless asked to, and moves what it deletes to the trash when it can.
//! Buffers open on files moved follow them to their new paths
//! ([`BufferUpdate::Moved`](crate::BufferUpdate::Moved)).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies a file explorer within one daemon. Never reused while it
/// runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FilesId(pub u32);

impl fmt::Display for FilesId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A file explorer to show in a new pane.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFiles {
    /// The directory at its root, on the daemon's machine. A relative one
    /// is from the directory the pane beside it is in. `None` uses the root
    /// of the project the pane beside it is in, as the finder does: its
    /// file's language server's, or its repository's, or its directory.
    pub dir: Option<String>,
}

/// What a client asks of an explorer it attached to. Not answered: what
/// comes of it comes as [`FilesUpdate`]s.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesAction {
    /// Lists the directory at this absolute path, sending it as
    /// [`FilesUpdate::Listed`], and again whenever it changes, until it's
    /// collapsed. Expanding one already expanded sends it again.
    Expand(String),
    /// Stops sending the directory at this path, and watching it, if no
    /// one else has it expanded.
    Collapse(String),
    /// Makes the directory at this path the explorer's root, as going up
    /// past it does. Every client attached is told the new
    /// [`FilesUpdate::Root`]; what they have expanded stays so.
    Root(String),
    /// Lists every directory the client has expanded again, and asks git
    /// again, for changes the daemon can't see, such as on a file system
    /// that can't be watched.
    Refresh,
}

/// What the daemon tells a client about an explorer it attached to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesUpdate {
    /// The directory at the explorer's root: sent first, and again
    /// whenever it changes.
    Root {
        /// The directory, made absolute and real.
        path: String,
        /// The directory as people write it, with `~` for the home
        /// directory.
        name: String,
    },
    /// What's in a directory the client expanded, as it is now, in the
    /// order to show it: directories first, then files, each by name.
    Listed {
        /// The directory's absolute path, as the client expanded it.
        dir: String,
        /// What's in it.
        entries: Vec<Entry>,
        /// It holds more than the most a listing sends, which are left out.
        cut_short: bool,
    },
    /// A directory the client expanded can't be listed, as this says: it's
    /// gone, or can't be read. It's collapsed.
    Unlisted {
        /// The directory's absolute path, as the client expanded it.
        dir: String,
        /// Why not.
        reason: String,
    },
    /// What git says of the files in the repository the root is in: every
    /// path it has something to say about, absolute and in order, in place
    /// of what it said before. A directory git says is untracked or
    /// changed in has a mark of its own only if git gives it one.
    Git(Vec<(String, GitMark)>),
}

/// One thing in a directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Its name. One that isn't UTF-8 has its odd bytes replaced, and
    /// can't be opened or moved by it.
    pub name: String,
    /// What it is.
    pub kind: EntryKind,
    /// `.gitignore`, or git's other excludes, leave it out.
    pub ignored: bool,
    /// What tells it, as it was when listed, from itself changed since,
    /// for a file operation to expect it (see [`Expect`]): a file's whole
    /// stamp, and a folder's device and inode alone, its size and time
    /// being 0, as they change with what's in it. None for the rest.
    pub stamp: Option<FileStamp>,
}

/// What a thing in a directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryKind {
    /// A file, which may be run if `executable`.
    File {
        /// Its owner may run it.
        executable: bool,
    },
    /// A directory.
    Dir,
    /// A symbolic link, to a directory, which expands as one, if `dir`,
    /// or else to a file or nothing.
    Link {
        /// It leads to a directory.
        dir: bool,
    },
    /// Something else: a pipe, a socket or a device.
    Other,
}

impl EntryKind {
    /// Whether it expands, as a directory does.
    #[must_use]
    pub fn expands(self) -> bool {
        matches!(self, Self::Dir | Self::Link { dir: true })
    }
}

/// What git says of a file: how it differs from `HEAD`, in the index or
/// the work tree, whichever has more to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GitMark {
    /// Changed.
    Modified,
    /// New, and in the index.
    Added,
    /// Deleted.
    Deleted,
    /// New, and not in the index.
    Untracked,
    /// Both sides of a merge changed it, and git can't say how to join
    /// them.
    Conflicted,
}

/// Something to do to files, on the daemon's machine. Paths are absolute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileOp {
    /// Makes an empty file, or a directory if `dir`, and any directories
    /// it's in that don't exist yet. Refused if something's there.
    Create {
        /// Where.
        path: String,
        /// Make a directory.
        dir: bool,
    },
    /// Moves, or renames, what's at `from` to `to`. Refused if a file is
    /// at `to`, unless `overwrite` says it, and always if a directory is,
    /// or `to` is in `from`, or `from` in `to`, or a file at `to` is open
    /// with changes not written. Buffers open on files moved follow them.
    Move {
        /// What to move.
        from: String,
        /// Where to.
        to: String,
        /// Replace the file at `to`, if it's still the file this says: the
        /// one [`FileOpError::Exists`] described. One changed since is
        /// described again instead.
        overwrite: Option<FileStamp>,
        /// What's to be at `from`, if the asker knows: refused if it isn't.
        expect: Option<Expect>,
    },
    /// Copies what's at `from` to `to`, a directory with everything in it.
    /// Refused as a move is. Pipes, sockets and devices aren't copied: a
    /// directory holding one is refused.
    Copy {
        /// What to copy.
        from: String,
        /// Where to.
        to: String,
        /// Replace the file at `to`, as for a move.
        overwrite: Option<FileStamp>,
        /// What's to be at `from`, as for a move.
        expect: Option<Expect>,
    },
    /// Deletes what's at `path`, a directory with everything in it: to the
    /// trash, as the freedesktop.org spec says, if `trash`, or else for
    /// good. Refused if another file system is mounted there, or, for good,
    /// anywhere in it.
    Delete {
        /// What to delete.
        path: String,
        /// Move it to the trash. Refused with
        /// [`FileOpError::NoTrash`] if there's no trash on its file
        /// system.
        trash: bool,
        /// What's to be at `path`, as for a move.
        expect: Option<Expect>,
    },
}

/// What a file operation expects to find where it starts, as its asker
/// last saw it listed: it's refused if something else is there now, as a
/// folder where a file was, or the file changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expect {
    /// What it was.
    pub kind: EntryKind,
    /// Which file it was, as it was, if it was a file and that was known.
    pub stamp: Option<FileStamp>,
}

/// What tells one file, as it is, from another, or from itself changed,
/// without reading it: where it is on disk, how big it is, and when it was
/// last written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    /// Its device.
    pub device: u64,
    /// Its inode.
    pub inode: u64,
    /// Its length, in bytes.
    pub len: u64,
    /// When it was last written: seconds since the Unix epoch...
    pub seconds: i64,
    /// ...and nanoseconds since then.
    pub nanoseconds: i64,
}

/// Why a [`FileOp`] wasn't done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum FileOpError {
    /// A file is already at `path`, and wasn't to be written over. A
    /// directory never is: that's [`Failed`](Self::Failed).
    #[error("{path} already exists: {what}")]
    Exists {
        /// Where.
        path: String,
        /// What's there, as people say it, such as "a file of 3 KB", so
        /// the client can say what writing over it would lose.
        what: String,
        /// Which file it is, to write over it with, so only the file
        /// described is.
        stamp: FileStamp,
    },
    /// What was to go to the trash can't, as this says: it can only be
    /// deleted for good.
    #[error("{0}")]
    NoTrash(String),
    /// It failed, as this says.
    #[error("{0}")]
    Failed(String),
}
