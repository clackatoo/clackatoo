//! Renaming, and every other edit a language server asks for across files
//! (a `WorkspaceEdit`): making its edits, in every file they touch. A
//! rename's are one; so are a code action's, and those a server asks for
//! itself, as it does when running a command.
//!
//! A buffer that's open is edited as the user, one undo step in each, so
//! every window following it is sent the edit, and `u` undoes it. A file
//! that isn't open is edited on disk, written as a save writes, and the
//! server told. Opening those as buffers with the changes not saved, as
//! Neovim does, would leave them where nothing shows them and nothing
//! saves them but opening each again.
//!
//! Files not open are read, and their new text written and synced beside
//! them, on a thread of their own, so a rename of something used
//! everywhere doesn't stop every terminal while the disk works. Nothing is
//! put in place there: once they're all written, the event loop checks,
//! at once, that none has changed on disk or been opened since it was
//! read, and that no buffer has been edited inside a place. Only then is
//! each file put in its place, by a rename, which is quick, keeping the
//! old by a second link, so that one that can't be put in place puts back
//! those that were; and the buffers are edited, in the same go. A rename
//! that fails changes nothing, unless putting back fails too, which it
//! says.
//!
//! The server counts each edit in the text it had when it was asked,
//! which may be older than the buffer's by the time it's made: each place
//! moves along with the edits made meanwhile, by anyone, and a rename
//! whose places were edited inside is refused. Two renames can't write one
//! file at once: the second is refused. A rename whose client goes while
//! its files are written is dropped, and an upgrade or shutdown drops
//! those under way, throwing away what they wrote.
//!
//! All of that holds for any edit made here, whatever it's for: "rename"
//! below means any of them.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Answer, Author, BufferId, Edit};
use clackatoo_text::{Bias, Rope};
use mio::Token;
use serde_json::Value;

use super::Daemon;
use super::language::{Before, ServerId};
use crate::buffer::{self, DiskStamp, Placed, Staged};
use crate::lsp::{self, Encoding, FileEdits, TextEdit};

/// The most files not open one rename edits: far more than most renames
/// touch, but short of writing thousands of files for one.
const MAX_ON_DISK: usize = 200;

/// How long an upgrade or shutdown waits for renames' threads to stop.
const ABANDON_WAIT: Duration = Duration::from_secs(1);

/// The renames whose files are being written.
#[derive(Debug, Default)]
pub(super) struct Renames {
    next: u64,
    under_way: HashMap<u64, Renaming>,
    /// What each thread wrote, by the rename's number, or why it wrote
    /// nothing.
    ended: Arc<Mutex<Vec<(u64, Staging)>>>,
    /// The files not open that renames under way are writing, which
    /// another can't write too.
    writing: HashSet<PathBuf>,
}

/// What a rename's thread wrote: each file's new text beside it, or why
/// it wrote none.
type Staging = Result<Vec<StagedFile>, String>;

/// A file's new text, written beside it, and the file as it was read.
#[derive(Debug)]
struct StagedFile {
    staged: Staged,
    read: DiskStamp,
}

/// A rename, until its files not open have been written, after which
/// they're put in place, its buffers edited, and whoever asked told.
#[derive(Debug)]
struct Renaming {
    server: ServerId,
    purpose: Purpose,
    buffers: Vec<BufferPlan>,
    occurrences: usize,
    files: usize,
    /// The files not open it writes.
    paths: Vec<PathBuf>,
    /// Set when it's to be dropped, as its client has gone: its thread
    /// stops, and what it wrote is thrown away.
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// What an edit across files is made for, and so who's told how it went,
/// and how.
#[derive(Debug)]
pub(super) enum Purpose {
    /// The rename the client at `token` asked for about `buffer`, numbered
    /// `id`.
    Rename {
        token: Token,
        buffer: BufferId,
        id: u64,
    },
    /// The code action called `title` the client at `token` picked, about
    /// `buffer`, numbered `id`. Its command, an LSP `Command`, if it has
    /// one, is run once its edit has been made.
    Action {
        token: Token,
        buffer: BufferId,
        id: u64,
        title: String,
        command: Option<Value>,
    },
    /// The server's own request to make it, numbered `request`, as when it
    /// runs a command.
    Asked { request: Value },
}

impl Purpose {
    /// The client that asked, and about which buffer, if one did.
    fn client(&self) -> Option<(Token, BufferId)> {
        match self {
            Self::Rename { token, buffer, .. } | Self::Action { token, buffer, .. } => {
                Some((*token, *buffer))
            }
            Self::Asked { .. } => None,
        }
    }
}

/// What an edit across files changed: so many places, in so many files.
/// None, if it asked for nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Made {
    pub(super) occurrences: usize,
    pub(super) files: usize,
}

