//! Buffers: files being edited. The daemon keeps their text and history,
//! so they outlive windows and upgrades, and are the same in every window.
//!
//! The file is read when the buffer opens, and written when it's saved,
//! by writing a new file beside it and renaming it over the old, so a
//! crash never leaves half a file. A save is refused if something else
//! has changed the file since it was read or last written, unless forced.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clackatoo_protocol::{Author, BufferSnapshot, Edit, Error, Op, TerminalId};
use clackatoo_text::{
    History, HistoryError, HistorySnapshot, MOST_PIECES, Merged, Reversal, Rope, StepPlace,
    StepSelection, StepState, StepVisual, Steps, merge,
};

use crate::handover;

/// A file being edited.
#[derive(Debug)]
pub(crate) struct Buffer {
    /// Its file's path, or an empty one for a buffer with no file yet (see
    /// [`blank`](Self::blank)), which no file's path, always absolute,
    /// ever equals.
    path: PathBuf,
    /// The directory paths opened beside it are from, and a blank one's
    /// name, once it's given one.
    dir: PathBuf,
    text: Rope,
    history: History<Author>,
    /// The text as last read or written. Ropes share what they have in
    /// common, so this costs only what's changed.
    saved: Rope,
    modified: bool,
    /// The file as last read or written, if it existed.
    disk: Option<DiskStamp>,
}

/// What tells one version of a file from another without reading it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiskStamp {
    seconds: i64,
    nanoseconds: i64,
    len: u64,
    inode: u64,
    device: u64,
}

impl DiskStamp {
    /// The file `metadata` is of, as it is.
    pub(crate) fn of(metadata: &fs::Metadata) -> Self {
        Self {
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
            len: metadata.size(),
            inode: metadata.ino(),
            device: metadata.dev(),
        }
    }

    /// Whether `metadata` is of the file this is of, as it is now or not:
    /// the same inode on the same device.
    pub(crate) fn is_file(&self, metadata: &fs::Metadata) -> bool {
        (self.inode, self.device) == (metadata.ino(), metadata.dev())
    }

    /// The stamp, for the next build.
    pub(crate) fn hand_over(self) -> handover::DiskStamp {
        handover::DiskStamp {
            seconds: self.seconds,
            nanoseconds: self.nanoseconds,
            len: self.len,
            inode: self.inode,
            device: self.device,
        }
    }

    /// A stamp the last build handed over.
    pub(crate) fn resume(handed: handover::DiskStamp) -> Self {
        Self {
            seconds: handed.seconds,
            nanoseconds: handed.nanoseconds,
            len: handed.len,
            inode: handed.inode,
            device: handed.device,
        }
    }

    /// How long the file is, in bytes.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// When the file was last written.
    pub(crate) fn written(&self) -> std::time::SystemTime {
        let since = std::time::Duration::new(
            u64::try_from(self.seconds).unwrap_or(0),
            u32::try_from(self.nanoseconds).unwrap_or(0),
        );
        std::time::UNIX_EPOCH + since
    }

    /// The file at `path` as it is now, if it exists.
    pub(crate) fn now(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self::of(&metadata))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// What a buffer whose file has moved knows of it on disk now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Restamp {
    /// What it knew: the file was renamed, and is the same file.
    Keep,
    /// This: the file was copied from another file system, unchanged since
    /// the buffer read it, and is a new file, or none if it's gone.
    To(Option<DiskStamp>),
}

/// Why an agent's change to a buffer wasn't made.
#[derive(Debug)]
pub(crate) enum Refused {
    /// It touches lines changed and not saved: these bytes of the text it
    /// was made to, each run of lines both touch.
    Conflict(Vec<Range<usize>>),
    /// The file couldn't be read or written, or has changed meanwhile.
    Failed(Error),
}

/// The most undo steps an agent keeps in a buffer, and the most bytes they
/// may take up (see [`Buffer::edit_keeping`]).
const AGENT_STEPS: usize = 16;
const AGENT_STEP_BYTES: usize = 1 << 20;

/// An edit made to a buffer.
#[derive(Debug)]
pub(crate) struct Made {
    /// The version it made.
    pub(crate) version: u64,
    /// The edit, made to the version before.
    pub(crate) edit: Edit,
}

/// An undo or a redo made (see [`Buffer::reverse_keeping`]).
#[derive(Debug)]
pub(crate) struct Reversed {
    /// The version it made, or the text's, if the step changes nothing.
    pub(crate) version: u64,
    /// The edit, which keeps the text if the step changes nothing.
    pub(crate) edit: Edit,
    /// What the step kept, its places in the text now.
    pub(crate) state: Option<StepState>,
    /// The step changes nothing, so no edit was made.
    pub(crate) empty: bool,
}

