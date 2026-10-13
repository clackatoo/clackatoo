//! Telling where another file system is mounted, and deleting a tree
//! without ever reaching into one.
//!
//! A mount is told by its mount ID (`statx`'s `STATX_MNT_ID`), and by the
//! kernel saying a directory is the root of one (`STATX_ATTR_MOUNT_ROOT`),
//! never by the device alone: a bind mount of a directory on the same
//! file system has the same device, and deleting a tree through one would
//! empty the directory bound there.
//!
//! A tree is deleted by a walk of its own, a directory at a time, each
//! opened from the one it's in without following links, and checked, once
//! opened, to be on the same mount as the tree's top: so a mount made
//! after the tree was checked, or a directory swapped for a link to
//! another, is never walked into.

use std::ffi::{CStr, CString, OsStr};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use clackatoo_protocol::FileOpError;
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, StatxAttributes, StatxFlags};
use rustix::io::Errno;

/// Which mount something is on, and whether it's that mount's root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Mount {
    /// The mount's ID, or, on a kernel too old to say, its device.
    id: u64,
    /// The kernel says it's where a mount is.
    root: bool,
}

impl Mount {
    /// Whether `inside`, something in a directory on this mount, is on
    /// another: a mount's root, or of another mount.
    pub(super) fn crossed_by(self, inside: Self) -> bool {
        inside.root || inside.id != self.id
    }

    /// For tests: a mount other than this one, for what's on this one to
    /// seem mounted on it.
    #[cfg(test)]
    pub(super) fn another(self) -> Self {
        Self {
            id: self.id.wrapping_add(1),
            root: false,
        }
    }
}

/// The mount of what's called `name` in the directory `dir`, not
/// following a link; or of `dir` itself if `name` is empty.
pub(super) fn mount(dir: BorrowedFd<'_>, name: &CStr) -> Result<Mount, Errno> {
    let mut flags = AtFlags::SYMLINK_NOFOLLOW;
    if name.is_empty() {
        flags |= AtFlags::EMPTY_PATH;
    }
    let found = rustix::fs::statx(dir, name, flags, StatxFlags::MNT_ID)?;
    let id = if found.stx_mask & StatxFlags::MNT_ID.bits() != 0 {
        found.stx_mnt_id
    } else {
        u64::from(found.stx_dev_major) << 32 | u64::from(found.stx_dev_minor)
    };
    let root = found
        .stx_attributes_mask
        .contains(StatxAttributes::MOUNT_ROOT)
        && found.stx_attributes.contains(StatxAttributes::MOUNT_ROOT);
    Ok(Mount { id, root })
}

/// The mount of what's at `path`, not following a link.
fn mount_at(path: &Path) -> Result<Mount, Errno> {
    mount(CWD, &c_path(path.as_os_str())?)
}

/// `name` as the kernel takes it.
fn c_path(name: &OsStr) -> Result<CString, Errno> {
    CString::new(name.as_bytes()).map_err(|_| Errno::INVAL)
}

/// Fails if another file system is mounted at `path`, or it's bound
/// there from elsewhere.
pub(super) fn not_a_mount(path: &Path) -> Result<(), FileOpError> {
    let Some(parent) = path.parent() else {
        return Err(mounted(path, path));
    };
    match (mount_at(parent), mount_at(path)) {
        (Ok(above), Ok(here)) if above.crossed_by(here) => Err(mounted(path, path)),
        _ => Ok(()),
    }
}

/// Fails if another file system is mounted, or bound, anywhere in the
/// directory at `path`. What's not a directory has nothing in it.
pub(super) fn no_mounts_in(path: &Path) -> Result<(), FileOpError> {
    let open = |at: &Path| open_dir(CWD, &c_path(at.as_os_str())?);
    let top = match open(path) {
        Ok(top) => top,
        Err(Errno::NOTDIR | Errno::LOOP) => return Ok(()),
        Err(errno) => return Err(failed(path, errno)),
    };
    let home = mount(top.as_fd(), c"").map_err(|errno| failed(path, errno))?;
    drop(top);
    // By path, so a wide tree never holds a directory open for each of
    // its directories. This is a check before anything's done: deleting
    // checks again as it goes.
    let mut stack = vec![path.to_owned()];
    while let Some(at) = stack.pop() {
        let dir = match open(&at) {
            Ok(dir) => dir,
            Err(Errno::NOTDIR | Errno::LOOP | Errno::NOENT) => continue,
            Err(errno) => return Err(failed(&at, errno)),
        };
        // The top's own mount is `not_a_mount`'s to check.
        let here = mount(dir.as_fd(), c"").map_err(|errno| failed(&at, errno))?;
        if at != path && home.crossed_by(here) {
            return Err(mounted(&at, path));
        }
        for (name, kind) in entries(&dir).map_err(|errno| failed(&at, errno))? {
            if matches!(kind, FileType::Directory | FileType::Unknown) {
                stack.push(at.join(OsStr::from_bytes(name.as_bytes())));
            }
        }
    }
    Ok(())
}

