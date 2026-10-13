//! Making, moving, copying and deleting files, on the thread for them.
//!
//! An operation never destroys what it wasn't told to, and says what it
//! would destroy before it's told to:
//!
//! - A file is written over only if asked to, after the asker was told
//!   what's there, and only if it's still that file (a [`FileStamp`]). A
//!   directory never is, nor is a file open with changes not written, by
//!   its path or by being the same file under another, nor anything that
//!   holds what's being moved or copied.
//! - A move asks the kernel not to replace what's there
//!   (`renameat2`'s `RENAME_NOREPLACE`), so what appears meanwhile is
//!   never lost.
//! - A copy is made beside where it goes, under a name nothing else has,
//!   then put in place by a rename, so a copy that fails partway leaves
//!   nothing behind, and never takes away what it was to replace, nor
//!   anything else that had the name it would have used.
//! - A move between file systems, which the kernel can't do in one step,
//!   copies, then deletes what it copied, file by file, and only those
//!   that haven't changed since: what changed or appeared meanwhile stays,
//!   and the answer says so. A file written so soon before it was copied
//!   that its time can't tell a change made in the same moment is compared
//!   with its copy, byte for byte, first, as git compares files it calls
//!   racy. Buffers follow what did move.
//! - Pipes, sockets and devices aren't copied: a directory holding one
//!   isn't, rather than arrive with holes.
//! - Nothing is deleted, nor trashed, with another file system mounted or
//!   bound inside it, and a mount point isn't at all (see
//!   [`super::mounts`]).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{FileOp, FileOpError, FileStamp};
use rustix::fs::{CWD, RenameFlags};
use rustix::io::Errno;

use super::mounts::{no_mounts_in, not_a_mount, remove_tree};
use super::{Changed, Follow, Open, trash};
use crate::buffer::{DiskStamp, Restamp};

/// The most a directory's contents are counted to, to say what writing
/// over it would lose.
const MOST_COUNTED: usize = 10_000;

/// How close to its copy a file's last write must be for its time not to
/// be trusted to tell a change made at once: more than any file system's
/// granularity, FAT's two seconds included.
const RACY: Duration = Duration::from_secs(2);

/// How many names beside where a copy goes are tried before giving up.
const NAMES: u32 = 100;

/// Does `op`, with `trash` as the home trash, and `open` the files open in
/// buffers. Returns how buffers are to follow what moved, and what was
/// written over, even when it went wrong partway, and how it went.
pub(super) fn run(
    op: FileOp,
    trash: Option<&Path>,
    open: &[Open],
) -> (Changed, Result<(), FileOpError>) {
    let mut changed = Changed::default();
    let outcome = match op {
        FileOp::Create { path, dir } => absolute(&path).and_then(|path| create(&path, dir)),
        FileOp::Move {
            from,
            to,
            overwrite,
        } => absolute(&from).and_then(|from| {
            let to = absolute(&to)?;
            move_to(&from, &to, overwrite, open, &mut changed)
        }),
        FileOp::Copy {
            from,
            to,
            overwrite,
        } => absolute(&from).and_then(|from| {
            let to = absolute(&to)?;
            copy(&from, &to, overwrite, open, &mut changed)
        }),
        FileOp::Delete { path, trash: true } => absolute(&path).and_then(|path| {
            exists(&path)?;
            not_a_mount(&path)?;
            no_mounts_in(&path)?;
            trash::put(&path, trash)
        }),
        FileOp::Delete { path, trash: false } => absolute(&path).and_then(|path| {
            exists(&path)?;
            not_a_mount(&path)?;
            no_mounts_in(&path)?;
            remove_tree(&path)
        }),
    };
    (changed, outcome)
}

/// `path`, if it's absolute, as every path in a file operation must be.
fn absolute(path: &str) -> Result<PathBuf, FileOpError> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(FileOpError::Failed(format!(
            "{} isn't an absolute path",
            path.display()
        )))
    }
}

/// Makes an empty file at `path`, or a directory if `dir`, and the
/// directories it's in.
fn create(path: &Path, dir: bool) -> Result<(), FileOpError> {
    make_parent(path)?;
    let made = if dir {
        fs::create_dir(path)
    } else {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map(drop)
    };
    made.map_err(|err| refused(path, &err))
}

