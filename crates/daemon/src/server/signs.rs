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
//! the index's time on a timer would wake the daemon forever. The watch is
//! on the daemon's one instance, which [`super::explorers`] reads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use clackatoo_protocol::{Answer, BufferId, BufferUpdate};
use clackatoo_text::{Bias, Hunks};
use mio::Token;

use super::Daemon;
use crate::git::{Found, Git};

/// The thread comparing buffers with git, once a buffer needs it, and
/// the stages asked for.
#[derive(Debug, Default)]
pub(super) struct Signs {
    git: Option<Git>,
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
        self.remove_index_watch(&git_dir);
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
                        self.add_index_watch(&git_dir);
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

    /// Watches the indexes of the repositories buffers are in again, as
    /// they couldn't be, and has those buffers compared again, as an index
    /// may have changed meanwhile.
    pub(super) fn watch_indexes_again(&mut self) {
        let mut git_dirs: Vec<PathBuf> = (self.buffers.values())
            .filter_map(|slot| slot.git_dir.clone())
            .collect();
        git_dirs.sort();
        git_dirs.dedup();
        for git_dir in &git_dirs {
            self.add_index_watch(git_dir);
        }
        self.indexes_changed(git_dirs);
    }

    /// Has the buffers in the repositories whose git directories are
    /// `git_dirs` compared again, as their indexes have changed.
    pub(super) fn indexes_changed(&mut self, git_dirs: Vec<PathBuf>) {
        if let Some(git) = &self.signs.git {
            for git_dir in git_dirs {
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
