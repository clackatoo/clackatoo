//! Git signs in the event loop: having each buffer whose file is in a git
//! work tree compared with the file's text in the index, on the thread
//! that does it (see [`crate::git`]), and sending the hunks it finds to
//! every client following the buffer; watching each repository's index,
//! so staging from a terminal shows at once; and staging hunks clients
//! ask to.
//!
//! The index is watched with inotify, on the repository's git directory
//! rather than the index file: git writes a new index beside the old and
//! renames it over it, so a watch on the file would stop at the first
//! change. A watch costs nothing until something changes, where checking
//! the index's time on a timer would wake the daemon forever.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use clackatoo_protocol::{Answer, BufferId, BufferUpdate};
use clackatoo_text::{Bias, Hunks};
use mio::Token;
use rustix::fs::inotify;
use rustix::io::Errno;

use super::{Daemon, Source};
use crate::git::{Found, Git};

/// The thread comparing buffers with git, once a buffer needs it, the
/// indexes watched, and the stages asked for.
#[derive(Debug, Default)]
pub(super) struct Signs {
    git: Option<Git>,
    /// The inotify instance indexes are watched with, once one is, and
    /// its token.
    inotify: Option<(OwnedFd, Token)>,
    /// The git directory each watch is on.
    watches: HashMap<i32, PathBuf>,
    /// Stages on their way, by the number the thread answers them with.
    stages: HashMap<u64, Staging>,
    next_ticket: u64,
}

/// A stage a client asked for.
#[derive(Debug)]
struct Staging {
    token: Token,
    buffer: BufferId,
    id: u64,
}