/// Moves what's at `from` to `to`, replacing the file there only if
/// `overwrite` names it, noting in `changed` how buffers follow.
fn move_to(
    from: &Path,
    to: &Path,
    overwrite: Option<FileStamp>,
    open: &[Open],
    changed: &mut Changed,
) -> Result<(), FileOpError> {
    exists(from)?;
    let real_from = real(from);
    if real_from == real(to) {
        return Ok(());
    }
    apart(from, to)?;
    let replacing = check_target(to, overwrite, open)?;
    make_parent(to)?;
    let renamed = if replacing {
        fs::rename(from, to).map_err(|err| Errno::from_io_error(&err))
    } else {
        rename_new(from, to)
    };
    match renamed {
        Ok(()) => {
            let real_to = real(to);
            for open in open {
                if let Some(moved) = moved_path(&open.path, &real_from, &real_to) {
                    changed.follow.push(Follow {
                        from: open.path.clone(),
                        to: moved,
                        disk: Restamp::Keep,
                    });
                }
            }
            if replacing {
                changed.replaced = Some(real_to);
            }
            Ok(())
        }
        Err(Some(Errno::XDEV)) => move_across(from, to, replacing, open, changed),
        Err(Some(Errno::EXIST)) => Err(exists_at(to)),
        Err(errno) => {
            let err = errno.map_or_else(|| io::Error::other("it failed"), io::Error::from);
            Err(failed(from, &err))
        }
    }
}

/// Moves what's at `from` to `to`, on another file system: copies it,
/// puts the copy in place, replacing the file there if `replacing`, then
/// deletes what was copied, and has buffers follow what was.
pub(super) fn move_across(
    from: &Path,
    to: &Path,
    replacing: bool,
    open: &[Open],
    changed: &mut Changed,
) -> Result<(), FileOpError> {
    no_specials_in(from)?;
    let copying = Copying::now();
    let copied = copy_in_place(from, to, replacing)?;
    let (real_from, real_to) = (real(from), real(to));
    if replacing {
        changed.replaced = Some(real_to.clone());
    }
    // What was copied, as it was copied, and only that, deepest first.
    let mut kept: Vec<String> = Vec::new();
    for item in copied.iter().rev() {
        let gone = match &item.kind {
            Kind::Dir => fs::remove_dir(&item.path),
            Kind::File { source, .. }
                if copying.unchanged(&item.path, source, &item.copy_in(to)) =>
            {
                fs::remove_file(&item.path)
            }
            Kind::Link(target) if fs::read_link(&item.path).ok().as_ref() == Some(target) => {
                fs::remove_file(&item.path)
            }
            Kind::File { .. } | Kind::Link(_) => {
                Err(io::Error::other("it changed while it was copied"))
            }
        };
        if let Err(err) = gone {
            kept.push(format!("{}: {err}", item.path.display()));
        }
    }
    for open in open {
        let Some(moved) = moved_path(&open.path, &real_from, &real_to) else {
            continue;
        };
        // Only what's gone from where it was has moved.
        if fs::symlink_metadata(&open.path).is_ok() {
            continue;
        }
        let stamps = copied.iter().find_map(|item| match &item.kind {
            Kind::File { source, copy } if real(&item.path) == open.path => Some((*source, *copy)),
            _ => None,
        });
        // The file the buffer read, unchanged since, copied to a file
        // that's still as it was made, is the file it knows now: its save
        // isn't refused. Anything else changed and still is.
        let disk = match stamps {
            Some((source, copy))
                if open.disk == Some(source) && DiskStamp::now(&moved).ok() == Some(Some(copy)) =>
            {
                Restamp::To(Some(copy))
            }
            _ => Restamp::Keep,
        };
        changed.follow.push(Follow {
            from: open.path.clone(),
            to: moved,
            disk,
        });
    }
    if kept.is_empty() {
        return Ok(());
    }
    Err(FileOpError::Failed(format!(
        "copied {} to {}, but {} of it couldn't be deleted after, and is in both places: {}",
        from.display(),
        to.display(),
        items(kept.len()),
        kept.join("; ")
    )))
}

/// Renames `from` to `to` if nothing's at `to`, failing with `EEXIST` if
/// something is. A file system that can't promise that is checked first.
pub(super) fn rename_new(from: &Path, to: &Path) -> Result<(), Option<Errno>> {
    match rustix::fs::renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Err(Errno::INVAL | Errno::NOSYS) => {
            if fs::symlink_metadata(to).is_ok() {
                return Err(Some(Errno::EXIST));
            }
            fs::rename(from, to).map_err(|err| Errno::from_io_error(&err))
        }
        other => other.map_err(Some),
    }
}

