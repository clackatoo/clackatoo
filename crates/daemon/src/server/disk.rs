//! Buffers' files changing on disk: what the daemon makes of a change to
//! a file open in a buffer that it didn't make itself.
//!
//! Each buffer's file is watched with inotify, through the directory it's
//! in (the watches are [`super::explorers`]'s, shared). A directory that
//! can't be watched, as one deleted by a branch switch, is tried again
//! now and then, and the file looked at once it is. A change heard of is
//! looked at late in the loop's pass, once what the daemon's own threads
//! wrote has been taken, and only if the file isn't as the buffer last
//! read or wrote it. The daemon's own saves, and the changes agents' tools
//! say they made, move what the buffer knows of the file, so they're never
//! taken for changes. A file deleted is left to saving, which then says
//! it's changed.
//!
//! A change is an agent's if it's heard of while a tool of the agent's
//! that may change files is running, or as it finishes (inotify is read
//! again then, for what the tool wrote last), in the one chat
//! whose project the file is in that has one running: an edit, or a
//! command, as `cargo fmt` or `sed -i`. It waits for the tool to finish,
//! or the turn to end. Then a change a tool said it made is left to what
//! the tool said, and any other is taken in as the agent's own, from its
//! commands (see [`super::agent_files`]). A change made while the agent is
//! only thinking, or waiting to be allowed to run something, isn't its.
//!
//! Otherwise it's from outside, as a git checkout's or another editor's,
//! and is taken as Vim's `autoread` takes one. A buffer with no changes
//! not saved has its windows told, and each has the file read again, as an
//! edit by the user, which can be undone, if its `autoread` says to. One
//! with changes keeps them, still refusing to be saved over the file unless
//! forced, and its windows say so, as they do for a file too big to read
//! again on the event loop.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Author, BufferId, BufferUpdate, Error, Response, TerminalId};
use mio::Token;

use super::Daemon;
use super::agent_files::agent_root;
use crate::buffer::{DiskStamp, Refused};

/// How long after a buffer's directory couldn't be watched it's tried
/// again, at first. The wait doubles each time, up to [`REWATCH_MOST`].
const REWATCH_LEAST: Duration = Duration::from_millis(100);

/// The longest wait between tries to watch a buffer's directory: a file
/// opened in a directory that doesn't exist yet costs one try this often.
const REWATCH_MOST: Duration = Duration::from_secs(10);

/// The biggest file read again for `autoread`, in bytes. A file is read on
/// the event loop, so a bigger one's windows only say it changed, and
/// `:e!` reads it when asked.
const AUTOREAD_MOST: u64 = 4 * 1024 * 1024;

/// Buffers' files changed on disk, waiting to be looked at.
#[derive(Debug, Default)]
pub(super) struct Disk {
    /// Buffers whose files inotify has said something happened to, since
    /// they were last looked at.
    touched: Vec<BufferId>,
    /// Changes found on disk while a chat's tool that may change files
    /// ran, by buffer, waiting for it to finish or the turn to end.
    noted: HashMap<BufferId, Noted>,
    /// Chats such a tool has finished in since changes were last looked
    /// at: a change heard of meanwhile is still the tool's.
    ran: Vec<TerminalId>,
    /// When to try again to watch the directories of buffers whose
    /// directories couldn't be watched, and how long the wait was.
    rewatch: Option<(Instant, Duration)>,
}

/// A change found on disk that a chat's agent made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Noted {
    chat: TerminalId,
    /// The turn it was found in.
    turn: usize,
}

impl Daemon {
    /// Watches buffer `id`'s file for changes, if it isn't watched, or has
    /// it tried again later if its directory can't be watched.
    pub(super) fn watch_buffer(&mut self, id: BufferId) {
        if !self.try_watch_buffer(id) {
            self.rewatch_later();
        }
    }

