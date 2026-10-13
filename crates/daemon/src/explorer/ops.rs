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
//! - What a move between file systems deletes, it reaches as the copy
//!   did, never by its path: from the directory the source was in, held
//!   open from before the copy began, each directory opened from the one
//!   it's in without following links and checked to be the one copied.
//!   So a folder swapped for a link, or for another folder, between the
//!   copy and the delete, is never reached through, and what's at the
//!   other end is never deleted.
//! - Pipes, sockets and devices aren't copied: a directory holding one
//!   isn't, rather than arrive with holes.
//! - Nothing is deleted, nor trashed, with another file system mounted or
//!   bound inside it, and a mount point isn't at all (see
//!   [`super::mounts`]).

use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{EntryKind, Expect, FileOp, FileOpError, FileStamp};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::mounts::{Mount, entries, mount, no_mounts_in, not_a_mount, open_dir, remove_tree};
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

/// For tests: a path that making panics, as a bug might.
#[cfg(test)]
pub(crate) const PANICS: &str = "/clackatoo test: a file operation that panics";

/// Does `op`, with `trash` as the home trash, and `open` the files open in
/// buffers. Returns how buffers are to follow what moved, and what was
/// written over, even when it went wrong partway, and how it went.
pub(super) fn run(
    op: FileOp,
    trash: Option<&Path>,
    open: &[Open],
) -> (Changed, Result<(), FileOpError>) {
    #[cfg(test)]
    if matches!(&op, FileOp::Create { path, .. } if path == PANICS) {
        panic!("a file operation that panics, for tests");
    }
    let mut changed = Changed::default();
    let outcome = match op {
        FileOp::Create { path, dir } => absolute(&path).and_then(|path| create(&path, dir)),
        FileOp::Move {
            from,
            to,
            overwrite,
            expect,
        } => absolute(&from).and_then(|from| {
            let to = absolute(&to)?;
            move_to(&from, &to, overwrite, expect.as_ref(), open, &mut changed)
        }),
        FileOp::Copy {
            from,
            to,
            overwrite,
            expect,
        } => absolute(&from).and_then(|from| {
            let to = absolute(&to)?;
            copy(&from, &to, overwrite, expect.as_ref(), open, &mut changed)
        }),
        FileOp::Delete {
            path,
            trash: true,
            expect,
        } => absolute(&path).and_then(|path| {
            exists(&path)?;
            not_a_mount(&path)?;
            no_mounts_in(&path)?;
            // Last, just before it's done, as what's there may have
            // changed while the rest was looked at.
            as_expected(&path, expect.as_ref())?;
            trash::put(&path, trash)
        }),
        FileOp::Delete {
            path,
            trash: false,
            expect,
        } => absolute(&path).and_then(|path| {
            exists(&path)?;
            not_a_mount(&path)?;
            no_mounts_in(&path)?;
            as_expected(&path, expect.as_ref())?;
            remove_tree(&path)
        }),
    };
    (changed, outcome)
}

/// Fails unless what's at `path` is what `expect` says, if it says: the
/// same kind of thing, and if it says which file, that file, unchanged.
fn as_expected(path: &Path, expect: Option<&Expect>) -> Result<(), FileOpError> {
    let Some(expect) = expect else {
        return Ok(());
    };
    let meta = fs::symlink_metadata(path).map_err(|err| failed(path, &err))?;
    let kind = meta.file_type();
    let (now, was) = (what_kind(&meta), what(expect.kind));
    let same = match expect.kind {
        EntryKind::Dir => kind.is_dir(),
        EntryKind::File { .. } => kind.is_file(),
        EntryKind::Link { .. } => kind.is_symlink(),
        EntryKind::Other => !kind.is_dir() && !kind.is_file() && !kind.is_symlink(),
    };
    if !same {
        return Err(FileOpError::Failed(format!(
            "{} is {now} now, not {was}: it's changed since it was listed",
            path.display()
        )));
    }
    // A folder is told by where it is on disk; a file by that, and its
    // size and time too.
    let changed = match (expect.stamp, kind.is_dir()) {
        (Some(stamp), true) => stamp != folder_stamp(&meta),
        (Some(stamp), false) => stamp != file_stamp(&meta),
        (None, _) => false,
    };
    if changed {
        let what = if kind.is_dir() {
            "was deleted and made again"
        } else {
            "has changed"
        };
        return Err(FileOpError::Failed(format!(
            "{} {what} since it was listed",
            path.display()
        )));
    }
    Ok(())
}

