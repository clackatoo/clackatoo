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
//! until it's collapsed: as what changed in it ([`FilesUpdate::Changed`]),
//! if the client asked for that and it's less to send. The daemon watches
//! the directories expanded, so files a terminal or an agent makes show at
//! once. Nothing is ever walked
//! whole: a tree with a hundred thousand files costs what's expanded.
//!
//! Which directories are expanded, where the cursor is and what's scrolled
//! to are the client's own, as an editor's cursor is: two windows can show
//! one explorer each their own way.
//!
//! What git says of the files comes in [`FilesUpdate::Git`], for the
//! repository the root is in: every path git has something to say about,
//! whenever that changes, or as what changed in it
//! ([`FilesUpdate::GitChanged`]), if the client asked for that and it's
//! less to send. A client rolls a directory's mark up from the paths under
//! it. What `.gitignore` leaves out comes with each listing.
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
    /// git can't be run, as this says, so nothing will come in
    /// [`FilesUpdate::Git`]: it isn't installed, or is too old to run
    /// safely.
    NoGit(String),
    /// What's changed in a directory the client expanded since it was last
    /// sent it, whole or as changes: sent in place of
    /// [`Listed`](Self::Listed) to a client that attached asking for
    /// changes, when they're less to send. Only ever sent once the client
    /// has been sent the directory whole since it last expanded it, so a
    /// client holding no listing of `dir`, or one from before it last
    /// expanded it, drops it. New in protocol 127.
    Changed {
        /// The directory's absolute path, as the client expanded it.
        dir: String,
        /// How many entries the listing it changes holds, for a client to
        /// tell it isn't the one it has.
        was: u32,
        /// What changed.
        changes: ListingChanges,
    },
    /// What git says of the files that changed since it was last sent,
    /// whole or as changes: sent in place of [`Git`](Self::Git) to a
    /// client that attached asking for that, when it's less to send. Only
    /// ever sent once the client has been sent what git says whole since
    /// it last attached, so a client holding none, or what was sent on
    /// another connection, drops it, and attaches again to be sent it
    /// whole. New in protocol 137.
    GitChanged {
        /// How many marks what it changes holds, for a client to tell it
        /// isn't what it has.
        was: u32,
        /// What changed, in what [`Git`](Self::Git) last said, as it's
        /// ordered.
        changes: MarksChanges,
    },
}

/// What changed from one listing of a directory to the next: the entries
/// gone and the entries new, a changed entry being both. Whether it's
/// still cut short isn't said: a listing that changes that is sent whole.
pub type ListingChanges = Changes<Entry>;

/// What git says of an explorer's files that changed since it last said
/// it: the marks gone and the marks new, a path whose mark changed being
/// both.
pub type MarksChanges = Changes<(String, GitMark)>;

/// What changed from one list to the next, by places in them, so a peer
/// holding the first needn't know what order they're in: the items gone
/// and the items new, a changed item being both.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes<T> {
    /// Where the items gone, or changed, were in the list before, in
    /// order.
    pub removed: Vec<u32>,
    /// The items new, or changed, each with where it is in the list after,
    /// in order.
    pub added: Vec<(u32, T)>,
}

impl<T> Default for Changes<T> {
    fn default() -> Self {
        Self {
            removed: Vec::new(),
            added: Vec::new(),
        }
    }
}

impl<T> Changes<T> {
    /// Whether nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty()
    }

    /// How many items changed, gone and new: what it costs to send,
    /// against a whole list's length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.removed.len() + self.added.len()
    }
}

impl<T: Clone + PartialEq> Changes<T> {
    /// What changed from `old` to `new`, each in the order `key` gives:
    /// both are walked once, side by side, keeping what's the same in
    /// each. Whatever their order, applying what's returned to `old` makes
    /// `new`; out of order, it's only more than it need be. `None` if
    /// either is too long for its places to be numbered.
    pub fn between<K: Ord>(old: &[T], new: &[T], key: impl Fn(&T) -> K) -> Option<Self> {
        u32::try_from(old.len().max(new.len())).ok()?;
        #[allow(clippy::cast_possible_truncation, reason = "checked above")]
        let at = |index: usize| index as u32;
        let mut changes = Self::default();
        let (mut i, mut j) = (0, 0);
        while let (Some(before), Some(after)) = (old.get(i), new.get(j)) {
            if before == after {
                i += 1;
                j += 1;
                continue;
            }
            // The same name changed is gone and new.
            let order = key(before).cmp(&key(after));
            if order.is_le() {
                changes.removed.push(at(i));
                i += 1;
            }
            if order.is_ge() {
                changes.added.push((at(j), after.clone()));
                j += 1;
            }
        }
        changes.removed.extend((i..old.len()).map(at));
        changes
            .added
            .extend((j..new.len()).map(|j| (at(j), new[j].clone())));
        Some(changes)
    }