    /// Watches buffer `id`'s file for changes, if it isn't watched.
    /// Returns whether it's watched, or gone, or has no file to watch.
    fn try_watch_buffer(&mut self, id: BufferId) -> bool {
        let Some(slot) = self.buffers.get(&id) else {
            return true;
        };
        if slot.watched || slot.buffer.is_blank() {
            return true;
        }
        let path = slot.buffer.path().to_owned();
        let watched = self.watch_buffer_file(id, &path);
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.watched = watched;
        }
        watched
    }

    /// No longer watches buffer `id`'s file, as it has closed or moved,
    /// forgetting the changes heard of.
    pub(super) fn unwatch_buffer(&mut self, id: BufferId) {
        self.unwatch_buffer_file(id);
        if let Some(slot) = self.buffers.get_mut(&id) {
            slot.watched = false;
        }
        self.disk.noted.remove(&id);
        self.disk.touched.retain(|&touched| touched != id);
    }

    /// Notes that the watches on the files of buffers `ids` have gone, as
    /// their directories did: each is tried again now and then.
    pub(super) fn buffer_files_unwatched(&mut self, ids: &[BufferId]) {
        for id in ids {
            if let Some(slot) = self.buffers.get_mut(id) {
                slot.watched = false;
            }
        }
        if !ids.is_empty() {
            self.rewatch_later();
        }
    }

    /// Has the directories of buffers not watched tried again soon, if
    /// they aren't to be already.
    fn rewatch_later(&mut self) {
        if self.disk.rewatch.is_none() {
            self.disk.rewatch = Some((Instant::now() + REWATCH_LEAST, REWATCH_LEAST));
        }
    }

    /// Has the directories of buffers not watched tried again at once, as
    /// there's room for more watches again.
    pub(super) fn rewatch_now(&mut self) {
        if self.buffers.values().any(|slot| !slot.watched) {
            self.disk.rewatch = Some((Instant::now(), REWATCH_LEAST));
        }
    }

    /// When the directories of buffers not watched are next tried, if any
    /// are to be.
    pub(super) fn next_rewatch(&self) -> Option<Instant> {
        self.disk.rewatch.map(|(due, _)| due)
    }

    /// Tries again to watch the directories of buffers not watched, if
    /// it's time, and has the file of each watched now looked at, as it
    /// may have changed meanwhile. Those still not watched are tried again
    /// after twice the wait.
    fn rewatch(&mut self) {
        let now = Instant::now();
        let Some((due, wait)) = self.disk.rewatch else {
            return;
        };
        if due > now {
            return;
        }
        self.disk.rewatch = None;
        let unwatched: Vec<BufferId> = (self.buffers.iter())
            .filter(|(_, slot)| !slot.watched)
            .map(|(&id, _)| id)
            .collect();
        let mut left = false;
        for id in unwatched {
            if self.try_watch_buffer(id) {
                self.disk_maybe_changed(id);
            } else {
                left = true;
            }
        }
        if left {
            let wait = (wait * 2).min(REWATCH_MOST);
            self.disk.rewatch = Some((now + wait, wait));
        }
    }

    /// Has buffer `id`'s file looked at again, late in this pass, once an
    /// agent's change merged into it has been made, unless a change found
    /// meanwhile is noted as an agent's: that's taken as its own then.
    pub(super) fn look_again(&mut self, id: BufferId) {
        if !self.disk.noted.contains_key(&id) {
            self.disk_maybe_changed(id);
        }
    }

    /// Has buffer `id`'s file looked at, late in this pass, as it may
    /// have changed.
    pub(super) fn disk_maybe_changed(&mut self, id: BufferId) {
        if !self.disk.touched.contains(&id) {
            self.disk.touched.push(id);
        }
    }

    /// A tool of the agent in `chat` that may change files has finished,
    /// or its turn ended with one running: the changes it made on disk are
    /// taken, late in this pass, with any heard of meanwhile.
    ///
    /// Called as the agent's message saying so is read, before anything is
    /// made of it, it first reads what inotify has to say. The kernel queues
    /// an event as a file is written, so what the tool wrote is queued
    /// before the agent said it finished. But the agent's pipe and inotify
    /// are read as the loop hears of each, and the finish may come first,
    /// leaving the change to be heard of in a later pass, with the tool no
    /// longer running, and taken for one from outside. Nothing changed
    /// after the finish is heard of here: nobody has been told of it yet.
    pub(super) fn writing_ended(&mut self, chat: TerminalId) {
        self.read_files_changes();
        if !self.disk.ran.contains(&chat) {
            self.disk.ran.push(chat);
        }
    }

    /// Looks at the files of the buffers inotify has said something
    /// happened to, then takes the changes agents made that are due: those
    /// in chats a tool that may change files has finished in, or whose turn
    /// isn't running any more.
    pub(super) fn check_disk(&mut self) {
        self.rewatch();
        for id in std::mem::take(&mut self.disk.touched) {
            self.look_at_file(id);
        }
        let ran = std::mem::take(&mut self.disk.ran);
        let noted: Vec<(BufferId, Noted)> = (self.disk.noted.iter())
            .map(|(&id, &noted)| (id, noted))
            .collect();
        for (id, noted) in noted {
            let merging = (self.buffers.get(&id))
                .is_some_and(|slot| self.agent_files.merging(noted.chat, slot.buffer.path()));
            if merging {
                // The chat's own change to the file is still being merged:
                // this is taken after it, from what it leaves, and is still
                // the tool's that finished, if one has.
                if ran.contains(&noted.chat) && !self.disk.ran.contains(&noted.chat) {
                    self.disk.ran.push(noted.chat);
                }
                continue;
            }
            let running = self.chat(noted.chat).is_some_and(|chat| chat.answering());
            if running && !ran.contains(&noted.chat) {
                continue;
            }
            self.disk.noted.remove(&id);
            if (self.buffers.get(&id)).is_some_and(super::buffers::BufferSlot::stale) {
                // Since the agent's change was noted, something other
                // than the agent changed the file too, and wasn't taken, so
                // none of what's on disk is taken as the agent's. A buffer
                // with unsaved changes was told then, so it isn't again.
                self.changed_outside(id, false);
            } else {
                self.take_from_commands(noted.chat, noted.turn, id);
            }
        }
    }

    /// Looks at buffer `id`'s file, which may have changed: if it has, the
    /// change is noted as an agent's, or taken as from outside.
    fn look_at_file(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let path = slot.buffer.path().to_owned();
        match DiskStamp::now(&path) {
            Ok(Some(now)) if Some(now) != slot.buffer.disk() => {}
            // As the buffer last read or wrote it, or gone.
            Ok(_) => return,
            Err(err) => {
                log::debug!("buffer {id}: {}: {err}", path.display());
                return;
            }
        }
        let by = if slot.stale() {
            None
        } else {
            self.writer_in_project(&path)
        };
        match (by, self.disk.noted.get(&id)) {
            (Some(by), None) => {
                self.disk.noted.insert(id, by);
            }
            (Some(by), Some(noted)) if noted.chat == by.chat => {}
            // An agent's change to it is still being merged into the
            // buffer, which doesn't know the file as it is yet: nothing is
            // taken as from outside until it's made, and the file looked
            // at again.
            _ if self.agent_files.pending(&path) => {}
            _ => {
                self.disk.noted.remove(&id);
                self.changed_outside(id, true);
            }
        }
    }

    /// The chat whose project the file at `path` is in, and its turn, if
    /// it's the one such chat with a tool that may change files running,
    /// or just finished.
    fn writer_in_project(&mut self, path: &Path) -> Option<Noted> {
        let mut found = None;
        for (&id, slot) in &mut self.terminals {
            let Some(chat) = slot.terminal.chat() else {
                continue;
            };
            if !chat.writing() && !self.disk.ran.contains(&id) {
                continue;
            }
            if !path.starts_with(agent_root(Path::new(chat.working_directory()))) {
                continue;
            }
            if found.is_some() {
                return None;
            }
            let (turn, _) = chat.working_on();
            found = Some(Noted { chat: id, turn });
        }
        found
    }

    /// Takes the change found on disk to buffer `id`'s file as from
    /// outside: one with changes not saved keeps them, and its followers
    /// are told so, if `tell`; one without has them told it's changed, to
    /// read it again as their `autoread` says.
    fn changed_outside(&mut self, id: BufferId, tell: bool) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        slot.changed_outside();
        if !slot.buffer.modified() {
            self.tell_changed(id, None);
        } else if tell {
            let notice = changed_notice(slot.buffer.path());
            self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
        }
    }

    /// Tells the client at `token`, which has just attached to buffer `id`,
    /// that its file has changed from outside, if it has, and the buffer
    /// has no changes not saved.
    pub(super) fn tell_if_changed_on_disk(&mut self, token: Token, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        if slot.stale() && !slot.buffer.modified() {
            self.tell_changed(id, Some(token));
        }
    }

    /// Tells the client at `to`, or else every client following buffer
    /// `id`, which has no changes not saved, that its file has changed
    /// from outside: for each to read it again as its `autoread` says, or,
    /// for a file too big to read again on the event loop, only that it
    /// changed. Nothing is said of a file that couldn't be read again, and
    /// hasn't changed since.
    fn tell_changed(&mut self, id: BufferId, to: Option<Token>) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let now = DiskStamp::now(slot.buffer.path()).ok().flatten();
        if now.is_some() && now == slot.unreadable {
            return;
        }
        let update = if now.is_some_and(|now| now.len() > AUTOREAD_MOST) {
            BufferUpdate::Notice(changed_notice(slot.buffer.path()))
        } else {
            BufferUpdate::ChangedOnDisk
        };
        match to {
            Some(token) => self.tell(token, id, update),
            None => self.tell_viewers(id, None, &update),
        }
    }

    /// Answers [`Request::Reload`](clackatoo_protocol::Request::Reload):
    /// the file read again, as an edit by the user, if it has changed and
    /// the buffer has no changes not saved. Only what's changed is
    /// changed, so places in the text stay where they were. A file too big
    /// to read again on the event loop isn't, and one that can't be read,
    /// as one no longer UTF-8, is said so of once, until it changes again.
    pub(super) fn reload_buffer(&mut self, id: BufferId) -> Result<Response, Error> {
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        if self.disk.noted.contains_key(&id) || self.agent_files.pending(slot.buffer.path()) {
            // An agent's, still to be taken as its own, or being merged.
            return Ok(Response::Done);
        }
        let notice = changed_notice(slot.buffer.path());
        let now = DiskStamp::now(slot.buffer.path()).ok().flatten();
        if slot.buffer.modified() || now.is_some_and(|now| now.len() > AUTOREAD_MOST) {
            if slot.stale() {
                self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
            }
            return Ok(Response::Done);
        }
        if now.is_some() && now == slot.unreadable {
            return Ok(Response::Done);
        }
        let (disk, stamp) = match slot.buffer.changed_on_disk() {
            Ok(Some(changed)) => changed,
            Ok(None) => return Ok(Response::Done),
            Err(err) => {
                slot.unreadable = now;
                let why = match err {
                    Error::OpenFailed { reason, .. } => reason,
                    other => other.to_string(),
                };
                let file = file_name(slot.buffer.path());
                let notice = format!("{file} changed on disk, and can't be read: {why}");
                self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
                return Ok(Response::Done);
            }
        };
        let steps = [(disk.clone(), true)];
        match (slot.buffer).take_written(&steps, (&disk, stamp), Author::User) {
            Ok(made) => {
                for made in made.into_iter().flatten() {
                    self.made_for_user(id, made);
                }
                self.note_modified(id);
                self.saved_in_server(id);
                Ok(Response::Done)
            }
            // With nothing changed and not saved, nothing can conflict.
            Err(Refused::Conflict(_)) => Ok(Response::Done),
            Err(Refused::Failed(err)) => Err(err),
        }
    }
}