/// Copies what's at `from` to `to`, replacing the file there only if
/// `overwrite` names it, noting in `changed` what was replaced.
fn copy(
    from: &Path,
    to: &Path,
    overwrite: Option<FileStamp>,
    open: &[Open],
    changed: &mut Changed,
) -> Result<(), FileOpError> {
    exists(from)?;
    if real(from) == real(to) {
        return Err(FileOpError::Failed(format!(
            "{} can't be copied onto itself",
            from.display()
        )));
    }
    apart(from, to)?;
    let replacing = check_target(to, overwrite, open)?;
    no_specials_in(from)?;
    copy_in_place(from, to, replacing)?;
    if replacing {
        changed.replaced = Some(real(to));
    }
    Ok(())
}

/// Copies what's at `from` beside `to`, under a name nothing has, then
/// puts the copy at `to`, replacing the file there if `replacing`. Returns
/// what was copied. A copy that fails takes away what it made, and only
/// that.
fn copy_in_place(from: &Path, to: &Path, replacing: bool) -> Result<Vec<Copied>, FileOpError> {
    copy_in_place_named(from, to, replacing, |attempt| beside(to, attempt))
}

/// As [`copy_in_place`], with `named` giving the name beside `to` to try
/// at each attempt.
fn copy_in_place_named(
    from: &Path,
    to: &Path,
    replacing: bool,
    named: impl Fn(u32) -> PathBuf,
) -> Result<Vec<Copied>, FileOpError> {
    make_parent(to)?;
    for attempt in 0..NAMES {
        let beside = named(attempt);
        let mut copied = Vec::new();
        let mut made = false;
        match copy_tree(from, &beside, Path::new(""), &mut copied, &mut made) {
            Ok(()) => {}
            // Something has the name: another is tried.
            Err(err) if !made && err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                if made {
                    let _ = remove_tree(&beside);
                }
                return Err(failed(from, &err));
            }
        }
        let placed = if replacing {
            fs::rename(&beside, to).map_err(|err| failed(to, &err))
        } else {
            rename_new(&beside, to).map_err(|errno| match errno {
                Some(Errno::EXIST) => exists_at(to),
                errno => {
                    let err = errno.map_or_else(|| io::Error::other("it failed"), io::Error::from);
                    failed(to, &err)
                }
            })
        };
        if placed.is_err() {
            let _ = remove_tree(&beside);
        }
        return placed.map(|()| copied);
    }
    Err(FileOpError::Failed(format!(
        "no name beside {} was free to copy to",
        to.display()
    )))
}

/// The `attempt`th name beside `to`, for a copy on its way there.
fn beside(to: &Path, attempt: u32) -> PathBuf {
    let name = to
        .file_name()
        .map_or_else(Default::default, |name| name.to_string_lossy());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    to.with_file_name(format!(
        ".{name}.clackatoo-{}-{nanos}-{attempt}",
        std::process::id()
    ))
}

/// One thing copied: its source, where it is in what was copied, and what
/// it was when it was copied.
#[derive(Debug)]
pub(super) struct Copied {
    path: PathBuf,
    rel: PathBuf,
    kind: Kind,
}

impl Copied {
    /// Where its copy is, once what was copied is at `to`.
    fn copy_in(&self, to: &Path) -> PathBuf {
        if self.rel.as_os_str().is_empty() {
            to.to_owned()
        } else {
            to.join(&self.rel)
        }
    }
}

#[derive(Debug)]
enum Kind {
    Dir,
    /// A file, as it was all the time it was read, and its copy, as it was
    /// made.
    File {
        source: DiskStamp,
        copy: DiskStamp,
    },
    Link(PathBuf),
}

