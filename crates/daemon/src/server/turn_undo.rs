//! Undoing an agent's turn: taking what it changed in files out of them,
//! keeping whatever anyone changed since, and making it again.
//!
//! Each of the turn's changes to a file is taken out by the merge agents'
//! writes use ([`clackatoo_text::merge`]), newest first: the change from
//! the text after it back to the text before, merged into the text now.
//! So a change made since to other lines stays, and one that touches the
//! same lines stops the file being undone, rather than mix the two. Runs
//! of the turn's edits with nothing between them go as one.
//!
//! A turn is one change, as the agent meant it, so if any of its files
//! can't be undone, none is, unless forced. Every file is worked out
//! before any is touched, and the new text of each is written beside it
//! before any is put in place. A file the turn made goes to the trash; one
//! it deleted comes back, if the log has its text.
//!
//! In an open buffer, the undo is an edit by the agent, one step, and the
//! file gets it under any changes not saved, as it gets the agent's own
//! writes. Undoing is redone the same way: the changes undoing made are
//! what the log keeps for the turn, and redoing takes them out in turn.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Author, BufferId, TerminalId, TurnFilesState};
use clackatoo_text::{Bias, Edit, Merged, Rope, compose, merge};

use super::Daemon;
use super::agent_files::line_numbers;
use super::edit_log::{Change, FileEdits, edit_bytes};
use crate::buffer::{Buffer, DiskStamp, Staged};

/// What undoing a turn does to one file, worked out before anything's
/// done.
pub(super) struct Plan {
    /// The file's index among the turn's.
    pub(super) index: usize,
    pub(super) path: PathBuf,
    pub(super) target: Target,
    /// What becomes of the file on disk.
    pub(super) disk: Disk,
}

/// Where a file's text is.
pub(super) enum Target {
    /// In buffer `id`, open: `edit` takes the turn out of its text.
    Buffer { id: BufferId, edit: Edit },
    /// On disk alone, as `file`, opened for the moment, holds it: `text`
    /// is the text it gets.
    File { file: Box<Buffer>, text: String },
}

/// What becomes of a file on disk.
pub(super) enum Disk {
    /// It's left as it is.
    Keep,
    /// It's given this text, made if it isn't there.
    Write(String),
    /// It goes to the trash, as the turn made it.
    Trash,
}

/// A file put right, for the log.
pub(super) struct Done {
    index: usize,
    /// Whether the file was there before, and is after.
    existed: bool,
    exists: bool,
    changes: Vec<Change>,
}

impl Daemon {
    /// Undoes, or with `redo` redoes, what turn `turn` of chat `chat`
    /// changed in files, and says how that went in the chat. If a file
    /// can't be, none is, unless `force`, which does the rest.
    pub(super) fn undo_turn(&mut self, chat: TerminalId, turn: usize, redo: bool, force: bool) {
        // Changes to big files still being worked out are taken in
        // first, so the log has them all, in order.
        self.settle_all_pending();
        let notice = self.reverse_turn(chat, turn, redo, force);
        if let Some(hosted) = self.chat(chat) {
            hosted.notify(notice);
        }
        let dropped = self.agent_files.log.trim(chat);
        self.refresh_files(chat, dropped.into_iter().chain([turn]));
    }

    /// Tells chat `chat` what each of `turns` changed in files now, as the
    /// log has it.
    pub(super) fn refresh_files(
        &mut self,
        chat: TerminalId,
        turns: impl IntoIterator<Item = usize>,
    ) {
        let summaries: Vec<_> = (turns.into_iter())
            .map(|turn| (turn, self.agent_files.log.summary(chat, turn)))
            .collect();
        let Some(hosted) = self.chat(chat) else {
            return;
        };
        for (turn, files) in summaries {
            hosted.set_files(turn, files);
        }
        self.collect_damage(chat);
    }

    /// Tells every chat what each of its turns changed in files, as after
    /// an upgrade, which doesn't hand that over with the chats.
    pub(super) fn refresh_all_files(&mut self) {
        let chats: Vec<TerminalId> = (self.terminals.iter())
            .filter(|(_, slot)| slot.terminal.is_chat())
            .map(|(&id, _)| id)
            .collect();
        for chat in chats {
            let turns = self.agent_files.log.turns_of(chat);
            self.refresh_files(chat, turns);
        }
    }