/// The name of the file at `path`, as the bar says it.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What a window says of a file at `path` changed on disk where the buffer
/// has changes not saved, or that's too big to read again.
fn changed_notice(path: &Path) -> String {
    let file = file_name(path);
    format!("{file} changed on disk: `:e!` to take it, `:w!` to keep yours")
}

#[cfg(test)]
pub(in crate::server) mod tests {
    use clackatoo_protocol::{Edit, OpenFile};
    use serde_json::{Value, json};

    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// Opens the file at `path` in a buffer of `daemon`.
    fn open(daemon: &mut Daemon, path: &Path) -> BufferId {
        let file = OpenFile {
            path: path.display().to_string(),
            cwd: None,
        };
        daemon.open_buffer(file, None).unwrap()
    }

    /// The buffers whose files the watches are for.
    fn watched(daemon: &Daemon) -> Vec<BufferId> {
        (daemon.watches())
            .flat_map(|watched| watched.buffers.iter().copied())
            .collect()
    }

    /// Has the agent in `chat` say `update`, as the event loop hears it.
    pub(in crate::server) fn says(daemon: &mut Daemon, chat: TerminalId, update: Value) {
        let update = serde_json::from_value(update).unwrap();
        if daemon.chat(chat).unwrap().finishes_writing(&update) {
            daemon.writing_ended(chat);
        }
        daemon.chat(chat).unwrap().update(update);
    }