impl Buffer {
    /// Opens the file at `path`, which is absolute, or a buffer for a new
    /// one there if there's none. Paths opened beside it are from `dir`.
    pub(crate) fn open(path: PathBuf, dir: PathBuf) -> Result<Self, Error> {
        let failed = |reason: String| Error::OpenFailed {
            path: path.display().to_string(),
            reason,
        };
        let (text, disk) = match read(&path) {
            Ok((text, disk)) => (text, Some(disk)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (String::new(), None),
            Err(err) if err.kind() == io::ErrorKind::IsADirectory => {
                return Err(failed("it's a directory".to_owned()));
            }
            Err(err) if err.kind() == io::ErrorKind::InvalidData => {
                return Err(failed("it isn't UTF-8 text".to_owned()));
            }
            Err(err) => return Err(failed(err.to_string())),
        };
        let text = Rope::from_str(&lf(text));
        Ok(Self {
            path,
            dir,
            saved: text.clone(),
            text,
            history: History::new(0),
            modified: false,
            disk,
        })
    }

    /// An empty buffer with no file yet, as Vim's `:enew` makes, which
    /// isn't written until it's named, with paths opened beside it, and
    /// its name, from `dir`.
    pub(crate) fn blank(dir: PathBuf) -> Self {
        Self {
            path: PathBuf::new(),
            dir,
            text: Rope::new(),
            history: History::new(0),
            saved: Rope::new(),
            modified: false,
            disk: None,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Whether it has no file yet: see [`blank`](Self::blank).
    pub(crate) fn is_blank(&self) -> bool {
        self.path.as_os_str().is_empty()
    }

    /// Its file has moved to `path`, as a file explorer moving it does.
    /// Renamed, it's the same file, so what's known of it on disk still
    /// holds; copied from another file system, it's a new one, which
    /// `disk` says of, if it was the file last read or written.
    pub(crate) fn moved_to(&mut self, path: PathBuf, disk: Restamp) {
        self.path = path;
        if let Restamp::To(disk) = disk {
            self.disk = disk;
        }
    }

    /// The file as last read or written, if it existed.
    pub(crate) fn disk(&self) -> Option<DiskStamp> {
        self.disk
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn modified(&self) -> bool {
        self.modified
    }

    pub(crate) fn text(&self) -> &Rope {
        &self.text
    }

    /// The edits made since `version`, oldest first, or `None` if that's
    /// further back than the history reaches.
    pub(crate) fn edits_since(&self, version: u64) -> Option<impl Iterator<Item = &Edit>> {
        let since = self.history.since(version).ok()?;
        Some(since.map(|(edit, _)| edit))
    }

    pub(crate) fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            path: self.path.display().to_string(),
            text: self.text.to_string(),
            version: self.history.version(),
            modified: self.modified,
            applied: None,
            landed: None,
            daemon: 0,
        }
    }

    /// Makes `edit`, which `author` made to version `base`, bringing it up
    /// to date first. `step` starts a new undo step. One of more than
    /// [`MOST_PIECES`] pieces, as the daemon makes of a language server's
    /// formatting or an agent's write, is made coarsened, so every edit
    /// kept is quick to bring others past: a window's never is, as the
    /// daemon refuses those first.
    pub(crate) fn edit(
        &mut self,
        edit: Edit,
        base: u64,
        step: bool,
        author: Author,
    ) -> Result<Made, HistoryError> {
        self.edit_keeping(edit, base, (step, None), author)
    }

    /// Makes `edit` as [`edit`](Self::edit) does, and if it starts a new
    /// undo step, has the step keep `state`.
    /// The state's places are in the text at `base`, and are brought up to
    /// date with the edit; if that's too far back, the step keeps nothing.
    pub(crate) fn edit_keeping(
        &mut self,
        edit: Edit,
        base: u64,
        (step, state): (bool, Option<StepState>),
        author: Author,
    ) -> Result<Made, HistoryError> {
        let mut edit = self.history.rebase(edit, base)?;
        // Before it's coarsened, which reads the text it takes away: a
        // window's may not fit.
        edit.check(&self.text)?;
        let state = state.and_then(|state| self.brought_up(state, base));
        if edit.ops().len() > MOST_PIECES {
            edit = edit.coarsened(&self.text, MOST_PIECES);
        }
        edit.apply(&mut self.text)?;
        let version = (self.history).record_with(edit.clone(), author, step, state)?;
        // An agent's own edits are undone by turn, from the edit log, not
        // step by step: it keeps only its last few, as each chat's agent is
        // an author of its own, and there may be many.
        if matches!(author, Author::Agent(_)) {
            self.history
                .keep_last(&author, AGENT_STEPS, AGENT_STEP_BYTES);
        }
        Ok(Made { version, edit })
    }

    /// Starts an undo step for `author` that changes nothing, keeping
    /// `state`, whose places are in the text at `base`.
    pub(crate) fn empty_step(
        &mut self,
        base: u64,
        state: StepState,
        author: Author,
    ) -> Result<(), HistoryError> {
        let state = self.brought_up(state, base);
        let len = self.text.len_bytes();
        self.history.empty_step(&author, len, state)
    }

    /// Undoes `author`'s last step, or with `redo`, redoes the last undone,
    /// steps that change nothing too, giving back what the step kept:
    /// `visual`, the window's last selection in the text at `base`, is
    /// kept in place of the step's (see [`History::reverse`]). `None` if
    /// there's nothing to.
    pub(crate) fn reverse_keeping(
        &mut self,
        author: Author,
        redo: bool,
        base: u64,
        visual: Option<StepVisual>,
    ) -> Result<Option<Reversed>, HistoryError> {
        let visual = visual.and_then(|visual| {
            let state = StepState {
                visual: Some(visual),
                ..StepState::default()
            };
            self.brought_up(state, base)?.visual
        });
        let Some(Reversal { edit, state, empty }) =
            self.history.reverse(&author, !redo, visual, false)?
        else {
            return Ok(None);
        };
        edit.apply(&mut self.text)?;
        let len = self.text.len_bytes();
        Ok(Some(Reversed {
            version: self.history.version(),
            edit,
            state: state.map(|state| state.within(len)),
            empty,
        }))
    }

    /// `state`, whose places are in the text at `base`, with them in the
    /// text now, as far as it goes; `None` if `base` is too far back.
    fn brought_up(&self, mut state: StepState, base: u64) -> Option<StepState> {
        for edit in self.history.since_within(base).ok()? {
            state.map(edit);
        }
        Some(state.within(self.text.len_bytes()))
    }

    /// Undoes `author`'s last step, or with `redo`, redoes the last they
    /// undid. `None` if there's nothing to.
    pub(crate) fn reverse(
        &mut self,
        author: Author,
        redo: bool,
    ) -> Result<Option<Made>, HistoryError> {
        let edit = if redo {
            self.history.redo(&author)?
        } else {
            self.history.undo(&author)?
        };
        let Some(edit) = edit else {
            return Ok(None);
        };
        edit.apply(&mut self.text)?;
        Ok(Some(Made {
            version: self.history.version(),
            edit,
        }))
    }

    /// The edit that made `version`, if the history still has it.
    pub(crate) fn edit_made(&self, version: u64) -> Option<Edit> {
        self.history.edit(version).map(|(edit, _)| edit.clone())
    }

    /// Whether `author` has had undo steps let go, the oldest first (see
    /// [`History::forgot`]).
    pub(crate) fn forgot(&self, author: Author) -> bool {
        self.history.forgot(&author)
    }

    /// The version the edits kept would start from once trimmed, if
    /// they've grown too big (see [`History::trim_due`]).
    pub(crate) fn trim_due(&self) -> Option<u64> {
        self.history.trim_due()
    }

    /// Lets go of the edits made before `version`. What needs to read the
    /// text at an older version, such as an agent's turn to be undone,
    /// takes it first.
    pub(crate) fn trim(&mut self, version: u64) {
        if let Err(err) = self.history.trim(version) {
            log::error!(
                "{}: its history couldn't be trimmed: {err}",
                self.path.display()
            );
        }
    }

    /// Notes whether the text is as it was last read or written, after a
    /// change. Returns the answer if it's changed.
    pub(crate) fn check_modified(&mut self) -> Option<bool> {
        let modified = self.text.len_bytes() != self.saved.len_bytes() || self.text != self.saved;
        (modified != self.modified).then(|| {
            self.modified = modified;
            modified
        })
    }

    /// Writes the text to the file, unless something else has changed the
    /// file since it was read or last written and not `force`. Returns
    /// whether that changed whether the buffer is modified.
    pub(crate) fn save(&mut self, force: bool) -> Result<Option<bool>, Error> {
        self.write_out(self.text.clone(), force)?;
        Ok(self.check_modified())
    }

    /// Writes `text` to the file, which needn't be the buffer's text,
    /// unless something else has changed the file since it was read or
    /// last written and not `force`.
    fn write_out(&mut self, text: Rope, force: bool) -> Result<(), Error> {
        let staged = self.stage(&text, force)?;
        let failed = |err: io::Error| Error::SaveFailed {
            path: self.path.display().to_string(),
            reason: err.to_string(),
        };
        // Again, just before it's put in place: something may have written
        // the file while the new text was being synced. Dropped, the new
        // file is removed.
        if !force && DiskStamp::now(&self.path).map_err(failed)? != self.disk {
            return Err(Error::ChangedOnDisk(self.path.display().to_string()));
        }
        let stamp = staged.commit().map_err(failed)?;
        self.written(text, Some(stamp));
        Ok(())
    }

    /// Writes `text` beside the file, to be put in its place, unless
    /// something else has changed the file since it was read or last
    /// written and not `force`. Once it is, [`written`](Self::written)
    /// says so. A buffer with no file yet has nowhere to write.
    pub(crate) fn stage(&self, text: &Rope, force: bool) -> Result<Staged, Error> {
        if self.is_blank() {
            return Err(Error::SaveFailed {
                path: "a buffer with no name".to_owned(),
                reason: ":w NAME names it".to_owned(),
            });
        }
        let failed = |err: io::Error| Error::SaveFailed {
            path: self.path.display().to_string(),
            reason: err.to_string(),
        };
        if !force && DiskStamp::now(&self.path).map_err(failed)? != self.disk {
            return Err(Error::ChangedOnDisk(self.path.display().to_string()));
        }
        // A file whose lines end with CRLF is written so again.
        let written = if crlf(&self.path) {
            Rope::from_str(&text.to_string().replace('\n', "\r\n"))
        } else {
            text.clone()
        };
        stage(&self.path, &written).map_err(failed)
    }

    /// The file now holds `text`, and is as `disk` says, or is gone.
    pub(crate) fn written(&mut self, text: Rope, disk: Option<DiskStamp>) {
        self.disk = disk;
        self.saved = text;
    }

    /// The text at each of `versions`, rebuilt from the text now by
    /// taking out the edits since. `None` if the history doesn't reach
    /// back to one of them.
    pub(crate) fn texts_at(&self, versions: &[u64]) -> Option<Vec<Rope>> {
        let oldest = versions.iter().copied().min()?;
        let since: Vec<&Edit> = self
            .history
            .since(oldest)
            .ok()?
            .map(|(edit, _)| edit)
            .collect();
        let mut wanted: Vec<(usize, u64)> = versions.iter().copied().enumerate().collect();
        wanted.sort_by_key(|&(_, version)| std::cmp::Reverse(version));
        let mut texts = vec![Rope::new(); versions.len()];
        let mut text = self.text.clone();
        let mut version = self.version();
        for (index, at) in wanted {
            while version > at {
                let edit = since[usize::try_from(version - oldest - 1).ok()?];
                edit.invert().apply(&mut text).ok()?;
                version -= 1;
            }
            texts[index] = text.clone();
        }
        Some(texts)
    }

    /// The file's text now, with its line ends as the buffer keeps them,
    /// and what it is, if something has changed it since it was read or
    /// last written. `None` if it hasn't, or it's gone.
    pub(crate) fn changed_on_disk(&self) -> Result<Option<(String, DiskStamp)>, Error> {
        let failed = |err: io::Error| Error::OpenFailed {
            path: self.path.display().to_string(),
            reason: err.to_string(),
        };
        if DiskStamp::now(&self.path).map_err(failed)? == self.disk {
            return Ok(None);
        }
        match read(&self.path) {
            Ok((text, disk)) => Ok(Some((lf(text), disk))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(Error::OpenFailed {
                path: self.path.display().to_string(),
                reason: "it isn't UTF-8 text".to_owned(),
            }),
            Err(err) => Err(failed(err)),
        }
    }

    /// The text as last read from the file or written to it.
    pub(crate) fn saved(&self) -> &Rope {
        &self.saved
    }

    /// Takes changes an agent wrote to the file itself, as Claude Code's
    /// tools do, into the buffer. `steps` are the file's text after each,
    /// in order, from the text saved, each with whether it starts a new
    /// undo step; the last is the file's text now, `disk`, which `stamp`
    /// says what it is. Each is made by `author`, merged into the changes
    /// not saved, and the file's text is then the text saved: a buffer
    /// that was saved stays so, and one that wasn't is changed only by
    /// what it was changed by before. Returns the edit each step made.
    ///
    /// If any change touches a line changed and not saved, nothing is
    /// made, and the buffer still knows the file has changed, so it isn't
    /// written over unless forced.
    pub(crate) fn take_written(
        &mut self,
        steps: &[(String, bool)],
        (disk, stamp): (&str, DiskStamp),
        author: Author,
    ) -> Result<Vec<Option<Made>>, Refused> {
        let path = self.path.display().to_string();
        let failed = |reason: String| {
            Refused::Failed(Error::OpenFailed {
                path: path.clone(),
                reason,
            })
        };
        if steps.last().map(|(text, _)| text.as_str()) != Some(disk) {
            return Err(failed("the changes don't make the file's text".into()));
        }
        let saved = self.saved.to_string();
        let mut now = self.text.to_string();
        let whole = merge(&saved, disk, &now).map_err(|err| failed(err.to_string()))?;
        if let Merged::Conflict(lines) = whole {
            return Err(Refused::Conflict(lines));
        }
        // Every step is merged before any is made, so one that can't be
        // leaves the buffer as it was. Each is merged from the text after
        // the one before, which only makes sense because the whole change
        // was found to touch no line changed and not saved, just above.
        let mut edits = Vec::with_capacity(steps.len());
        let mut before = saved.as_str();
        for (text, _) in steps {
            match merge(before, text, &now).map_err(|err| failed(err.to_string()))? {
                Merged::Made(edit) => {
                    now = edit.apply_to(&now).map_err(|err| failed(err.to_string()))?;
                    edits.push(edit);
                }
                Merged::Conflict(lines) => return Err(Refused::Conflict(lines)),
            }
            before = text;
        }
        let mut made = Vec::with_capacity(steps.len());
        for (edit, &(_, step)) in edits.into_iter().zip(steps) {
            if edit.is_noop() {
                made.push(None);
                continue;
            }
            let version = self.version();
            let one = self.edit(edit, version, step, author);
            made.push(Some(one.map_err(|err| failed(err.to_string()))?));
        }
        self.saved = Rope::from_str(disk);
        self.disk = Some(stamp);
        Ok(made)
    }

    /// Makes an agent's write of `content` to the file, having read
    /// `base`: the change from `base` to `content`, made by `author` and
    /// merged into whatever's changed since. The file gets the change too,
    /// under any changes not saved: the agent's commands see its write,
    /// but the user's unsaved work stays unsaved. `None` if it changes
    /// nothing. `step` starts a new undo step.
    ///
    /// Refused, changing nothing, if the change touches a line changed
    /// since `base`, in the buffer or in the file, or if something else
    /// has changed the file since it was read or last written.
    pub(crate) fn write_merged(
        &mut self,
        base: &str,
        content: &str,
        author: Author,
        step: bool,
    ) -> Result<Option<Made>, Refused> {
        let path = self.path.display().to_string();
        let failed = |reason: String| {
            Refused::Failed(Error::SaveFailed {
                path: path.clone(),
                reason,
            })
        };
        let saved = self.saved.to_string();
        let merged = |now: &str| match merge(base, content, now) {
            Ok(Merged::Made(edit)) => Ok(edit),
            Ok(Merged::Conflict(lines)) => Err(Refused::Conflict(lines)),
            Err(err) => Err(failed(err.to_string())),
        };
        let (to_file, edit) = match (merged(&saved), merged(&self.text.to_string())) {
            (Ok(to_file), Ok(edit)) => (to_file, edit),
            (Err(Refused::Conflict(mut lines)), Err(Refused::Conflict(more))) => {
                lines.extend(more);
                lines.sort_by_key(|lines| lines.start);
                return Err(Refused::Conflict(lines));
            }
            (Err(refused), _) | (_, Err(refused)) => return Err(refused),
        };
        if !to_file.is_noop() || self.disk.is_none() {
            let written = to_file
                .apply_to(&saved)
                .map_err(|err| failed(err.to_string()))?;
            self.write_out(Rope::from_str(&written), false)
                .map_err(Refused::Failed)?;
        }
        if edit.is_noop() {
            return Ok(None);
        }
        let version = self.version();
        self.edit(edit, version, step, author)
            .map(Some)
            .map_err(|err| failed(err.to_string()))
    }

    /// The edit that throws away the changes that aren't saved, reading the
    /// file again. Making it is up to the caller, so it's an edit like any
    /// other, which can be undone. A buffer with no file yet is emptied.
    pub(crate) fn discard(&mut self) -> Result<Edit, Error> {
        let read = if self.is_blank() {
            Err(io::ErrorKind::NotFound.into())
        } else {
            read(&self.path)
        };
        let (text, disk) = match read {
            Ok((text, disk)) => (text, Some(disk)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (String::new(), None),
            Err(err) => {
                return Err(Error::OpenFailed {
                    path: self.path.display().to_string(),
                    reason: err.to_string(),
                });
            }
        };
        let now = self.text.to_string();
        let text = lf(text);
        self.disk = disk;
        self.saved = Rope::from_str(&text);
        Ok(Edit::replace(now.len(), 0, &now, &text))
    }

    pub(crate) fn version(&self) -> u64 {
        self.history.version()
    }

    /// The buffer, for the next build.
    pub(crate) fn hand_over(&self, id: u32) -> handover::Buffer {
        let snapshot = self.history.snapshot();
        handover::Buffer {
            id,
            path: self.path.as_os_str().as_bytes().to_vec(),
            dir: self.dir.as_os_str().as_bytes().to_vec(),
            text: self.text.to_string(),
            saved: self.modified.then(|| self.saved.to_string()),
            disk: self.disk.map(DiskStamp::hand_over),
            history: handover::BufferHistory {
                first: snapshot.first,
                edits: (snapshot.edits.into_iter())
                    .map(|(edit, author)| (edit_ops(&edit), hand_over_author(author)))
                    .collect(),
                steps: (snapshot.steps.into_iter())
                    .map(|steps| handover::AuthorSteps {
                        author: hand_over_author(steps.author),
                        version: steps.version,
                        undo: steps.undo.iter().map(edit_ops).collect(),
                        redo: steps.redo.iter().map(edit_ops).collect(),
                    })
                    .collect(),
            },
        }
    }

    /// What the buffer's undo steps keep, for the next build, if they
    /// keep anything, or older steps were let go.
    pub(crate) fn hand_over_undo(&self, id: u32) -> Option<handover::BufferUndo> {
        let snapshot = self.history.snapshot();
        let kept = (snapshot.steps.iter()).any(|steps| {
            steps.forgot
                || (steps.undo_states.iter().chain(&steps.redo_states)).any(Option::is_some)
                || (steps.undo_empty.iter().chain(&steps.redo_empty)).any(|&empty| empty)
        });
        kept.then(|| handover::BufferUndo {
            buffer: id,
            authors: (snapshot.steps.into_iter())
                .map(|steps| handover::AuthorUndo {
                    undo: (steps.undo_states.iter())
                        .map(|state| hand_over_state(state.as_ref()))
                        .collect(),
                    redo: (steps.redo_states.iter())
                        .map(|state| hand_over_state(state.as_ref()))
                        .collect(),
                    undo_empty: steps.undo_empty,
                    redo_empty: steps.redo_empty,
                    forgot: steps.forgot,
                })
                .collect(),
        })
    }

    /// A buffer the last build handed over, with what its undo steps keep,
    /// if they keep anything.
    pub(crate) fn resume(handed: handover::Buffer, undo: Option<handover::BufferUndo>) -> Self {
        let text = Rope::from_str(&handed.text);
        let saved = handed
            .saved
            .as_deref()
            .map_or_else(|| text.clone(), Rope::from_str);
        let history = handed.history;
        let mut buffer = Self {
            path: PathBuf::from(OsString::from_vec(handed.path)),
            dir: PathBuf::from(OsString::from_vec(handed.dir)),
            text,
            history: History::restore(HistorySnapshot {
                first: history.first,
                edits: (history.edits.into_iter())
                    .map(|(ops, author)| (resume_edit(ops), resume_author(author)))
                    .collect(),
                steps: (history.steps.into_iter())
                    .zip(
                        undo.map(|undo| undo.authors)
                            .unwrap_or_default()
                            .into_iter()
                            .map(Some)
                            .chain(std::iter::repeat_with(|| None)),
                    )
                    .map(|(steps, kept)| {
                        let kept = kept.unwrap_or_default();
                        let resume = |states: Vec<Option<handover::UndoState>>| {
                            states
                                .into_iter()
                                .map(|state| state.map(resume_state))
                                .collect()
                        };
                        let (undo_states, redo_states, forgot) =
                            (resume(kept.undo), resume(kept.redo), kept.forgot);
                        let (undo_empty, redo_empty) = (kept.undo_empty, kept.redo_empty);
                        Steps {
                            author: resume_author(steps.author),
                            version: steps.version,
                            undo: steps.undo.into_iter().map(resume_edit).collect(),
                            redo: steps.redo.into_iter().map(resume_edit).collect(),
                            undo_states,
                            redo_states,
                            undo_empty,
                            redo_empty,
                            forgot,
                        }
                    })
                    .collect(),
            }),
            saved,
            modified: false,
            disk: handed.disk.map(DiskStamp::resume),
        };
        buffer.check_modified();
        buffer
    }
}

/// `text` with its lines ending `\n`, as a buffer keeps them, though the
/// file ends them `\r\n`, as Windows does: every edit, place and
/// language server then counts one way, and saving puts the `\r` back.
pub(crate) fn lf(text: String) -> String {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n")
    } else {
        text
    }
}

/// Whether the file at `path` ends its lines with CRLF, as its first line
/// says. One that doesn't exist, or has one line, doesn't.
fn crlf(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut start = Vec::new();
    if file.take(64 * 1024).read_to_end(&mut start).is_err() {
        return false;
    }
    start
        .iter()
        .position(|&byte| byte == b'\n')
        .is_some_and(|at| at > 0 && start[at - 1] == b'\r')
}

/// The biggest file read, in bytes: far more than any file edited by
/// hand, but short of a device such as `/dev/zero` filling the memory.
const MAX_FILE: u64 = 256 * 1024 * 1024;

/// Reads the file at `path` as text, and what it was as it was read. Only
/// a regular file is read: a pipe would hang the daemon as it opened, and
/// a device could go on for ever.
pub(crate) fn read(path: &Path) -> io::Result<(String, DiskStamp)> {
    let regular = |metadata: &fs::Metadata| {
        if metadata.is_dir() {
            Err(io::Error::from(io::ErrorKind::IsADirectory))
        } else if !metadata.is_file() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "it isn't a regular file",
            ))
        } else if metadata.len() > MAX_FILE {
            Err(io::Error::from(io::ErrorKind::FileTooLarge))
        } else {
            Ok(())
        }
    };
    regular(&fs::metadata(path)?)?;
    let mut file = File::open(path)?;
    // Again, as what's there may have changed since.
    let metadata = file.metadata()?;
    regular(&metadata)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    (&mut file).take(MAX_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    let text = String::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok((text, DiskStamp::of(&metadata)))
}

/// A file's new text, written and synced beside it, but not yet in its
/// place: what can go wrong in writing it, such as the disk being full,
/// has gone right. Dropped without being put in place, the new file is
/// removed, so a thread that panics leaves none behind.
#[derive(Debug)]
pub(crate) struct Staged {
    temporary: PathBuf,
    path: PathBuf,
    /// The new file as written, which renaming it doesn't change.
    stamp: DiskStamp,
}

/// A file's new text put in its place, with the old kept under another
/// name until it's known whether to put it back.
#[derive(Debug)]
pub(crate) struct Placed {
    path: PathBuf,
    kept: PathBuf,
}

/// Writes `text` to a new file beside `path`, with the same permissions as
/// the file there, to be put in its place with [`Staged::commit`].
pub(crate) fn stage(path: &Path, text: &Rope) -> io::Result<Staged> {
    // Each new file's name is its own, though several are written beside
    // one file at once.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "that's no file's path",
        ));
    };
    let permissions = match fs::metadata(path) {
        Ok(metadata) => Some(metadata.permissions()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    let mut temporary = name.to_owned();
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    temporary.push(format!(".clackatoo-{}-{number}", std::process::id()));
    let temporary = dir.join(temporary);
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o666)
            .open(&temporary)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        for chunk in text.chunks() {
            file.write_all(chunk.as_bytes())?;
        }
        file.sync_all()?;
        Ok(DiskStamp::of(&file.metadata()?))
    })();
    match written {
        Ok(stamp) => Ok(Staged {
            temporary,
            path: path.to_owned(),
            stamp,
        }),
        Err(err) => {
            let _ = fs::remove_file(&temporary);
            Err(err)
        }
    }
}