/// Copies what's at `from` to `to`, where nothing is, `rel` inside what's
/// being copied: a directory with everything in it, a link as a link, and
/// each file made new, noting each in `copied`, a directory before what's
/// in it. Notes in `made` when it's made what's at `to`, if `rel` is
/// empty: from then on, what's there is its own.
fn copy_tree(
    from: &Path,
    to: &Path,
    rel: &Path,
    copied: &mut Vec<Copied>,
    made: &mut bool,
) -> io::Result<()> {
    let top = rel.as_os_str().is_empty();
    let meta = fs::symlink_metadata(from)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = fs::read_link(from)?;
        std::os::unix::fs::symlink(&target, to)?;
        *made |= top;
        copied.push(Copied {
            path: from.to_owned(),
            rel: rel.to_owned(),
            kind: Kind::Link(target),
        });
    } else if kind.is_dir() {
        fs::create_dir(to)?;
        *made |= top;
        copied.push(Copied {
            path: from.to_owned(),
            rel: rel.to_owned(),
            kind: Kind::Dir,
        });
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let name = entry.file_name();
            copy_tree(
                &entry.path(),
                &to.join(&name),
                &rel.join(&name),
                copied,
                made,
            )?;
        }
        fs::set_permissions(to, meta.permissions())?;
    } else if kind.is_file() {
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(from)?;
        let before = DiskStamp::of(&source.metadata()?);
        let mut copy = OpenOptions::new().write(true).create_new(true).open(to)?;
        *made |= top;
        io::copy(&mut source, &mut copy)?;
        copy.set_permissions(meta.permissions())?;
        // Read while it was being written: what was copied may be neither.
        if DiskStamp::of(&source.metadata()?) != before {
            return Err(io::Error::other(format!(
                "{} changed while it was copied",
                from.display()
            )));
        }
        copied.push(Copied {
            path: from.to_owned(),
            rel: rel.to_owned(),
            kind: Kind::File {
                source: before,
                copy: DiskStamp::of(&copy.metadata()?),
            },
        });
    } else {
        // Refused before copying began; here only if it appeared since.
        return Err(io::Error::other(format!(
            "{} is a pipe, socket or device, which isn't copied",
            from.display()
        )));
    }
    Ok(())
}

/// When a copy started, to tell files written too close to it to trust
/// their times.
struct Copying {
    started: SystemTime,
}

impl Copying {
    fn now() -> Self {
        Self {
            started: SystemTime::now(),
        }
    }

    /// Whether the file at `path` is still as `source` says it was while
    /// it was copied, to `copy`: by its stamp, and, if it was written too
    /// close to the copy for its time to tell, by its bytes too.
    fn unchanged(&self, path: &Path, source: &DiskStamp, copy: &Path) -> bool {
        if DiskStamp::now(path).ok().flatten().as_ref() != Some(source) {
            return false;
        }
        if source.written() + RACY < self.started {
            return true;
        }
        same_bytes(path, copy).unwrap_or(false)
    }
}

/// Whether the files at `a` and `b` hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (File::open(a)?, File::open(b)?);
    if a.metadata()?.len() != b.metadata()?.len() {
        return Ok(false);
    }
    let (mut left, mut right) = (vec![0; 64 * 1024], vec![0; 64 * 1024]);
    loop {
        let read = a.read(&mut left)?;
        if read == 0 {
            return Ok(b.read(&mut right[..1])? == 0);
        }
        b.read_exact(&mut right[..read])?;
        if left[..read] != right[..read] {
            return Ok(false);
        }
    }
}

/// Fails if what's at `path` is, or holds, a pipe, socket or device,
/// which aren't copied, not following links.
fn no_specials_in(path: &Path) -> Result<(), FileOpError> {
    let mut stack = vec![path.to_owned()];
    while let Some(at) = stack.pop() {
        let meta = fs::symlink_metadata(&at).map_err(|err| failed(&at, &err))?;
        let kind = meta.file_type();
        if kind.is_dir() {
            for entry in fs::read_dir(&at).map_err(|err| failed(&at, &err))? {
                stack.push(entry.map_err(|err| failed(&at, &err))?.path());
            }
        } else if !kind.is_file() && !kind.is_symlink() {
            return Err(FileOpError::Failed(format!(
                "{} is a pipe, socket or device, which isn't copied: nothing was",
                at.display()
            )));
        }
    }
    Ok(())
}

