//! Buffers in the event loop: opening files into panes, sending each
//! window that attached to a buffer its edits, and making the edits windows
//! send. And having buffers parsed, and sending their highlights.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use clackatoo_protocol::{
    Author, BufferId, BufferUpdate, Change, Content, Edit, Error, Event, OpenFile, PaneId,
    Response, ServerMessage, frame,
};
use clackatoo_syntax::{Highlighted, Syntax};
use clackatoo_text::Highlights;
use mio::Token;

use super::language::Opened;
use super::{Daemon, Source};
use crate::buffer::{Buffer, Made};

/// A buffer, and the clients following it.
#[derive(Debug)]
pub(super) struct BufferSlot {
    pub(super) buffer: Buffer,
    pub(super) viewers: Vec<Token>,
    /// Its highlights, as of the last parse, moved along with each edit
    /// since.
    highlights: Highlights,
    /// Whether it's being parsed: its language has a pack.
    parsed: bool,
    /// How its language server knows it, while it's open in one.
    pub(super) server: Option<Opened>,
}

impl BufferSlot {
    pub(super) fn new(buffer: Buffer) -> Self {
        Self {
            buffer,
            viewers: Vec::new(),
            highlights: Highlights::new(),
            parsed: false,
            server: None,
        }
    }
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
        self.buffers.insert(id, BufferSlot::new(buffer));
        self.parse_buffer(id);
        self.open_in_server(id);
        Ok(id)
    }

    /// Starts sending `id` to the client at `token`, with a snapshot.
    pub(super) fn attach_buffer(&mut self, token: Token, id: BufferId) -> Result<(), Error> {
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        if !slot.viewers.contains(&token) {
            slot.viewers.push(token);
        }
        if !slot.parsed {
            // Its language may have been installed since it was opened.
            self.parse_buffer(id);
        }
        // Its server may have stopped, or it came through an upgrade,
        // which servers don't.
        self.open_in_server(id);
        self.send_snapshot(token, id);
        Ok(())
    }

    /// Sends the client at `token` all of buffer `id`: its text, then its
    /// highlights and diagnostics.
    fn send_snapshot(&mut self, token: Token, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let version = slot.buffer.version();
        let snapshot = BufferUpdate::Snapshot(slot.buffer.snapshot());
        let highlights = (!slot.highlights.spans().is_empty()).then(|| BufferUpdate::Highlights {
            version,
            start: 0,
            end: slot.buffer.text().len_bytes(),
            spans: slot.highlights.spans().to_vec(),
        });
        let diagnostics = (slot.server.as_ref())
            .filter(|opened| !opened.diagnostics.all().is_empty())
            .map(|opened| BufferUpdate::Diagnostics {
                version,
                diagnostics: opened.diagnostics.all().to_vec(),
            });
        self.tell(token, id, snapshot);
        for update in highlights.into_iter().chain(diagnostics) {
            self.tell(token, id, update);
        }
    }

    /// Stops sending `id` to the client at `token`.
    pub(super) fn detach_buffer(&mut self, token: Token, id: BufferId) {
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.viewers.retain(|&viewer| viewer != token);
        }
        self.forget_queries(token, Some(id));
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
                self.edited(id, version, &edit);
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
                self.send_snapshot(token, id);
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
        self.saved_in_server(id);
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
        self.edited(id, made.version, &made.edit);
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
        self.close_if_unwanted(buffer);
    }

    /// Answers [`Request::OpenHere`](clackatoo_protocol::Request::OpenHere):
    /// `pane`, which shows a file, shows `file` instead, and the file it
    /// showed closes if nothing else shows it and it has no changes to
    /// save.
    pub(super) fn open_here(&mut self, pane: PaneId, file: OpenFile) -> Result<Response, Error> {
        let Content::Buffer(old) = self.workspaces.check_pane(pane)? else {
            return Err(Error::NotAFile(pane));
        };
        let new = self.open_buffer(file, Some(pane))?;
        if new != old {
            self.workspaces.set_content(pane, Content::Buffer(new))?;
            self.workspaces_changed();
            self.close_if_unwanted(old);
        }
        Ok(Response::Done)
    }

    /// Closes `buffer` if no pane shows it and it has no changes to save.
    /// One with changes waits, unseen, to be opened again.
    fn close_if_unwanted(&mut self, buffer: BufferId) {
        let unwanted = !self.workspaces.shows(buffer)
            && (self.buffers.get(&buffer)).is_some_and(|slot| !slot.buffer.modified());
        if unwanted && let Some(slot) = self.buffers.remove(&buffer) {
            log::info!("buffer {buffer} closed: {}", slot.buffer.path().display());
            if let Some(syntax) = &self.syntax
                && slot.parsed
            {
                syntax.close(key(buffer));
            }
            self.closed_in_server(slot.buffer.path(), slot.server);
        }
    }

    /// Has buffer `id` parsed, if its language has a pack, starting the
    /// thread that parses if it's the first.
    pub(super) fn parse_buffer(&mut self, id: BufferId) {
        let (Some(languages), Some(slot)) = (&mut self.languages, self.buffers.get_mut(&id)) else {
            return;
        };
        let Some(pack) = languages.find(slot.buffer.path()) else {
            return;
        };
        if self.syntax.is_none() {
            let waker = Arc::clone(&self.waker);
            let wake = move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for highlights failed: {err}");
                }
            };
            match Syntax::start(wake) {
                Ok(syntax) => self.syntax = Some(syntax),
                Err(err) => {
                    log::error!("buffers won't be parsed: {err}");
                    self.languages = None;
                    return;
                }
            }
        }
        let Some(syntax) = &self.syntax else {
            return;
        };
        log::info!("buffer {id}: parsed as {}", pack.name);
        let (text, version) = (slot.buffer.text().clone(), slot.buffer.version());
        syntax.open(key(id), pack, text, version);
        slot.parsed = true;
    }

    /// Tells buffer `id`'s server about `edit`, which made `version`, and
    /// moves its highlights along with it, having it parsed again.
    fn edited(&mut self, id: BufferId, version: u64, edit: &Edit) {
        self.edited_in_server(id, version, edit);
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if !slot.parsed {
            return;
        }
        slot.highlights.map(edit);
        if let Some(syntax) = &self.syntax {
            syntax.edit(key(id), version, edit.clone(), slot.buffer.text().clone());
        }
    }

    /// Sends the highlights parses have found since last asked to every
    /// client following their buffers.
    pub(super) fn take_highlights(&mut self) {
        let Some(syntax) = &self.syntax else {
            return;
        };
        let found: Vec<Highlighted> = syntax.take().collect();
        for found in found {
            let Some(id) = u32::try_from(found.buffer).ok().map(BufferId) else {
                continue;
            };
            let Some(slot) = self.buffers.get_mut(&id) else {
                continue;
            };
            // Edits made while it was parsing move it along, as they moved
            // what was there.
            let Some(edits) = slot.buffer.edits_since(found.version) else {
                log::warn!("buffer {id}: highlights for a version long gone");
                continue;
            };
            let range = (slot.highlights).replace_since(found.range, found.spans, edits);
            let update = BufferUpdate::Highlights {
                version: slot.buffer.version(),
                start: range.start,
                end: range.end,
                spans: slot.highlights.within(range.start, range.end).to_vec(),
            };
            self.tell_viewers(id, None, &update);
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
    pub(super) fn tell_viewers(
        &mut self,
        id: BufferId,
        except: Option<Token>,
        update: &BufferUpdate,
    ) {
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
    pub(super) fn tell(&mut self, token: Token, id: BufferId, update: BufferUpdate) {
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

/// Buffer `id`, as the thread parsing buffers knows it.
fn key(id: BufferId) -> clackatoo_syntax::Key {
    u64::from(id.0)
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
