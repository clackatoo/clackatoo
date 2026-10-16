//! Buffers in the event loop: opening files into panes, sending each
//! window that attached to a buffer its edits, and making the edits windows
//! send. And having buffers parsed, and sending their highlights.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use clackatoo_protocol::{
    Author, BufferId, BufferSnapshot, BufferUpdate, Change, Content, Edit, Error, Event, OpenBlank,
    OpenFile, OpenFiles, PROTOCOL_VERSION, PaneId, Response, Resume, ServerMessage, frame, wire,
};
use clackatoo_syntax::{Found, Highlighted, Syntax, pack};
use clackatoo_text::{AgentHunks, Highlights, Hunks, Maker, Rope, Span};
use mio::Token;

use super::language::Opened;
use super::recovery::Recovered;
use super::{Daemon, Source};
use crate::buffer::{Buffer, DiskStamp, Made, Restamp};

/// A buffer, and the clients following it.
#[derive(Debug)]
pub(super) struct BufferSlot {
    pub(super) buffer: Buffer,
    pub(super) viewers: Vec<Token>,
    /// Its highlights, as of the last parse, moved along with each edit
    /// since.
    highlights: Highlights,
    /// Whether it's being parsed: its language has a pack.
    pub(super) parsed: bool,
    /// The characters its words are made of besides letters, digits and
    /// `_`, as its language says, as of when its pack was last looked for:
    /// what [`BufferUpdate::Language`] tells windows.
    word: String,
    /// How its language server knows it, while it's open in one.
    pub(super) server: Option<Opened>,
    /// Whether it's being compared with its file's text in the index.
    pub(super) compared: bool,
    /// The git directory of the repository its file is in, once the
    /// thread comparing it says.
    pub(super) git_dir: Option<PathBuf>,
    /// What it has added, changed and deleted since the file's text in
    /// the index, as of the last comparison, moved along with each edit
    /// since.
    pub(super) hunks: Hunks,
    /// The lines agents have changed, and whether they've been reviewed,
    /// moved along with each edit.
    pub(super) agents: AgentHunks,
    /// Whether the directory its file is in is watched for changes.
    pub(super) watched: bool,
    /// Whether something other than an agent has changed its file on
    /// disk, in a way it hasn't taken (see [`stale`](Self::stale)).
    outside: Option<Outside>,
    /// The file as it was when it last couldn't be read again, as when
    /// it's no longer UTF-8: windows aren't asked to read it again until
    /// it changes.
    pub(super) unreadable: Option<DiskStamp>,
    /// For each window that has sent it changes, the last one dealt with,
    /// made or not: a window that lost its connection learns from it which
    /// of its changes arrived (see
    /// [`BufferSnapshot::applied`](clackatoo_protocol::BufferSnapshot)).
    pub(super) applied: HashMap<u64, WindowChange>,
    /// What came back to it from a recovery file as the daemon started,
    /// while that's still to be dealt with.
    pub(super) recovered: Recovered,
}

/// The last change a window sent a buffer that the daemon dealt with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WindowChange {
    /// The window's number for it.
    pub(super) seq: u64,
    /// The version it made, if it made one.
    pub(super) landed: Option<u64>,
}

/// What a buffer knew of its file when something other than an agent
/// changed it, in a way the buffer hasn't taken. It holds as long as
/// that's still what the buffer knows, so reading or writing the file
/// again ends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outside {
    /// The file as last read or written, if it existed.
    knew: Option<DiskStamp>,
}

impl BufferSlot {
    pub(super) fn new(buffer: Buffer) -> Self {
        Self {
            buffer,
            viewers: Vec::new(),
            highlights: Highlights::new(),
            parsed: false,
            word: String::new(),
            server: None,
            compared: false,
            git_dir: None,
            hunks: Hunks::new(),
            agents: AgentHunks::new(),
            watched: false,
            outside: None,
            unreadable: None,
            applied: HashMap::new(),
            recovered: Recovered::default(),
        }
    }