/// What `meta` says is there, as people say it.
fn what_kind(meta: &fs::Metadata) -> &'static str {
    let kind = meta.file_type();
    if kind.is_symlink() {
        "a link"
    } else if kind.is_dir() {
        "a folder"
    } else if kind.is_file() {
        "a file"
    } else {
        "a pipe, socket or device"
    }
}

/// What `kind` is, as people say it.
fn what(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Link { .. } => "a link",
        EntryKind::Dir => "a folder",
        EntryKind::File { .. } => "a file",
        EntryKind::Other => "a pipe, socket or device",
    }
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
    expect: Option<&Expect>,
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
    // Last, just before it's done.
    as_expected(from, expect)?;
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
    move_across_and(from, to, replacing, open, changed, || {})
}

/// As [`move_across`], doing `between` once the copy is in place, before
/// anything's deleted: for tests, to change what was copied meanwhile.
fn move_across_and(
    from: &Path,
    to: &Path,
    replacing: bool,
    open: &[Open],
    changed: &mut Changed,
    between: impl FnOnce(),
) -> Result<(), FileOpError> {
    // A mount point, or a folder bound from elsewhere, is never emptied, nor
    // what's mounted in a folder: the kernel says another file system
    // before it says busy, so they get this far.
    not_a_mount(from)?;
    no_mounts_in(from)?;
    no_specials_in(from)?;
    let copying = Copying::now();
    let copied = copy_in_place(from, to, replacing)?;
    let (real_from, real_to) = (real(from), real(to));
    if replacing {
        changed.replaced = Some(real_to.clone());
    }
    between();
    let kept = copied.delete(&copying, to);
    for open in open {
        let Some(moved) = moved_path(&open.path, &real_from, &real_to) else {
            continue;
        };
        // Only what's gone from where it was has moved.
        if fs::symlink_metadata(&open.path).is_ok() {
            continue;
        }
        let stamps = copied.items.iter().find_map(|item| match &item.kind {
            Kind::File { source, copy, .. } if real(&item.path) == open.path => {
                Some((*source, *copy))
            }
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
    expect: Option<&Expect>,
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
    // Last, just before it's done.
    as_expected(from, expect)?;
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
fn copy_in_place(from: &Path, to: &Path, replacing: bool) -> Result<CopiedTree, FileOpError> {
    copy_in_place_named(from, to, replacing, |attempt| beside(to, attempt))
}

/// As [`copy_in_place`], with `named` giving the name beside `to` to try
/// at each attempt.
fn copy_in_place_named(
    from: &Path,
    to: &Path,
    replacing: bool,
    named: impl Fn(u32) -> PathBuf,
) -> Result<CopiedTree, FileOpError> {
    let (Some(parent), Some(name)) = (from.parent(), from.file_name()) else {
        return Err(FileOpError::Failed(format!(
            "{} can't be copied",
            from.display()
        )));
    };
    let fail = |errno: Errno| failed(from, &io::Error::from(errno));
    // The directory it's in, as its path says, links and all: only what's
    // in it is never reached through a link. Only ever a place to start
    // from, never read, so it needn't be readable, as it needn't for a
    // move by path.
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let above = rustix::fs::open(parent, flags, Mode::empty()).map_err(fail)?;
    let name = c_name(name).map_err(fail)?;
    make_parent(to)?;
    for attempt in 0..NAMES {
        let beside = named(attempt);
        let mut copied = Vec::new();
        let mut made = false;
        let source = Source {
            dir: above.as_fd(),
            name: &name,
            path: from,
        };
        match copy_tree(source, &beside, Path::new(""), &mut copied, &mut made) {
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
        return placed.map(|()| CopiedTree {
            above,
            name,
            items: copied,
        });
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

/// Something to copy: what's called `name` in the directory `dir`, which
/// is at `path`, said in what's noted of it and in errors.
#[derive(Clone, Copy)]
struct Source<'a> {
    dir: BorrowedFd<'a>,
    name: &'a CStr,
    path: &'a Path,
}

/// What a copy copied: the directory its source is in, held open from
/// before the copy began, the source's name there, and each thing copied,
/// a directory before what's in it.
pub(super) struct CopiedTree {
    above: OwnedFd,
    name: CString,
    items: Vec<Copied>,
}

impl CopiedTree {
    /// Deletes what was copied, as it was copied, and only that, from
    /// where it was copied, deepest first. `copying` says when the copy
    /// began, and `to` is where the copy is. Returns what's kept, and why.
    ///
    /// Each thing is reached through the directory it's in, opened from
    /// the one above it without following links, from the directory held
    /// since the copy began, and checked to be the directory copied: by
    /// where it is on disk, and that no other file system is mounted
    /// there, nor at the top, against the directory it's in. Only the
    /// directories down to where the walk is are held
    /// open, rather than each one the copy opened, so a wide tree never
    /// holds a descriptor for each of its directories.
    ///
    /// A name is still unlinked after it's checked, as nothing can unlink
    /// a name only if it's still a given file; but whatever took its place
    /// meanwhile is only unlinked from that directory, and a directory
    /// that isn't empty isn't removed.
    fn delete(&self, copying: &Copying, to: &Path) -> Vec<String> {
        self.delete_within(mount(self.above.as_fd(), c""), copying, to)
    }

    /// As [`delete`](Self::delete), reaching nothing that isn't on the
    /// mount `home`, which the directory what was copied is in is on.
    fn delete_within(
        &self,
        home: Result<Mount, Errno>,
        copying: &Copying,
        to: &Path,
    ) -> Vec<String> {
        let places: std::collections::HashMap<&Path, Place> = (self.items.iter())
            .filter_map(|item| match item.kind {
                Kind::Dir(place) => Some((item.rel.as_path(), place)),
                _ => None,
            })
            .collect();
        let mut walk = Walk {
            tree: self,
            places,
            open: Vec::new(),
            home,
        };
        let mut kept = Vec::new();
        for item in self.items.iter().rev() {
            let gone = match (item.rel.parent(), item.rel.file_name()) {
                (Some(parent), Some(name)) => walk.reach(parent).and_then(|dir| {
                    let name = c_name(name).map_err(io::Error::from)?;
                    delete_one(dir, &name, item, copying, to)
                }),
                _ => delete_one(self.above.as_fd(), &self.name, item, copying, to),
            };
            if matches!(item.kind, Kind::Dir(_)) {
                walk.open.retain(|(at, _)| !at.starts_with(&item.rel));
            }
            if let Err(err) = gone {
                kept.push(format!("{}: {err}", item.path.display()));
            }
        }
        kept
    }
}

/// The directories open, from the top of what was copied down, as
/// [`CopiedTree::delete`] walks it.
struct Walk<'a> {
    tree: &'a CopiedTree,
    /// Where each directory copied was on disk, by where it is in what
    /// was copied.
    places: std::collections::HashMap<&'a Path, Place>,
    /// The directories open, each in the one before it, by where each is
    /// in what was copied.
    open: Vec<(PathBuf, OwnedFd)>,
    /// The mount the directory what was copied is in is on, which every
    /// directory reached must be on too, the top included.
    home: Result<Mount, Errno>,
}

impl Walk<'_> {
    /// The directory at `rel` in what was copied, opened from the top,
    /// each directory on the way the one that was copied.
    fn reach(&mut self, rel: &Path) -> io::Result<BorrowedFd<'_>> {
        while self.open.last().is_some_and(|(at, _)| !rel.starts_with(at)) {
            self.open.pop();
        }
        loop {
            let depth = match self.open.last() {
                Some((at, _)) if at == rel => break,
                Some((at, _)) => at.components().count() + 1,
                None => 0,
            };
            let next: PathBuf = rel.components().take(depth).collect();
            // As people know it: from the name of what was moved.
            let top = Path::new(OsStr::from_bytes(self.tree.name.as_bytes()));
            let named = if depth == 0 {
                top.to_owned()
            } else {
                top.join(&next)
            };
            let not_copied = || {
                io::Error::other(format!(
                    "{} isn't the folder that was copied",
                    named.display()
                ))
            };
            let opened = match self.open.last() {
                Some((_, dir)) => {
                    let name = c_name(next.file_name().ok_or(Errno::INVAL)?)?;
                    open_dir(dir.as_fd(), &name)
                }
                None => open_dir(self.tree.above.as_fd(), &self.tree.name),
            };
            // Swapped for a link, or for what isn't a folder at all.
            let opened = match opened {
                Err(Errno::NOTDIR | Errno::LOOP) => return Err(not_copied()),
                opened => opened?,
            };
            let copied = self.places.get(next.as_path()).copied();
            if copied != Some(Place::of(&rustix::fs::fstat(&opened)?)) {
                return Err(not_copied());
            }
            if self.home?.crossed_by(mount(opened.as_fd(), c"")?) {
                return Err(io::Error::other(format!(
                    "another file system is mounted at {}",
                    named.display()
                )));
            }
            self.open.push((next, opened));
        }
        Ok(self.open.last().expect("just reached").1.as_fd())
    }
}

/// Deletes `item`, called `name` in the directory `dir`, if it's as it
/// was copied: the same directory, the same file unchanged, or a link to
/// the same place. `copying` says when the copy began, and `to` is where
/// the copy is.
fn delete_one(
    dir: BorrowedFd<'_>,
    name: &CStr,
    item: &Copied,
    copying: &Copying,
    to: &Path,
) -> io::Result<()> {
    match &item.kind {
        Kind::Dir(place) => {
            let found = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if FileType::from_raw_mode(found.st_mode) != FileType::Directory
                || Place::of(&found) != *place
            {
                return Err(io::Error::other("it isn't the folder that was copied"));
            }
            Ok(rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR)?)
        }
        Kind::File { source, ctime, .. }
            if copying.unchanged(dir, name, (source, *ctime), &item.copy_in(to)) =>
        {
            Ok(rustix::fs::unlinkat(dir, name, AtFlags::empty())?)
        }
        Kind::Link(target) if read_link(dir, name).ok().as_ref() == Some(target) => {
            Ok(rustix::fs::unlinkat(dir, name, AtFlags::empty())?)
        }
        Kind::File { .. } | Kind::Link(_) => {
            Err(io::Error::other("it changed while it was copied"))
        }
    }
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
    /// A directory, and where it was on disk.
    Dir(Place),
    /// A file, as it was all the time it was read, and its copy, as it was
    /// made.
    File {
        source: DiskStamp,
        ctime: Ctime,
        copy: DiskStamp,
    },
    Link(PathBuf),
}

/// When a file's inode last changed: what a writer that puts the file's
/// time back after writing it, as `rsync -t`, `touch -r` and tar do,
/// can't put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ctime {
    seconds: i64,
    nanoseconds: i64,
}

impl Ctime {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            seconds: meta.ctime(),
            nanoseconds: meta.ctime_nsec(),
        }
    }
}