    /// Has the agent in `chat` start running a command, as tool call `c`.
    pub(in crate::server) fn start_command(daemon: &mut Daemon, chat: TerminalId) {
        let call = json!({
            "sessionUpdate": "tool_call", "toolCallId": "c", "title": "cargo fmt",
            "kind": "execute", "status": "in_progress",
        });
        says(daemon, chat, call);
    }

    /// Has the agent in `chat` say its command, tool call `c`, finished.
    pub(in crate::server) fn finish_command(daemon: &mut Daemon, chat: TerminalId) {
        let done = json!({
            "sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "completed",
        });
        says(daemon, chat, done);
    }

    /// Writes `text` to the file at `path`, and has the daemon look at it,
    /// as inotify has it.
    fn write(daemon: &mut Daemon, id: BufferId, path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        daemon.disk_maybe_changed(id);
        daemon.check_disk();
    }

    #[test]
    fn a_buffers_file_is_watched_until_it_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);
        assert_eq!(watched(&daemon), [id]);
        assert!(daemon.buffers[&id].watched);

        // Shown in no pane, and with nothing to save, it closes.
        daemon.close_buffer_pane(clackatoo_protocol::PaneId(999), id);
        assert!(!daemon.buffers.contains_key(&id));
        assert_eq!(watched(&daemon), []);
        assert_eq!(daemon.watches().count(), 0, "the watch itself is gone");
    }

    #[test]
    fn a_buffer_in_a_directory_not_there_yet_is_watched_once_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().canonicalize().unwrap().join("sub");
        let path = sub.join("a.txt");
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);
        assert!(!daemon.buffers[&id].watched);
        let (_, wait) = daemon.disk.rewatch.unwrap();
        assert_eq!(wait, REWATCH_LEAST);

        // Tried again, it waits twice as long each time.
        daemon.disk.rewatch = Some((Instant::now(), wait));
        daemon.check_disk();
        assert_eq!(daemon.disk.rewatch.unwrap().1, REWATCH_LEAST * 2);
        std::fs::create_dir(&sub).unwrap();
        daemon.disk.rewatch = Some((Instant::now(), wait));
        daemon.check_disk();
        assert!(daemon.buffers[&id].watched);
        assert_eq!(watched(&daemon), [id]);
        assert_eq!(daemon.disk.rewatch, None);
    }

    #[test]
    fn with_no_room_for_more_watches_a_directory_watched_already_still_is() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("other")).unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let first = open(&mut daemon, &root.join("a.txt"));
        daemon.fill_watches();
        let beside = open(&mut daemon, &root.join("b.txt"));
        assert!(daemon.buffers[&beside].watched);
        let elsewhere = open(&mut daemon, &root.join("other/c.txt"));
        assert!(!daemon.buffers[&elsewhere].watched);
        assert_eq!(watched(&daemon), [first, beside]);
    }

    #[test]
    fn what_an_agents_commands_change_is_taken_once_one_finishes_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);
        let text = |daemon: &Daemon| daemon.buffers[&id].buffer.text().to_string();
        // Yours, unsaved, on a line the command leaves be.
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer
            .edit(Edit::replace(14, 0, "", "> "), 0, true, Author::User)
            .unwrap();

        start_command(&mut daemon, chat);
        write(&mut daemon, id, &path, "one\nTWO\nTHREE\n");
        // Noted as the agent's, but not taken while its command may be
        // writing yet.
        assert_eq!(text(&daemon), "> one\ntwo\nthree\n");
        assert!(daemon.disk.noted.contains_key(&id));

        finish_command(&mut daemon, chat);
        daemon.check_disk();
        assert_eq!(text(&daemon), "> one\nTWO\nTHREE\n");
        // The turn's, in no tool call.
        let turn = daemon.agent_files.log.turn(chat, 0).unwrap();
        assert_eq!(turn.files.len(), 1);
        assert_eq!(
            (turn.files[0].tool.as_deref(), turn.files[0].known),
            (None, true)
        );
        // One step, which undoing the agent takes back whole, leaving yours.
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::Agent(chat), false).unwrap().unwrap();
        assert_eq!(text(&daemon), "> one\ntwo\nthree\n");
        buffer_redo(&mut daemon, id, chat);
        assert_eq!(text(&daemon), "> one\nTWO\nTHREE\n");

        // Undoing the turn takes it out too, as any of the agent's changes.
        let hosted = daemon.chat(chat).unwrap();
        hosted.answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
        daemon.settle_turn(chat);
        daemon.undo_turn(chat, 0, false, false);
        assert_eq!(text(&daemon), "> one\ntwo\nthree\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_change_inotify_tells_of_after_its_command_finished_is_still_the_agents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);

        // The change is queued in inotify, unread, as the agent's pipe is
        // read first in the pass that hears the command finish.
        start_command(&mut daemon, chat);
        replace(&path, "one\ntwo\n");
        finish_command(&mut daemon, chat);
        daemon.check_disk();
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "one\ntwo\n");
        assert!(!daemon.buffers[&id].stale());
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::Agent(chat), false).unwrap().unwrap();
        assert_eq!(buffer.text().to_string(), "one\n");

        // A change after the finish, heard of in the next pass, isn't its.
        replace(&path, "one\ntwo\nthree\n");
        daemon.read_files_changes();
        daemon.check_disk();
        assert!(!daemon.disk.noted.contains_key(&id));
        assert!(daemon.buffers[&id].stale());
    }

    /// Replaces the file at `path` with one holding `text`, renamed over
    /// it, as most programs write a file. inotify hears of a rename as it's
    /// made. A file written in place is heard of only once nothing has it
    /// open, and a child another test is starting may hold it open until it
    /// runs its program.
    fn replace(path: &Path, text: &str) {
        let new = path.with_extension("new");
        std::fs::write(&new, text).unwrap();
        std::fs::rename(&new, path).unwrap();
    }

    /// Redoes the last step of the agent in `chat` in buffer `id`.
    fn buffer_redo(daemon: &mut Daemon, id: BufferId, chat: TerminalId) {
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::Agent(chat), true).unwrap().unwrap();
    }

    #[test]
    fn a_change_while_the_agent_thinks_or_waits_to_be_allowed_isnt_its() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);

        // Its turn is running, but no command is.
        write(&mut daemon, id, &path, "two\n");
        assert!(!daemon.disk.noted.contains_key(&id));
        assert!(daemon.buffers[&id].stale());
        daemon.reload_buffer(id).unwrap();

        // A command it's asking to run isn't running yet.
        start_command(&mut daemon, chat);
        let tool = serde_json::from_value(json!({ "toolCallId": "c" })).unwrap();
        daemon.chat(chat).unwrap().ask(json!(9), &tool, &[], &[]);
        write(&mut daemon, id, &path, "three\n");
        assert!(!daemon.disk.noted.contains_key(&id));
        assert!(daemon.buffers[&id].stale());
    }

    #[test]
    fn a_change_from_outside_isnt_any_agents_until_the_file_is_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);
        start_command(&mut daemon, chat);
        // Changed from outside, and not read again, as when another chat
        // had a command running too.
        daemon.buffers.get_mut(&id).unwrap().changed_outside();
        write(&mut daemon, id, &path, "one\ntwo\n");
        assert!(!daemon.disk.noted.contains_key(&id), "not the agent's");
        daemon.writing_ended(chat);
        daemon.check_disk();
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "one\n");

        // Read again, the next change is the agent's once more.
        daemon.reload_buffer(id).unwrap();
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "one\ntwo\n");
        assert!(!daemon.buffers[&id].stale());
        write(&mut daemon, id, &path, "one\ntwo\nthree\n");
        assert!(daemon.disk.noted.contains_key(&id));
    }

    #[test]
    fn a_file_too_big_isnt_read_again_for_autoread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\n").unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let id = open(&mut daemon, &path);
        let big = "x".repeat(usize::try_from(AUTOREAD_MOST).unwrap() + 1);
        std::fs::write(&path, &big).unwrap();
        daemon.reload_buffer(id).unwrap();
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "one\n");
    }
}
