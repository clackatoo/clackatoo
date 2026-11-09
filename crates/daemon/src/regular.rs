//! Opening files that must be regular ones, as every file the daemon reads
//! whole is: a pipe would have the open wait for a writer, for ever if
//! none comes, and a device such as `/dev/zero` could be read for ever.
//!
//! What's at the path is first opened only to be looked at (`O_PATH`),
//! which does nothing to what it is, and only if that's a regular file is
//! that very file opened to read, through `/proc/self/fd`: opening a
//! device can do something by itself, as opening a serial port raises its
//! DTR line, and a device or pipe swapped in for the file between a look
//! and an open would otherwise be opened. Where `/proc` isn't there, it's
//! opened by its path again, without waiting, and looked at again, so a
//! pipe swapped in is still never waited on.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsFd, AsRawFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use rustix::fs::{FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

/// Whether a link at a file's path is followed to the file it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Links {
    /// Followed, as for a file the user opened by a link's name.
    Follow,
    /// Not: what a folder from anyone holds, such as a `.gitignore`, which
    /// could name any file of the user's, or one of `/proc`'s.
    Refuse,
}

/// Opens the file at `path` to read, if it's a regular file: not a pipe,
/// a device or a directory (which fails `IsADirectory`), nor, unless
/// `links` follows them, a link.
pub(crate) fn open(path: &Path, links: Links) -> io::Result<File> {
    open_at(rustix::fs::CWD, path, links)
}

/// As [`open`], `path` from the directory open as `dir`.
pub(crate) fn open_at(dir: impl AsFd, path: &Path, links: Links) -> io::Result<File> {
    let refuse = |why: &str| Err(io::Error::new(io::ErrorKind::InvalidInput, why.to_owned()));
    let nofollow = if links == Links::Refuse {
        OFlags::NOFOLLOW
    } else {
        OFlags::empty()
    };
    let looked = rustix::fs::openat(
        &dir,
        path,
        OFlags::PATH | OFlags::CLOEXEC | nofollow,
        Mode::empty(),
    )?;
    let stat = rustix::fs::fstat(&looked)?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::RegularFile => {}
        FileType::Directory => return Err(io::ErrorKind::IsADirectory.into()),
        FileType::Symlink => return refuse("it's a link"),
        _ => return refuse("it isn't a regular file"),
    }
    let to_read = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let that = format!("/proc/self/fd/{}", looked.as_raw_fd());
    let file = match rustix::fs::open(&that, to_read, Mode::empty()) {
        Ok(file) => File::from(file),
        // No `/proc`: by its path again.
        Err(_) => File::from(rustix::fs::openat(
            &dir,
            path,
            to_read | nofollow,
            Mode::empty(),
        )?),
    };
    let meta = file.metadata()?;
    let same = (stat.st_dev, stat.st_ino) == (meta.dev(), meta.ino());
    if !meta.is_file() || !same {
        return refuse("it changed as it was opened");
    }
    Ok(file)
}

/// The directory `below` the directory at `root`, opened from it a part
/// at a time without following a link or going up out of it, and made,
/// each folder that isn't there, if `make`: so a link put in the way, even
/// between a check of the path and its use, can never lead elsewhere.
/// Linux's `openat2` does it in one step where it can.
pub(crate) fn dir_below(root: &Path, below: &Path, make: bool) -> io::Result<OwnedFd> {
    use std::path::Component;
    let flags = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let root = rustix::fs::open(root, flags, Mode::empty())?;
    let mut parts = Vec::new();
    for part in below.components() {
        match part {
            Component::Normal(name) => parts.push(name),
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} leads out of the folder it's in", below.display()),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Ok(root);
    }
    let how = ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS;
    match rustix::fs::openat2(&root, below, flags, Mode::empty(), how) {
        Ok(dir) => return Ok(dir),
        // Not there, and to be made, or no `openat2`: a part at a time.
        Err(Errno::NOENT) if make => {}
        Err(Errno::NOSYS | Errno::PERM | Errno::INVAL | Errno::AGAIN) => {}
        Err(err) => return Err(err.into()),
    }
    let mut at = root;
    for part in parts {
        // Never a link: with `O_NOFOLLOW`, one isn't a directory.
        let next = match rustix::fs::openat(&at, part, flags | OFlags::NOFOLLOW, Mode::empty()) {
            Err(Errno::NOENT) if make => {
                match rustix::fs::mkdirat(&at, part, Mode::from_raw_mode(0o777)) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(err) => return Err(err.into()),
                }
                rustix::fs::openat(&at, part, flags | OFlags::NOFOLLOW, Mode::empty())?
            }
            opened => opened?,
        };
        if FileType::from_raw_mode(rustix::fs::fstat(&next)?.st_mode) != FileType::Directory {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a part of the path is a link",
            ));
        }
        at = next;
    }
    Ok(at)
}

/// Opens the file at `below`, a plain relative path (no `..`), from the
/// directory at `root`, to read, if it's a regular file: never through a
/// link, at any part of the path, nor out of `root`, as [`dir_below`]
/// opens its folder.
pub(crate) fn open_below(root: &Path, below: &Path) -> io::Result<File> {
    let refuse = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, why.to_owned());
    let name = below
        .file_name()
        .ok_or_else(|| refuse("it isn't a file's path"))?;
    let folder = below.parent().unwrap_or_else(|| Path::new(""));
    let dir = dir_below(root, folder, false)?;
    open_at(&dir, Path::new(name), Links::Refuse)
}