/// An edit across files, checked and planned: its edits to buffers, and
/// to files not open, how many places it changes, and how its server
/// counts columns.
#[derive(Debug)]
struct Planned {
    buffers: Vec<BufferPlan>,
    disk: Vec<FileEdits>,
    occurrences: usize,
    encoding: Encoding,
}

/// A rename's edit to an open buffer, checked against its text at `base`.
#[derive(Debug)]
struct BufferPlan {
    id: BufferId,
    base: u64,
    replaces: Vec<Replace>,
}

impl Renames {
    /// Notes that `renaming`'s thread has ended, or is ending: it writes
    /// its files no more.
    fn ended_one(&mut self, renaming: &mut Renaming) {
        for path in &renaming.paths {
            self.writing.remove(path);
        }
        if let Some(thread) = renaming.thread.take()
            && thread.join().is_err()
        {
            log::error!("a rename's thread panicked");
        }
    }
}

/// Throws away the new files `staging` wrote, if any.
fn discard(staging: Staging) {
    for file in staging.into_iter().flatten() {
        file.staged.discard();
    }
}

impl Daemon {
    /// Makes the edits server `server` asks for in `edit`, a
    /// `WorkspaceEdit`, such as its answer to a rename, for `purpose`.
    /// They're counted in the buffers open in it as they were `before`,
    /// and in the files not open as they are on disk. Whoever asked is
    /// told how it went, once it has: at once, or once files not open have
    /// been written.
    pub(super) fn edit_workspace(
        &mut self,
        server: ServerId,
        before: &[Before],
        edit: &Value,
        purpose: Purpose,
    ) {
        if let Some((purpose, made)) = self.start_edit(server, before, edit, purpose) {
            self.edited_workspace(server, purpose, made);
        }
    }

    /// Starts [`edit_workspace`](Self::edit_workspace)'s work: returns how
    /// it went, with what it was for, or `None` if files not open are
    /// being written first, as [`take_renames`](Self::take_renames) says.
    fn start_edit(
        &mut self,
        server: ServerId,
        before: &[Before],
        edit: &Value,
        purpose: Purpose,
    ) -> Option<(Purpose, Result<Made, String>)> {
        let Planned {
            buffers,
            disk,
            occurrences,
            encoding,
        } = match self.plan_edit(server, before, edit) {
            Ok(Some(planned)) => planned,
            Ok(None) => return Some((purpose, Ok(Made::default()))),
            Err(why) => return Some((purpose, Err(why))),
        };
        let mut renaming = Renaming {
            server,
            purpose,
            files: buffers.len() + disk.len(),
            buffers,
            occurrences,
            paths: disk.iter().map(|file| file.path.clone()).collect(),
            cancel: Arc::new(AtomicBool::new(false)),
            thread: None,
        };
        if disk.is_empty() {
            let made = self.edit_buffers(&renaming);
            return Some((renaming.purpose, made));
        }
        let key = self.renames.next;
        self.renames.next += 1;
        let ended = Arc::clone(&self.renames.ended);
        let waker = Arc::clone(&self.waker);
        let cancel = Arc::clone(&renaming.cancel);
        let gate = self.config.rename_gate.clone();
        let spawned = std::thread::Builder::new()
            .name("rename".into())
            .spawn(move || {
                let mut staging = stage_files(&disk, encoding, &cancel);
                if let Some(gate) = gate {
                    while !gate.exists() && !cancel.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                // Dropped while it wrote, as an upgrade, which may not
                // wait for it, does: what it wrote goes now.
                if cancel.load(Ordering::Relaxed) {
                    discard(staging);
                    staging = Err("the edit was dropped".to_owned());
                }
                (ended.lock().unwrap_or_else(PoisonError::into_inner)).push((key, staging));
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for a rename failed: {err}");
                }
            });
        match spawned {
            Ok(thread) => renaming.thread = Some(thread),
            Err(err) => {
                let why = format!("the edit couldn't start: {err}");
                return Some((renaming.purpose, Err(why)));
            }
        }
        (self.renames.writing).extend(renaming.paths.iter().cloned());
        self.renames.under_way.insert(key, renaming);
        None
    }