    /// Undoes or redoes turn `turn` of `chat`, as [`undo_turn`] does, and
    /// returns what to say of it.
    ///
    /// [`undo_turn`]: Self::undo_turn
    fn reverse_turn(&mut self, chat: TerminalId, turn: usize, redo: bool, force: bool) -> String {
        let (done, undo) = if redo {
            ("Redid", "redo")
        } else {
            ("Undid", "undo")
        };
        let Some(hosted) = self.chat(chat) else {
            return String::new();
        };
        if hosted.answering() {
            return format!("Wait for the agent to finish before you {undo} a turn.");
        }
        let turns = &hosted.node().turns;
        if turn >= turns.len() {
            let numbered = turns.iter().filter(|turn| turn.switch.is_none()).count();
            return format!("There's no turn {}.", numbered + turn - turns.len() + 1);
        }
        // Counted as people count them, without where the chat moved.
        let Some(number) = clackatoo_protocol::turn_number(turns, turn) else {
            return format!(
                "That's where the chat moved to another agent: there's nothing to {undo}."
            );
        };
        let cwd = PathBuf::from(hosted.working_directory());
        let Some(edits) = self.agent_files.log.turn(chat, turn) else {
            return match self.agent_files.log.summary(chat, turn) {
                Some(files) if files.state == TurnFilesState::TooOld => format!(
                    "Turn {number} is too old to {undo}: clackatoo no longer keeps what it changed."
                ),
                _ => format!("Turn {number} changed no files."),
            };
        };
        // A file whose undo changed nothing has nothing to redo.
        let wanted: Vec<usize> = (edits.files.iter().enumerate())
            .filter(|(_, file)| file.undone == redo && !(file.known && file.changes.is_empty()))
            .map(|(index, _)| index)
            .collect();
        if wanted.is_empty() {
            return format!("Turn {number} has nothing to {undo}.");
        }
        let mut plans = Vec::new();
        let mut refused = Vec::new();
        for index in wanted {
            let file = &edits.files[index];
            match self.plan_undo(file, index) {
                Ok(plan) => plans.push(plan),
                Err(why) => refused.push(format!("in {}, {why}", shown(&file.path, &cwd))),
            }
        }
        if !refused.is_empty() && (!force || plans.is_empty()) {
            let command = if redo { "agentredo" } else { "agentundo" };
            let rest = if plans.is_empty() {
                String::new()
            } else {
                format!(" :{command}! {number} does the other files.")
            };
            return format!(
                "Couldn't {undo} turn {number}, so nothing was: {}.{rest}",
                refused.join("; ")
            );
        }
        let names: Vec<String> = (plans.iter()).map(|plan| shown(&plan.path, &cwd)).collect();
        let remakes = redo.then_some(turn);
        let made = match self.carry_out_undo(chat, plans, remakes) {
            Ok(made) => made,
            Err(why) => return format!("Couldn't {undo} turn {number}, so nothing was: {why}."),
        };
        let Some(edits) = self.agent_files.log.turn_mut(chat, turn) else {
            return String::new();
        };
        let mut put_right = Vec::new();
        let mut failed = Vec::new();
        for (name, made) in names.into_iter().zip(made) {
            match made {
                Ok(made) => {
                    let file = &mut edits.files[made.index];
                    file.existed = made.existed;
                    file.exists = made.exists;
                    file.changes = made.changes;
                    file.undone = !redo;
                    file.tool = None;
                    put_right.push(name);
                }
                Err(why) => failed.push(why),
            }
        }
        let names = put_right.join(", ");
        let mut notice = match (refused.is_empty(), put_right.is_empty()) {
            (_, true) => format!("Couldn't {undo} turn {number}."),
            (true, false) => format!("{done} turn {number}: {names}."),
            (false, false) => format!(
                "{done} turn {number} in {names}; not {}.",
                refused.join("; ")
            ),
        };
        if !failed.is_empty() {
            let _ = write!(notice, " Left as it was: {}.", failed.join("; "));
        }
        notice
    }

    /// Works out what taking `file`'s changes out does, changing nothing,
    /// or says why it can't be done.
    fn plan_undo(&self, file: &FileEdits, index: usize) -> Result<Plan, String> {
        if !file.known {
            return Err("what the file held before wasn't seen".to_owned());
        }
        let buffer = (file.changes.iter())
            .find_map(|change| match change {
                Change::Buffer { buffer, .. } => Some(*buffer),
                Change::Text { .. } => None,
            })
            .or_else(|| self.buffer_of(&file.path));
        let (texts, disk) = match buffer {
            Some(id) => {
                let slot = (self.buffers.get(&id)).ok_or("the buffer it was in has gone")?;
                let buffer = &slot.buffer;
                let disk = DiskStamp::now(buffer.path()).map_err(|err| err.to_string())?;
                if disk != buffer.disk() {
                    return Err(
                        "it's changed on disk since it was read: `:e!` or `:w!` it first"
                            .to_owned(),
                    );
                }
                (Some((id, buffer)), disk)
            }
            None => (
                None,
                DiskStamp::now(&file.path).map_err(|err| err.to_string())?,
            ),
        };
        let there = disk.is_some();
        if file.exists && !there {
            return Err("it's been deleted since".to_owned());
        }
        if !file.exists && there {
            return Err("it's been made again since".to_owned());
        }
        let pairs =
            pairs(texts, &file.changes).ok_or("its history no longer reaches back to the turn")?;
        if let Some((id, buffer)) = texts {
            let text = buffer.text().to_string();
            let saved = buffer.saved().to_string();
            let (text_after, edit) = take_out(&pairs, &text)?;
            let (saved_after, _) = if saved == text {
                (text_after.clone(), edit.clone())
            } else {
                take_out(&pairs, &saved)?
            };
            let disk = if !file.existed && saved_after.is_empty() && text_after.is_empty() {
                Disk::Trash
            } else if saved_after != saved {
                Disk::Write(saved_after)
            } else {
                Disk::Keep
            };
            Ok(Plan {
                index,
                path: file.path.clone(),
                target: Target::Buffer { id, edit },
                disk,
            })
        } else {
            let dir = file.path.parent().unwrap_or(&file.path).to_owned();
            let opened = Buffer::open(file.path.clone(), dir).map_err(|err| err.to_string())?;
            let now = opened.text().to_string();
            let (text, _) = take_out(&pairs, &now)?;
            let disk = if !file.existed && text.is_empty() {
                Disk::Trash
            } else if text != now || !there {
                Disk::Write(text.clone())
            } else {
                Disk::Keep
            };
            Ok(Plan {
                index,
                path: file.path.clone(),
                target: Target::File {
                    file: Box::new(opened),
                    text,
                },
                disk,
            })
        }
    }

    /// Carries out `plans`: first writing each file's new text beside it,
    /// so that if one can't be written, nothing has changed, then putting
    /// them in place, and editing the buffers. Returns what each did, for
    /// the log, or why it couldn't be put in place, which leaves it as it
    /// was.
    fn carry_out_undo(
        &mut self,
        chat: TerminalId,
        plans: Vec<Plan>,
        remakes: Option<usize>,
    ) -> Result<Vec<Result<Done, String>>, String> {
        let staged = self.stage_plans(&plans)?;
        Ok(self.put_in_place(chat, plans, staged, remakes))
    }

