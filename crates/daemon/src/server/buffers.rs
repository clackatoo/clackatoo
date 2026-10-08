//! Buffers in the event loop: opening files into panes, sending each
//! window that attached to a buffer its edits, and making the edits windows
//! send.

use std::path::{Component, Path, PathBuf};

use clackatoo_protocol::{
    Author, BufferId, BufferUpdate, Change, Error, Event, OpenFile, PaneId, Response,
    ServerMessage, frame,
};
use mio::Token;

use super::{Daemon, Source};
use crate::buffer::{Buffer, Made};

/// A buffer, and the clients following it.
#[derive(Debug)]
pub(super) struct BufferSlot {
    pub(super) buffer: Buffer,
    pub(super) viewers: Vec<Token>,
}

impl Daemon {
    /// The buffer for `file`: the one already open for it, or a new one.
    /// A relative path is from `file.cwd`, or else the directory of the
    /// pane `beside`, or else the home directory.
    pub(super) fn open_buffer(
        &mut self,
        file: OpenFile,
        beside: Option<PaneId>,
    ) -> Result<BufferId, Error> {
        let home = &self.config.home;
        let dir = (file.cwd.map(PathBuf::from))
            .or_else(|| {
                beside
                    .and_then(|pane| self.working_directory(pane))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| home.clone());
        let path = resolve(&file.path, &dir, home);
        if let Some((&id, _)) = (self.buffers.iter()).find(|(_, slot)| slot.buffer.path() == path) {
            return Ok(id);
        }
        let buffer = Buffer::open(path, dir)?;
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        log::info!("buffer {id}: {}", buffer.path().display());
        self.buffers.insert(
            id,
            BufferSlot {
                buffer,
                viewers: Vec::new(),
            },
        );
        Ok(id)
    }

    /// Starts sending `id` to the client at `token`, with a snapshot.
    pub(super) fn attach_buffer(&mut self, token: Token, id: BufferId) -> Result<(), Error> {
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        if !slot.viewers.contains(&token) {
            slot.viewers.push(token);
        }
        let snapshot = BufferUpdate::Snapshot(slot.buffer.snapshot());
        self.tell(token, id, snapshot);
        Ok(())
    }

    /// Stops sending `id` to the client at `token`.
    pub(super) fn detach_buffer(&mut self, token: Token, id: BufferId) {
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.viewers.retain(|&viewer| viewer != token);
        }
    }

