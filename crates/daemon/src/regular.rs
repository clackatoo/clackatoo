//! Opening files that must be regular ones, as every file the daemon reads
//! whole is: a pipe would have the open wait for a writer, for ever if
//! none comes, and a device such as `/dev/zero` could be read for ever.
//!
//! Each is opened without waiting, then asked what it is, so what's
//! checked is what was opened, never what was at its path a moment
//! before: something swapped in between would otherwise be opened
//! unchecked. A regular file reads the same either way.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

use rustix::fs::{Mode, OFlags};

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
    let mut flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    if links == Links::Refuse {
        flags |= OFlags::NOFOLLOW;
    }
    let file = File::from(rustix::fs::open(path, flags, Mode::empty())?);
    let meta = file.metadata()?;
    if meta.is_dir() {
        return Err(io::ErrorKind::IsADirectory.into());
    }
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it isn't a regular file",
        ));
    }
    Ok(file)
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
/// [`open`] opens it with `links`, if it's a regular file: a line cut
/// short by `most` is left out, and bytes that aren't UTF-8 are read as
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
        let end = bytes[..bytes.len() - 1]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |at| at + 1);
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
