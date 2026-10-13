//! Agents' files: the daemon as an agent's file system, and what agents
//! change in files, which lands in buffers as the agent's own edits.
//!
//! An agent changes a file in one of two ways:
//!
//! - **Through the daemon**, with ACP's `fs/write_text_file`. Its reads,
//!   `fs/read_text_file`, are answered from the buffer if the file is
//!   open, unsaved changes and all, and a write is merged into whatever
//!   has changed since the agent read the file. Only files under the
//!   chat's working directory, or the git worktree it's in, are read and
//!   written.
//! - **Itself**, as Claude Code's tools do: its adapter says what a tool
//!   changed once it has. Each file open in a buffer is read again, and
//!   if it's what the tool's changes make of the text saved, they're
//!   merged into the buffer. If it isn't, something else changed it too,
//!   and nothing is taken.
//!
//! The agent's commands change files too, as `cargo fmt` and `sed -i` do,
//! saying nothing of it. A change to a file open in a buffer, heard of
//! while a tool of one chat's agent that may change files runs (see
//! [`super::disk`]), and that no tool said it made, is that agent's: it's
//! merged in the same way, in no tool call, once the tool finishes.
//!
//! Either way, the change is an edit by `Author::Agent(chat)`, one undo
//! step for each of the agent's tool calls, so undoing it leaves your own
//! edits be. The file on disk gets the agent's change under any changes
//! you haven't saved: the agent's commands (`cargo build`) see its work,
//! and yours stays unsaved, as you left it.
//!
//! A change is never mixed into yours. One that touches a line you've
//! changed and not saved is refused, for an agent writing through the
//! daemon, which can read the file again; and for one that wrote the file
//! itself, the buffer is left as you have it, still refusing to be
//! written over the file unless forced, and you're told.
//!
//! Every change is noted in the [`EditLog`], by chat, turn and tool call,
//! so a turn can be undone (see `turn_undo`). For a file not open that an
//! agent writes itself, the daemon needs its text from before: it reads
//! the file as soon as a tool that's running names it, which with Claude
//! Code's adapter is as the tool's input streams in, before it can run,
//! and takes that text only if the tool's own changes make of it what's
//! on disk once the tool's done.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Author, BufferId, BufferUpdate, TerminalId};
use clackatoo_text::Rope;
use serde_json::{Value, json};

use super::Daemon;
use super::buffers::resolve;
use super::chats::AgentId;
use super::edit_log::{EditLog, Maker, edit_bytes};
use crate::agent::{FAILED, NOT_FOUND};
use crate::buffer::{self, Buffer, Made, Refused};
use crate::chat::{RawDiff, Wrote};

/// How many files' text, as agents last read them, are kept, to merge
/// their writes against. A write to a file whose read has been forgotten
/// is made against the text as it is.
const READS: usize = 256;

/// How much text of files as agents last read them is kept, at most, in
/// bytes, counting each file whole, though a buffer's shares its text.
const READ_BYTES: usize = 64 * 1024 * 1024;

/// How much text of files not open is read before the agent's tools
/// change them, at most, in bytes. A file that would go past it isn't
/// read, and what the tool does to it can't be undone.
const CAPTURE_BYTES: usize = 64 * 1024 * 1024;

/// What the daemon knows of agents' files.
#[derive(Debug, Default)]
pub(super) struct AgentFiles {
    /// The text of each file as an agent last read or wrote it, newest
    /// last.
    reads: VecDeque<Read>,
    /// What agents' tools said they changed in files, not yet taken into
    /// buffers or the log.
    reports: Vec<Report>,
    /// The text of files not open, as the turns' tools are about to change
    /// them.
    pub(super) captures: Vec<Capture>,
    pub(super) log: EditLog,
}

/// A file's text as the agent in one chat last read or wrote it.
#[derive(Debug)]
struct Read {
    chat: TerminalId,
    path: PathBuf,
    text: Rope,
}

/// A file not open, as it was before the tools of the turn the agent in
/// `chat` is answering changed it, or as they last left it, as far as is
/// known.
#[derive(Debug)]
pub(super) struct Capture {
    chat: TerminalId,
    path: PathBuf,
    pub(super) text: Seen,
}

/// What a file not open was found to hold.
#[derive(Clone, Debug)]
pub(super) enum Seen {
    /// This text.
    Text(String),
    /// Nothing: there was no file.
    Absent,
    /// It couldn't be read: it isn't a regular file, or is too big, or
    /// reading it failed. It isn't read again, and what the turn does to
    /// it isn't known.
    Unreadable,
}

impl AgentFiles {
    /// The text the agent in `chat` last read or wrote of `path`.
    fn read(&self, chat: TerminalId, path: &Path) -> Option<&Rope> {
        (self.reads.iter().rev())
            .find(|read| read.chat == chat && read.path == path)
            .map(|read| &read.text)
    }