    /// Its text, and its highlights over what a preview of lines `first`
    /// to `last` shows (see [`crate::find::preview_spans`]).
    pub(super) fn preview(&self, first: usize, last: usize) -> (Rope, Vec<Span>) {
        let text = self.buffer.text();
        let spans = crate::find::preview_spans(text, &self.highlights, first, last);
        (text.clone(), spans)
    }

    /// Whether its file has changed on disk, not only by an agent, since
    /// it was last read or written: changes found on disk since aren't
    /// all any one agent's.
    pub(super) fn stale(&self) -> bool {
        (self.outside).is_some_and(|outside| outside.knew == self.buffer.disk())
    }

    /// Notes that something other than an agent has changed its file on
    /// disk, in a way it hasn't taken: it's [`stale`](Self::stale) until
    /// it reads or writes the file again.
    pub(super) fn changed_outside(&mut self) {
        self.outside = Some(Outside {
            knew: self.buffer.disk(),
        });
    }
}

impl Daemon {
    /// What a pane opening `file` shows: the file, in its buffer, or if
    /// it's a folder, the folder's listing, made from an explorer of its
    /// own, rooted there. A relative path is from where
    /// [`open_buffer`](Self::open_buffer) says.
    pub(super) fn open_content(
        &mut self,
        file: OpenFile,
        beside: Option<PaneId>,
    ) -> Result<Content, Error> {
        let dir = self.opened_from(file.cwd.as_deref(), beside);
        let path = resolve(&file.path, &dir, &self.config.home);
        // A stat, as reading a file here is: neither waits on anything
        // but the disk.
        if path.is_dir()
            && let Some(dir) = path.to_str()
        {
            let open = OpenFiles {
                dir: Some(dir.to_owned()),
            };
            return self.new_files(open, beside).map(Content::Folder);
        }
        // A path ending `/` is a folder's, which a file isn't made at.
        if file.path.ends_with('/') {
            return Err(Error::OpenFailed {
                path: file.path,
                reason: "it isn't a folder".to_owned(),
            });
        }
        self.open_buffer(file, beside).map(Content::Buffer)
    }