/// Deletes what's at `path` for good: a directory with everything in it,
/// a link but not what it leads to. It stops, failing, at another file
/// system mounted or bound anywhere in it, before reaching into it.
pub(super) fn remove_tree(path: &Path) -> Result<(), FileOpError> {
    let fail = |errno: Errno| failed(path, errno);
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(mounted(path, path));
    };
    let parent_dir = open_dir(CWD, &c_path(parent.as_os_str()).map_err(fail)?).map_err(fail)?;
    let name = c_path(name).map_err(fail)?;
    let home = mount(parent_dir.as_fd(), c"").map_err(fail)?;
    match open_dir(parent_dir.as_fd(), &name) {
        Ok(dir) => {
            let here = mount(dir.as_fd(), c"").map_err(fail)?;
            if home.crossed_by(here) {
                return Err(mounted(path, path));
            }
            empty(&dir, here, path)?;
            rustix::fs::unlinkat(parent_dir.as_fd(), &*name, AtFlags::REMOVEDIR).map_err(fail)
        }
        Err(Errno::NOTDIR | Errno::LOOP) => {
            rustix::fs::unlinkat(parent_dir.as_fd(), &*name, AtFlags::empty()).map_err(fail)
        }
        Err(errno) => Err(fail(errno)),
    }
}

/// Deletes everything in `dir`, the directory at `path`, on the mount
/// `home`, stopping at anything on another.
fn empty(dir: &OwnedFd, home: Mount, path: &Path) -> Result<(), FileOpError> {
    for (name, kind) in entries(dir).map_err(|errno| failed(path, errno))? {
        let inside = path.join(OsStr::from_bytes(name.as_bytes()));
        let fail = |errno: Errno| failed(&inside, errno);
        let opened = match kind {
            FileType::Directory | FileType::Unknown => open_dir(dir.as_fd(), &name),
            _ => Err(Errno::NOTDIR),
        };
        match opened {
            Ok(opened) => {
                let here = mount(opened.as_fd(), c"").map_err(fail)?;
                if home.crossed_by(here) {
                    return Err(mounted(&inside, path));
                }
                empty(&opened, home, &inside)?;
                rustix::fs::unlinkat(dir.as_fd(), &*name, AtFlags::REMOVEDIR).map_err(fail)?;
            }
            Err(Errno::NOTDIR | Errno::LOOP) => {
                rustix::fs::unlinkat(dir.as_fd(), &*name, AtFlags::empty()).map_err(fail)?;
            }
            Err(Errno::NOENT) => {}
            Err(errno) => return Err(fail(errno)),
        }
    }
    Ok(())
}

/// Opens the directory called `name` in `dir`, never following a link.
pub(super) fn open_dir(dir: BorrowedFd<'_>, name: &CStr) -> Result<OwnedFd, Errno> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
}

/// What's in the directory `dir`, by name and kind, read whole before
/// anything's done to it.
pub(super) fn entries(dir: &OwnedFd) -> Result<Vec<(CString, FileType)>, Errno> {
    let mut found = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        found.push((name.to_owned(), entry.file_type()));
    }
    Ok(found)
}

/// Why `deleting` isn't deleted: another file system is mounted at `at`.
fn mounted(at: &Path, deleting: &Path) -> FileOpError {
    FileOpError::Failed(format!(
        "another file system is mounted at {}: {} isn't deleted; unmount it first",
        at.display(),
        deleting.display()
    ))
}

/// Why something at `path` failed: `errno`.
fn failed(path: &Path, errno: Errno) -> FileOpError {
    FileOpError::Failed(format!(
        "{}: {}",
        path.display(),
        std::io::Error::from(errno)
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[test]
    fn a_mount_is_told_by_its_id_not_its_device() {
        let here = Mount { id: 1, root: false };
        // A bind mount of a directory on the same file system: the same
        // device, another mount.
        assert!(here.crossed_by(Mount { id: 2, root: false }));
        assert!(here.crossed_by(Mount { id: 1, root: true }));
        assert!(!here.crossed_by(Mount { id: 1, root: false }));
        let refused = not_a_mount(Path::new("/proc")).unwrap_err().to_string();
        assert!(refused.contains("mounted at /proc"), "{refused}");
    }

    /// Any mount on this machine whose device is its parent's, such as a
    /// bind mount, is still told apart. Only asked of: nothing is deleted.
    #[test]
    fn a_mount_on_the_same_device_as_its_parent_is_still_a_mount() {
        let Ok(mounts) = fs::read_to_string("/proc/self/mountinfo") else {
            return;
        };
        let device = |path: &Path| fs::symlink_metadata(path).map(|meta| meta.dev()).ok();
        let bound = (mounts.lines())
            .filter_map(|line| line.split(' ').nth(4))
            .map(Path::new)
            .find(|at| {
                *at != Path::new("/")
                    && !at.as_os_str().as_bytes().contains(&b'\\')
                    && device(at).is_some()
                    && device(at) == at.parent().and_then(device)
            });
        let Some(bound) = bound else { return };
        assert!(not_a_mount(bound).is_err(), "{}", bound.display());
    }

    #[test]
    fn a_tree_is_deleted_whole_without_following_links_out_of_it() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("kept.txt"), "kept").unwrap();
        let tree = dir.path().join("tree");
        fs::create_dir_all(tree.join("a/b/c")).unwrap();
        fs::write(tree.join("a/b/c/deep.txt"), "").unwrap();
        fs::write(tree.join("top.txt"), "").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("a/link")).unwrap();
        no_mounts_in(&tree).unwrap();
        remove_tree(&tree).unwrap();
        assert!(!tree.exists());
        assert_eq!(
            fs::read_to_string(outside.join("kept.txt")).unwrap(),
            "kept"
        );
        // A link is deleted, not what it leads to; a file is deleted.
        std::os::unix::fs::symlink(&outside, dir.path().join("link")).unwrap();
        remove_tree(&dir.path().join("link")).unwrap();
        assert!(outside.join("kept.txt").exists());
        remove_tree(&outside.join("kept.txt")).unwrap();
        assert!(!outside.join("kept.txt").exists());
        assert!(remove_tree(&dir.path().join("gone")).is_err());
    }
}