    /// Notes that the agent in `chat` has read or written `text` as the
    /// text of `path`.
    fn remember(&mut self, chat: TerminalId, path: &Path, text: Rope) {
        self.reads
            .retain(|read| read.chat != chat || read.path != path);
        let mut bytes = text.len_bytes();
        for read in &self.reads {
            bytes += read.text.len_bytes();
        }
        while self.reads.len() >= READS || (bytes > READ_BYTES && !self.reads.is_empty()) {
            if let Some(old) = self.reads.pop_front() {
                bytes -= old.text.len_bytes();
            }
        }
        self.reads.push_back(Read {
            chat,
            path: path.to_owned(),
            text,
        });
    }

    /// Forgets what the agent in `chat` read and changed, as the chat has
    /// closed: nothing can undo its turns now.
    pub(super) fn forget(&mut self, chat: TerminalId) {
        self.reads.retain(|read| read.chat != chat);
        self.reports.retain(|report| report.chat != chat);
        self.captures.retain(|capture| capture.chat != chat);
        self.log.forget(chat);
    }

    /// Reads the file `path`, not open, as a tool of the agent in `chat`
    /// is about to change it, unless it has been already this turn.
    fn capture(&mut self, chat: TerminalId, path: &Path) {
        if (self.captures.iter()).any(|capture| capture.chat == chat && capture.path == path) {
            return;
        }
        let kept: usize = (self.captures.iter())
            .map(|capture| match &capture.text {
                Seen::Text(text) => text.len(),
                Seen::Absent | Seen::Unreadable => 0,
            })
            .sum();
        let not_found = |err: &std::io::Error| err.kind() == std::io::ErrorKind::NotFound;
        // Its size first, so a big file or a device isn't read at all.
        let text = match std::fs::metadata(path) {
            Err(err) if not_found(&err) => Seen::Absent,
            Ok(metadata)
                if metadata.is_file()
                    && kept
                        .saturating_add(usize::try_from(metadata.len()).unwrap_or(usize::MAX))
                        <= CAPTURE_BYTES =>
            {
                match buffer::read(path) {
                    Ok((text, _)) => Seen::Text(buffer::lf(text)),
                    Err(err) if not_found(&err) => Seen::Absent,
                    Err(err) => {
                        log::debug!("{}: {err}", path.display());
                        Seen::Unreadable
                    }
                }
            }
            Ok(_) | Err(_) => Seen::Unreadable,
        };
        self.captures.push(Capture {
            chat,
            path: path.to_owned(),
            text,
        });
    }
}

/// What a tool of an agent said it changed in a file, waiting for the file
/// to be checked against it.
#[derive(Debug)]
struct Report {
    chat: TerminalId,
    turn: usize,
    /// Where the tool is among its turn's items.
    order: usize,
    tool: String,
    path: PathBuf,
    /// The tool's changes to the file, as it last said them.
    diffs: Vec<RawDiff>,
}

impl Daemon {
    /// Answers the agent's `fs/read_text_file`: the buffer's text, if the
    /// file is open, or else the file's.
    pub(super) fn read_for_agent(
        &mut self,
        (agent, name): (AgentId, &str),
        session: &str,
        request: &Value,
        path: &Path,
        (line, limit): (Option<u32>, Option<u32>),
    ) {
        let result = self
            .read_file(name, session, path)
            .map(|(chat, path, text)| {
                let content = lines(&text, line, limit);
                self.agent_files.remember(chat, &path, text);
                json!({ "content": content })
            });
        self.answer_agent(agent, request, result);
    }

    fn read_file(
        &mut self,
        name: &str,
        session: &str,
        path: &Path,
    ) -> Result<(TerminalId, PathBuf, Rope), (i64, String)> {
        let (chat, path) = self.agent_path(name, session, path)?;
        if let Some(id) = self.buffer_of(&path) {
            let text = self.buffers[&id].buffer.text().clone();
            return Ok((chat, path, text));
        }
        match buffer::read(&path) {
            Ok((text, _)) => Ok((chat, path, Rope::from_str(&buffer::lf(text)))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err((NOT_FOUND, format!("{} doesn't exist", path.display())))
            }
            Err(err) => Err((FAILED, format!("{}: {err}", path.display()))),
        }
    }

    /// Makes the agent's `fs/write_text_file`, and answers it.
    pub(super) fn write_for_agent(
        &mut self,
        (agent, name): (AgentId, &str),
        session: &str,
        request: &Value,
        path: &Path,
        content: &str,
    ) {
        let result = (self.write_file(name, session, path, content)).map(|()| json!({}));
        self.answer_agent(agent, request, result);
    }