    /// The directory a relative path is from: `cwd`, or else the directory
    /// of the pane `beside`, or else the home directory. A `cwd` that isn't
    /// absolute is from nowhere in particular, so it's passed over.
    fn opened_from(&self, cwd: Option<&str>, beside: Option<PaneId>) -> PathBuf {
        (cwd.map(PathBuf::from))
            .filter(|cwd| cwd.is_absolute())
            .or_else(|| {
                beside
                    .and_then(|pane| self.working_directory(pane))
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| self.config.home.clone())
    }

    /// The buffer for `file`: the one already open for it, or a new one.
    /// A relative path is from `file.cwd`, or else the directory of the
    /// pane `beside`, or else the home directory.
    pub(super) fn open_buffer(
        &mut self,
        file: OpenFile,
        beside: Option<PaneId>,
    ) -> Result<BufferId, Error> {
        let OpenFile { path, cwd } = file;
        let dir = self.opened_from(cwd.as_deref(), beside);
        let path = resolve(&path, &dir, &self.config.home);
        if let Some((&id, _)) = (self.buffers.iter()).find(|(_, slot)| slot.buffer.path() == path) {
            return Ok(id);
        }
        let buffer = Buffer::open(path, dir)?;
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        log::info!("buffer {id}: {}", buffer.path().display());
        self.buffers.insert(id, BufferSlot::new(buffer));
        self.opened_in_review(id);
        self.parse_buffer(id);
        self.open_in_server(id);
        self.compare_buffer(id);
        self.watch_buffer(id);
        Ok(id)
    }

    /// A new empty buffer with no file yet, never one already open, as
    /// each `:enew` is a buffer of its own. Its name, once it's given one,
    /// is from `blank.cwd`, or else the directory of the pane `beside`, or
    /// else the home directory. With no file, it has no language, git or
    /// watch until it's named (see [`save_as`](Self::save_as)).
    pub(super) fn open_blank(&mut self, blank: &OpenBlank, beside: Option<PaneId>) -> BufferId {
        let dir = self.opened_from(blank.cwd.as_deref(), beside);
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        log::info!("buffer {id}: blank, in {}", dir.display());
        self.buffers.insert(id, BufferSlot::new(Buffer::blank(dir)));
        // What its words are made of, as for a file with no language.
        self.find_words(id);
        id
    }

    /// Answers [`Request::BlankHere`](clackatoo_protocol::Request::BlankHere):
    /// `pane`, which shows a file or a folder, shows a new empty buffer
    /// with no file yet in place of it, which goes as
    /// [`open_here`](Self::open_here) says, and is refused as it is.
    pub(super) fn blank_here(
        &mut self,
        pane: PaneId,
        blank: &OpenBlank,
    ) -> Result<Response, Error> {
        let old = self.workspaces.check_pane(pane)?;
        if !matches!(old, Content::Buffer(_) | Content::Folder(_)) {
            return Err(Error::NotAFile(pane));
        }
        if self.loses_blank(&[pane]) {
            return Err(blank_kept(PROTOCOL_VERSION));
        }
        let new = Content::Buffer(self.open_blank(blank, Some(pane)));
        self.workspaces.set_content(pane, new)?;
        self.workspaces_changed();
        match old {
            Content::Buffer(old) => self.close_if_unwanted(old),
            Content::Folder(files) => self.close_files(files),
            _ => {}
        }
        Ok(Response::Done)
    }

    /// Whether closing `panes`, or showing something else in them, would
    /// lose a buffer with no file yet that has changes: one no other pane
    /// shows, which nothing could open again, as a file's path opens a
    /// file's buffer kept unseen.
    pub(super) fn loses_blank(&self, panes: &[PaneId]) -> bool {
        if panes.is_empty() {
            return false;
        }
        let workspaces = self.workspaces.get();
        let all: Vec<_> = (workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .collect();
        let shown_elsewhere = |buffer| {
            (all.iter())
                .any(|pane| !panes.contains(&pane.id) && pane.content == Content::Buffer(buffer))
        };
        (all.iter())
            .filter(|pane| panes.contains(&pane.id))
            .filter_map(|pane| pane.content.buffer())
            .any(|buffer| {
                (self.buffers.get(&buffer))
                    .is_some_and(|slot| slot.buffer.is_blank() && slot.buffer.modified())
                    && !shown_elsewhere(buffer)
            })
    }

    /// Answers [`Request::SaveAs`](clackatoo_protocol::Request::SaveAs):
    /// writes buffer `id`, which has no file yet, to `path`, from the
    /// directory it was opened in, and has it edit that file from then on,
    /// as though it had opened it. Refused, with nothing changed, for a
    /// buffer with a file, a path another buffer edits, a folder, or
    /// unless `force`, a file that's there. The client at `token`, which
    /// asked, is offered its language's install, as one attaching is.
    pub(super) fn save_as(
        &mut self,
        token: Token,
        id: BufferId,
        path: &str,
        force: bool,
    ) -> Result<Response, Error> {
        let slot = self.buffers.get(&id).ok_or(Error::NoSuchBuffer(id))?;
        let refused = |to: &Path, reason: &str| Error::SaveFailed {
            path: to.display().to_string(),
            reason: reason.to_owned(),
        };
        if !slot.buffer.is_blank() {
            return Err(refused(slot.buffer.path(), "it has a name already"));
        }
        let to = resolve(path, slot.buffer.dir(), &self.config.home);
        if path.is_empty() || path.ends_with('/') || to.is_dir() {
            return Err(refused(&to, "it's a folder"));
        }
        if (self.buffers.values()).any(|slot| slot.buffer.path() == to) {
            return Err(refused(&to, "it's open already: :e it"));
        }
        // A stat, as opening a file is.
        if !force && to.symlink_metadata().is_ok() {
            return Err(refused(&to, "it's there already: :w! NAME writes over it"));
        }
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        slot.buffer.moved_to(to.clone(), Restamp::To(None));
        // Written over only if asked to, the file there having been looked
        // at just now.
        let saved = match slot.buffer.save(force) {
            Ok(saved) => saved,
            Err(err) => {
                slot.buffer.moved_to(PathBuf::new(), Restamp::To(None));
                return Err(err);
            }
        };
        log::info!("buffer {id}: named {}", to.display());
        let shown = to.display().to_string();
        self.tell_viewers(id, None, &BufferUpdate::Moved(shown));
        if let Some(modified) = saved {
            self.tell_viewers(id, None, &BufferUpdate::Modified(modified));
        }
        self.files_written(&to);
        self.recovery_touched(id);
        // As a file opened is, now it has one.
        self.opened_in_review(id);
        self.parse_buffer(id);
        self.open_in_server(id);
        self.compare_buffer(id);
        self.watch_buffer(id);
        self.offer_install(token, id);
        Ok(Response::Done)
    }

    /// Starts sending `id` to the client at `token`, with a snapshot, or,
    /// for a window attaching again with `resume`, the edits since what it
    /// knew, if they're still to hand.
    pub(super) fn attach_buffer(
        &mut self,
        token: Token,
        id: BufferId,
        resume: Option<Resume>,
    ) -> Result<(), Error> {
        let parsed = self.buffers.get(&id).ok_or(Error::NoSuchBuffer(id))?.parsed;
        // Its language may have been installed since it was opened, or its
        // pack changed: looked at before the client follows it, so what
        // its words are made of comes to it after its text, not before.
        if parsed {
            self.find_words(id);
        } else {
            self.parse_buffer(id);
        }
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        if !slot.viewers.contains(&token) {
            slot.viewers.push(token);
        }
        // Its server may have stopped, or it came through an upgrade,
        // which servers don't.
        self.open_in_server(id);
        // It came through an upgrade, which the thread comparing doesn't.
        self.compare_buffer(id);
        self.send_snapshot(token, id, resume);
        self.offer_install(token, id);
        self.tell_if_changed_on_disk(token, id);
        self.tell_recovered(token, id);
        Ok(())
    }

    /// Sends the client at `token` all of buffer `id`: its text, or with
    /// `resume`, from this daemon, the edits since the version it knew if
    /// the history still reaches it; then its highlights, diagnostics and
    /// hunks.
    fn send_snapshot(&mut self, token: Token, id: BufferId, resume: Option<Resume>) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let version = slot.buffer.version();
        let window = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.window,
            _ => None,
        };
        let change = window.and_then(|window| slot.applied.get(&window).copied());
        let since = (resume)
            .filter(|resume| resume.daemon == self.id)
            .and_then(|resume| slot.buffer.edits_since(resume.version))
            .map(|edits| edits.cloned().collect::<Vec<_>>());
        let snapshot = match since {
            Some(edits) => BufferUpdate::Resumed {
                daemon: self.id,
                path: slot.buffer.path().display().to_string(),
                edits,
                applied: change.map(|change| change.seq),
                landed: change.and_then(|change| change.landed),
                modified: slot.buffer.modified(),
            },
            None => BufferUpdate::Snapshot(BufferSnapshot {
                applied: change.map(|change| change.seq),
                landed: change.and_then(|change| change.landed),
                daemon: self.id,
                ..slot.buffer.snapshot()
            }),
        };
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
        let features = self.server_features(id).map(BufferUpdate::Features);
        // A window reads a buffer as plain text until told otherwise.
        let language = (!slot.word.is_empty()).then(|| BufferUpdate::Language {
            word: slot.word.clone(),
        });
        // A window catching up may still have hunks gone meanwhile.
        let resumed = matches!(snapshot, BufferUpdate::Resumed { .. });
        self.tell(token, id, snapshot);
        let hunks = self.hunks_update(id);
        let agents = self.agents_update(id).or_else(|| {
            resumed.then_some(BufferUpdate::Agents {
                version,
                hunks: Vec::new(),
                chats: Vec::new(),
            })
        });
        let after = (language.into_iter())
            .chain(highlights)
            .chain(diagnostics)
            .chain(hunks)
            .chain(agents);
        for update in after.chain(features) {
            self.tell(token, id, update);
        }
    }

