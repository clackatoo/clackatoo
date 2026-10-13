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
//! for checkpoints, review and the provenance of each line to build on.

use std::collections::VecDeque;
use std::ops::Range;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Author, BufferId, BufferUpdate, TerminalId};
use clackatoo_text::{Edit, Op, Rope};
use serde_json::{Value, json};

use super::Daemon;
use super::buffers::resolve;
use super::chats::AgentId;
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

/// How many changes the [`EditLog`] keeps, at most.
const LOG_CHANGES: usize = 4096;

/// How much text the [`EditLog`] keeps, at most, in bytes put in and
/// taken away: past it, the oldest changes go.
const LOG_BYTES: usize = 16 * 1024 * 1024;

/// What the daemon knows of agents' files.
#[derive(Debug, Default)]
pub(super) struct AgentFiles {
    /// The text of each file as an agent last read or wrote it, newest
    /// last.
    reads: VecDeque<Read>,
    /// What agents' tools said they changed in files open in buffers, not
    /// yet taken into them.
    reports: Vec<Report>,
    pub(super) log: EditLog,
}

/// A file's text as the agent in one chat last read or wrote it.
#[derive(Debug)]
struct Read {
    chat: TerminalId,
    path: PathBuf,
    text: Rope,
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

    /// Forgets what the agent in `chat` read, as the chat has closed. What
    /// it changed stays in the log.
    pub(super) fn forget(&mut self, chat: TerminalId) {
        self.reads.retain(|read| read.chat != chat);
        self.reports.retain(|report| report.chat != chat);
    }
}

/// Every change agents have made to files, oldest first, as far back as
/// it reaches.
#[derive(Debug, Default)]
pub(super) struct EditLog {
    changes: VecDeque<AgentChange>,
    /// The bytes of text the changes keep.
    bytes: usize,
}

/// A change an agent made to a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentChange {
    /// The chat whose agent made it.
    pub(super) chat: TerminalId,
    /// The turn of the chat it was made in, counting from 0.
    pub(super) turn: usize,
    /// The tool call that made it, if the agent said.
    pub(super) tool: Option<String>,
    /// The file, by its real path.
    pub(super) path: PathBuf,
    /// What it did to the file, if the daemon saw: not for a file not open
    /// that the agent wrote itself.
    pub(super) edit: Option<LoggedEdit>,
}

/// What a change did to a file's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LoggedEdit {
    /// The version of the buffer it made, if the file was open: the edit
    /// is to the version before. For a file not open, it's to the file's
    /// text as it was.
    pub(super) version: Option<u64>,
    /// The edit. It keeps the text it takes away, so [`inverse`] undoes
    /// it without the text it was made to.
    ///
    /// [`inverse`]: Self::inverse
    pub(super) edit: Edit,
}

impl LoggedEdit {
    /// The edit that undoes it, made to the text it made.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "for checkpoints and review to use (CLACKA-7)")
    )]
    pub(super) fn inverse(&self) -> Edit {
        self.edit.invert()
    }
}

impl EditLog {
    /// Notes `change`, letting the oldest go past the log's bounds.
    fn push(&mut self, change: AgentChange) {
        self.bytes += size(&change);
        self.changes.push_back(change);
        while self.changes.len() > LOG_CHANGES || self.bytes > LOG_BYTES {
            let Some(old) = self.changes.pop_front() else {
                break;
            };
            self.bytes -= size(&old);
        }
    }

    /// The changes kept, oldest first.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "for checkpoints and review to read (CLACKA-7)")
    )]
    pub(super) fn changes(&self) -> impl Iterator<Item = &AgentChange> {
        self.changes.iter()
    }

    /// Whether the agent in `chat` last changed `path` in tool call `tool`
    /// of turn `turn`, so another change there joins its undo step.
    fn continues(&self, chat: TerminalId, path: &Path, turn: usize, tool: Option<&str>) -> bool {
        tool.is_some()
            && (self.changes.iter().rev())
                .find(|change| change.chat == chat && change.path == path)
                .is_some_and(|change| change.turn == turn && change.tool.as_deref() == tool)
    }
}

/// The bytes of text `change` keeps.
fn size(change: &AgentChange) -> usize {
    let ops = change
        .edit
        .as_ref()
        .map_or(&[][..], |logged| logged.edit.ops());
    (ops.iter())
        .map(|op| match op {
            Op::Retain(_) => 0,
            Op::Insert(text) | Op::Delete(text) => text.len(),
        })
        .sum()
}

/// Where a change is being made from: which chat's agent, in which turn
/// and tool call.
struct Maker {
    chat: TerminalId,
    turn: usize,
    tool: Option<String>,
}