    fn write_file(
        &mut self,
        name: &str,
        session: &str,
        path: &Path,
        content: &str,
    ) -> Result<(), (i64, String)> {
        let (chat, path) = self.agent_path(name, session, path)?;
        let (turn, tool) = (self.chat(chat))
            .map(|chat| chat.working_on())
            .unwrap_or_default();
        let maker = Maker { chat, turn, tool };
        let content = buffer::lf(content.to_owned());
        let step = !(self.agent_files.log).continues(chat, &path, turn, maker.tool.as_deref());
        let author = Author::Agent(chat);
        // The text the agent last saw: what it read, or else the file's,
        // never changes it can't have seen.
        let read = self.agent_files.read(chat, &path).map(Rope::to_string);
        let refused = |refused: Refused, base: &str| match refused {
            Refused::Conflict(lines) => (
                FAILED,
                format!(
                    "the user has unsaved edits on {} of {}: read the file again",
                    line_numbers(base, &lines),
                    path.display()
                ),
            ),
            Refused::Failed(err) => (FAILED, err.to_string()),
        };
        if let Some(id) = self.buffer_of(&path) {
            let slot = self.buffers.get_mut(&id).expect("the buffer just found");
            let base = read.unwrap_or_else(|| slot.buffer.saved().to_string());
            let existed = slot.buffer.disk().is_some();
            let made = (slot.buffer)
                .write_merged(&base, &content, author, step)
                .map_err(|err| refused(err, &base))?;
            // Its server hears of the edit before that the file's saved.
            self.landed(id, &maker, &path, made, existed);
            self.saved_in_server(id);
        } else {
            // A file not open is changed as a buffer opened for the moment,
            // so it's written as saving writes, safely.
            if let Some(dir) = path.parent()
                && !dir.exists()
            {
                std::fs::create_dir_all(dir)
                    .map_err(|err| (FAILED, format!("{}: {err}", dir.display())))?;
            }
            let dir = path.parent().unwrap_or(&path).to_owned();
            let mut file =
                Buffer::open(path.clone(), dir).map_err(|err| (FAILED, err.to_string()))?;
            let base = read.unwrap_or_else(|| file.saved().to_string());
            let before = file.disk().is_some().then(|| file.saved().to_string());
            file.write_merged(&base, &content, author, true)
                .map_err(|err| refused(err, &base))?;
            let after = file.saved().to_string();
            let dropped =
                (self.agent_files.log).note_text(&maker, &path, before.as_deref(), Some(&after));
            let written = file.disk();
            self.agent_changed_file(
                &path,
                (before.as_deref(), Some(&after)),
                maker.hunks(),
                written,
            );
            self.refresh_files(chat, dropped.into_iter().chain([turn]));
        }
        self.agent_files
            .remember(chat, &path, Rope::from_str(&content));
        Ok(())
    }

    /// Takes what a tool of the agent in `chat` wrote to files itself into
    /// the buffers open for them, once each file is found to be what the
    /// tool says it made it, and into the log. A tool still running names
    /// files it's about to change: those not open are read, so what it
    /// does to them can be undone.
    pub(super) fn take_written(&mut self, chat: TerminalId, wrote: &Wrote) {
        let Some(cwd) = (self.chat(chat)).map(|chat| PathBuf::from(chat.working_directory()))
        else {
            return;
        };
        let home = self.config.home.clone();
        for path in &wrote.paths {
            let path = resolve(path, &cwd, &home);
            let open = self.buffer_of(&path);
            if !wrote.done {
                if open.is_none() {
                    self.agent_files.capture(chat, &path);
                }
                continue;
            }
            let diffs: Vec<RawDiff> = (wrote.diffs.iter())
                .filter(|diff| resolve(&diff.path, &cwd, &home) == path)
                .cloned()
                .collect();
            let reports = &mut self.agent_files.reports;
            let same = |report: &&mut Report| {
                report.chat == chat && report.tool == wrote.tool && report.path == path
            };
            match reports.iter_mut().find(same) {
                Some(report) if !diffs.is_empty() => report.diffs = diffs,
                Some(_) => {}
                None => reports.push(Report {
                    chat,
                    turn: wrote.turn,
                    order: wrote.order,
                    tool: wrote.tool.clone(),
                    path: path.clone(),
                    diffs,
                }),
            }
            match open {
                Some(id) => self.settle(chat, &path, id, false),
                None => self.settle_closed(chat, &path, false),
            }
        }
    }

    /// The agent in `chat` has finished answering: a file it changed that
    /// isn't what its tools said they made it was changed by something
    /// else too, and the buffer open on it is told so. A file not open
    /// whose text before its tools isn't known is noted so, as what they
    /// did to it can't be undone.
    pub(super) fn settle_turn(&mut self, chat: TerminalId) {
        let mut paths: Vec<PathBuf> = (self.agent_files.reports.iter())
            .filter(|report| report.chat == chat)
            .map(|report| report.path.clone())
            .collect();
        paths.sort();
        paths.dedup();
        for path in paths {
            match self.buffer_of(&path) {
                Some(id) => self.settle(chat, &path, id, true),
                None => self.settle_closed(chat, &path, true),
            }
        }
        let files = &mut self.agent_files;
        files.reports.retain(|report| report.chat != chat);
        files.captures.retain(|capture| capture.chat != chat);
    }

    /// The reports of the tools of the agent in `chat` about `path`, in
    /// the order the agent made the changes.
    fn reports_of(&self, chat: TerminalId, path: &Path) -> Vec<&Report> {
        let mut reports: Vec<&Report> = (self.agent_files.reports.iter())
            .filter(|report| report.chat == chat && report.path == path)
            .collect();
        reports.sort_by_key(|report| (report.turn, report.order));
        reports
    }