/// Where a directory is on disk, which tells it from any other, even one
/// made again under its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    device: u64,
    inode: u64,
}

impl Place {
    fn of(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev,
            inode: stat.st_ino,
        }
    }
}

/// Copies `from` to `to`, where nothing is, `rel` inside what's being
/// copied: a directory with everything in it, a link as a link, and each
/// file made new, noting each in `copied`, a directory before what's in
/// it. Each is reached from the directory it's in, never following a
/// link. Notes in `made` when it's made what's at `to`, if `rel` is empty:
/// from then on, what's there is its own.
fn copy_tree(
    from: Source<'_>,
    to: &Path,
    rel: &Path,
    copied: &mut Vec<Copied>,
    made: &mut bool,
) -> io::Result<()> {
    let top = rel.as_os_str().is_empty();
    let found = rustix::fs::statat(from.dir, from.name, AtFlags::SYMLINK_NOFOLLOW)?;
    match FileType::from_raw_mode(found.st_mode) {
        FileType::Symlink => {
            let target = read_link(from.dir, from.name)?;
            std::os::unix::fs::symlink(&target, to)?;
            *made |= top;
            copied.push(Copied {
                path: from.path.to_owned(),
                rel: rel.to_owned(),
                kind: Kind::Link(target),
            });
        }
        FileType::Directory => {
            // Swapped for a link since, it isn't opened.
            let dir = open_dir(from.dir, from.name)?;
            let opened = rustix::fs::fstat(&dir)?;
            fs::create_dir(to)?;
            *made |= top;
            copied.push(Copied {
                path: from.path.to_owned(),
                rel: rel.to_owned(),
                kind: Kind::Dir(Place::of(&opened)),
            });
            for (name, _) in entries(&dir)? {
                let named = OsStr::from_bytes(name.as_bytes());
                let inside = Source {
                    dir: dir.as_fd(),
                    name: &name,
                    path: &from.path.join(named),
                };
                copy_tree(inside, &to.join(named), &rel.join(named), copied, made)?;
            }
            fs::set_permissions(to, fs::Permissions::from_mode(opened.st_mode))?;
        }
        FileType::RegularFile => {
            // Never waiting on a pipe swapped in since: it's refused below.
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            let mut source = File::from(rustix::fs::openat(
                from.dir,
                from.name,
                flags,
                Mode::empty(),
            )?);
            let meta = source.metadata()?;
            if !meta.is_file() {
                return Err(io::Error::other(format!(
                    "{} changed while it was copied",
                    from.path.display()
                )));
            }
            let before = (DiskStamp::of(&meta), Ctime::of(&meta));
            let mut copy = OpenOptions::new().write(true).create_new(true).open(to)?;
            *made |= top;
            io::copy(&mut source, &mut copy)?;
            copy.set_permissions(meta.permissions())?;
            // Read while it was being written: what was copied may be neither.
            let after = source.metadata()?;
            if (DiskStamp::of(&after), Ctime::of(&after)) != before {
                return Err(io::Error::other(format!(
                    "{} changed while it was copied",
                    from.path.display()
                )));
            }
            copied.push(Copied {
                path: from.path.to_owned(),
                rel: rel.to_owned(),
                kind: Kind::File {
                    source: before.0,
                    ctime: before.1,
                    copy: DiskStamp::of(&copy.metadata()?),
                },
            });
        }
        _ => {
            // Refused before copying began; here only if it appeared since.
            return Err(io::Error::other(format!(
                "{} is a pipe, socket or device, which isn't copied",
                from.path.display()
            )));
        }
    }
    Ok(())
}

