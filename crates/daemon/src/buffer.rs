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
use clackatoo_text::{History, HistoryError, HistorySnapshot, Merged, Rope, Steps, merge};

use crate::handover;

/// A file being edited.
#[derive(Debug)]
pub(crate) struct Buffer {
    path: PathBuf,
    /// The directory paths opened beside it are from.
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

/// An edit made to a buffer.
#[derive(Debug)]
pub(crate) struct Made {
    /// The version it made.
    pub(crate) version: u64,
    /// The edit, made to the version before.
    pub(crate) edit: Edit,
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

    pub(crate) fn path(&self) -> &Path {
        &self.path
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
    /// to date first. `step` starts a new undo step.
    pub(crate) fn edit(
        &mut self,
        edit: Edit,
        base: u64,
        step: bool,
        author: Author,
    ) -> Result<Made, HistoryError> {
        let edit = self.history.rebase(edit, base)?;
        edit.apply(&mut self.text)?;
        let version = self.history.record(edit.clone(), author, step)?;
        Ok(Made { version, edit })
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
    /// says so.
    pub(crate) fn stage(&self, text: &Rope, force: bool) -> Result<Staged, Error> {
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
    /// other, which can be undone.
    pub(crate) fn discard(&mut self) -> Result<Edit, Error> {
        let (text, disk) = match read(&self.path) {
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
            disk: self.disk.map(|disk| handover::DiskStamp {
                seconds: disk.seconds,
                nanoseconds: disk.nanoseconds,
                len: disk.len,
                inode: disk.inode,
                device: disk.device,
            }),
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

    /// A buffer the last build handed over.
    pub(crate) fn resume(handed: handover::Buffer) -> Self {
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
                    .map(|steps| Steps {
                        author: resume_author(steps.author),
                        version: steps.version,
                        undo: steps.undo.into_iter().map(resume_edit).collect(),
                        redo: steps.redo.into_iter().map(resume_edit).collect(),
                    })
                    .collect(),
            }),
            saved,
            modified: false,
            disk: handed.disk.map(|disk| DiskStamp {
                seconds: disk.seconds,
                nanoseconds: disk.nanoseconds,
                len: disk.len,
                inode: disk.inode,
                device: disk.device,
            }),
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
        .into()
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
        let mut resumed = Buffer::resume(buffer.hand_over(3));
        assert_eq!(resumed.snapshot(), buffer.snapshot());
        assert!(resumed.modified());
        resumed.reverse(Author::User, false).unwrap().unwrap();
        assert_eq!(resumed.snapshot().text, "");
    }
}