    /// Checks buffer `id`'s file, `path`, against what the tools of the
    /// agent in `chat` said they changed in it. If the file is what their
    /// changes, made in turn to the text saved, make it, each is taken
    /// into the buffer as the agent's own; if one touches a line changed
    /// and not saved, none is, and the user is told. Otherwise, with
    /// `last`, as the agent's turn ends, the user is told the file changed
    /// some other way; without, more may yet be said.
    fn settle(&mut self, chat: TerminalId, path: &Path, id: BufferId, last: bool) {
        let forget = |files: &mut AgentFiles| {
            (files.reports).retain(|report| report.chat != chat || report.path != path);
        };
        let buffer = &self.buffers[&id].buffer;
        let (disk, stamp) = match buffer.changed_on_disk() {
            Ok(Some(changed)) => changed,
            Ok(None) => return forget(&mut self.agent_files),
            Err(err) => {
                log::warn!("buffer {id}: {err}");
                return forget(&mut self.agent_files);
            }
        };
        let existed = buffer.disk().is_some();
        let reports = self.reports_of(chat, path);
        // Each report's own changes, in the order the agent made them.
        let mut text = buffer.saved().to_string();
        let mut steps = Vec::with_capacity(reports.len());
        let mut makers = Vec::with_capacity(reports.len());
        for report in &reports {
            let Some(next) = apply(&text, &report.diffs) else {
                break;
            };
            text = next;
            let tool = Some(report.tool.as_str());
            let step = !(self.agent_files.log).continues(chat, path, report.turn, tool);
            steps.push((text.clone(), step));
            makers.push(Maker {
                chat,
                turn: report.turn,
                tool: Some(report.tool.clone()),
            });
        }
        if steps.len() != reports.len() || text != disk {
            // Claude Code's adapter says a tab-indented file's changes with
            // spaces for each leading tab, two in 0.88.0, so they make the
            // file only with its tabs so too. Then the file's text is what the tools
            // made, as one change, since what each made can't be told
            // exactly. Not if something else changed the file too, which
            // the spaces could hide.
            if let Some(report) = reports.last()
                && !self.buffers[&id].stale()
                && made_with_tabs_as_spaces(buffer.saved(), &reports, &disk)
            {
                let tool = Some(report.tool.as_str());
                let step = !(self.agent_files.log).continues(chat, path, report.turn, tool);
                steps = vec![(disk.clone(), step)];
                makers = vec![Maker {
                    chat,
                    turn: report.turn,
                    tool: Some(report.tool.clone()),
                }];
            } else {
                if last {
                    forget(&mut self.agent_files);
                    self.tell_of_change(chat, id, path, false);
                }
                return;
            }
        }
        forget(&mut self.agent_files);
        let slot = self.buffers.get_mut(&id).expect("the buffer just found");
        match slot
            .buffer
            .take_written(&steps, (&disk, stamp), Author::Agent(chat))
        {
            Ok(made) => {
                for (made, maker) in made.into_iter().zip(&makers) {
                    self.landed(id, maker, path, made, existed);
                }
                self.note_modified(id);
                self.saved_in_server(id);
            }
            Err(Refused::Conflict(_)) => self.tell_of_change(chat, id, path, true),
            Err(Refused::Failed(err)) => log::warn!("buffer {id}: {err}"),
        }
    }

    /// Takes the change found on disk to buffer `id`'s file into it, as a
    /// change the agent in `chat` made in turn `turn` by its commands, in
    /// no tool call: one undo step, merged into the changes not saved as a
    /// tool's change is, and in the log as the turn's, so undoing the turn
    /// takes it out too. If it touches a line changed and not saved, it
    /// isn't, and the user is told. A file a tool of the agent has said it
    /// changed is left to what the tool said, which comes to it at the
    /// turn's end, if not before.
    pub(super) fn take_from_commands(&mut self, chat: TerminalId, turn: usize, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let path = slot.buffer.path().to_owned();
        let reported = (self.agent_files.reports.iter())
            .any(|report| report.chat == chat && report.path == path);
        if reported {
            return;
        }
        let (disk, stamp) = match slot.buffer.changed_on_disk() {
            Ok(Some(changed)) => changed,
            Ok(None) => return,
            Err(err) => {
                log::warn!("buffer {id}: {err}");
                return;
            }
        };
        let existed = slot.buffer.disk().is_some();
        let slot = self.buffers.get_mut(&id).expect("the buffer just found");
        let steps = [(disk.clone(), true)];
        match (slot.buffer).take_written(&steps, (&disk, stamp), Author::Agent(chat)) {
            Ok(made) => {
                let maker = Maker {
                    chat,
                    turn,
                    tool: None,
                };
                for made in made {
                    self.landed(id, &maker, &path, made, existed);
                }
                self.note_modified(id);
                self.saved_in_server(id);
            }
            Err(Refused::Conflict(_)) => self.tell_of_change(chat, id, &path, true),
            Err(Refused::Failed(err)) => log::warn!("buffer {id}: {err}"),
        }
    }