impl Staged {
    /// The file whose new text it is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Puts the new file in the old one's place. Returns the file as it is
    /// then: as it was written, as renaming changes nothing of it.
    pub(crate) fn commit(mut self) -> io::Result<DiskStamp> {
        fs::rename(&self.temporary, &self.path)?;
        self.temporary.clear();
        Ok(self.stamp)
    }

    /// Puts the new file in the old one's place, keeping the old one, so
    /// it can be put back with [`Placed::undo`]: by a second link to it,
    /// or, where links can't be made, by moving it aside. Each step is a
    /// rename or a link, and so quick, with nothing to sync.
    pub(crate) fn place(mut self) -> io::Result<Placed> {
        let mut kept = self.temporary.clone().into_os_string();
        kept.push("-was");
        let kept = PathBuf::from(kept);
        match fs::hard_link(&self.path, &kept) {
            Ok(()) => {
                if let Err(err) = fs::rename(&self.temporary, &self.path) {
                    let _ = fs::remove_file(&kept);
                    return Err(err);
                }
            }
            Err(err) if links_refused(&err) => {
                fs::rename(&self.path, &kept)?;
                if let Err(err) = fs::rename(&self.temporary, &self.path) {
                    // Back as it was, as nearly as can be.
                    let _ = fs::rename(&kept, &self.path);
                    return Err(err);
                }
            }
            Err(err) => return Err(err),
        }
        self.temporary.clear();
        Ok(Placed {
            path: std::mem::take(&mut self.path),
            kept,
        })
    }