    /// Checks the edits server `server` asks for in `edit`, as
    /// [`edit_workspace`](Self::edit_workspace) says, and plans them:
    /// those to buffers, checked against their text, and those to files not
    /// open. `None` if it asks for none; an error, saying why, if any can't
    /// be made, when none are.
    fn plan_edit(
        &self,
        server: ServerId,
        before: &[Before],
        edit: &Value,
    ) -> Result<Option<Planned>, String> {
        if edit.is_null() {
            return Ok(None);
        }
        let (Some(encoding), Some(root)) = (self.server_encoding(server), self.server_root(server))
        else {
            return Err("its language server has stopped".into());
        };
        let files = lsp::workspace_edit(edit)?;
        if files.is_empty() {
            return Ok(None);
        }
        // How many are files not open, before reading any: a buffer's path
        // is as the server names it, unless it names it through a link.
        let open = |path: &Path| (self.buffers.values()).any(|slot| slot.buffer.path() == path);
        let on_disk = files.iter().filter(|file| !open(&file.path)).count();
        if on_disk > MAX_ON_DISK {
            return Err(format!(
                "the edit touches {on_disk} files not open, more than the {MAX_ON_DISK} it may"
            ));
        }
        let files = by_real_path(files, &open)?;
        let occurrences = files.iter().map(|file| file.edits.len()).sum();
        let mut buffers = Vec::new();
        let mut disk = Vec::new();
        for file in files {
            if !file.path.starts_with(&root) {
                return Err(format!(
                    "the language server wants to edit {}, outside its project, {}",
                    file.path.display(),
                    root.display()
                ));
            }
            match self.buffer_of(&file.path) {
                Some(id) => buffers.push(self.plan(id, &file, (server, before), encoding)?),
                None => disk.push(file),
            }
        }
        if let Some(path) =
            (disk.iter().map(|file| &file.path)).find(|path| self.renames.writing.contains(*path))
        {
            return Err(format!(
                "another edit is writing {}: try again once it's done",
                path.display()
            ));
        }
        Ok(Some(Planned {
            buffers,
            disk,
            occurrences,
            encoding,
        }))
    }

    /// Finishes the renames whose files have been written: puts the files
    /// in place and edits their buffers, if nothing has changed since,
    /// and tells whoever asked.
    pub(super) fn take_renames(&mut self) {
        if self.renames.under_way.is_empty() {
            return;
        }
        let ended = std::mem::take(
            &mut *(self.renames.ended.lock()).unwrap_or_else(PoisonError::into_inner),
        );
        for (key, staging) in ended {
            let Some(mut renaming) = self.renames.under_way.remove(&key) else {
                discard(staging);
                continue;
            };
            self.renames.ended_one(&mut renaming);
            if renaming.cancel.load(Ordering::Relaxed) {
                // Its client has gone.
                discard(staging);
                continue;
            }
            let made = match staging {
                Err(why) => Err(why),
                Ok(files) => self.commit_rename(&renaming, files),
            };
            log::info!("edit across files: {made:?}");
            self.edited_workspace(renaming.server, renaming.purpose, made);
        }
    }

    /// Tells whoever asked for an edit across files, as `purpose` says,
    /// how it went, `made`: the client, or server `server`. A code action's
    /// command is run now, its edit made, and its client told once it's
    /// run.
    fn edited_workspace(&mut self, server: ServerId, purpose: Purpose, made: Result<Made, String>) {
        match purpose {
            Purpose::Rename { token, buffer, id } => {
                let answer = match made {
                    Ok(made) if made.files == 0 => Answer::Nothing,
                    Ok(Made { occurrences, files }) => Answer::Renamed { occurrences, files },
                    Err(why) => Answer::Failed(why),
                };
                self.answer_client((token, buffer, id), answer);
            }
            Purpose::Action {
                token,
                buffer,
                id,
                title,
                command,
            } => match (made, command) {
                (Ok(made), Some(command)) => {
                    self.run_command(server, (token, buffer, id), title, &command, made.files);
                }
                (Ok(made), None) => {
                    let answer = Answer::Acted {
                        title,
                        files: made.files,
                    };
                    self.answer_client((token, buffer, id), answer);
                }
                (Err(why), _) => self.answer_client((token, buffer, id), Answer::Failed(why)),
            },
            Purpose::Asked { request } => self.answer_apply_edit(server, &request, made),
        }
    }