/// The text of the small file at `path`, if it's a regular file, not a
/// link, of no more than `most` bytes, as a file a folder from anyone
/// holds is read: never followed, waited on or read whole.
pub(crate) fn read_small(path: &Path, most: u64) -> Option<String> {
    let file = open(path, Links::Refuse).ok()?;
    if file.metadata().ok()?.len() > most {
        return None;
    }
    let mut text = String::new();
    file.take(most).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The whole lines in the first `most` bytes of the file at `path`, as
/// [`open`] opens it with `links`, if it's a regular file: a line whose
/// text runs past `most` is left out (one whose text ends there, its line
/// break just after, isn't), and bytes that aren't UTF-8 are read as
/// U+FFFD, so the rest of the file still counts. For files of lines a
/// folder from anyone may hold, such as ignore files, which git reads as
/// bytes.
pub(crate) fn read_lines(path: &Path, links: Links, most: u64) -> Option<String> {
    let file = open(path, links).ok()?;
    let mut bytes = Vec::new();
    file.take(most.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > most {
        // More than `most`: up to its last whole line.
        let end = (bytes.iter().rposition(|&byte| byte == b'\n')).map_or(0, |at| at + 1);
        bytes.truncate(end);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fifo(path: &Path) {
        rustix::fs::mknodat(
            rustix::fs::CWD,
            path,
            rustix::fs::FileType::Fifo,
            Mode::from(0o600),
            0,
        )
        .unwrap();
    }

    #[test]
    fn only_a_regular_file_is_opened_and_a_link_only_if_followed() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "text").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let pipe = dir.path().join("pipe");
        fifo(&pipe);
        assert!(open(&file, Links::Refuse).is_ok());
        assert!(open(&link, Links::Follow).is_ok());
        assert!(open(&link, Links::Refuse).is_err());
        // Not waited on for a writer: refused at once.
        let err = open(&pipe, Links::Follow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = open(Path::new("/dev/zero"), Links::Follow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = open(dir.path(), Links::Follow).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::IsADirectory);
    }

    #[test]
    fn a_directory_below_is_reached_only_without_links_and_made_if_asked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("out")).unwrap();
        std::os::unix::fs::symlink(root.join("a"), root.join("in")).unwrap();
        assert!(dir_below(&root, Path::new("a/b"), false).is_ok());
        assert!(dir_below(&root, Path::new(""), false).is_ok());
        // A link, even to a folder within, or up and out: never.
        for below in ["out", "out/x", "in/b", "a/../out", "../root/a"] {
            assert!(dir_below(&root, Path::new(below), true).is_err(), "{below}");
        }
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
        // Made, a part at a time, where it isn't there.
        assert_eq!(
            dir_below(&root, Path::new("a/new/deeper"), false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        let made = dir_below(&root, Path::new("a/new/deeper"), true).unwrap();
        assert!(root.join("a/new/deeper").is_dir());
        // What's open is that folder, wherever its name goes after.
        std::fs::rename(root.join("a"), root.join("moved")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), root.join("a")).unwrap();
        rustix::fs::mkdirat(&made, "here", Mode::from_raw_mode(0o700)).unwrap();
        assert!(root.join("moved/new/deeper/here").is_dir());
        assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_device_is_never_opened() {
        // Refused for what it is, before it's opened: opened, the
        // terminal would fail with no controlling one, or be one.
        for links in [Links::Follow, Links::Refuse] {
            let err = open(Path::new("/dev/tty"), links).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{links:?}");
        }
    }

    #[test]
    fn lines_are_read_whole_up_to_the_most_whatever_their_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, b"one\n\xff\xfe two\nthree\nfour").unwrap();
        let read = |most| read_lines(&file, Links::Refuse, most);
        assert_eq!(
            read(100).as_deref(),
            Some("one\n\u{fffd}\u{fffd} two\nthree\nfour")
        );
        // Cut short within `three`: up to the line before.
        assert_eq!(read(12).as_deref(), Some("one\n\u{fffd}\u{fffd} two\n"));
        // Its text just within, its line break just past the most.
        assert_eq!(
            read(16).as_deref(),
            Some("one\n\u{fffd}\u{fffd} two\nthree\n")
        );
        // Just after it.
        assert_eq!(
            read(17).as_deref(),
            Some("one\n\u{fffd}\u{fffd} two\nthree\n")
        );
        assert_eq!(read(2).as_deref(), Some(""));
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_lines(&link, Links::Refuse, 100), None);
        assert!(read_lines(&link, Links::Follow, 100).is_some());
        let pipe = dir.path().join("pipe");
        fifo(&pipe);
        assert_eq!(read_lines(&pipe, Links::Follow, 100), None);
    }

    #[test]
    fn a_small_file_is_read_only_if_its_small() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "text").unwrap();
        assert_eq!(read_small(&file, 4).as_deref(), Some("text"));
        assert_eq!(read_small(&file, 3), None);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_small(&link, 4), None);
    }
}