    /// Puts each of `plans`' new texts, `staged`, in place, or its file in
    /// the trash, and edits the buffers. A file found changed since it was
    /// read, as by a command or another editor while the others were being
    /// written, is left as it is, its new text thrown away. With
    /// `remakes`, a turn being redone, the changes are that turn's of the
    /// agent's again, for reviewing.
    pub(super) fn put_in_place(
        &mut self,
        chat: TerminalId,
        plans: Vec<Plan>,
        staged: Vec<Option<(Staged, Rope)>>,
        remakes: Option<usize>,
    ) -> Vec<Result<Done, String>> {
        let maker = remakes.map(|turn| clackatoo_text::Maker {
            chat: chat.0,
            turn: u32::try_from(turn).unwrap_or(u32::MAX),
        });
        let author = Author::Agent(chat);
        let trash = self.config.trash.clone();
        let mut made = Vec::with_capacity(plans.len());
        for (plan, staged) in plans.into_iter().zip(staged) {
            let existed = match &plan.target {
                Target::Buffer { id, .. } => self.buffers[id].buffer.disk().is_some(),
                Target::File { file, .. } => file.disk().is_some(),
            };
            let mut exists = existed;
            let mut written = None;
            let read = match &plan.target {
                Target::Buffer { id, .. } => self.buffers[id].buffer.disk(),
                Target::File { file, .. } => file.disk(),
            };
            let changing = staged.is_some() || matches!(plan.disk, Disk::Trash);
            if changing && DiskStamp::now(&plan.path).ok().flatten() != read {
                let file = plan.path.display();
                made.push(Err(format!("{file}, which changed meanwhile")));
                continue;
            }
            if let Some((staged, text)) = staged {
                match staged.commit() {
                    Ok(stamp) => {
                        written = Some((text, Some(stamp)));
                        exists = true;
                    }
                    Err(err) => {
                        made.push(Err(format!("{}: {err}", plan.path.display())));
                        continue;
                    }
                }
            }
            if matches!(plan.disk, Disk::Trash) {
                match crate::explorer::trash(&plan.path, trash.as_deref()) {
                    Ok(()) => {
                        written = Some((Rope::new(), None));
                        exists = false;
                    }
                    Err(err) => {
                        made.push(Err(err.to_string()));
                        continue;
                    }
                }
            }
            let changes = match plan.target {
                Target::Buffer { id, edit } => {
                    self.put_buffer_in_place(id, edit, written, (author, maker))
                }
                Target::File { file, text } => {
                    let before = file.text().to_string();
                    let after = if exists { text } else { String::new() };
                    let stamp = written.and_then(|(_, stamp)| stamp);
                    match maker {
                        Some(maker) => self.agent_changed_file(
                            &plan.path,
                            (Some(&before), exists.then_some(after.as_str())),
                            maker,
                            stamp,
                        ),
                        None => {
                            self.file_changed_in_review(&plan.path, (&before, &after), stamp);
                        }
                    }
                    vec![Change::Text {
                        edit: Edit::between(&before, &after),
                        after: Rope::from_str(&after),
                    }]
                }
            };
            made.push(Ok(Done {
                index: plan.index,
                existed,
                exists,
                changes,
            }));
        }
        made
    }

    /// Edits buffer `id` with `edit`, as `author`, an agent's own change
    /// again if `maker` says whose, and takes `written`, the text and stamp
    /// its file was given, if it was. Returns the changes, for the log.
    pub(super) fn put_buffer_in_place(
        &mut self,
        id: BufferId,
        edit: Edit,
        written: Option<(Rope, Option<DiskStamp>)>,
        (author, maker): (Author, Option<clackatoo_text::Maker>),
    ) -> Vec<Change> {
        let slot = self
            .buffers
            .get_mut(&id)
            .expect("planned on an open buffer");
        let saved = slot.buffer.saved().clone();
        if let Some((text, stamp)) = written {
            slot.buffer.written(text, stamp);
        }
        let mut changes = Vec::new();
        // Only the file changed, as the buffer had lost the turn's change
        // already: the file's change is what redoing takes out.
        if edit.is_noop() && *slot.buffer.saved() != saved {
            let after = slot.buffer.saved().clone();
            changes.push(Change::Text {
                edit: Edit::between(&saved.to_string(), &after.to_string()),
                after,
            });
        }
        if !edit.is_noop() {
            let version = slot.buffer.version();
            match slot.buffer.edit(edit, version, true, author) {
                Ok(one) => {
                    let bytes = edit_bytes(&one.edit);
                    changes.push(Change::Buffer {
                        buffer: id,
                        version: one.version,
                        bytes,
                    });
                    match maker {
                        Some(maker) => self.made_by_agent(id, one, maker),
                        None => self.made_by(id, one, author),
                    }
                }
                Err(err) => log::warn!("buffer {id}: {err}"),
            }
        }
        self.note_modified(id);
        self.saved_in_server(id);
        changes
    }

    /// Writes each of `plans`' new texts beside its file, checking the
    /// file is as it was when it was read, and the text it is, if any.
    pub(super) fn stage_plans(
        &self,
        plans: &[Plan],
    ) -> Result<Vec<Option<(Staged, Rope)>>, String> {
        let mut staged: Vec<Option<(Staged, Rope)>> = Vec::with_capacity(plans.len());
        for plan in plans {
            let failed = |err: clackatoo_protocol::Error| format!("{}: {err}", plan.path.display());
            let buffer = match &plan.target {
                Target::Buffer { id, .. } => &self.buffers[id].buffer,
                Target::File { file, .. } => file,
            };
            staged.push(match &plan.disk {
                Disk::Write(text) => {
                    let text = Rope::from_str(text);
                    Some((buffer.stage(&text, false).map_err(failed)?, text))
                }
                Disk::Trash => {
                    if DiskStamp::now(&plan.path).ok().flatten() != buffer.disk() {
                        return Err(format!("{} has changed meanwhile", plan.path.display()));
                    }
                    None
                }
                Disk::Keep => None,
            });
        }
        Ok(staged)
    }

    /// Buffer `id` is closing: the log's changes to it are turned into
    /// changes to its file, with the texts before and after each, as its
    /// history goes with it.
    pub(super) fn buffer_closing(&mut self, id: BufferId) {
        self.keep_texts(id, u64::MAX);
    }