    /// Takes off buffer `id`'s viewers any other connection that attached
    /// as `window`, which the client at `token` is: one the window lost.
    /// Anything still on its way from it, such as a change sent just as it
    /// went, is no longer taken, as the window sends it again.
    pub(super) fn forget_lost_connections(&mut self, token: Token, id: BufferId, window: u64) {
        let lost: Vec<Token> = (self.buffers.get(&id))
            .map(|slot| slot.viewers.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|&viewer| {
                viewer != token
                    && matches!(self.sources.get(viewer.0),
                        Some(Source::Client(client)) if client.window == Some(window))
            })
            .collect();
        for viewer in lost {
            log::info!("buffer {id}: a connection its window lost is no longer followed");
            self.detach_buffer(viewer, id);
        }
    }

    /// Stops sending `id` to the client at `token`.
    pub(super) fn detach_buffer(&mut self, token: Token, id: BufferId) {
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.viewers.retain(|&viewer| viewer != token);
        }
        self.forget_queries(token, Some(id));
        self.forget_stages(token, Some(id));
        self.forget_piece_questions(token, Some(id));
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
        // Dealt with, whatever comes of it, so it's never sent again.
        let window = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.window,
            _ => None,
        };
        if let Some(window) = window {
            slot.applied
                .insert(window, WindowChange { seq, landed: None });
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
                if let Some(window) = window
                    && let Some(change) =
                        (self.buffers.get_mut(&id)).and_then(|slot| slot.applied.get_mut(&window))
                {
                    change.landed = Some(version);
                }
                self.edited(id, version, &edit, None);
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
                self.send_snapshot(token, id, None);
            }
        }
    }

    /// Answers [`Request::Save`](clackatoo_protocol::Request::Save).
    pub(super) fn save_buffer(&mut self, id: BufferId, force: bool) -> Result<Response, Error> {
        self.forget_formats(id);
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        let saved = slot.buffer.save(force)?;
        let path = slot.buffer.path().to_owned();
        if let Some(modified) = saved {
            self.tell_viewers(id, None, &BufferUpdate::Modified(modified));
        }
        log::info!("buffer {id} saved");
        self.files_written(&path);
        self.recovery_touched(id);
        // Its directory may be new.
        self.watch_buffer(id);
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
        self.made_for_user(id, made);
        Ok(Response::Done)
    }

    /// Passes on an edit the daemon made to buffer `id` for the user,
    /// rather than one a window sent: to its server and parser, and to
    /// every window following it, none of which has made it yet.
    pub(super) fn made_for_user(&mut self, id: BufferId, made: Made) {
        self.made_by(id, made, Author::User);
    }

    /// Passes on an edit `author` made to buffer `id` in the daemon, as
    /// [`made_for_user`](Self::made_for_user) does.
    pub(super) fn made_by(&mut self, id: BufferId, made: Made, author: Author) {
        self.pass_on(id, made, author, None);
    }

    /// Passes on an agent's own change to buffer `id`, by `maker`, as
    /// [`made_by`](Self::made_by) does: the lines it changed become agent
    /// hunks, which every window following the buffer is sent.
    pub(super) fn made_by_agent(&mut self, id: BufferId, made: Made, maker: Maker) {
        let author = Author::Agent(clackatoo_protocol::TerminalId(maker.chat));
        self.pass_on(id, made, author, Some(maker));
    }

    /// Passes on an edit `author` made to buffer `id`, an agent's own
    /// change if `agent` says who made it.
    fn pass_on(&mut self, id: BufferId, made: Made, author: Author, agent: Option<Maker>) {
        self.edited(id, made.version, &made.edit, agent);
        let update = BufferUpdate::Edited {
            version: made.version,
            edit: made.edit,
            author,
            answers: None,
        };
        self.tell_viewers(id, None, &update);
        if agent.is_some() {
            self.tell_agents(id);
        }
        self.note_modified(id);
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
    /// `pane`, which shows a file or a folder, shows `file` instead, which
    /// may be a folder too. The file it showed closes if nothing else
    /// shows it and it has no changes to save; the folder it showed always
    /// does, its listing being only the pane's.
    pub(super) fn open_here(&mut self, pane: PaneId, file: OpenFile) -> Result<Response, Error> {
        let old = self.workspaces.check_pane(pane)?;
        if !matches!(old, Content::Buffer(_) | Content::Folder(_)) {
            return Err(Error::NotAFile(pane));
        }
        // Not over the last view of a buffer with no file yet, with
        // changes: nothing could show them again.
        if self.loses_blank(&[pane]) {
            return Err(blank_kept(PROTOCOL_VERSION));
        }
        let new = self.open_content(file, Some(pane))?;
        if new != old {
            self.workspaces.set_content(pane, new)?;
            self.workspaces_changed();
            match old {
                Content::Buffer(old) => self.close_if_unwanted(old),
                Content::Folder(files) => self.close_files(files),
                _ => {}
            }
        }
        Ok(Response::Done)
    }

    /// Has buffer `id` follow its file, which has moved to `to`, as a file
    /// explorer moves it: it's closed in its language server, parser and
    /// git, and opened in them again at its new path, as its language may
    /// have changed with its name, and every client following it told.
    pub(super) fn move_buffer(&mut self, id: BufferId, to: PathBuf, disk: crate::buffer::Restamp) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let from = slot.buffer.path().to_owned();
        let shown = to.display().to_string();
        log::info!("buffer {id}: moved to {shown}");
        slot.buffer.moved_to(to, disk);
        let server = slot.server.take();
        let git_dir = slot.git_dir.take();
        let parsed = std::mem::take(&mut slot.parsed);
        slot.compared = false;
        slot.hunks = Hunks::new();
        slot.highlights = Highlights::new();
        let version = slot.buffer.version();
        let end = slot.buffer.text().len_bytes();
        if parsed && let Some(syntax) = &self.syntax {
            syntax.close(key(id));
        }
        self.closed_in_server(&from, server);
        self.closed_in_git(id, git_dir);
        self.unwatch_buffer(id);
        self.watch_buffer(id);
        // What was said of it at its old path is unsaid, until it's said
        // again at its new one.
        let unsaid = [
            BufferUpdate::Highlights {
                version,
                start: 0,
                end,
                spans: Vec::new(),
            },
            BufferUpdate::Diagnostics {
                version,
                diagnostics: Vec::new(),
            },
            BufferUpdate::Hunks {
                version,
                hunks: Vec::new(),
            },
            BufferUpdate::Moved(shown),
        ];
        for update in &unsaid {
            self.tell_viewers(id, None, update);
        }
        self.parse_buffer(id);
        self.open_in_server(id);
        self.compare_buffer(id);
        // Its recovery file names its file.
        self.recovery_moved(id);
    }

    /// Has the panes showing buffer `old`, whose file buffer `new` has just
    /// been moved over, show `new` instead, and closes `old`: in its
    /// language server, parser and git too. One that got changes not
    /// written meanwhile is kept, unseen, as `:q` keeps one, with its save
    /// refused, its file having changed.
    pub(super) fn replace_buffer(&mut self, old: BufferId, new: BufferId) {
        if (self.buffers.get(&old)).is_some_and(|slot| slot.buffer.modified()) {
            log::warn!("buffer {old}: written over with changes not written; kept");
            return;
        }
        if self.workspaces.show_instead(old, new) {
            self.workspaces_changed();
        }
        self.close_if_unwanted(old);
    }

    /// Has buffer `id`, whose file has been written over, read it again,
    /// as an edit by the user, which can be undone. One with changes not
    /// written is left as it is, its save refused, its file having
    /// changed.
    pub(super) fn reread_buffer(&mut self, id: BufferId) {
        if (self.buffers.get(&id)).is_none_or(|slot| slot.buffer.modified()) {
            return;
        }
        if let Err(err) = self.discard_buffer(id) {
            log::warn!("buffer {id}: its file was written over and couldn't be read: {err}");
        }
    }

    /// Closes `buffer` if no pane shows it and it has no changes to save.
    /// One with changes waits, unseen, to be opened again, as does one
    /// with changes set aside from a recovery file. One with no file
    /// yet couldn't be, so closing or replacing the last pane showing it
    /// with changes is refused first (see [`loses_blank`](Self::loses_blank)):
    /// it goes once they're thrown away, as `:q!` does.
    pub(super) fn close_if_unwanted(&mut self, buffer: BufferId) {
        let unwanted = !self.workspaces.shows(buffer)
            && (self.buffers.get(&buffer))
                .is_some_and(|slot| !slot.buffer.modified() && slot.recovered.aside.is_none());
        if unwanted {
            // What agents changed in it outlives it, in the log, and its
            // agent hunks in the reviews.
            self.buffer_closing(buffer);
            self.closing_in_review(buffer);
        }
        if unwanted && let Some(slot) = self.buffers.remove(&buffer) {
            log::info!("buffer {buffer} closed: {}", slot.buffer.path().display());
            if let Some(syntax) = &self.syntax
                && slot.parsed
            {
                syntax.close(key(buffer));
            }
            self.closed_in_server(slot.buffer.path(), slot.server);
            self.closed_in_git(buffer, slot.git_dir);
            self.unwatch_buffer(buffer);
            self.recovery_forget(buffer);
        }
    }

    /// Has buffer `id` parsed, if its language has a pack, starting the
    /// thread that parses if it's the first; and works out what its words
    /// are made of, as [`find_words`](Self::find_words) does.
    pub(super) fn parse_buffer(&mut self, id: BufferId) {
        self.find_words(id);
        let (Some(languages), Some(slot)) = (&mut self.languages, self.buffers.get(&id)) else {
            return;
        };
        let Some(pack) = languages.find(slot.buffer.path()) else {
            return;
        };
        let (text, version) = (slot.buffer.text().clone(), slot.buffer.version());
        let Some(syntax) = self.start_syntax() else {
            return;
        };
        log::info!("buffer {id}: parsed as {}", pack.name);
        syntax.open(key(id), pack, text, version);
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.parsed = true;
        }
    }

    /// Works out what buffer `id`'s words are made of besides letters,
    /// digits and `_`: as its pack says, if packs are looked for and it has
    /// one, or else as clackatoo's own pack of its language says, which
    /// needs no grammar. Tells the windows following it if that's changed,
    /// as it may have with its file's name, or a pack installed.
    pub(super) fn find_words(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        let path = slot.buffer.path();
        let pack = (self.languages.as_mut()).and_then(|languages| languages.find(path));
        let word = pack::word_of(pack.as_deref(), path);
        if word == slot.word {
            return;
        }
        word.clone_into(&mut slot.word);
        let update = BufferUpdate::Language {
            word: slot.word.clone(),
        };
        self.tell_viewers(id, None, &update);
    }

    /// The thread that parses buffers, started if it's the first need of
    /// it, or `None` if it can't be, and so nothing is parsed.
    pub(super) fn start_syntax(&mut self) -> Option<&Syntax> {
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
                    return None;
                }
            }
        }
        self.syntax.as_ref()
    }

    /// Tells buffer `id`'s server about `edit`, which made `version`, and
    /// moves its highlights and hunks along with it, having it parsed and
    /// compared again. An agent's own change, as `agent` says, makes agent
    /// hunks; any other edit moves them.
    fn edited(&mut self, id: BufferId, version: u64, edit: &Edit, agent: Option<Maker>) {
        self.edited_in_server(id, version, edit);
        self.edited_in_git(id, edit);
        match agent {
            Some(maker) => self.agent_edited(id, edit, maker),
            None => self.edited_in_review(id, edit),
        }
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
    /// client following their buffers, and the answers to questions about
    /// their pieces of code, and the colours of hovers' code, to the
    /// clients that asked.
    pub(super) fn take_highlights(&mut self) {
        let Some(syntax) = &self.syntax else {
            return;
        };
        let found: Vec<Found> = syntax.take().collect();
        let stopped = syntax.stopped();
        for found in found {
            match found {
                Found::Highlighted(found) => self.highlighted(found),
                Found::Answered(answered) => self.answered_pieces(answered),
                Found::Coloured(coloured) => self.coloured(&coloured),
            }
        }
        if stopped {
            self.fail_piece_questions();
            self.forget_all_colourings();
        }
    }

    /// Sends what a parse found to every client following its buffer.
    fn highlighted(&mut self, found: Highlighted) {
        let Some(id) = u32::try_from(found.buffer).ok().map(BufferId) else {
            return;
        };
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        // Edits made while it was parsing move it along, as they moved
        // what was there.
        let Some(edits) = slot.buffer.edits_since(found.version) else {
            log::warn!("buffer {id}: highlights for a version long gone");
            return;
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

    /// Tells every client following `id` whether it's modified, if that's
    /// changed.
    pub(super) fn note_modified(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if let Some(modified) = slot.buffer.check_modified() {
            self.tell_viewers(id, None, &BufferUpdate::Modified(modified));
        }
        // After every change: its recovery file is written soon, or goes.
        self.recovery_touched(id);
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
        self.send_event(token, Event::Buffer { buffer: id, update });
    }

    /// Sends `event` to the client at `token`.
    pub(super) fn send_event(&mut self, token: Token, event: Event) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        match frame::encode(client.wire, &ServerMessage::Event(event)) {
            Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
            Err(err) => {
                log::error!("encoding an event failed: {err}");
                return;
            }
        }
        self.flush_client(token);
    }
}

/// Buffer `id`, as the thread parsing buffers knows it.
pub(super) fn key(id: BufferId) -> clackatoo_syntax::Key {
    u64::from(id.0)
}

/// Why closing or replacing panes is refused, for a client speaking `wire`:
/// it would lose a buffer with no file yet that has changes (see
/// [`Daemon::loses_blank`]). In words of its own for a client that reads
/// them, and, for one from before [`wire::FALLBACKS`], which has no
/// [`Error::Other`] and would be dropped for one, as a save that can't be.
pub(super) fn blank_kept(wire: u32) -> Error {
    if wire >= wire::FALLBACKS {
        Error::Other("[No Name] has changes no file holds: :w NAME writes them".to_owned())
    } else {
        Error::SaveFailed {
            path: "[No Name]".to_owned(),
            reason: "it has changes no file holds, which closing it would lose".to_owned(),
        }
    }
}

/// `path` made absolute, from `dir` if it's relative, or from `home` if it
/// starts with `~/`, with its links followed as far as they exist.
pub(super) fn resolve(path: &str, dir: &Path, home: &Path) -> PathBuf {
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

    #[test]
    fn a_blank_buffer_with_changes_outlives_an_upgrade_and_is_named_after() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (mut daemon, _) = super::super::agent_files::tests::daemon_with_chat(&root);
        let blank = OpenBlank {
            cwd: Some(root.display().to_string()),
        };
        let id = daemon.open_blank(&blank, None);
        let name = daemon.workspaces.name_for_new(None).unwrap();
        let pane = daemon.workspaces.new_workspace(name, Content::Buffer(id));
        let slot = daemon.buffers.get_mut(&id).unwrap();
        let edit = Edit::replace(0, 0, "", "draft\n");
        slot.buffer.edit(edit, 0, true, Author::User).unwrap();
        slot.buffer.check_modified();

        let config = daemon.config.clone();
        let handover = daemon.hand_over(std::time::Instant::now()).unwrap();
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config).unwrap();
        let slot = &resumed.buffers[&id];
        assert!(slot.buffer.is_blank() && slot.buffer.modified());
        assert_eq!((slot.buffer.dir(), slot.watched), (root.as_path(), false));
        assert_eq!(
            resumed.workspaces.check_pane(pane).unwrap(),
            Content::Buffer(id)
        );
        assert_eq!(
            resumed
                .save_as(Token(usize::MAX), id, "draft.md", false)
                .unwrap(),
            Response::Done
        );
        let written = std::fs::read_to_string(root.join("draft.md")).unwrap();
        assert_eq!(written, "draft\n");
        let slot = &resumed.buffers[&id];
        assert!(!slot.buffer.is_blank() && !slot.buffer.modified() && slot.watched);
    }
}