    /// Checks the file `path`, not open, against what the tools of the
    /// agent in `chat` said they changed in it, and the text it had before
    /// them, if that was read. If their changes, made in turn to that
    /// text, make the file's, the change is noted in the log, and the
    /// file's text is the one the next changes are made to. Otherwise,
    /// with `last`, as the agent's turn ends, the change is noted as not
    /// known; without, more may yet be said.
    fn settle_closed(&mut self, chat: TerminalId, path: &Path, last: bool) {
        let reports = self.reports_of(chat, path);
        let Some(first) = reports.first() else {
            return;
        };
        let maker = Maker {
            chat,
            turn: first.turn,
            tool: reports.last().map(|report| report.tool.clone()),
        };
        let before = (self.agent_files.captures.iter())
            .find(|capture| capture.chat == chat && capture.path == path)
            .and_then(|capture| match &capture.text {
                Seen::Text(text) => Some(Some(text.clone())),
                Seen::Absent => Some(None),
                Seen::Unreadable => None,
            });
        let mut read_as = None;
        let disk = match buffer::read(path) {
            Ok((text, stamp)) => {
                read_as = Some(stamp);
                Some(buffer::lf(text))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            // Not to be taken for gone: if it can't be read by the turn's
            // end, what was done to it isn't known.
            Err(err) => {
                log::warn!("{}: {err}", path.display());
                if last {
                    (self.agent_files.reports)
                        .retain(|report| report.chat != chat || report.path != path);
                    self.agent_files.log.note_unknown(&maker, path);
                    self.refresh_files(chat, [maker.turn]);
                }
                return;
            }
        };
        // What the tools' changes make of the text before: a file deleted
        // by tools that show no changes is gone.
        let made = before.as_ref().and_then(|before| {
            if disk.is_none() && before.is_some() && reports.iter().all(|r| r.diffs.is_empty()) {
                return Some(None);
            }
            let mut text = before.clone().unwrap_or_default();
            for report in &reports {
                text = apply(&text, &report.diffs)?;
            }
            Some(Some(text))
        });
        let forget = |files: &mut AgentFiles| {
            (files.reports).retain(|report| report.chat != chat || report.path != path);
        };
        match (before, made) {
            (Some(before), Some(made)) if made == disk => {
                forget(&mut self.agent_files);
                let dropped = (self.agent_files.log).note_text(
                    &maker,
                    path,
                    before.as_deref(),
                    disk.as_deref(),
                );
                let texts = (before.as_deref(), disk.as_deref());
                self.agent_changed_file(path, texts, maker.hunks(), read_as);
                if let Some(capture) = (self.agent_files.captures.iter_mut())
                    .find(|capture| capture.chat == chat && capture.path == path)
                {
                    capture.text = disk.map_or(Seen::Absent, Seen::Text);
                }
                self.refresh_files(chat, dropped.into_iter().chain([maker.turn]));
            }
            _ if last => {
                forget(&mut self.agent_files);
                self.agent_files.log.note_unknown(&maker, path);
                self.refresh_files(chat, [maker.turn]);
            }
            _ => {}
        }
    }

    /// Tells everyone following buffer `id`, and the chat, that the file
    /// changed on disk under changes not saved, so the buffer is as it
    /// was: by the agent in `chat`, where the user changed it too
    /// (`conflict`), or some other way, too.
    fn tell_of_change(&mut self, chat: TerminalId, id: BufferId, path: &Path, conflict: bool) {
        let file = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = (self.chat(chat)).map_or_else(
            || "the agent".to_owned(),
            |chat| chat.agent_title().to_owned(),
        );
        let notice = if conflict {
            format!(
                "{title} changed {file} where you have unsaved edits: \
                 `:e!` takes its version, `:w!` keeps yours"
            )
        } else {
            format!(
                "{file} changed on disk, and not only as {title} said: \
                 `:e!` takes the file's version, `:w!` keeps yours"
            )
        };
        if !conflict && let Some(slot) = self.buffers.get_mut(&id) {
            // What's on disk now isn't all the agent's.
            slot.changed_outside();
        }
        self.tell_viewers(id, None, &BufferUpdate::Notice(notice.clone()));
        if let Some(hosted) = self.chat(chat) {
            hosted.notify(notice);
            self.collect_damage(chat);
        }
    }

    /// Passes on the edit an agent's change made to buffer `id`, if it
    /// made one, and notes it in the log. `existed` says whether the file
    /// was there before.
    fn landed(
        &mut self,
        id: BufferId,
        maker: &Maker,
        path: &Path,
        made: Option<Made>,
        existed: bool,
    ) {
        let Some(made) = made else {
            // The file may have been written all the same.
            self.note_modified(id);
            return;
        };
        let change = (id, made.version, edit_bytes(&made.edit));
        let dropped = (self.agent_files.log).note_buffer(maker, path, change, existed);
        self.made_by_agent(id, made, maker.hunks());
        self.refresh_files(maker.chat, dropped.into_iter().chain([maker.turn]));
    }

    /// The chat with `session`, and `path` as a buffer's path would be,
    /// if the chat's agent may read and write it: it's under the chat's
    /// working directory, or the git worktree that's in.
    fn agent_path(
        &mut self,
        name: &str,
        session: &str,
        path: &Path,
    ) -> Result<(TerminalId, PathBuf), (i64, String)> {
        let Some((chat, hosted)) = self.chat_in(name, session) else {
            return Err((FAILED, format!("there's no session {session}")));
        };
        let cwd = PathBuf::from(hosted.working_directory());
        let Some(asked) = path.to_str() else {
            return Err((FAILED, format!("{} isn't UTF-8", path.display())));
        };
        let path = resolve(asked, &cwd, &self.config.home);
        let root = agent_root(&cwd);
        if !path.starts_with(&root) {
            return Err((
                FAILED,
                format!(
                    "{} is outside {}, where this chat works: clackatoo lets an agent \
                     read and write files only there",
                    path.display(),
                    root.display()
                ),
            ));
        }
        Ok((chat, path))
    }

    /// Sends the agent the answer to its request about a file.
    fn answer_agent(
        &mut self,
        agent: AgentId,
        request: &Value,
        result: Result<Value, (i64, String)>,
    ) {
        self.agent_answer(agent, request, result);
    }
}

/// Where an agent working in `cwd` may read and write files through the
/// daemon: the git worktree `cwd` is in, or else `cwd`. A chat working in
/// the cache directory, as the tutor's lessons do in their scratch
/// directories, looks for its worktree no higher than inside the cache,
/// so it never takes in a repository the cache happens to be in.
pub(super) fn agent_root(cwd: &Path) -> PathBuf {
    root_below(cwd, cache_dir().as_deref())
}

/// The git worktree `cwd` is in, or else `cwd`, looking no higher than
/// inside `stop` when `cwd` is in it.
fn root_below(cwd: &Path, stop: Option<&Path>) -> PathBuf {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
    let stop = stop
        .map(|stop| stop.canonicalize().unwrap_or_else(|_| stop.to_owned()))
        .filter(|stop| cwd.starts_with(stop));
    (cwd.ancestors())
        .take_while(|dir| stop.as_deref() != Some(*dir))
        .find(|dir| dir.join(".git").exists())
        .map_or_else(|| cwd.clone(), Path::to_path_buf)
}

/// Where programs keep files they can do without: `$XDG_CACHE_HOME`, or
/// else `~/.cache`. A relative path in either is ignored.
fn cache_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))
}