    /// Throws the new file away, leaving the old one as it was.
    pub(crate) fn discard(self) {
        drop(self);
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.temporary.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

/// Whether making a link failed because links can't be made there, as on
/// vfat and many FUSE mounts, or to a file someone else owns, under
/// `fs.protected_hardlinks`, or because the file has as many as it may.
fn links_refused(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported | io::ErrorKind::TooManyLinks
    )
}

impl Placed {
    /// The file whose new text it is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Puts the old file back in its place.
    pub(crate) fn undo(self) -> io::Result<()> {
        fs::rename(&self.kept, &self.path)
    }

    /// Lets the old file go, keeping the new.
    pub(crate) fn finish(self) {
        if let Err(err) = fs::remove_file(&self.kept) {
            log::warn!("{}: {err}", self.kept.display());
        }
    }
}

pub(crate) fn edit_ops(edit: &Edit) -> Vec<handover::EditOp> {
    (edit.ops().iter())
        .map(|op| match op {
            Op::Retain(len) => handover::EditOp::Retain(*len as u64),
            Op::Insert(text) => handover::EditOp::Insert(text.clone()),
            Op::Delete(text) => handover::EditOp::Delete(text.clone()),
        })
        .collect()
}

pub(crate) fn resume_edit(ops: Vec<handover::EditOp>) -> Edit {
    (ops.into_iter())
        .map(|op| match op {
            handover::EditOp::Retain(len) => Op::Retain(usize::try_from(len).unwrap_or(usize::MAX)),
            handover::EditOp::Insert(text) => Op::Insert(text),
            handover::EditOp::Delete(text) => Op::Delete(text),
        })
        .collect::<Vec<_>>()
        .try_into()
        // A daemon only ever wrote edits it could make. One too long for
        // any text stays out of the way as an edit of nothing: anything
        // brought up to date through it fails, and its window starts again.
        .unwrap_or_default()
}

/// What an undo step keeps, for the next build.
fn hand_over_state(state: Option<&StepState>) -> Option<handover::UndoState> {
    let place = |place: StepPlace| (place.line, place.col);
    state.map(|state| handover::UndoState {
        cursor: place(state.cursor),
        taken: state.taken,
        visual: state.visual.map(|visual| handover::UndoVisual {
            start: place(visual.start),
            end: place(visual.end),
            kind: match visual.kind {
                StepSelection::Characters => 0,
                StepSelection::Lines => 1,
                StepSelection::Block => 2,
            },
            backward: visual.backward,
            to_end: visual.to_end,
        }),
    })
}

/// What an undo step keeps, as the last build handed it over.
fn resume_state(state: handover::UndoState) -> StepState {
    let place = |(line, col)| StepPlace { line, col };
    StepState {
        cursor: place(state.cursor),
        taken: state.taken,
        visual: state.visual.map(|visual| StepVisual {
            start: place(visual.start),
            end: place(visual.end),
            kind: match visual.kind {
                0 => StepSelection::Characters,
                1 => StepSelection::Lines,
                _ => StepSelection::Block,
            },
            backward: visual.backward,
            to_end: visual.to_end,
        }),
    }
}

fn hand_over_author(author: Author) -> handover::Author {
    match author {
        Author::User => handover::Author::User,
        Author::Agent(chat) => handover::Author::Agent(chat.0),
    }
}

fn resume_author(author: handover::Author) -> Author {
    match author {
        handover::Author::User => Author::User,
        handover::Author::Agent(chat) => Author::Agent(TerminalId(chat)),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_new_text_put_in_place_can_be_put_back_or_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, "old").unwrap();
        let listed = || std::fs::read_dir(dir.path()).unwrap().count();
        // Put in place and back.
        let placed = super::stage(&path, &Rope::from_str("new"))
            .unwrap()
            .place()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        placed.undo().unwrap();
        assert_eq!(
            (std::fs::read_to_string(&path).unwrap().as_str(), listed()),
            ("old", 1)
        );
        // Dropped without being put in place, as by a thread that panics,
        // nothing's left of it.
        drop(super::stage(&path, &Rope::from_str("new")).unwrap());
        assert_eq!(listed(), 1);
        // Where links can't be made, the old is moved aside instead.
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::Unsupported,
            io::ErrorKind::TooManyLinks,
        ] {
            assert!(super::links_refused(&io::Error::from(kind)));
        }
        assert!(!super::links_refused(&io::Error::from(
            io::ErrorKind::StorageFull
        )));
    }

    #[test]
    fn only_a_regular_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        // Not hanging, as opening a pipe with no writer would.
        let err = super::read(&fifo).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            super::read(dir.path()).unwrap_err().kind(),
            io::ErrorKind::IsADirectory
        );
    }

    use super::*;

    fn typed(buffer: &mut Buffer, at: usize, text: &str, step: bool) {
        let len = buffer.text.len_bytes();
        let base = buffer.version();
        buffer
            .edit(Edit::replace(len, at, "", text), base, step, Author::User)
            .unwrap();
    }

    #[test]
    fn a_buffer_saves_over_its_file_keeping_its_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        fs::write(&path, "one\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let mut buffer = Buffer::open(path.clone(), dir.path().to_owned()).unwrap();
        typed(&mut buffer, 4, "two\n", true);
        assert_eq!(buffer.check_modified(), Some(true));
        assert_eq!(buffer.save(false).unwrap(), Some(false));
        assert_eq!(fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");
    }

    #[test]
    fn a_file_changed_since_it_was_read_isnt_written_over_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "mine").unwrap();
        let mut buffer = Buffer::open(path.clone(), dir.path().to_owned()).unwrap();
        typed(&mut buffer, 4, "!", true);
        fs::write(&path, "someone else's").unwrap();
        assert!(matches!(buffer.save(false), Err(Error::ChangedOnDisk(_))));
        assert_eq!(fs::read_to_string(&path).unwrap(), "someone else's");
        buffer.save(true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine!");
    }

    #[test]
    fn an_agents_write_wont_write_over_changes_made_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "one\n").unwrap();
        let mut buffer = Buffer::open(path.clone(), dir.path().to_owned()).unwrap();
        fs::write(&path, "someone else's\n").unwrap();
        let agent = Author::Agent(TerminalId(1));
        assert!(matches!(
            buffer.write_merged("one\n", "two\n", agent, true),
            Err(Refused::Failed(Error::ChangedOnDisk(_)))
        ));
        assert_eq!(buffer.snapshot().text, "one\n");
        assert_eq!(buffer.version(), 0);
        // Taken from disk instead, as though the agent had written it.
        let (disk, stamp) = buffer.changed_on_disk().unwrap().unwrap();
        let steps = [(disk.clone(), true)];
        let made = buffer.take_written(&steps, (&disk, stamp), agent).unwrap();
        assert_eq!(made[0].as_ref().unwrap().version, 1);
        assert_eq!(buffer.snapshot().text, "someone else's\n");
        assert_eq!(buffer.check_modified(), None);
        // Then it's as it was.
        assert!(buffer.changed_on_disk().unwrap().is_none());
    }

    #[test]
    fn an_agents_change_to_lines_changed_and_not_saved_is_never_made() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "a\nc\n").unwrap();
        let mut buffer = Buffer::open(path.clone(), dir.path().to_owned()).unwrap();
        typed(&mut buffer, 2, "foo bar\n", true);
        let agent = Author::Agent(TerminalId(1));
        // It read your unsaved line, and changes it: the file has no such
        // line, so nothing's written, rather than "a\nzc\n" or the like.
        let refused = buffer.write_merged("a\nfoo bar\nc\n", "a\nfoo baz\nc\n", agent, true);
        assert!(
            matches!(refused, Err(Refused::Conflict(lines)) if lines == std::iter::once(2..10).collect::<Vec<_>>())
        );
        assert_eq!(buffer.snapshot().text, "a\nfoo bar\nc\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nc\n");
        // A change to the line after yours merges, and goes on disk alone.
        let made = buffer.write_merged("a\nc\n", "a\nC\n", agent, true);
        assert!(made.unwrap().is_some());
        assert_eq!(buffer.snapshot().text, "a\nfoo bar\nC\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nC\n");

        // Written by the agent itself, across your line: the buffer stays
        // as you have it, and still won't write over the file unless
        // forced.
        fs::write(&path, "X\n").unwrap();
        let (disk, stamp) = buffer.changed_on_disk().unwrap().unwrap();
        let steps = [(disk.clone(), true)];
        let refused = buffer.take_written(&steps, (&disk, stamp), agent);
        assert!(matches!(refused, Err(Refused::Conflict(_))));
        assert_eq!(buffer.snapshot().text, "a\nfoo bar\nC\n");
        assert!(matches!(buffer.save(false), Err(Error::ChangedOnDisk(_))));
    }

    #[test]
    fn a_new_file_is_made_when_first_saved_and_a_directory_isnt_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.rs");
        let mut buffer = Buffer::open(path.clone(), dir.path().to_owned()).unwrap();
        assert_eq!(buffer.snapshot().text, "");
        typed(&mut buffer, 0, "fn main() {}\n", true);
        buffer.save(false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "fn main() {}\n");
        assert!(matches!(
            Buffer::open(dir.path().to_owned(), dir.path().to_owned()),
            Err(Error::OpenFailed { reason, .. }) if reason == "it's a directory"
        ));
        fs::write(&path, b"\xff\xfe").unwrap();
        assert!(matches!(
            Buffer::open(path, dir.path().to_owned()),
            Err(Error::OpenFailed { reason, .. }) if reason == "it isn't UTF-8 text"
        ));
    }

    #[test]
    fn undoing_back_to_what_was_saved_isnt_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut buffer = Buffer::open(dir.path().join("a"), dir.path().to_owned()).unwrap();
        typed(&mut buffer, 0, "x", true);
        assert_eq!(buffer.check_modified(), Some(true));
        buffer.reverse(Author::User, false).unwrap().unwrap();
        assert_eq!(buffer.check_modified(), Some(false));
        buffer.reverse(Author::User, true).unwrap().unwrap();
        assert_eq!(buffer.check_modified(), Some(true));
    }

    #[test]
    fn an_edit_the_daemon_makes_of_too_many_pieces_is_kept_coarsened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "ab".repeat(MOST_PIECES)).unwrap();
        let mut buffer = Buffer::open(path, dir.path().to_owned()).unwrap();
        // As a formatter's edit, changing every other character.
        let mut edit = Edit::new();
        for _ in 0..MOST_PIECES {
            edit.retain(1).delete("b").insert("B");
        }
        let made = buffer.edit(edit, 0, true, Author::User).unwrap();
        assert!(made.edit.ops().len() <= MOST_PIECES);
        assert_eq!(buffer.text().to_string(), "aB".repeat(MOST_PIECES));
        buffer.reverse(Author::User, false).unwrap().unwrap();
        assert_eq!(buffer.text().to_string(), "ab".repeat(MOST_PIECES));
    }

    #[test]
    fn discarding_reads_the_file_again_as_an_edit_that_can_be_undone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "saved").unwrap();
        let mut buffer = Buffer::open(path, dir.path().to_owned()).unwrap();
        typed(&mut buffer, 5, " and changed", true);
        buffer.check_modified();
        let edit = buffer.discard().unwrap();
        let base = buffer.version();
        buffer.edit(edit, base, true, Author::User).unwrap();
        assert_eq!(buffer.snapshot().text, "saved");
        assert_eq!(buffer.check_modified(), Some(false));
        buffer.reverse(Author::User, false).unwrap();
        assert_eq!(buffer.snapshot().text, "saved and changed");
    }

    #[test]
    fn a_buffer_is_handed_over_whole() {
        let dir = tempfile::tempdir().unwrap();
        let mut buffer = Buffer::open(dir.path().join("a"), dir.path().to_owned()).unwrap();
        typed(&mut buffer, 0, "héllo", true);
        typed(&mut buffer, 6, "!", false);
        buffer.check_modified();
        let mut resumed = Buffer::resume(buffer.hand_over(3), buffer.hand_over_undo(3));
        assert_eq!(resumed.snapshot(), buffer.snapshot());
        assert!(resumed.modified());
        resumed.reverse(Author::User, false).unwrap().unwrap();
        assert_eq!(resumed.snapshot().text, "");
    }

    #[test]
    fn a_late_edit_that_doesnt_fit_once_brought_up_to_date_is_refused_not_coarsened() {
        // Brought past an agent's edit, a window's edit of fewer pieces
        // than the most has more, and takes away text that isn't there:
        // coarsening it would read the text off a character's boundary.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, "é".repeat(24_000)).unwrap();
        let mut buffer = Buffer::open(path, dir.path().to_owned()).unwrap();
        let mut agents = Edit::new();
        let mut late = Edit::new();
        for _ in 0..6000 {
            agents.retain(4).insert("y").retain(4);
            late.retain(3).delete("abcd").retain(1);
        }
        let agent = Author::Agent(TerminalId(1));
        buffer.edit(agents, 0, true, agent).unwrap();
        let refused = buffer.edit(late, 0, true, Author::User);
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(buffer.version(), 1);
    }

    #[test]
    fn an_agent_keeps_only_its_last_few_undo_steps() {
        let dir = tempfile::tempdir().unwrap();
        let mut buffer = Buffer::open(dir.path().join("a"), dir.path().to_owned()).unwrap();
        let agent = Author::Agent(TerminalId(1));
        for at in 0..AGENT_STEPS + 4 {
            let edit = Edit::replace(at, at, "", "x");
            let version = buffer.version();
            buffer.edit(edit, version, true, agent).unwrap();
        }
        let mut undone = 0;
        while buffer.reverse(agent, false).unwrap().is_some() {
            undone += 1;
        }
        assert_eq!(undone, AGENT_STEPS);
        assert!(buffer.forgot(agent));
    }

    #[test]
    fn what_undo_steps_keep_is_handed_over() {
        let dir = tempfile::tempdir().unwrap();
        let mut buffer = Buffer::open(dir.path().join("a"), dir.path().to_owned()).unwrap();
        assert!(buffer.hand_over_undo(3).is_none(), "nothing kept");
        let state = |col| StepState {
            cursor: StepPlace { line: 0, col },
            taken: col > 0,
            visual: Some(StepVisual {
                start: StepPlace { line: 0, col: 1 },
                end: StepPlace { line: 0, col },
                kind: StepSelection::Block,
                backward: true,
                to_end: false,
            }),
        };
        let edit = Edit::replace(0, 0, "", "abc");
        buffer
            .edit_keeping(edit, 0, (true, Some(state(0))), Author::User)
            .unwrap();
        buffer.empty_step(1, state(2), Author::User).unwrap();
        let mut resumed = Buffer::resume(buffer.hand_over(3), buffer.hand_over_undo(3));
        let first = resumed
            .reverse_keeping(Author::User, false, 1, None)
            .unwrap()
            .unwrap();
        assert!(first.empty);
        assert_eq!(first.state, Some(state(2)));
        let second = resumed
            .reverse_keeping(Author::User, false, 1, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            (second.state, resumed.text().to_string()),
            (Some(state(0)), String::new())
        );
    }
}