impl Daemon {
    /// Has buffer `id` compared with git, if its file is in a work tree,
    /// starting the thread that compares if it's the first. One with no
    /// file yet has nothing to compare.
    pub(super) fn compare_buffer(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if slot.compared || slot.buffer.is_blank() {
            return;
        }
        if self.signs.git.is_none() {
            let waker = Arc::clone(&self.waker);
            let wake = move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for git's signs failed: {err}");
                }
            };
            match Git::start(wake) {
                Ok(git) => self.signs.git = Some(git),
                Err(err) => {
                    log::error!("buffers won't be compared with git: {err}");
                    return;
                }
            }
        }
        let Some(git) = &self.signs.git else { return };
        let (text, version) = (slot.buffer.text().clone(), slot.buffer.version());
        git.open(key(id), slot.buffer.path().to_owned(), text, version);
        slot.compared = true;
    }

    /// Moves buffer `id`'s hunks along with `edit`, which made `version`,
    /// and has it compared again once edits pause.
    pub(super) fn edited_in_git(&mut self, id: BufferId, edit: &clackatoo_protocol::Edit) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if !slot.compared {
            return;
        }
        slot.hunks.map(edit);
        // Even before the thread says the file's in a work tree: the text
        // it compares must be the buffer's.
        if let Some(git) = &self.signs.git {
            git.edit(key(id), slot.buffer.text().clone(), slot.buffer.version());
        }
    }

    /// Stops comparing buffer `id`, which has closed and was in the
    /// repository whose git directory is `git_dir`, if any, no longer
    /// watching its index if no other buffer is in it.
    pub(super) fn closed_in_git(&mut self, id: BufferId, git_dir: Option<PathBuf>) {
        if let Some(git) = &self.signs.git {
            git.close(key(id));
        }
        let Some(git_dir) = git_dir else { return };
        if (self.buffers.values()).any(|slot| slot.git_dir.as_ref() == Some(&git_dir)) {
            return;
        }
        let watch = (self.signs.watches.iter()).find(|(_, dir)| **dir == git_dir);
        if let (Some((&watch, _)), Some((inotify, _))) = (watch, &self.signs.inotify) {
            self.signs.watches.remove(&watch);
            if let Err(err) = inotify::remove_watch(inotify, watch) {
                log::debug!("no longer watching {}: {err}", git_dir.display());
            }
        }
    }

    /// Answers [`ClientMessage::Stage`](clackatoo_protocol::ClientMessage::Stage):
    /// has the hunk at byte `at` of buffer `buffer`'s text at `version`
    /// staged, for the client at `token`, which numbered it `id`.
    pub(super) fn serve_stage(
        &mut self,
        token: Token,
        buffer: BufferId,
        (id, version, at): (u64, u64, usize),
    ) {
        let Some(slot) = self.buffers.get(&buffer) else {
            return;
        };
        if !slot.viewers.contains(&token) {
            log::debug!("a stage in buffer {buffer} from a client not following it");
            return;
        }
        let now = slot.buffer.version();
        let failed = |why: &str| BufferUpdate::Answer {
            id,
            version: now,
            answer: Answer::Failed(why.to_owned()),
        };
        let (Some(git), Some(_)) = (&self.signs.git, &slot.git_dir) else {
            return self.tell(token, buffer, failed("this file isn't in a git repository"));
        };
        // The place, moved along with edits made since the client's text,
        // such as an agent's.
        let Some(edits) = slot.buffer.edits_since(version) else {
            return self.tell(token, buffer, failed("the file has changed since"));
        };
        let at = edits.fold(at, |at, edit| edit.map(at, Bias::Before));
        let text = slot.buffer.text().clone();
        let ticket = self.signs.next_ticket;
        self.signs.next_ticket += 1;
        git.stage(key(buffer), (text, now), at, ticket);
        let staging = Staging { token, buffer, id };
        self.signs.stages.insert(ticket, staging);
    }

    /// Forgets the stages the client at `token` asked for in `buffer`, or
    /// in any buffer, as it's stopped following it, or gone. They're made
    /// all the same, but not answered.
    pub(super) fn forget_stages(&mut self, token: Token, buffer: Option<BufferId>) {
        (self.signs.stages).retain(|_, staging| {
            staging.token != token || buffer.is_some_and(|buffer| buffer != staging.buffer)
        });
    }

    /// The update a client attaching to buffer `id` is sent with its
    /// snapshot, if it has hunks.
    pub(super) fn hunks_update(&self, id: BufferId) -> Option<BufferUpdate> {
        let slot = self.buffers.get(&id)?;
        (!slot.hunks.is_empty()).then(|| BufferUpdate::Hunks {
            version: slot.buffer.version(),
            hunks: slot.hunks.all().to_vec(),
        })
    }

    /// Acts on what the thread comparing buffers has found since last
    /// asked: watches the indexes of repositories buffers are in, sends
    /// clients the hunks that changed, and answers stages.
    pub(super) fn take_signs(&mut self) {
        let Some(git) = &self.signs.git else { return };
        let found: Vec<Found> = git.take().collect();
        for found in found {
            match found {
                Found::Repository { buffer, git_dir } => {
                    let Some(id) = buffer_id(buffer) else {
                        continue;
                    };
                    // A buffer closed meanwhile wants no watch, which
                    // nothing would take away.
                    if let Some(slot) = self.buffers.get_mut(&id) {
                        slot.git_dir = Some(git_dir.clone());
                        self.watch_index(&git_dir);
                    }
                }
                Found::Hunks {
                    buffer,
                    version,
                    hunks,
                } => {
                    let Some(id) = buffer_id(buffer) else {
                        continue;
                    };
                    self.signed(id, version, hunks);
                }
                Found::Staged { ticket, answer } => {
                    let Some(Staging { token, buffer, id }) = self.signs.stages.remove(&ticket)
                    else {
                        continue;
                    };
                    let Some(slot) = self.buffers.get(&buffer) else {
                        continue;
                    };
                    let version = slot.buffer.version();
                    let update = BufferUpdate::Answer {
                        id,
                        version,
                        answer,
                    };
                    self.tell(token, buffer, update);
                }
            }
        }
    }

    /// Takes buffer `id`'s hunks against its text at `version`, moving
    /// them along with edits made since, and sends them to every client
    /// following it if they aren't what it has.
    fn signed(&mut self, id: BufferId, version: u64, mut hunks: Hunks) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let Some(edits) = slot.buffer.edits_since(version) else {
            log::warn!("buffer {id}: hunks for a version long gone");
            return;
        };
        for edit in edits {
            hunks.map(edit);
        }
        // What clients have was moved along with the same edits: after
        // most pauses, typing within a hunk leaves it as it was.
        if hunks == slot.hunks {
            return;
        }
        slot.hunks = hunks;
        let update = BufferUpdate::Hunks {
            version: slot.buffer.version(),
            hunks: slot.hunks.all().to_vec(),
        };
        self.tell_viewers(id, None, &update);
    }

    /// Watches the index of the repository whose git directory is
    /// `git_dir`, if it isn't watched already.
    fn watch_index(&mut self, git_dir: &PathBuf) {
        if self.signs.watches.values().any(|dir| dir == git_dir) {
            return;
        }
        if self.signs.inotify.is_none() {
            let flags = inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK;
            let made = inotify::init(flags).map_err(std::io::Error::from);
            let registered = made.and_then(|fd| {
                let token = self.register_fd(Some(fd.as_raw_fd()), Source::Index)?;
                Ok((fd, token.expect("an fd was given")))
            });
            match registered {
                Ok(inotify) => self.signs.inotify = Some(inotify),
                Err(err) => {
                    log::warn!("indexes won't be watched: {err}");
                    return;
                }
            }
        }
        let Some((inotify, _)) = &self.signs.inotify else {
            return;
        };
        // A new index is renamed over the old; some tools write it in place.
        let flags = inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::CREATE
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::ONLYDIR;
        match inotify::add_watch(inotify, git_dir, flags) {
            Ok(watch) => {
                self.signs.watches.insert(watch, git_dir.clone());
            }
            Err(err) => log::warn!("{} won't be watched: {err}", git_dir.display()),
        }
    }

    /// Reads what inotify says has changed in the git directories watched,
    /// and has the buffers in those whose index changed compared again.
    pub(super) fn read_index_changes(&mut self) {
        let Some((inotify, _)) = &self.signs.inotify else {
            return;
        };
        let mut changed: Vec<PathBuf> = Vec::new();
        let mut gone: Vec<i32> = Vec::new();
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(inotify, &mut buffer);
        // Read until there's nothing left: the loop hears of more only
        // once something new comes.
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::INTR) => continue,
                Err(Errno::WOULDBLOCK) => break,
                Err(err) => {
                    log::warn!("reading what changed in git directories failed: {err}");
                    break;
                }
            };
            if event.events().contains(inotify::ReadFlags::QUEUE_OVERFLOW) {
                // Changes were lost: any index may have changed.
                changed = self.signs.watches.values().cloned().collect();
                continue;
            }
            if event.events().contains(inotify::ReadFlags::IGNORED) {
                gone.push(event.wd());
                continue;
            }
            let index = event
                .file_name()
                .is_some_and(|name| name.to_bytes() == b"index");
            if let Some(git_dir) = self.signs.watches.get(&event.wd())
                && index
                && !changed.contains(git_dir)
            {
                changed.push(git_dir.clone());
            }
        }
        for watch in gone {
            // Its directory went, as a repository deleted does.
            self.signs.watches.remove(&watch);
        }
        if let Some(git) = &self.signs.git {
            for git_dir in changed {
                git.index_changed(git_dir);
            }
        }
    }
}

/// Buffer `id`, as the thread comparing buffers knows it.
fn key(id: BufferId) -> crate::git::Key {
    u64::from(id.0)
}

/// The buffer the thread comparing buffers knows as `key`.
fn buffer_id(key: crate::git::Key) -> Option<BufferId> {
    u32::try_from(key).ok().map(BufferId)
}