/// Where the link called `name` in the directory `dir` leads.
fn read_link(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<PathBuf> {
    let target = rustix::fs::readlinkat(dir, name, Vec::new())?;
    Ok(PathBuf::from(OsString::from_vec(target.into_bytes())))
}

/// `name` as the kernel takes it.
fn c_name(name: &OsStr) -> Result<CString, Errno> {
    CString::new(name.as_bytes()).map_err(|_| Errno::INVAL)
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

    /// Whether the file called `name` in the directory `dir` is still as
    /// `source` and `ctime` say it was while it was copied, to `copy`: by
    /// its stamp and when its inode last changed, and, if it was written
    /// too close to the copy for its time to tell, by its bytes too. A
    /// link is never followed to it.
    fn unchanged(
        &self,
        dir: BorrowedFd<'_>,
        name: &CStr,
        (source, ctime): (&DiskStamp, Ctime),
        copy: &Path,
    ) -> bool {
        let stamp = |flags: OFlags| -> io::Result<(File, (DiskStamp, Ctime))> {
            let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let file = File::from(rustix::fs::openat(dir, name, flags, Mode::empty())?);
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(io::Error::other("not a file"));
            }
            Ok((file, (DiskStamp::of(&meta), Ctime::of(&meta))))
        };
        let as_copied = (*source, ctime);
        // Only looked at, not opened to read, which a device may notice.
        if !stamp(OFlags::PATH).is_ok_and(|(_, now)| now == as_copied) {
            return false;
        }
        if source.written() + RACY < self.started {
            return true;
        }
        match stamp(OFlags::RDONLY | OFlags::NONBLOCK) {
            Ok((file, now)) if now == as_copied => same_bytes(file, copy).unwrap_or(false),
            _ => false,
        }
    }
}

/// Whether the file `a` and the file at `b` hold the same bytes.
fn same_bytes(mut a: File, b: &Path) -> io::Result<bool> {
    let mut b = File::open(b)?;
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

/// What tells the folder `meta` is of from any other, even one made again
/// under its name: only where it is on disk, as its size and time change
/// with what's in it.
pub(crate) fn folder_stamp(meta: &fs::Metadata) -> FileStamp {
    FileStamp {
        device: meta.dev(),
        inode: meta.ino(),
        len: 0,
        seconds: 0,
        nanoseconds: 0,
    }
}

/// What tells the file `meta` is of, as it is, from any other.
pub(crate) fn file_stamp(meta: &fs::Metadata) -> FileStamp {
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