    /// Buffer `id`'s history is about to let go of the edits that made
    /// `version` and those before: the log's changes to it that reach back
    /// that far are turned into changes to its file, as they are when it
    /// closes.
    pub(super) fn history_trimming(&mut self, id: BufferId, version: u64) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let buffer = &slot.buffer;
        let mut chats = Vec::new();
        for (chat, file) in self.agent_files.log.changes_to(id) {
            // Only the changes that reach back that far, each as the edit
            // the history has for it and the text it made, which shares
            // what it has in common with the text now.
            let reaching: Vec<u64> = (file.changes.iter())
                .filter_map(|change| match change {
                    Change::Buffer {
                        buffer: of,
                        version: made,
                        ..
                    } if *of == id && *made <= version => Some(*made),
                    _ => None,
                })
                .collect();
            if reaching.is_empty() {
                continue;
            }
            let made: Option<Vec<(Rope, Edit)>> = (buffer.texts_at(&reaching)).and_then(|texts| {
                (texts.into_iter().zip(&reaching))
                    .map(|(after, &made)| Some((after, buffer.edit_made(made)?)))
                    .collect()
            });
            if let Some(made) = made {
                let mut made = made.into_iter();
                for change in &mut file.changes {
                    if matches!(change, Change::Buffer { buffer: of, version: v, .. }
                        if *of == id && *v <= version)
                        && let Some((after, edit)) = made.next()
                    {
                        *change = Change::Text { after, edit };
                    }
                }
            } else {
                file.known = false;
                file.changes.clear();
            }
            chats.push(chat);
        }
        chats.dedup();
        for chat in chats {
            let dropped = self.agent_files.log.trim(chat);
            self.refresh_files(chat, dropped);
        }
    }

    /// Turns the log's changes to buffer `id` into changes to its file,
    /// with the texts before and after each.
    fn keep_texts(&mut self, id: BufferId, version: u64) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let buffer = &slot.buffer;
        let mut chats = Vec::new();
        let reaching = |file: &FileEdits| {
            (file.changes.iter()).any(|change| {
                matches!(change, Change::Buffer { buffer, version: made, .. }
                    if *buffer == id && *made <= version)
            })
        };
        for (chat, file) in self.agent_files.log.changes_to(id) {
            if !reaching(file) {
                continue;
            }
            if let Some(pairs) = pairs(Some((id, buffer)), &file.changes) {
                file.changes = (pairs.into_iter())
                    .map(|(after, before)| Change::Text {
                        edit: Edit::between(&before, &after),
                        after: Rope::from_str(&after),
                    })
                    .collect();
            } else {
                file.known = false;
                file.changes.clear();
            }
            chats.push(chat);
        }
        chats.dedup();
        for chat in chats {
            let dropped = self.agent_files.log.trim(chat);
            self.refresh_files(chat, dropped);
        }
    }
}

/// The text after each of `changes` and before it, oldest first, the
/// changes to `buffer` rebuilt from its history, runs of its versions with
/// nothing between them as one. `None` if one is to a buffer that isn't
/// `buffer`, or its history doesn't reach back to it.
fn pairs(buffer: Option<(BufferId, &Buffer)>, changes: &[Change]) -> Option<Vec<(String, String)>> {
    // Whether each change starts a run: it does unless it's the version
    // after the change before it.
    let mut starts = Vec::with_capacity(changes.len());
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut previous = None;
    for change in changes {
        let version = match change {
            Change::Buffer {
                buffer: id,
                version,
                ..
            } => {
                if buffer.is_none_or(|(open, _)| open != *id) {
                    return None;
                }
                Some(*version)
            }
            Change::Text { .. } => None,
        };
        let starts_run = match (previous, version, runs.last_mut()) {
            (Some(previous), Some(version), Some(run)) if previous + 1 == version => {
                run.1 = version;
                false
            }
            (_, Some(version), _) => {
                runs.push((version, version));
                true
            }
            _ => true,
        };
        starts.push(starts_run);
        previous = version;
    }
    let mut texts = match buffer {
        Some((_, buffer)) if !runs.is_empty() => {
            let versions: Vec<u64> = (runs.iter())
                .flat_map(|&(first, last)| [last, first.saturating_sub(1)])
                .collect();
            buffer.texts_at(&versions)?.into_iter()
        }
        _ => Vec::new().into_iter(),
    };
    let mut pairs = Vec::new();
    for (change, starts_run) in changes.iter().zip(starts) {
        match change {
            Change::Buffer { .. } if !starts_run => {}
            Change::Buffer { .. } => {
                let after = texts.next()?.to_string();
                let before = texts.next()?.to_string();
                pairs.push((after, before));
            }
            Change::Text { after, edit } => {
                let after = after.to_string();
                let before = edit.invert().apply_to(&after).ok()?;
                pairs.push((after, before));
            }
        }
    }
    Some(pairs)
}

/// `now` with each of `pairs`' changes taken out, newest first: the
/// change from the text after it back to the text before, merged into
/// what `now` has become. Returns the text made and the edit that makes
/// it of `now`, or says which lines have changed since, if a change
/// touches lines changed since it was made.
pub(super) fn take_out(pairs: &[(String, String)], now: &str) -> Result<(String, Edit), String> {
    let mut text = now.to_owned();
    let mut whole = Edit::keep(now.len());
    for (after, before) in pairs.iter().rev() {
        match merge(after, before, &text).map_err(|err| err.to_string())? {
            Merged::Made(edit) => {
                text = edit.apply_to(&text).map_err(|err| err.to_string())?;
                whole = compose(&whole, &edit).map_err(|err| err.to_string())?;
            }
            Merged::Conflict(ranges) => {
                // Where those lines are in the text now.
                let since = Edit::between(after, &text);
                let ranges: Vec<_> = (ranges.iter())
                    .map(|range| {
                        since.map(range.start, Bias::Before)..since.map(range.end, Bias::After)
                    })
                    .collect();
                let lines = line_numbers(&text, &ranges);
                let verb = if lines.starts_with("lines") {
                    "have"
                } else {
                    "has"
                };
                return Err(format!("{lines} {verb} changed since"));
            }
        }
    }
    Ok((text, whole))
}