    /// Puts a rename's files, written beside them, in their places, and
    /// edits its buffers, all at once, unless a file has changed on disk
    /// or been opened since it was read, or a buffer has been edited
    /// inside a place: then nothing.
    fn commit_rename(
        &mut self,
        renaming: &Renaming,
        files: Vec<StagedFile>,
    ) -> Result<Made, String> {
        let refused = (files.iter())
            .find_map(|file| {
                let path = file.staged.path();
                if self.buffer_of(path).is_some() {
                    Some(format!(
                        "{} was opened while the edit was being written: try again",
                        path.display()
                    ))
                } else if DiskStamp::now(path).ok().flatten() != Some(file.read) {
                    Some(format!(
                        "{} changed on disk while the edit was being written: try again",
                        path.display()
                    ))
                } else {
                    None
                }
            })
            .or_else(|| (renaming.buffers.iter()).find_map(|plan| self.moved(plan).err()));
        if let Some(why) = refused {
            discard(Ok(files));
            return Err(why);
        }
        let mut placed: Vec<Placed> = Vec::with_capacity(files.len());
        let mut files = files.into_iter();
        for file in files.by_ref() {
            let path = file.staged.path().display().to_string();
            match file.staged.place() {
                Ok(one) => placed.push(one),
                Err(err) => {
                    files.for_each(|file| file.staged.discard());
                    let not_put_back: Vec<String> = (placed.into_iter().rev())
                        .filter_map(|one| {
                            let path = one.path().display().to_string();
                            one.undo().err().map(|err| format!("{path} ({err})"))
                        })
                        .collect();
                    if not_put_back.is_empty() {
                        return Err(format!(
                            "{path} couldn't be put in place, so nothing was changed: {err}"
                        ));
                    }
                    return Err(format!(
                        "{path} couldn't be put in place: {err}; and these, changed, couldn't be put back: {}",
                        not_put_back.join(", ")
                    ));
                }
            }
        }
        let paths: Vec<PathBuf> = placed.iter().map(|one| one.path().to_owned()).collect();
        placed.into_iter().for_each(Placed::finish);
        // Checked just now, with nothing made since.
        let made = self.edit_buffers(renaming);
        self.changed_on_disk(renaming.server, &paths);
        made
    }