/// What a tool of an agent said it changed in a file open in a buffer,
/// waiting for the file to be checked against it.
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
            let made = (slot.buffer)
                .write_merged(&base, &content, author, step)
                .map_err(|err| refused(err, &base))?;
            // Its server hears of the edit before that the file's saved.
            self.landed(id, &maker, &path, made);
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
            let made = file
                .write_merged(&base, &content, author, true)
                .map_err(|err| refused(err, &base))?;
            if let Some(made) = made {
                self.agent_files.log.push(AgentChange {
                    chat,
                    turn,
                    tool: maker.tool.clone(),
                    path: path.clone(),
                    edit: Some(LoggedEdit {
                        version: None,
                        edit: made.edit,
                    }),
                });
            }
        }
        self.agent_files
            .remember(chat, &path, Rope::from_str(&content));
        Ok(())
    }

    /// Takes what a tool of the agent in `chat` wrote to files itself into
    /// the buffers open for them, once each file is found to be what the
    /// tool says it made it.
    pub(super) fn take_written(&mut self, chat: TerminalId, wrote: &Wrote) {
        let Some(cwd) = (self.chat(chat)).map(|chat| PathBuf::from(chat.working_directory()))
        else {
            return;
        };
        let home = self.config.home.clone();
        for path in &wrote.paths {
            let path = resolve(path, &cwd, &home);
            let Some(id) = self.buffer_of(&path) else {
                // Not open: noted, though what changed wasn't seen, once
                // however often the agent says so.
                let tool = Some(wrote.tool.as_str());
                if !(self.agent_files.log).continues(chat, &path, wrote.turn, tool) {
                    self.agent_files.log.push(AgentChange {
                        chat,
                        turn: wrote.turn,
                        tool: Some(wrote.tool.clone()),
                        path,
                        edit: None,
                    });
                }
                continue;
            };
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
            self.settle(chat, &path, id, false);
        }
    }

    /// The agent in `chat` has finished answering: a file it changed that
    /// isn't what its tools said they made it was changed by something
    /// else too, and the buffer open on it is told so.
    pub(super) fn settle_turn(&mut self, chat: TerminalId) {
        let mut paths: Vec<PathBuf> = (self.agent_files.reports.iter())
            .filter(|report| report.chat == chat)
            .map(|report| report.path.clone())
            .collect();
        paths.dedup();
        for path in paths {
            if let Some(id) = self.buffer_of(&path) {
                self.settle(chat, &path, id, true);
            }
        }
        self.agent_files
            .reports
            .retain(|report| report.chat != chat);
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
        let mut reports: Vec<&Report> = (self.agent_files.reports.iter())
            .filter(|report| report.chat == chat && report.path == path)
            .collect();
        reports.sort_by_key(|report| (report.turn, report.order));
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
            if last {
                forget(&mut self.agent_files);
                self.tell_of_change(chat, id, path, false);
            }
            return;
        }
        forget(&mut self.agent_files);
        let slot = self.buffers.get_mut(&id).expect("the buffer just found");
        match slot
            .buffer
            .take_written(&steps, (&disk, stamp), Author::Agent(chat))
        {
            Ok(made) => {
                for (made, maker) in made.into_iter().zip(&makers) {
                    self.landed(id, maker, path, made);
                }
                self.note_modified(id);
                self.saved_in_server(id);
            }
            Err(Refused::Conflict(_)) => self.tell_of_change(chat, id, path, true),
            Err(Refused::Failed(err)) => log::warn!("buffer {id}: {err}"),
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
        self.tell_viewers(id, None, &BufferUpdate::Notice(notice.clone()));
        if let Some(hosted) = self.chat(chat) {
            hosted.notify(notice);
            self.collect_damage(chat);
        }
    }

    /// Passes on the edit an agent's change made to buffer `id`, if it
    /// made one, and notes it in the log.
    fn landed(&mut self, id: BufferId, maker: &Maker, path: &Path, made: Option<Made>) {
        let Some(made) = made else {
            // The file may have been written all the same.
            self.note_modified(id);
            return;
        };
        self.agent_files.log.push(AgentChange {
            chat: maker.chat,
            turn: maker.turn,
            tool: maker.tool.clone(),
            path: path.to_owned(),
            edit: Some(LoggedEdit {
                version: Some(made.version),
                edit: made.edit.clone(),
            }),
        });
        self.made_by(id, made, Author::Agent(maker.chat));
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
/// daemon: the git worktree `cwd` is in, or else `cwd`.
fn agent_root(cwd: &Path) -> PathBuf {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_owned());
    (cwd.ancestors())
        .find(|dir| dir.join(".git").exists())
        .map_or_else(|| cwd.clone(), Path::to_path_buf)
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

/// The lines `ranges` of bytes of `text` are on, counting from 1, as
/// "line 3" or "lines 3–5", for the agent to read.
fn line_numbers(text: &str, ranges: &[Range<usize>]) -> String {
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
mod tests {
    use clackatoo_protocol::{ChatAction, OpenFile, Size};

    use super::*;
    use crate::DaemonConfig;
    use crate::agent::Opened;
    use crate::chat::HostedChat;

    /// A daemon in `dir`, not running, with a chat in it whose session is
    /// `s1`, answering a message.
    fn daemon_with_chat(dir: &Path) -> (Daemon, TerminalId) {
        let config = DaemonConfig {
            terminal: clackatoo_term::Config::default(),
            shell: "/bin/sh".into(),
            home: dir.to_owned(),
            shell_integration: None,
            agents: crate::AgentConfig::default(),
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            server_idle: std::time::Duration::ZERO,
            rename_gate: None,
            trash: None,
        };
        let mut daemon = Daemon::bind(&dir.join("daemon.sock"), config).unwrap();
        let id = TerminalId(1);
        let agent = crate::AgentConfig::default().find("claude").unwrap();
        let cwd = dir.display().to_string();
        let mut chat = HostedChat::new(id, Size::new(40, 10).unwrap(), cwd, &agent);
        chat.connected(Opened {
            session: "s1".into(),
            settings: None,
        });
        chat.act(ChatAction::Send("go".into()));
        daemon.add_chat(chat);
        (daemon, id)
    }

    /// Has the chat's agent start tool call `tool`.
    fn start_tool(daemon: &mut Daemon, chat: TerminalId, tool: &str) {
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

        let logged: Vec<_> = (daemon.agent_files.log.changes())
            .map(|change| {
                let edit = change.edit.as_ref().unwrap();
                (
                    change.chat,
                    change.turn,
                    change.tool.as_deref(),
                    edit.version,
                )
            })
            .collect();
        assert_eq!(
            logged,
            [
                (chat, 0, Some("t1"), Some(2)),
                (chat, 0, Some("t1"), Some(3)),
                (chat, 0, Some("t2"), Some(4)),
            ]
        );
        let first = daemon.agent_files.log.changes().next().unwrap();
        assert_eq!(first.path, path);
        let edit = first.edit.as_ref().unwrap();
        assert_eq!(edit.edit.changes().collect::<Vec<_>>(), [(0, "one", "ONE")]);
        assert_eq!(
            edit.inverse().apply_to("ONE\ntwo\nthree\n").unwrap(),
            "one\ntwo\nthree\n"
        );

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
        let logged: Vec<_> = (daemon.agent_files.log.changes())
            .map(|change| {
                let edit = &change.edit.as_ref().unwrap().edit;
                (
                    change.tool.as_deref(),
                    edit.changes().next().map(|(_, old, _)| old.to_owned()),
                )
            })
            .collect();
        assert_eq!(
            logged,
            [
                (Some("t1"), Some("one".to_owned())),
                (Some("t2"), Some("two".to_owned()))
            ]
        );
        // Each its own step.
        let buffer = &mut daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::Agent(chat), false).unwrap().unwrap();
        assert_eq!(buffer.text().to_string(), "ONE\ntwo\n");
        assert_eq!(buffer.check_modified(), Some(true));
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
    fn lines_are_read_from_one_and_as_many_as_asked() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        assert_eq!(lines(&text, None, None), "one\ntwo\nthree\n");
        assert_eq!(lines(&text, Some(2), None), "two\nthree\n");
        assert_eq!(lines(&text, Some(2), Some(1)), "two\n");
        assert_eq!(lines(&text, Some(9), Some(1)), "");
        assert_eq!(lines(&text, Some(0), Some(1)), "one\n");
    }

    fn change(chat: u32, tool: &str, inserted: &str) -> AgentChange {
        let mut edit = Edit::new();
        edit.insert(inserted);
        AgentChange {
            chat: TerminalId(chat),
            turn: 0,
            tool: Some(tool.to_owned()),
            path: PathBuf::from("/a"),
            edit: Some(LoggedEdit {
                version: Some(1),
                edit,
            }),
        }
    }

    #[test]
    fn the_log_lets_its_oldest_changes_go_past_its_bounds() {
        let mut log = EditLog::default();
        for _ in 0..=LOG_CHANGES {
            log.push(change(1, "t", "x"));
        }
        assert_eq!(log.changes().count(), LOG_CHANGES);
        assert_eq!(log.bytes, LOG_CHANGES);
        // A change bigger than the log can hold leaves only itself.
        log.push(change(1, "t", &"y".repeat(LOG_BYTES + 1)));
        assert_eq!(log.changes().count(), 0);
        assert_eq!(log.bytes, 0);
    }

    #[test]
    fn a_change_in_the_same_tool_call_continues_its_step() {
        let mut log = EditLog::default();
        let a = Path::new("/a");
        assert!(!log.continues(TerminalId(1), a, 0, Some("t1")));
        log.push(change(1, "t1", "x"));
        assert!(log.continues(TerminalId(1), a, 0, Some("t1")));
        assert!(!log.continues(TerminalId(1), a, 0, Some("t2")));
        assert!(!log.continues(TerminalId(1), a, 1, Some("t1")));
        assert!(!log.continues(TerminalId(2), a, 0, Some("t1")));
        assert!(!log.continues(TerminalId(1), Path::new("/b"), 0, Some("t1")));
        // A write in no tool call is a step of its own.
        assert!(!log.continues(TerminalId(1), a, 0, None));
        let inverse = log
            .changes()
            .next()
            .unwrap()
            .edit
            .as_ref()
            .unwrap()
            .inverse();
        assert_eq!(inverse.apply_to("x").unwrap(), "");
    }
}