/// `path` as the chat shows it: from its working directory, if it's in
/// it.
pub(super) fn shown(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

#[cfg(test)]
pub(in crate::server) mod tests {
    use agent_client_protocol_schema::v1::StopReason;
    use clackatoo_protocol::{ChatAction, Node, OpenFile, TurnFiles};
    use serde_json::json;

    use super::*;
    use crate::chat::{RawDiff, Wrote};
    use crate::server::agent_files::tests::{daemon_with_chat, start_tool};
    use crate::server::agent_files::{Capture, Seen};
    use crate::server::chats::AgentId;

    pub(in crate::server) const AGENT: (AgentId, &str) = (AgentId(0), "claude");

    /// A daemon with a chat answering its first message, in a directory of
    /// its own.
    pub(in crate::server) struct World {
        _dir: tempfile::TempDir,
        pub(in crate::server) root: PathBuf,
        pub(in crate::server) daemon: Daemon,
        pub(in crate::server) chat: TerminalId,
        tools: usize,
    }

    impl World {
        pub(in crate::server) fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let (daemon, chat) = daemon_with_chat(&root);
            Self {
                _dir: dir,
                root,
                daemon,
                chat,
                tools: 0,
            }
        }

        pub(in crate::server) fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }

        pub(in crate::server) fn open(&mut self, name: &str) -> BufferId {
            let file = OpenFile {
                path: self.path(name).display().to_string(),
                cwd: None,
            };
            self.daemon.open_buffer(file, None).unwrap()
        }

        pub(in crate::server) fn text(&self, id: BufferId) -> String {
            self.daemon.buffers[&id].buffer.text().to_string()
        }

        pub(in crate::server) fn disk(&self, name: &str) -> Option<String> {
            std::fs::read_to_string(self.path(name)).ok()
        }

        /// The agent writes `text` to `name` through the daemon, in a tool
        /// call of its own.
        pub(in crate::server) fn write(&mut self, name: &str, text: &str) {
            self.write_as(self.chat, "s1", name, text);
        }

        /// The agent of chat `chat`, whose session is `session`, writes
        /// `text` to `name` through the daemon, in a tool call of its own.
        pub(in crate::server) fn write_as(
            &mut self,
            chat: TerminalId,
            session: &str,
            name: &str,
            text: &str,
        ) {
            self.tools += 1;
            start_tool(&mut self.daemon, chat, &format!("w{}", self.tools));
            let path = self.path(name);
            (self.daemon).write_for_agent(AGENT, session, &json!(1), &path, text);
        }

        /// Ends the turn being answered.
        pub(in crate::server) fn end_turn(&mut self) {
            let chat = self.daemon.chat(self.chat).unwrap();
            chat.answered(Ok(StopReason::EndTurn));
            self.daemon.settle_turn(self.chat);
        }

        /// Starts another turn.
        pub(in crate::server) fn next_turn(&mut self) {
            let chat = self.daemon.chat(self.chat).unwrap();
            chat.act(ChatAction::Send("more".into()));
        }

        /// The user types `inserted` over `removed` at `at` in buffer `id`.
        pub(in crate::server) fn type_in(
            &mut self,
            id: BufferId,
            at: usize,
            removed: &str,
            inserted: &str,
        ) {
            let buffer = &mut self.daemon.buffers.get_mut(&id).unwrap().buffer;
            let edit = Edit::replace(buffer.text().len_bytes(), at, removed, inserted);
            let version = buffer.version();
            let made = buffer.edit(edit, version, true, Author::User).unwrap();
            self.daemon.made_for_user(id, made);
        }

        /// Undoes or redoes turn `turn`, and returns what the chat says.
        pub(in crate::server) fn undo(&mut self, turn: usize, redo: bool, force: bool) -> String {
            self.daemon.undo_turn(self.chat, turn, redo, force);
            self.chat_node().notice.unwrap_or_default()
        }

        pub(in crate::server) fn chat_node(&mut self) -> clackatoo_protocol::ChatNode {
            let Node::Chat(chat) = self.daemon.chat(self.chat).unwrap().view().root else {
                panic!("not a chat");
            };
            *chat
        }

        /// What the chat shows turn `turn` changed in files.
        pub(in crate::server) fn files(&mut self, turn: usize) -> Option<TurnFiles> {
            self.chat_node().turns[turn].files
        }

        pub(in crate::server) fn state(&mut self, turn: usize) -> Option<TurnFilesState> {
            self.files(turn).map(|files| files.state)
        }
    }

    #[test]
    fn a_turn_is_undone_keeping_later_edits_and_redone() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\nthree\n");
        world.end_turn();
        let made = TurnFiles {
            count: 1,
            state: TurnFilesState::Made,
        };
        assert_eq!(world.files(0), Some(made));
        // Yours, since, on another line, not saved.
        world.type_in(id, 8, "three", "THREE");
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.text(id), "one\ntwo\nTHREE\n");
        // The file gets the undo under your unsaved edit.
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(world.state(0), Some(TurnFilesState::Undone));
        assert_eq!(world.undo(0, false, false), "Turn 1 has nothing to undo.");

        assert_eq!(world.undo(0, true, false), "Redid turn 1: a.txt.");
        assert_eq!(world.text(id), "ONE\ntwo\nTHREE\n");
        assert_eq!(world.disk("a.txt").as_deref(), Some("ONE\ntwo\nthree\n"));
        assert_eq!(world.state(0), Some(TurnFilesState::Made));
        // And again: the redo is what's taken out now.
        world.undo(0, false, false);
        assert_eq!(world.text(id), "one\ntwo\nTHREE\n");
        // An undo is one step of the agent's, which it can take back.
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        (buffer.reverse(Author::Agent(world.chat), false))
            .unwrap()
            .unwrap();
        assert_eq!(world.text(id), "ONE\ntwo\nTHREE\n");
    }

    #[test]
    fn an_undo_is_refused_where_lines_have_changed_since() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(world.path("b.txt"), "b\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "one\nTWO\nthree\n");
        world.write("b.txt", "B\n");
        world.end_turn();
        world.type_in(id, 4, "TWO", "TWO!");
        assert_eq!(
            world.undo(0, false, false),
            "Couldn't undo turn 1, so nothing was: in a.txt, line 2 has changed since. \
             :agentundo! 1 does the other files."
        );
        assert_eq!(world.text(id), "one\nTWO!\nthree\n");
        assert_eq!(world.disk("b.txt").as_deref(), Some("B\n"));
        assert_eq!(world.state(0), Some(TurnFilesState::Made));
        // Forced, the rest is undone.
        assert_eq!(
            world.undo(0, false, true),
            "Undid turn 1 in b.txt; not in a.txt, line 2 has changed since."
        );
        assert_eq!(world.disk("b.txt").as_deref(), Some("b\n"));
        assert_eq!(world.state(0), Some(TurnFilesState::Partly));
        // Once yours is undone, the rest goes too.
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.reverse(Author::User, false).unwrap().unwrap();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.text(id), "one\ntwo\nthree\n");
        assert_eq!(world.state(0), Some(TurnFilesState::Undone));
    }

    #[test]
    fn an_earlier_turn_is_undone_keeping_a_later_one() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\nthree\nfour\n");
        world.end_turn();
        world.next_turn();
        world.write("a.txt", "ONE\ntwo\nthree\nFOUR\n");
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.text(id), "one\ntwo\nthree\nFOUR\n");
        assert_eq!(
            world.disk("a.txt").as_deref(),
            Some("one\ntwo\nthree\nFOUR\n")
        );
        assert_eq!(world.state(1), Some(TurnFilesState::Made));
        // The later one, then the earlier one again.
        world.undo(1, false, false);
        assert_eq!(world.text(id), "one\ntwo\nthree\nfour\n");
        world.undo(0, true, false);
        assert_eq!(world.text(id), "ONE\ntwo\nthree\nfour\n");
        // A turn whose lines a later one changed can't be undone without
        // it.
        world.undo(1, true, false);
        world.next_turn();
        world.write("a.txt", "1\ntwo\nthree\nFOUR\n");
        world.end_turn();
        let notice = world.undo(0, false, false);
        assert!(notice.contains("line 1 has changed since"), "{notice}");
        assert_eq!(world.text(id), "1\ntwo\nthree\nFOUR\n");
    }

    #[test]
    fn closed_files_are_undone_on_disk_and_a_file_made_goes_to_the_trash() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        world.write("a.txt", "ONE\ntwo\n");
        world.write("new.txt", "made\n");
        world.end_turn();
        assert_eq!(world.files(0).map(|files| files.count), Some(2));
        // Changed since, by something else, on another line.
        std::fs::write(world.path("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt, new.txt.");
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(world.disk("new.txt"), None);
        let trashed = world.root.join("Trash/files/new.txt");
        assert_eq!(std::fs::read_to_string(trashed).unwrap(), "made\n");
        // Back as they were.
        assert_eq!(world.undo(0, true, false), "Redid turn 1: a.txt, new.txt.");
        assert_eq!(world.disk("a.txt").as_deref(), Some("ONE\ntwo\nthree\n"));
        assert_eq!(world.disk("new.txt").as_deref(), Some("made\n"));
        // A file made again since isn't written over.
        world.undo(0, false, false);
        std::fs::write(world.path("new.txt"), "mine\n").unwrap();
        let notice = world.undo(0, true, false);
        assert!(
            notice.contains("in new.txt, it's been made again since"),
            "{notice}"
        );
        assert_eq!(world.disk("new.txt").as_deref(), Some("mine\n"));
    }

    #[test]
    fn a_buffer_closing_leaves_its_changes_to_be_undone_on_disk() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        world.write("a.txt", "ONE\nTWO\n");
        world.end_turn();
        world.daemon.buffer_closing(id);
        world.daemon.buffers.remove(&id);
        let file = &(world.daemon.agent_files.log.turn(world.chat, 0))
            .unwrap()
            .files[0];
        // Two edits, one after the other: one change.
        assert!(matches!(file.changes[..], [Change::Text { .. }]));
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\ntwo\n"));
    }

    #[test]
    fn a_turn_whose_edits_a_buffer_lets_go_of_can_still_be_undone() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\ntwo\n");
        world.end_turn();
        // Typed and taken out again, as big pastes are, until the history
        // has grown past what it keeps.
        let big = "x".repeat(4 << 20);
        let kept = clackatoo_text::Limits::default().kept_bytes;
        for _ in 0..=kept / big.len() / 2 {
            world.type_in(id, 8, "", &big);
            world.type_in(id, 8, &big, "");
        }
        let buffer = &world.daemon.buffers[&id].buffer;
        assert!(
            buffer.edits_since(1).is_none(),
            "the turn's edit was let go"
        );
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.text(id), "one\ntwo\n");
    }

    #[test]
    fn letting_edits_go_keeps_texts_only_for_the_changes_that_reach_back_that_far() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        let id = world.open("a.txt");
        // Two writes in one turn, with typing between them.
        world.write("a.txt", "ONE\ntwo\n");
        let big = "x".repeat(4 << 20);
        for _ in 0..3 {
            world.type_in(id, 8, "", &big);
            world.type_in(id, 8, &big, "");
        }
        world.write("a.txt", "ONE\nTWO\n");
        world.end_turn();
        world.type_in(id, 8, "", &big);
        world.type_in(id, 8, &big, "");
        let buffer = &world.daemon.buffers[&id].buffer;
        assert!(
            buffer.edits_since(1).is_none(),
            "the first write was let go"
        );
        let log = &world.daemon.agent_files.log;
        let changes = log.turn(world.chat, 0).unwrap().files[0].changes.clone();
        assert!(
            matches!(changes[..], [Change::Text { .. }, Change::Buffer { .. }]),
            "only the first, which reached back, is a text"
        );
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.text(id), "one\ntwo\n");
    }

    /// What Claude Code's adapter says of tool `e1` writing `name` itself:
    /// that it's going to, or that it has, and the change it made.
    fn wrote(world: &World, name: &str, done: bool, diff: Option<(&str, &str)>) -> Wrote {
        let path = world.path(name).display().to_string();
        Wrote {
            done,
            turn: 0,
            tool: "e1".into(),
            order: 0,
            paths: vec![path.clone()],
            diffs: (diff.into_iter())
                .map(|(old, new)| RawDiff {
                    path: path.clone(),
                    old: Some(old.into()),
                    new: new.into(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_closed_file_an_agent_writes_itself_is_read_before_it_does() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(world.path("late.txt"), "x\n").unwrap();
        let chat = world.chat;
        // The tool names the file as its input streams in, then writes it.
        world
            .daemon
            .take_written(chat, &wrote(&world, "a.txt", false, None));
        std::fs::write(world.path("a.txt"), "one\nTWO\n").unwrap();
        let done = wrote(&world, "a.txt", true, Some(("two", "TWO")));
        world.daemon.take_written(chat, &done);
        // Read only after it wrote, as if the daemon lagged: not known.
        std::fs::write(world.path("late.txt"), "X\n").unwrap();
        world
            .daemon
            .take_written(chat, &wrote(&world, "late.txt", false, None));
        let late = wrote(&world, "late.txt", true, Some(("x", "X")));
        world.daemon.take_written(chat, &late);
        world.end_turn();
        assert_eq!(
            world.undo(0, false, false),
            "Couldn't undo turn 1, so nothing was: in late.txt, what the file held before \
             wasn't seen. :agentundo! 1 does the other files."
        );
        assert_eq!(
            world.undo(0, false, true),
            "Undid turn 1 in a.txt; not in late.txt, what the file held before wasn't seen."
        );
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\ntwo\n"));
        assert_eq!(world.disk("late.txt").as_deref(), Some("X\n"));
    }

    #[test]
    fn undoing_waits_for_the_agent_and_says_what_it_cant() {
        let mut world = World::new();
        world.write("a.txt", "a\n");
        assert_eq!(
            world.undo(0, false, false),
            "Wait for the agent to finish before you undo a turn."
        );
        world.end_turn();
        assert_eq!(world.undo(3, false, false), "There's no turn 4.");
        world.next_turn();
        world.end_turn();
        assert_eq!(world.undo(1, false, false), "Turn 2 changed no files.");
        assert_eq!(world.undo(0, true, false), "Turn 1 has nothing to redo.");
    }

    #[test]
    fn a_file_changed_while_the_others_are_written_is_left_as_it_is() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\n").unwrap();
        std::fs::write(world.path("b.txt"), "two\n").unwrap();
        world.write("a.txt", "ONE\n");
        world.write("b.txt", "TWO\n");
        world.end_turn();
        let edits = world.daemon.agent_files.log.turn(world.chat, 0).unwrap();
        let plans: Vec<Plan> = (edits.files.iter().enumerate())
            .map(|(index, file)| world.daemon.plan_undo(file, index).unwrap())
            .collect();
        let staged = world.daemon.stage_plans(&plans).unwrap();
        // Written by something else as the undo's new texts were synced.
        std::fs::write(world.path("b.txt"), "theirs\n").unwrap();
        let made = world.daemon.put_in_place(world.chat, plans, staged, None);
        assert!(made[0].is_ok());
        let skipped = format!("{}, which changed meanwhile", world.path("b.txt").display());
        assert_eq!(made[1].as_ref().err(), Some(&skipped));
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\n"));
        assert_eq!(world.disk("b.txt").as_deref(), Some("theirs\n"));
        let left: Vec<String> = (std::fs::read_dir(&world.root).unwrap().flatten())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            left.iter().all(|name| !name.contains(".clackatoo-")),
            "{left:?}"
        );
    }

    #[test]
    fn a_file_that_cant_be_read_before_a_tool_writes_it_is_never_taken_for_new() {
        use std::os::unix::fs::PermissionsExt;
        let mut world = World::new();
        let path = world.path("secret.txt");
        std::fs::write(&path, "old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            eprintln!("running as root: every file can be read; skipping");
            return;
        }
        let chat = world.chat;
        let named = wrote(&world, "secret.txt", false, None);
        world.daemon.take_written(chat, &named);
        assert!(matches!(
            world.daemon.agent_files.captures[..],
            [Capture {
                text: Seen::Unreadable,
                ..
            }]
        ));
        // The tool writes it, and says so as of a file it made.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&path, "new\n").unwrap();
        let mut done = wrote(&world, "secret.txt", true, Some(("", "new\n")));
        done.diffs[0].old = None;
        world.daemon.take_written(chat, &done);
        world.end_turn();
        assert_eq!(
            world.undo(0, false, false),
            "Couldn't undo turn 1, so nothing was: in secret.txt, what the file held before \
             wasn't seen."
        );
        assert_eq!(world.disk("secret.txt").as_deref(), Some("new\n"));
    }

    #[test]
    fn an_undo_only_the_file_needed_is_redone_on_the_file() {
        let mut world = World::new();
        std::fs::write(world.path("a.txt"), "one\n").unwrap();
        let id = world.open("a.txt");
        world.write("a.txt", "ONE\n");
        world.end_turn();
        // You took it back yourself, and haven't saved.
        world.type_in(id, 0, "ONE", "one");
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.disk("a.txt").as_deref(), Some("one\n"));
        assert_eq!(world.undo(0, true, false), "Redid turn 1: a.txt.");
        assert_eq!(world.disk("a.txt").as_deref(), Some("ONE\n"));
        // With the file and the buffer both as they were, there's nothing
        // to redo.
        world.undo(0, false, false);
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.save(false).unwrap();
        world.undo(0, true, false);
        world.type_in(id, 0, "ONE", "one");
        let buffer = &mut world.daemon.buffers.get_mut(&id).unwrap().buffer;
        buffer.save(false).unwrap();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: a.txt.");
        assert_eq!(world.undo(0, true, false), "Turn 1 has nothing to redo.");
    }

    /// A file of 300,000 lines, as `big.txt` in `world`: more than is
    /// worked out on the loop. Returns its text.
    fn big_file(world: &World) -> String {
        let mut text = String::new();
        for n in 1..=300_000 {
            use std::fmt::Write as _;
            writeln!(text, "line {n}").unwrap();
        }
        std::fs::write(world.path("big.txt"), &text).unwrap();
        text
    }

    /// Has tool `tool` of the chat's agent change line `line` of `big.txt`,
    /// `before`, itself, and say so, so the change is worked out off the
    /// loop. Returns the file's text after it.
    fn change_big_file(world: &mut World, tool: (&str, u32), before: &str) -> String {
        change_big(world, "big.txt", tool, before)
    }

    /// As [`change_big_file`], to the file `name`.
    fn change_big(
        world: &mut World,
        name: &str,
        (tool, line): (&str, u32),
        before: &str,
    ) -> String {
        let chat = world.chat;
        let path = world.path(name).display().to_string();
        start_tool(&mut world.daemon, chat, tool);
        let said = |done, diffs| Wrote {
            done,
            turn: 0,
            tool: tool.into(),
            order: 0,
            paths: vec![path.clone()],
            diffs,
        };
        // About to change it: its text before is kept.
        world.daemon.take_written(chat, &said(false, vec![]));
        let (old, new) = (format!("line {line}\n"), format!("line #{line}\n"));
        let after = before.replacen(&old, &new, 1);
        std::fs::write(world.path(name), &after).unwrap();
        let diff = RawDiff {
            path: path.clone(),
            old: Some(old),
            new,
        };
        world.daemon.take_written(chat, &said(true, vec![diff]));
        assert!(
            world.daemon.agent_files.diffing(chat),
            "worked out off the loop"
        );
        after
    }

    #[test]
    fn a_write_while_a_big_change_is_worked_out_is_logged_after_it() {
        let mut world = World::new();
        let first = big_file(&world);
        change_big_file(&mut world, ("t1", 1), &first);
        // Another write to the file, through the daemon, waits for it.
        world.write("big.txt", "small\n");
        assert!(!world.daemon.agent_files.diffing(world.chat));
        world.daemon.take_settled();
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.disk("big.txt"), Some(first));
    }

    #[test]
    fn opening_a_file_whose_big_change_is_worked_out_takes_it_in_first() {
        let mut world = World::new();
        let first = big_file(&world);
        let second = change_big_file(&mut world, ("t1", 1), &first);
        let id = world.open("big.txt");
        assert!(!world.daemon.agent_files.diffing(world.chat));
        assert!(!world.daemon.reviews.keeps_closed(&world.path("big.txt")));
        assert!(
            !world.daemon.buffers[&id].agents.is_empty(),
            "its hunk came along"
        );
        assert_eq!(world.text(id), second);
        world.end_turn();
        assert_eq!(world.undo(0, false, false), "Undid turn 1: big.txt.");
        assert_eq!(world.text(id), first);
    }

    #[test]
    fn a_big_change_the_worker_fails_at_is_noted_unknown() {
        use crate::server::settling::{CAUGHT, GONE};
        let mut world = World::new();
        let first = big_file(&world);
        // It panics working it out.
        world.daemon.agent_files.panic_next(CAUGHT);
        let second = change_big_file(&mut world, ("t1", 1), &first);
        world.daemon.settle_all_pending();
        assert!(!world.daemon.agent_files.diffing(world.chat));
        world.end_turn();
        assert_eq!(
            world.undo(0, false, false),
            "Couldn't undo turn 1, so nothing was: in big.txt, what the file held before \
             wasn't seen."
        );
        // The worker itself goes: what it had is unknown, nothing waits on.
        world.next_turn();
        world.daemon.agent_files.panic_next(GONE);
        let third = change_big_file(&mut world, ("t2", 2), &second);
        world.daemon.settle_all_pending();
        assert!(!world.daemon.agent_files.diffing(world.chat));
        // And the next turn's is worked out by a new one.
        world.end_turn();
        world.next_turn();
        change_big_file(&mut world, ("t3", 3), &third);
        world.daemon.settle_all_pending();
        assert!(!world.daemon.agent_files.diffing(world.chat));
    }

    #[test]
    fn changes_given_a_worker_found_gone_are_noted_unknown() {
        use crate::server::settling::GONE;
        let mut world = World::new();
        let first = big_file(&world);
        std::fs::write(world.path("big2.txt"), &first).unwrap();
        // The worker goes as it takes the first change, which it never
        // gives back; the next change finds it gone.
        world.daemon.agent_files.panic_next(GONE);
        change_big_file(&mut world, ("t1", 1), &first);
        // Give it time to go.
        std::thread::sleep(std::time::Duration::from_millis(500));
        let chat = world.chat;
        let path = world.path("big2.txt").display().to_string();
        start_tool(&mut world.daemon, chat, "t2");
        let said = |done, diffs| Wrote {
            done,
            turn: 0,
            tool: "t2".into(),
            order: 0,
            paths: vec![path.clone()],
            diffs,
        };
        world.daemon.take_written(chat, &said(false, vec![]));
        let after = first.replacen("line 2\n", "line #2\n", 1);
        std::fs::write(world.path("big2.txt"), &after).unwrap();
        let diff = RawDiff {
            path: path.clone(),
            old: Some("line 2\n".into()),
            new: "line #2\n".into(),
        };
        // Worked out now, and the first, which the worker had, is failed
        // at once rather than left waiting.
        world.daemon.take_written(chat, &said(true, vec![diff]));
        assert!(!world.daemon.agent_files.diffing(chat));
        world.end_turn();
        let notice = world.undo(0, false, false);
        assert!(
            notice.contains("big.txt, what the file held before wasn't seen"),
            "{notice}"
        );
    }
}