    /// Drops the renames under way that the client at `token` asked about
    /// `buffer`, or about any buffer, as when it goes: their threads stop,
    /// and what they wrote is thrown away.
    pub(super) fn forget_renames(&mut self, token: Token, buffer: Option<BufferId>) {
        for renaming in self.renames.under_way.values() {
            let forgotten = (renaming.purpose.client()).is_some_and(|(asker, about)| {
                asker == token && buffer.is_none_or(|buffer| buffer == about)
            });
            if forgotten {
                renaming.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Drops the edits under way that server `server` asked for itself, as
    /// it's stopping, and so won't hear how they went: their threads stop,
    /// and what they wrote is thrown away.
    pub(super) fn forget_asked_edits(&mut self, server: ServerId) {
        for renaming in self.renames.under_way.values() {
            if renaming.server == server && matches!(renaming.purpose, Purpose::Asked { .. }) {
                renaming.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Drops every rename under way, as before an upgrade or shutting
    /// down, which would leave new files beside their old ones: their
    /// threads throw away what they wrote. They're waited for a moment,
    /// but no longer, as a file system that hangs mustn't hold up the
    /// upgrade, and every terminal with it.
    pub(super) fn abandon_renames(&mut self) {
        let renames = &mut self.renames;
        for renaming in renames.under_way.values() {
            renaming.cancel.store(true, Ordering::Relaxed);
        }
        let deadline = Instant::now() + ABANDON_WAIT;
        let under_way: Vec<Renaming> = std::mem::take(&mut renames.under_way)
            .into_values()
            .collect();
        while Instant::now() < deadline
            && (under_way.iter()).any(|renaming| {
                (renaming.thread.as_ref()).is_some_and(|thread| !thread.is_finished())
            })
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        for mut renaming in under_way {
            if (renaming.thread.as_ref()).is_some_and(|thread| !thread.is_finished()) {
                log::warn!("a rename's thread is still writing, and is left to finish");
                renaming.thread = None;
            }
            renames.ended_one(&mut renaming);
        }
        let ended =
            std::mem::take(&mut *(renames.ended.lock()).unwrap_or_else(PoisonError::into_inner));
        for (_, staging) in ended {
            discard(staging);
        }
    }

    /// Makes a rename's edits to its buffers, all or none: each place moved
    /// along with the edits made since it was planned, and none edited
    /// inside meanwhile.
    fn edit_buffers(&mut self, renaming: &Renaming) -> Result<Made, String> {
        let mut made = Vec::with_capacity(renaming.buffers.len());
        for plan in &renaming.buffers {
            made.push((plan.id, self.moved(plan)?));
        }
        for (id, edit) in made {
            let Some(slot) = self.buffers.get_mut(&id) else {
                continue;
            };
            let base = slot.buffer.version();
            match slot.buffer.edit(edit, base, true, Author::User) {
                Ok(made) => self.made_for_user(id, made),
                // Checked, so a mistake of the daemon's.
                Err(err) => log::error!("buffer {id}: a rename's edit couldn't be made: {err}"),
            }
        }
        log::info!(
            "edited {} places in {} files",
            renaming.occurrences,
            renaming.files
        );
        Ok(Made {
            occurrences: renaming.occurrences,
            files: renaming.files,
        })
    }

    /// Makes the edits server `server` answered with, `value`, when asked
    /// to format buffer `id` at version `version`, when its text was
    /// `text`, as one edit by the user. They're counted in that text, and
    /// moved along with edits made since, as a rename's are; if text they
    /// replace has changed meanwhile, none are made.
    pub(super) fn formatted(
        &mut self,
        server: ServerId,
        (id, version, text): (BufferId, u64, &Rope),
        value: &Value,
    ) -> Answer {
        let Some(encoding) = self.server_encoding(server) else {
            return Answer::Failed("its language server has stopped".into());
        };
        let edits = match lsp::text_edits(value) {
            Ok(edits) => edits,
            Err(why) => return Answer::Failed(why),
        };
        let replaces: Vec<Replace> = (replaces(text, &edits, encoding).into_iter())
            .map(|replace| trimmed(&replace))
            .filter(|replace| replace.removed != replace.inserted)
            .collect();
        if replaces.is_empty() {
            return Answer::Formatted { changed: false };
        }
        let plan = BufferPlan {
            id,
            base: version,
            replaces,
        };
        let edit = match self.moved(&plan) {
            Ok(edit) => edit,
            Err(why) => return Answer::Failed(why),
        };
        let Some(slot) = self.buffers.get_mut(&id) else {
            return Answer::Nothing;
        };
        let base = slot.buffer.version();
        match slot.buffer.edit(edit, base, true, Author::User) {
            Ok(made) => self.made_for_user(id, made),
            Err(err) => return Answer::Failed(format!("formatting couldn't be made: {err}")),
        }
        Answer::Formatted { changed: true }
    }

    /// The buffer open for the file at `path`, if there's one.
    pub(super) fn buffer_of(&self, path: &Path) -> Option<BufferId> {
        (self.buffers.iter())
            .find(|(_, slot)| slot.buffer.path() == path)
            .map(|(&id, _)| id)
    }

    /// Checks the edits a rename makes to `file`, open as buffer `id`, and
    /// plans them. They're counted in the text server `server` had, as
    /// `before` says, in `encoding`'s columns.
    fn plan(
        &self,
        id: BufferId,
        file: &FileEdits,
        (server, before): (ServerId, &[Before]),
        encoding: Encoding,
    ) -> Result<BufferPlan, String> {
        let slot = &self.buffers[&id];
        let changed = || changed(slot.buffer.path());
        let asked = before.iter().find(|before| before.buffer == id);
        // The text the edits are counted in: the version the server says,
        // which is one the daemon sent it, or else what it had when asked.
        // A buffer that wasn't open in it then is what's on disk to it, if
        // it has no changes not saved.
        let (base, text) = match (file.version, asked) {
            (Some(version), Some(asked)) if asked.version == version => (version, &asked.text),
            (Some(version), _) => {
                let sent = (slot.server.as_ref())
                    .filter(|opened| opened.server() == server)
                    .and_then(|opened| opened.sent_at(version));
                match sent {
                    Some(text) => (version, text),
                    None if slot.buffer.version() == version => (version, slot.buffer.text()),
                    None => return Err(changed()),
                }
            }
            (None, Some(asked)) => (asked.version, &asked.text),
            (None, None) if !slot.buffer.modified() => (slot.buffer.version(), slot.buffer.text()),
            (None, None) => {
                return Err(format!(
                    "{} has changes not saved that its language server doesn't know of",
                    slot.buffer.path().display()
                ));
            }
        };
        let plan = BufferPlan {
            id,
            base,
            replaces: replaces(text, &file.edits, encoding),
        };
        // Checked now, so as not to write files for a rename that can't be
        // made, and again when it's made.
        self.moved(&plan)?;
        Ok(plan)
    }

    /// `plan`'s edit, made to its buffer's text now: each place moves along
    /// with the edits made since, as a mark does, so text typed just
    /// before or just after what's replaced stays outside it. Rebasing the
    /// whole edit, as a window's late edit is, would put text typed just
    /// before a name after the new one. An error if the text inside a
    /// place has changed, as then the server renamed something that isn't
    /// there now.
    fn moved(&self, plan: &BufferPlan) -> Result<Edit, String> {
        let Some(slot) = self.buffers.get(&plan.id) else {
            return Err("a file being edited has closed".into());
        };
        let changed = || changed(slot.buffer.path());
        let mut places: Vec<(usize, usize)> = (plan.replaces.iter())
            .map(|replace| (replace.start, replace.end))
            .collect();
        for edit in slot.buffer.edits_since(plan.base).ok_or_else(changed)? {
            for (start, end) in &mut places {
                let empty = start == end;
                *start = edit.map(*start, Bias::After);
                *end = if empty {
                    *start
                } else {
                    edit.map(*end, Bias::Before).max(*start)
                };
            }
        }
        let now = slot.buffer.text();
        let moved: Vec<Replace> = (plan.replaces.iter().zip(places))
            .map(|(replace, (start, end))| Replace {
                start,
                end,
                ..replace.clone()
            })
            .collect();
        let edited_inside = |replace: &Replace| {
            now.get_byte_slice(replace.start..replace.end)
                .is_none_or(|there| there != replace.removed.as_str())
        };
        if moved.iter().any(edited_inside) {
            return Err(changed());
        }
        edit_of(now, &moved)
    }
}

/// Why a rename in the file at `path` wasn't made.
fn changed(path: &Path) -> String {
    format!(
        "{} changed while the language server worked: try again",
        path.display()
    )
}

/// `files`, each under its real path, with links followed, as buffers'
/// paths are, and those that are one file under two names put together.
/// Files `open` says are open are named as their buffers are already.
fn by_real_path(
    files: Vec<FileEdits>,
    open: &impl Fn(&Path) -> bool,
) -> Result<Vec<FileEdits>, String> {
    let mut real: Vec<FileEdits> = Vec::with_capacity(files.len());
    for mut file in files {
        if !open(&file.path) {
            file.path = std::fs::canonicalize(&file.path)
                .map_err(|err| format!("{} couldn't be found: {err}", file.path.display()))?;
        }
        match real.iter_mut().find(|other| other.path == file.path) {
            Some(other) if other.version == file.version => {
                lsp::merge(&mut other.edits, file.edits);
            }
            Some(_) => return Err("the language server's edits make no sense".into()),
            None => real.push(file),
        }
    }
    Ok(real)
}

/// Reads `files`, which aren't open, makes a rename's edits to them, with
/// `encoding`'s columns, and writes each one's new text, synced, beside
/// it. All of them, or, saying why, none, as when the disk is full or a
/// directory can't be written to; or none if `cancel` is set meanwhile.
fn stage_files(files: &[FileEdits], encoding: Encoding, cancel: &AtomicBool) -> Staging {
    let mut staged = Vec::with_capacity(files.len());
    for file in files {
        let one = (!cancel.load(Ordering::Relaxed))
            .then_some(())
            .ok_or_else(|| "the edit was dropped".to_owned())
            .and_then(|()| stage_file(file, encoding));
        match one {
            Ok(one) => staged.push(one),
            Err(why) => {
                discard(Ok(staged));
                return Err(why);
            }
        }
    }
    Ok(staged)
}

/// Reads `file`, makes its edits, and writes its new text beside it.
fn stage_file(file: &FileEdits, encoding: Encoding) -> Result<StagedFile, String> {
    let path = &file.path;
    let (text, read) =
        buffer::read(path).map_err(|err| format!("{} couldn't be read: {err}", path.display()))?;
    let mut text = Rope::from_str(&text);
    let edit = edit_of(&text, &replaces(&text, &file.edits, encoding))?;
    edit.apply(&mut text).map_err(|err| err.to_string())?;
    let staged = buffer::stage(path, &text).map_err(|err| {
        format!(
            "{} couldn't be written, so nothing was changed: {err}",
            path.display()
        )
    })?;
    Ok(StagedFile { staged, read })
}

/// One of a rename's edits, at bytes of a text: `removed`, from `start` to
/// `end`, is replaced with `inserted`.
#[derive(Clone, Debug)]
struct Replace {
    start: usize,
    end: usize,
    removed: String,
    inserted: String,
}

/// `edits`, counted in `text` in `encoding`'s columns, as bytes of it, in
/// order. Edits at one place stay in the order given.
fn replaces(text: &Rope, edits: &[TextEdit], encoding: Encoding) -> Vec<Replace> {
    let mut replaces: Vec<Replace> = (edits.iter())
        .map(|edit| {
            let start = lsp::place(text, edit.start, encoding);
            let end = lsp::place(text, edit.end, encoding).max(start);
            Replace {
                start,
                end,
                removed: text.byte_slice(start..end).to_string(),
                inserted: edit.text.clone(),
            }
        })
        .collect();
    replaces.sort_by_key(|replace| (replace.start, replace.end));
    replaces
}

/// `replace` without what it removes and puts back alike at its start and
/// end: formatting often replaces a whole line, or file, to change a few
/// characters, and a place that isn't replaced doesn't move.
fn trimmed(replace: &Replace) -> Replace {
    let alike = |removed: &mut dyn Iterator<Item = char>,
                 inserted: &mut dyn Iterator<Item = char>| {
        removed
            .zip(inserted)
            .take_while(|(removed, inserted)| removed == inserted)
            .map(|(removed, _)| removed.len_utf8())
            .sum::<usize>()
    };
    let front = alike(&mut replace.removed.chars(), &mut replace.inserted.chars());
    let (removed, inserted) = (&replace.removed[front..], &replace.inserted[front..]);
    let back = alike(&mut removed.chars().rev(), &mut inserted.chars().rev());
    Replace {
        start: replace.start + front,
        end: replace.end - back,
        removed: removed[..removed.len() - back].to_owned(),
        inserted: inserted[..inserted.len() - back].to_owned(),
    }
}

/// `replaces`, in order, as one edit to `text`. Edits that overlap make
/// no sense.
fn edit_of(text: &Rope, replaces: &[Replace]) -> Result<Edit, String> {
    let mut made = Edit::new();
    let mut at = 0;
    for replace in replaces {
        if replace.start < at {
            return Err("the language server's edits overlap".into());
        }
        made.retain(replace.start - at)
            .insert(&replace.inserted)
            .delete(&replace.removed);
        at = replace.end;
    }
    made.retain(text.len_bytes() - at);
    Ok(made)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::lsp::Position;

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextEdit {
        let at = |(line, character)| Position { line, character };
        TextEdit {
            start: at(start),
            end: at(end),
            text: text.to_owned(),
        }
    }

    fn made(text: &Rope, edits: &[TextEdit], encoding: Encoding) -> Result<String, String> {
        let edit = edit_of(text, &replaces(text, edits, encoding))?;
        Ok(edit.apply_to(&text.to_string()).unwrap())
    }

    #[test]
    fn a_replacement_is_trimmed_to_what_changes() {
        let replace = |start, removed: &str, inserted: &str| Replace {
            start,
            end: start + removed.len(),
            removed: removed.into(),
            inserted: inserted.into(),
        };
        // A whole line replaced to put in a space keeps what's either side.
        let trimmed = trimmed(&replace(4, "fn é(){}", "fn é() {}"));
        assert_eq!(
            (
                trimmed.start,
                trimmed.end,
                trimmed.removed.as_str(),
                trimmed.inserted.as_str()
            ),
            (4 + "fn é()".len(), 4 + "fn é()".len(), "", " ")
        );
        let same = super::trimmed(&replace(0, "abc", "abc"));
        assert_eq!((same.removed.as_str(), same.inserted.as_str()), ("", ""));
    }

    #[test]
    fn edits_given_in_any_order_make_one_edit() {
        let text = Rope::from_str("let é = 1;\né + é\n");
        // Given last first, as servers often do, in UTF-16 columns.
        let edits = [
            edit((1, 4), (1, 5), "new"),
            edit((0, 4), (0, 5), "new"),
            edit((1, 0), (1, 1), "new"),
        ];
        assert_eq!(
            made(&text, &edits, Encoding::Utf16).unwrap(),
            "let new = 1;\nnew + new\n"
        );
        let overlapping = [edit((0, 0), (0, 3), "x"), edit((0, 2), (0, 4), "y")];
        assert!(made(&text, &overlapping, Encoding::Utf8).is_err());
        // Two insertions at one place go in the order given.
        let both = [edit((0, 0), (0, 0), "a"), edit((0, 0), (0, 0), "b")];
        assert!(
            made(&text, &both, Encoding::Utf8)
                .unwrap()
                .starts_with("ablet")
        );
    }

    #[test]
    fn files_that_cant_all_be_written_are_none_of_them_written() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        std::fs::create_dir(dir.join("locked")).unwrap();
        std::fs::write(dir.join("a"), "old\n").unwrap();
        std::fs::write(dir.join("locked/b"), "old\n").unwrap();
        let file = |path: PathBuf| FileEdits {
            path,
            version: None,
            edits: vec![edit((0, 0), (0, 3), "new")],
        };
        let files = [file(dir.join("a")), file(dir.join("locked/b"))];
        let listed = || {
            let mut names: Vec<_> = (std::fs::read_dir(dir).unwrap())
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before = listed();
        // Nothing can be written beside b.
        let locked = std::fs::Permissions::from_mode(0o555);
        std::fs::set_permissions(dir.join("locked"), locked).unwrap();
        let staged = stage_files(&files, Encoding::Utf8, &AtomicBool::new(false));
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let why = staged.unwrap_err();
        assert!(why.contains("nothing was changed"), "{why}");
        assert_eq!(listed(), before, "no new files left");

        // Writable, both are written beside their files, which are as
        // they were until each is put in place.
        let staged = stage_files(&files, Encoding::Utf8, &AtomicBool::new(false)).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "old\n");
        let placed: Vec<Placed> = (staged.into_iter())
            .map(|file| file.staged.place().unwrap())
            .collect();
        assert_eq!(std::fs::read_to_string(dir.join("a")).unwrap(), "new\n");
        // And can be put back.
        for one in placed {
            one.undo().unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("locked/b")).unwrap(),
            "old\n"
        );
        assert_eq!(listed(), before);

        // Dropped, nothing's written.
        let dropped = stage_files(&files, Encoding::Utf8, &AtomicBool::new(true));
        assert!(dropped.is_err());
        assert_eq!(listed(), before);
    }

    #[test]
    fn one_file_under_two_names_is_edited_once() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        std::fs::write(dir.join("a"), "x\n").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("alias")).unwrap();
        let file = |path: PathBuf, text| FileEdits {
            path,
            version: None,
            edits: vec![edit((0, 0), (0, 1), text)],
        };
        let files = vec![
            file(dir.join("a"), "y"),
            file(dir.join("alias/a"), "y"),
            file(dir.join("alias/missing"), "y"),
        ];
        assert!(
            by_real_path(files.clone(), &|_| false).is_err(),
            "no such file"
        );
        let real = by_real_path(files[..2].to_vec(), &|_| false).unwrap();
        assert_eq!(real.len(), 1);
        assert_eq!(
            (real[0].path.clone(), real[0].edits.len()),
            (dir.join("a"), 1)
        );
    }
}