/// `text` with the changes a tool said it made, in order: each `old`
/// found after the one before and replaced by its `new`. A file made new
/// is its `new`. `None` if a change doesn't fit.
///
/// An `old` that's there twice is taken where it's first found, which may
/// not be where the tool changed it: the caller checks the text made is
/// the file's, whole, so such a mistake takes nothing.
fn apply(text: &str, diffs: &[RawDiff]) -> Option<String> {
    if diffs.is_empty() {
        return None;
    }
    let mut made = String::with_capacity(text.len());
    let mut done = 0;
    for diff in diffs {
        match diff.old.as_deref() {
            None | Some("") => {
                if !text.is_empty() || !made.is_empty() {
                    return None;
                }
                made.push_str(&diff.new);
            }
            Some(old) => {
                let at = done + text[done..].find(old)?;
                made.push_str(&text[done..at]);
                made.push_str(&diff.new);
                done = at + old.len();
            }
        }
    }
    made.push_str(&text[done..]);
    Some(made)
}

/// How many spaces an agent that shows a file's tabs as spaces may show
/// each leading tab as: Claude Code's adapter 0.88.0 shows two.
const TAB_WIDTHS: [usize; 3] = [2, 4, 8];

/// Whether the changes `reports` said, made in turn to `saved`, make
/// `disk` once the leading tabs of each line of both are spaces, as an
/// agent that shows a file's tabs so says its changes: as many for each
/// tab as one of [`TAB_WIDTHS`].
fn made_with_tabs_as_spaces(saved: &Rope, reports: &[&Report], disk: &str) -> bool {
    let saved = saved.to_string();
    TAB_WIDTHS.into_iter().any(|width| {
        let mut text = tabs_as_spaces(&saved, width);
        for report in reports {
            match apply(&text, &report.diffs) {
                Some(next) => text = next,
                None => return false,
            }
        }
        text == tabs_as_spaces(disk, width)
    })
}

/// `text` with each tab in the indent each line starts with as `width`
/// spaces.
fn tabs_as_spaces(text: &str, width: usize) -> String {
    let tab = " ".repeat(width);
    let mut spaced = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        for blank in line[..indent].chars() {
            spaced.push_str(if blank == '\t' { &tab } else { " " });
        }
        spaced.push_str(&line[indent..]);
    }
    spaced
}

/// The lines `ranges` of bytes of `text` are on, counting from 1, as
/// "line 3" or "lines 3–5", for the agent to read.
pub(super) fn line_numbers(text: &str, ranges: &[Range<usize>]) -> String {
    let line =
        |at: usize| memchr::memchr_iter(b'\n', &text.as_bytes()[..at.min(text.len())]).count() + 1;
    let named: Vec<String> = (ranges.iter())
        .map(|range| {
            let (first, last) = (line(range.start), line(range.end.max(range.start + 1) - 1));
            if first == last {
                format!("line {first}")
            } else {
                format!("lines {first}–{last}")
            }
        })
        .collect();
    named.join(", ")
}

/// `limit` lines of `text` from line `line`, counting from 1: all of it,
/// if neither's given.
fn lines(text: &Rope, line: Option<u32>, limit: Option<u32>) -> String {
    let count = text.len_lines();
    let first = (line.map_or(0, |line| line.saturating_sub(1) as usize)).min(count);
    let end = limit.map_or(count, |limit| {
        first.saturating_add(limit as usize).min(count)
    });
    text.byte_slice(text.line_to_byte(first)..text.line_to_byte(end))
        .to_string()
}