    /// Makes a change the client at `token` sent to buffer `id`, and tells
    /// it and every other client following the buffer.
    pub(super) fn serve_edit(&mut self, token: Token, id: BufferId, seq: u64, change: Change) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            log::debug!("an edit to buffer {id}, which isn't open");
            return;
        };
        if !slot.viewers.contains(&token) {
            log::debug!("an edit to buffer {id} from a client not following it");
            return;
        }
        let buffer = &mut slot.buffer;
        let made = match change {
            Change::Edit { edit, base, step } => buffer
                .edit(edit, base, step, Author::User)
                .map(|made| (Some(made), true)),
            Change::Undo | Change::Redo => buffer
                .reverse(Author::User, change == Change::Redo)
                .map(|made| (made, false)),
        };
        match made {
            Ok((Some(Made { version, edit }), acked)) => {
                let answer = if acked {
                    BufferUpdate::Acked { version, seq }
                } else {
                    BufferUpdate::Edited {
                        version,
                        edit: edit.clone(),
                        author: Author::User,
                        answers: Some(seq),
                    }
                };
                self.tell(token, id, answer);
                let others = BufferUpdate::Edited {
                    version,
                    edit,
                    author: Author::User,
                    answers: None,
                };
                self.tell_viewers(id, Some(token), &others);
                self.note_modified(id);
            }
            Ok((None, _)) => self.tell(token, id, BufferUpdate::NoChange(seq)),
            Err(err) => {
                // The client's copy is wrong somehow: start it again.
                log::warn!("buffer {id}: an edit couldn't be made: {err}");
                let snapshot = BufferUpdate::Snapshot(slot.buffer.snapshot());
                self.tell(token, id, snapshot);
            }
        }
    }

    /// Answers [`Request::Save`](clackatoo_protocol::Request::Save).
    pub(super) fn save_buffer(&mut self, id: BufferId, force: bool) -> Result<Response, Error> {
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        if let Some(modified) = slot.buffer.save(force)? {
            self.tell_viewers(id, None, &BufferUpdate::Modified(modified));
        }
        log::info!("buffer {id} saved");
        Ok(Response::Done)
    }

    /// Answers [`Request::Discard`](clackatoo_protocol::Request::Discard):
    /// the file read again, as an edit every client is sent.
    pub(super) fn discard_buffer(&mut self, id: BufferId) -> Result<Response, Error> {
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        let edit = slot.buffer.discard()?;
        let base = slot.buffer.version();
        let made = slot
            .buffer
            .edit(edit, base, true, Author::User)
            .map_err(|err| {
                log::error!("buffer {id}: discarding its changes failed: {err}");
                Error::OpenFailed {
                    path: slot.buffer.path().display().to_string(),
                    reason: err.to_string(),
                }
            })?;
        let update = BufferUpdate::Edited {
            version: made.version,
            edit: made.edit,
            author: Author::User,
            answers: None,
        };
        self.tell_viewers(id, None, &update);
        self.note_modified(id);
        Ok(Response::Done)
    }

    /// Closes `pane`, which shows `buffer`. The buffer goes too if no
    /// other pane shows it, unless it has changes that aren't saved: then
    /// it waits, unseen, to be opened again.
    pub(super) fn close_buffer_pane(&mut self, pane: PaneId, buffer: BufferId) {
        if self.workspaces.remove_pane(pane) {
            self.workspaces_changed();
        }
        let unwanted = !self.workspaces.shows(buffer)
            && (self.buffers.get(&buffer)).is_some_and(|slot| !slot.buffer.modified());
        if unwanted && let Some(slot) = self.buffers.remove(&buffer) {
            log::info!("buffer {buffer} closed: {}", slot.buffer.path().display());
        }
    }

    /// Tells every client following `id` whether it's modified, if that's
    /// changed.
    fn note_modified(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if let Some(modified) = slot.buffer.check_modified() {
            self.tell_viewers(id, None, &BufferUpdate::Modified(modified));
        }
    }

    /// Sends `update` about buffer `id` to every client following it, but
    /// `except`.
    fn tell_viewers(&mut self, id: BufferId, except: Option<Token>, update: &BufferUpdate) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let viewers: Vec<Token> = (slot.viewers.iter().copied())
            .filter(|&viewer| Some(viewer) != except)
            .collect();
        for viewer in viewers {
            self.tell(viewer, id, update.clone());
        }
    }

    /// Sends `update` about buffer `id` to the client at `token`.
    fn tell(&mut self, token: Token, id: BufferId, update: BufferUpdate) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        let message = ServerMessage::Event(Event::Buffer { buffer: id, update });
        match frame::encode(&message) {
            Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
            Err(err) => {
                log::error!("encoding a buffer's update failed: {err}");
                return;
            }
        }
        self.flush_client(token);
    }
}

/// `path` made absolute, from `dir` if it's relative, or from `home` if it
/// starts with `~/`, with its links followed as far as they exist.
fn resolve(path: &str, dir: &Path, home: &Path) -> PathBuf {
    let path = match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => dir.join(path),
    };
    if let Ok(path) = path.canonicalize() {
        return path;
    }
    // A new file: its directory, if that exists, is as real as it gets.
    let lexical = lexical(&path);
    match (lexical.parent(), lexical.file_name()) {
        (Some(parent), Some(name)) => match parent.canonicalize() {
            Ok(parent) => parent.join(name),
            Err(_) => lexical,
        },
        _ => lexical,
    }
}

/// `path` with its `.` and `..` worked out, without looking at the disk.
fn lexical(path: &Path) -> PathBuf {
    let mut made = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                made.pop();
            }
            other => made.push(other),
        }
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_made_absolute_and_real() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::create_dir(real.join("src")).unwrap();
        std::fs::write(real.join("src/main.rs"), "").unwrap();
        std::os::unix::fs::symlink(real.join("src"), real.join("link")).unwrap();
        let home = Path::new("/nowhere");
        assert_eq!(
            resolve("src/main.rs", &real, home),
            real.join("src/main.rs")
        );
        assert_eq!(
            resolve("link/main.rs", &real, home),
            real.join("src/main.rs")
        );
        assert_eq!(
            resolve("./link/../src/new.rs", &real, home),
            real.join("src/new.rs")
        );
        let absolute = real.join("a").display().to_string();
        assert_eq!(resolve(&absolute, Path::new("/x"), home), real.join("a"));
        assert_eq!(
            resolve("no/such/dir/a", &real, home),
            real.join("no/such/dir/a")
        );
        assert_eq!(resolve("~/a", &real, &real), real.join("a"));
    }
}