    /// `entries` with these changes made, or `None` if they aren't changes
    /// to it: a place past its end, or out of order.
    #[must_use]
    pub fn apply(&self, entries: &[T]) -> Option<Vec<T>> {
        let kept = entries.len().checked_sub(self.removed.len())?;
        let mut out = Vec::with_capacity(kept + self.added.len());
        let mut removed = self.removed.iter().map(|&at| at as usize).peekable();
        let mut added = self.added.iter().peekable();
        let mut entries = entries.iter().enumerate();
        loop {
            // What's new here goes before what's kept.
            if let Some((_, entry)) = added.next_if(|(at, _)| *at as usize == out.len()) {
                out.push(entry.clone());
                continue;
            }
            let Some((index, entry)) = entries.next() else {
                break;
            };
            if removed.next_if_eq(&index).is_none() {
                out.push(entry.clone());
            }
        }
        // Anything left was out of order, or past the end.
        (removed.next().is_none() && added.next().is_none()).then_some(out)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.to_owned(),
            kind,
            ignored: false,
            stamp: None,
        }
    }

    fn file(name: &str) -> Entry {
        named(name, EntryKind::File { executable: false })
    }

    /// Folders first, then by name, as the daemon lists.
    fn key(entry: &Entry) -> (bool, String) {
        (!entry.kind.expands(), entry.name.clone())
    }

    #[test]
    fn changes_are_what_differs_and_make_the_new_listing() {
        let old = [named("d", EntryKind::Dir), file("a"), file("b"), file("c")];
        let mut ignored_c = file("c");
        ignored_c.ignored = true;
        let new = [
            named("d", EntryKind::Dir),
            named("e", EntryKind::Dir),
            file("a"),
            ignored_c.clone(),
        ];
        let changes = ListingChanges::between(&old, &new, key).unwrap();
        assert_eq!(changes.removed, [2, 3], "b gone, and c changed");
        assert_eq!(
            changes.added,
            [(1, named("e", EntryKind::Dir)), (3, ignored_c)]
        );
        assert_eq!(changes.apply(&old).unwrap(), new);
        let same = ListingChanges::between(&new, &new, key).unwrap();
        assert!(same.is_empty());
        assert_eq!(same.apply(&new).unwrap(), new);
    }

    #[test]
    fn any_two_listings_are_changes_apart_in_any_order() {
        // A small generator, so a failure comes again.
        let mut seed = 0x9e37_79b9_u32;
        let mut next = move |most: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % most
        };
        for round in 0..2000 {
            let mut listing = || -> Vec<Entry> {
                let len = next(8);
                let mut entries: Vec<Entry> = (0..len)
                    .map(|_| {
                        let letter = b'a' + u8::try_from(next(6)).unwrap();
                        let name = char::from(letter).to_string();
                        match next(3) {
                            0 => named(&name, EntryKind::Dir),
                            1 => named(&name, EntryKind::File { executable: true }),
                            _ => file(&name),
                        }
                    })
                    .collect();
                // Mostly in order, with names twice, as names a disk can't
                // say in UTF-8 can be; now and then in none.
                if round % 7 != 0 {
                    entries.sort_by_key(key);
                }
                entries
            };
            let old = listing();
            let new = listing();
            let changes = ListingChanges::between(&old, &new, key).unwrap();
            assert_eq!(
                changes.apply(&old).as_ref(),
                Some(&new),
                "{old:?} to {new:?}"
            );
        }
    }

    #[test]
    fn marks_change_by_their_places_as_listings_do() {
        let mark = |path: &str, mark| (path.to_owned(), mark);
        let old = [
            mark("/p/a", GitMark::Modified),
            mark("/p/b", GitMark::Untracked),
            mark("/p/c", GitMark::Added),
        ];
        let new = [
            mark("/p/a", GitMark::Modified),
            mark("/p/c", GitMark::Modified),
            mark("/p/d", GitMark::Untracked),
        ];
        let key = |(path, _): &(String, GitMark)| path.clone();
        let changes = MarksChanges::between(&old, &new, key).unwrap();
        assert_eq!(changes.removed, [1, 2], "b gone, and c changed");
        assert_eq!(
            changes.added,
            [(1, mark("/p/c", GitMark::Modified)), (2, new[2].clone())]
        );
        assert_eq!(changes.apply(&old).unwrap(), new);
        assert!(changes.apply(&new[..1]).is_none(), "not changes to it");
    }

    #[test]
    fn changes_to_another_listing_are_refused() {
        let old = [file("a"), file("b")];
        let refused = |removed: Vec<u32>, added: Vec<(u32, Entry)>| {
            ListingChanges { removed, added }.apply(&old).is_none()
        };
        assert!(refused(vec![2], Vec::new()), "gone past the end");
        assert!(refused(vec![1, 0], Vec::new()), "out of order");
        assert!(refused(vec![1, 1], Vec::new()), "gone twice");
        assert!(refused(vec![0, 1, 2], Vec::new()), "more gone than there");
        assert!(
            refused(Vec::new(), vec![(5, file("c"))]),
            "put past the end"
        );
        assert!(
            refused(Vec::new(), vec![(1, file("c")), (0, file("d"))]),
            "put out of order"
        );
    }
}