#[cfg(test)]
pub(super) mod tests {
    use clackatoo_protocol::{ChatAction, OpenFile, Size};
    use clackatoo_text::Edit;

    use super::*;
    use crate::DaemonConfig;
    use crate::agent::Opened;
    use crate::chat::HostedChat;
    use crate::server::edit_log::Change;

    /// A daemon in `dir`, not running, with a chat in it whose session is
    /// `s1`, answering a message.
    pub(in crate::server) fn daemon_with_chat(dir: &Path) -> (Daemon, TerminalId) {
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.to_owned(),
            shell_integration: None,
            agents: crate::AgentConfig::default(),
            mcp: None,
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            server_idle: std::time::Duration::ZERO,
            prompt_wait: crate::DEFAULT_PROMPT_WAIT,
            probe_wait: crate::DEFAULT_PROBE_WAIT,
            rename_gate: None,
            trash: Some(dir.join("Trash")),
            boards: None,
        };
        let mut daemon = Daemon::bind(&dir.join("daemon.sock"), config).unwrap();
        let id = TerminalId(1);
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let cwd = dir.display().to_string();
        let tools = clackatoo_protocol::AgentTools::Ask;
        let mut chat = HostedChat::new(id, Size::new(40, 10).unwrap(), cwd, &agent, tools, true);
        chat.connected(Opened {
            session: "s1".into(),
            settings: None,
        });
        chat.act(ChatAction::Send("go".into()));
        daemon.add_chat(chat);
        (daemon, id)
    }

    /// Has the chat's agent start tool call `tool`.
    pub(in crate::server) fn start_tool(daemon: &mut Daemon, chat: TerminalId, tool: &str) {
        let update = serde_json::from_value(json!({
            "sessionUpdate": "tool_call", "toolCallId": tool, "title": "Edit",
            "kind": "edit", "status": "in_progress",
        }))
        .unwrap();
        daemon.chat(chat).unwrap().update(update);
    }

    #[test]
    fn each_tool_calls_writes_are_one_undo_step_and_are_logged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let file = OpenFile {
            path: path.display().to_string(),
            cwd: None,
        };
        let id = daemon.open_buffer(file, None).unwrap();
        let text = |daemon: &Daemon| daemon.buffers[&id].buffer.text().to_string();
        // Yours, unsaved.
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        let edit = Edit::replace(8, 8, "", "three\n");
        buffer.edit(edit, 0, true, Author::User).unwrap();

        // Two writes in one tool call, then one in another, having read
        // your line.
        start_tool(&mut daemon, chat, "t1");
        daemon.read_for_agent((AgentId(0), "claude"), "s1", &json!(0), &path, (None, None));
        daemon.write_for_agent(
            (AgentId(0), "claude"),
            "s1",
            &json!(1),
            &path,
            "ONE\ntwo\nthree\n",
        );
        daemon.write_for_agent(
            (AgentId(0), "claude"),
            "s1",
            &json!(2),
            &path,
            "ONE\nTWO\nthree\n",
        );
        start_tool(&mut daemon, chat, "t2");
        daemon.write_for_agent(
            (AgentId(0), "claude"),
            "s1",
            &json!(3),
            &path,
            "ONE\nTWO\nthree\nfour\n",
        );
        assert_eq!(text(&daemon), "ONE\nTWO\nthree\nfour\n");

        let file = &daemon.agent_files.log.turn(chat, 0).unwrap().files[0];
        assert_eq!(
            (file.path.as_path(), file.tool.as_deref()),
            (path.as_path(), Some("t2"))
        );
        let versions: Vec<_> = (file.changes.iter())
            .map(|change| match change {
                Change::Buffer {
                    buffer, version, ..
                } => (*buffer, *version),
                Change::Text { .. } => panic!("the file is open"),
            })
            .collect();
        assert_eq!(versions, [(id, 2), (id, 3), (id, 4)]);

        // Undoing the agent takes back one tool call at a time, and leaves
        // yours.
        let author = Author::Agent(chat);
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(author, false).unwrap().unwrap();
        assert_eq!(text(&daemon), "ONE\nTWO\nthree\n");
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(author, false).unwrap().unwrap();
        assert_eq!(text(&daemon), "one\ntwo\nthree\n");
        // The file has the agent's last write, under your unsaved line.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ONE\nTWO\nfour\n");
    }

    #[test]
    fn changes_said_out_of_order_are_each_taken_as_their_own_tools() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap().join("a.txt");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let file = OpenFile {
            path: path.display().to_string(),
            cwd: None,
        };
        let id = daemon.open_buffer(file, None).unwrap();
        // Two tools change the file, and the second says so first.
        std::fs::write(&path, "ONE\nTWO\n").unwrap();
        let wrote = |tool: &str, order, old: &str, new: &str| Wrote {
            done: true,
            turn: 0,
            tool: tool.into(),
            order,
            paths: vec![path.display().to_string()],
            diffs: vec![RawDiff {
                path: path.display().to_string(),
                old: Some(old.into()),
                new: new.into(),
            }],
        };
        daemon.take_written(chat, &wrote("t2", 1, "two", "TWO"));
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "one\ntwo\n");
        daemon.take_written(chat, &wrote("t1", 0, "one", "ONE"));
        assert_eq!(daemon.buffers[&id].buffer.text().to_string(), "ONE\nTWO\n");
        let changes = &daemon.agent_files.log.turn(chat, 0).unwrap().files[0].changes;
        assert!(matches!(
            changes[..],
            [
                Change::Buffer { version: 1, .. },
                Change::Buffer { version: 2, .. }
            ]
        ));
        // Each its own step.
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::Agent(chat), false).unwrap().unwrap();
        assert_eq!(buffer.text().to_string(), "ONE\ntwo\n");
        assert_eq!(buffer.check_modified(), Some(true));
    }

    #[test]
    fn changes_said_with_tabs_as_spaces_make_the_file_only_with_its_indent() {
        let report = |old: &str, new: &str| Report {
            chat: TerminalId(1),
            turn: 0,
            order: 0,
            tool: "t".into(),
            path: "/Makefile".into(),
            diffs: vec![RawDiff {
                path: "/Makefile".into(),
                old: Some(old.into()),
                new: new.into(),
            }],
        };
        let saved = Rope::from_str("all:\n\tcc a.c\n\t\techo\n");
        // As Claude Code's adapter says it: two spaces a tab.
        let said = report("all:\n  cc a.c", "all:\n  cc -O2 a.c");
        let disk = "all:\n\tcc -O2 a.c\n\t\techo\n";
        assert!(made_with_tabs_as_spaces(&saved, &[&said], disk));
        let four = report("    cc a.c\n        echo", "    cc -O2 a.c\n        echo");
        assert!(made_with_tabs_as_spaces(&saved, &[&four], disk));
        // Indented differently on disk than the change says: not the tool's.
        let reindented = "all:\n\tcc -O2 a.c\n\techo\n";
        assert!(!made_with_tabs_as_spaces(&saved, &[&said], reindented));
    }

    #[test]
    fn a_tools_changes_are_found_in_order_or_not_at_all() {
        let diff = |old: Option<&str>, new: &str| RawDiff {
            path: "/a".into(),
            old: old.map(str::to_owned),
            new: new.into(),
        };
        let text = "one\ntwo\nthree\ntwo\n";
        // Hunks with their lines of context, as Claude Code's adapter
        // gives them, each found after the one before.
        let diffs = [
            diff(Some("one\ntwo"), "one\nTWO"),
            diff(Some("two\n"), "2\n"),
        ];
        assert_eq!(apply(text, &diffs).as_deref(), Some("one\nTWO\nthree\n2\n"));
        assert_eq!(apply(text, &[diff(Some("four"), "4")]), None);
        assert_eq!(apply(text, &[]), None);
        // A new file is its text, and only a file that was empty is new.
        assert_eq!(apply("", &[diff(None, "hi\n")]).as_deref(), Some("hi\n"));
        assert_eq!(apply(text, &[diff(None, "hi\n")]), None);
    }

    #[test]
    fn lines_are_named_for_the_agent_from_one() {
        let text = "a\nbb\nc\nd\n";
        assert_eq!(line_numbers(text, &[2..5, 2..4]), "line 2, line 2");
        assert_eq!(line_numbers(text, &[2..7, 9..9]), "lines 2–3, line 5");
    }

    #[test]
    fn an_agent_works_under_its_directory_or_the_git_worktree_its_in() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(real.join("repo/.git")).unwrap();
        std::fs::create_dir_all(real.join("repo/src")).unwrap();
        assert_eq!(agent_root(&real.join("repo/src")), real.join("repo"));
        assert_eq!(agent_root(&real), real);
    }

    #[test]
    fn an_agent_in_the_cache_never_climbs_out_of_it_to_a_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        // A home that's a repository, with the cache in it.
        std::fs::create_dir_all(real.join(".git")).unwrap();
        let scratch = real.join(".cache/clackatoo/tutor/7/agents");
        std::fs::create_dir_all(&scratch).unwrap();
        let cache = real.join(".cache");
        assert_eq!(root_below(&scratch, Some(&cache)), scratch);
        // A repository inside the cache is still one.
        std::fs::create_dir_all(cache.join("clackatoo/.git")).unwrap();
        assert_eq!(root_below(&scratch, Some(&cache)), cache.join("clackatoo"));
        // Outside the cache, nothing changes.
        std::fs::create_dir_all(real.join("src")).unwrap();
        assert_eq!(root_below(&real.join("src"), Some(&cache)), real);
    }

    #[test]
    fn lines_are_read_from_one_and_as_many_as_asked() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        assert_eq!(lines(&text, None, None), "one\ntwo\nthree\n");
        assert_eq!(lines(&text, Some(2), None), "two\nthree\n");
        assert_eq!(lines(&text, Some(2), Some(1)), "two\n");
        assert_eq!(lines(&text, Some(9), Some(1)), "");
        assert_eq!(lines(&text, Some(0), Some(1)), "one\n");
    }
}