/// Checks that something may be put at `to`: nothing's there, or a file
/// is, which `overwrite` names as it is now, and which isn't open with
/// changes not written. Returns whether a file is there, to be replaced.
fn check_target(
    to: &Path,
    overwrite: Option<FileStamp>,
    open: &[Open],
) -> Result<bool, FileOpError> {
    let meta = match fs::symlink_metadata(to) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(failed(to, &err)),
    };
    if meta.is_dir() {
        return Err(FileOpError::Failed(format!(
            "{} is a folder with {}: a folder is never written over; delete it first",
            to.display(),
            items(count(to))
        )));
    }
    // By its path, or as the file it is, under any name.
    let real_to = real(to);
    let file = fs::metadata(to).ok();
    let open_here = open.iter().filter(|open| open.modified).any(|open| {
        open.path == real_to
            || (open.disk.zip(file.as_ref())).is_some_and(|(disk, file)| disk.is_file(file))
    });
    if open_here {
        return Err(FileOpError::Failed(format!(
            "{} is open with changes not written: write them or throw them away first",
            to.display()
        )));
    }
    // Only the file described: one changed since is described again.
    match overwrite {
        Some(stamp) if stamp == file_stamp(&meta) => Ok(true),
        _ => Err(exists_at(to)),
    }
}

/// Fails if `to` is in `from`, or `from` in `to`: one can't go over the
/// other.
fn apart(from: &Path, to: &Path) -> Result<(), FileOpError> {
    let (real_from, real_to) = (real(from), real(to));
    if real_to.starts_with(&real_from) {
        return Err(FileOpError::Failed(format!(
            "{} can't go inside itself",
            from.display()
        )));
    }
    if real_from.starts_with(&real_to) {
        return Err(FileOpError::Failed(format!(
            "{} holds {}: it can't be written over by what's in it",
            to.display(),
            from.display()
        )));
    }
    Ok(())
}

/// Where the file at `path` is once what's at `from` has moved to `to`,
/// if it's at or in `from`.
fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() {
        to.to_owned()
    } else {
        to.join(rest)
    })
}

/// How many things are in the directory at `dir`, all the way down, up
/// to [`MOST_COUNTED`].
fn count(dir: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![dir.to_owned()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            count += 1;
            if count > MOST_COUNTED {
                return count;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    count
}

/// `count` things, as people say it.
fn items(count: usize) -> String {
    match count {
        1 => "1 item".to_owned(),
        count if count > MOST_COUNTED => format!("more than {MOST_COUNTED} items"),
        count => format!("{count} items"),
    }
}

/// What tells the file `meta` is of, as it is, from any other.
fn file_stamp(meta: &fs::Metadata) -> FileStamp {
    FileStamp {
        device: meta.dev(),
        inode: meta.ino(),
        len: meta.len(),
        seconds: meta.mtime(),
        nanoseconds: meta.mtime_nsec(),
    }
}

/// Why something can't be put at `to`: a file's there, which this says
/// of, and names, to write over it with.
fn exists_at(to: &Path) -> FileOpError {
    let meta = match fs::symlink_metadata(to) {
        Ok(meta) => meta,
        Err(err) => return failed(to, &err),
    };
    let what = if meta.file_type().is_symlink() {
        "a link".to_owned()
    } else if meta.is_dir() {
        format!("a folder with {}", items(count(to)))
    } else if meta.is_file() {
        format!("a file of {}", size(meta.len()))
    } else {
        "a pipe, socket or device".to_owned()
    };
    FileOpError::Exists {
        path: to.display().to_string(),
        what,
        stamp: file_stamp(&meta),
    }
}

/// `bytes`, as people say it.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes == 1 {
        return "1 byte".to_owned();
    }
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{} {}", value.div_ceil(1024), UNITS[unit])
}

/// Makes the directories `path` is in, if they aren't there.
fn make_parent(path: &Path) -> Result<(), FileOpError> {
    match path.parent() {
        Some(parent) => fs::create_dir_all(parent).map_err(|err| failed(parent, &err)),
        None => Ok(()),
    }
}

/// Fails unless something's at `path`.
fn exists(path: &Path) -> Result<(), FileOpError> {
    fs::symlink_metadata(path)
        .map(drop)
        .map_err(|err| failed(path, &err))
}

/// `path` with the directory it's in made real, its links followed, but
/// not `path` itself, which may be a link, or not exist: the path the
/// daemon knows a buffer of it by.
pub(super) fn real(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map_or_else(|_| path.to_owned(), |parent| parent.join(name)),
        _ => path.to_owned(),
    }
}

/// Why making something at `path` failed: something's there, or `err`.
fn refused(path: &Path, err: &io::Error) -> FileOpError {
    if err.kind() == io::ErrorKind::AlreadyExists {
        exists_at(path)
    } else {
        failed(path, err)
    }
}

/// Why something at `path` failed: `err`.
fn failed(path: &Path, err: &io::Error) -> FileOpError {
    FileOpError::Failed(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests;
