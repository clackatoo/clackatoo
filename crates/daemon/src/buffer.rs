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
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Author, BufferSnapshot, Edit, Error, Op, TerminalId};
use clackatoo_text::{History, HistoryError, HistorySnapshot, Rope, Steps};

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
struct DiskStamp {
    seconds: i64,
    nanoseconds: i64,
    len: u64,
    inode: u64,
    device: u64,
}

impl DiskStamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            seconds: metadata.mtime(),
            nanoseconds: metadata.mtime_nsec(),
            len: metadata.size(),
            inode: metadata.ino(),
            device: metadata.dev(),
        }
    }

    /// The file at `path` as it is now, if it exists.
    fn now(path: &Path) -> io::Result<Option<Self>> {
        match fs::metadata(path) {
            Ok(metadata) => Ok(Some(Self::of(&metadata))),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }
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
        let text = Rope::from_str(&text);
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
        let failed = |err: io::Error| Error::SaveFailed {
            path: self.path.display().to_string(),
            reason: err.to_string(),
        };
        if !force && DiskStamp::now(&self.path).map_err(failed)? != self.disk {
            return Err(Error::ChangedOnDisk(self.path.display().to_string()));
        }
        self.disk = Some(write(&self.path, &self.text).map_err(failed)?);
        self.saved = self.text.clone();
        Ok(self.check_modified())
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

/// Reads the file at `path` as text, and what it was as it was read.
fn read(path: &Path) -> io::Result<(String, DiskStamp)> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        return Err(io::ErrorKind::IsADirectory.into());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    Ok((text, DiskStamp::of(&metadata)))
}

/// Writes `text` to a new file beside `path`, with the same permissions
/// as the file there, then renames it over that file.
fn write(path: &Path, text: &Rope) -> io::Result<DiskStamp> {
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
    temporary.push(format!(".clackatoo-{}", std::process::id()));
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
        fs::rename(&temporary, path)?;
        DiskStamp::now(path)?.ok_or_else(|| io::ErrorKind::NotFound.into())
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

fn edit_ops(edit: &Edit) -> Vec<handover::EditOp> {
    (edit.ops().iter())
        .map(|op| match op {
            Op::Retain(len) => handover::EditOp::Retain(*len as u64),
            Op::Insert(text) => handover::EditOp::Insert(text.clone()),
            Op::Delete(text) => handover::EditOp::Delete(text.clone()),
        })
        .collect()
}

fn resume_edit(ops: Vec<handover::EditOp>) -> Edit {
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
