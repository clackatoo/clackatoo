//! Moving files to the trash, as the freedesktop.org trash spec (1.0) has
//! it, so a desktop's file manager can show them and put them back.
//!
//! A trash is a directory holding `files`, what was deleted, and `info`,
//! a `NAME.trashinfo` for each, saying where it was and when it was
//! deleted. What's deleted is renamed into the trash on its own file
//! system, never copied: the home trash, if it's on the same one, or else
//! one at the top of that file system, `.Trash/UID` if the administrator
//! made `.Trash`, real and sticky, or else `.Trash-UID`. Each part of one
//! there must be a real directory the user owns, never a link, as gio
//! checks, since anyone who can write the top of a file system could
//! otherwise point it elsewhere. A file system with none of those has no
//! trash, and what's on it can only be deleted for good.
//!
//! When it was deleted is written in UTC: the spec asks for local time,
//! which would need the time zone database, so a file manager may show it
//! a few hours out.

use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clackatoo_protocol::FileOpError;

/// How many names are tried for something deleted before giving up, as
/// `NAME`, `NAME.2`, `NAME.3`…
const NAMES: u32 = 1000;

/// Moves what's at `path` to the trash on its file system, `home` being
/// the home trash.
pub(crate) fn put(path: &Path, home: Option<&Path>) -> Result<(), FileOpError> {
    let failed = |err: &io::Error| FileOpError::Failed(format!("{}: {err}", path.display()));
    let device = fs::symlink_metadata(path)
        .map_err(|err| failed(&err))?
        .dev();
    let (trash, written) = find(path, device, home).ok_or_else(|| {
        FileOpError::NoTrash(format!(
            "{} is on a file system with no trash",
            path.display()
        ))
    })?;
    let name = path
        .file_name()
        .ok_or_else(|| FileOpError::Failed(format!("{} can't be trashed", path.display())))?;
    let info = info(&written);
    for number in 1..=NAMES {
        let mut taken = name.to_owned();
        if number > 1 {
            taken.push(format!(".{number}"));
        }
        let mut info_name = taken.clone();
        info_name.push(".trashinfo");
        let info_path = trash.join("info").join(info_name);
        // Made new, so two trashing at once never take one name.
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&info_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(failed(&err)),
        };
        if let Err(err) = file.write_all(info.as_bytes()) {
            let _ = fs::remove_file(&info_path);
            return Err(failed(&err));
        }
        // Never over something already in the trash under that name, as
        // one put there without its info: the next name is tried.
        match super::ops::rename_new(path, &trash.join("files").join(&taken)) {
            Ok(()) => return Ok(()),
            Err(errno) => {
                let _ = fs::remove_file(&info_path);
                if errno == Some(rustix::io::Errno::EXIST) {
                    continue;
                }
                let err = errno.map_or_else(|| io::Error::other("it failed"), io::Error::from);
                return Err(failed(&err));
            }
        }
    }
    Err(FileOpError::Failed(format!(
        "the trash has too many called {}",
        name.to_string_lossy()
    )))
}

/// The trash for what's at `path`, on the file system `device`, with
/// `files` and `info` made in it, and how `path` is written in its info:
/// absolute in the home trash, and from the top of the file system in one
/// there.
fn find(path: &Path, device: u64, home: Option<&Path>) -> Option<(PathBuf, PathBuf)> {
    if let Some(home) = home
        && made(home)
        && fs::metadata(home).is_ok_and(|meta| meta.dev() == device)
    {
        return Some((home.to_owned(), path.to_owned()));
    }
    let top = path
        .ancestors()
        .skip(1)
        .take_while(|dir| fs::symlink_metadata(dir).is_ok_and(|meta| meta.dev() == device))
        .last()?;
    let trash = top_trash(top, device, rustix::process::getuid().as_raw())?;
    let written = path.strip_prefix(top).unwrap_or(path).to_owned();
    Some((trash, written))
}

/// The trash at the top of a file system, `top`, on `device`, for the
/// user `uid`, made if it isn't there, as gio finds it: `.Trash/UID` if
/// the administrator made `.Trash`, a real directory, not a link, and
/// sticky, so no one deletes another's; or else `.Trash-UID`. Each part
/// of it must be a real directory the user owns, never a link someone
/// else could point elsewhere, and is made without following one.
fn top_trash(top: &Path, device: u64, uid: u32) -> Option<PathBuf> {
    let shared = top.join(".Trash");
    let usable = fs::symlink_metadata(&shared).is_ok_and(|meta| {
        meta.file_type().is_dir() && meta.permissions().mode() & 0o1000 != 0 && meta.dev() == device
    });
    let trash = if usable {
        shared.join(uid.to_string())
    } else {
        top.join(format!(".Trash-{uid}"))
    };
    let owned = |dir: &Path| owned_dir(dir, device, uid);
    (owned(&trash) && owned(&trash.join("files")) && owned(&trash.join("info"))).then_some(trash)
}

/// Whether `dir` is a real directory on `device` that `uid` owns, making
/// it, readable by its owner only, if nothing's there. A link isn't
/// followed: making one fails if a link is there, and a link is never a
/// directory here.
fn owned_dir(dir: &Path, device: u64, uid: u32) -> bool {
    let fine = |meta: &fs::Metadata| {
        meta.file_type().is_dir() && meta.uid() == uid && meta.dev() == device
    };
    match fs::symlink_metadata(dir) {
        Ok(meta) => fine(&meta),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(dir).is_ok()
                && fs::symlink_metadata(dir).is_ok_and(|meta| fine(&meta))
        }
        Err(_) => false,
    }
}

/// Makes the home trash at `trash`, with its `files` and `info`, readable
/// by its owner only, if they aren't there. Returns whether they are now.
fn made(trash: &Path) -> bool {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(trash.join("files")).is_ok() && builder.create(trash.join("info")).is_ok()
}

/// The info file for something deleted from `path`, now.
fn info(path: &Path) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encoded(path),
        date(seconds)
    )
}

/// `path` as a URL writes it, as the spec asks: bytes other than letters,
/// digits, `/` and `-._~` as `%XX`.
fn encoded(path: &Path) -> String {
    let mut written = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            written.push(char::from(byte));
        } else {
            let _ = write!(written, "%{byte:02X}");
        }
    }
    written
}

/// The date and time `seconds` after the Unix epoch, as `YYYY-MM-DDThh:mm:ss`,
/// in UTC.
fn date(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let time = seconds % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_written_as_the_spec_says() {
        assert_eq!(date(0), "1970-01-01T00:00:00");
        assert_eq!(date(951_782_400), "2000-02-29T00:00:00");
        assert_eq!(date(1_791_891_045), "2026-10-13T11:30:45");
    }

    #[test]
    fn paths_are_written_as_urls() {
        assert_eq!(encoded(Path::new("/a b/ü%.txt")), "/a%20b/%C3%BC%25.txt");
    }

    #[test]
    fn what_is_in_the_trash_without_its_info_is_never_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("Trash");
        fs::create_dir_all(home.join("files")).unwrap();
        fs::create_dir_all(home.join("info")).unwrap();
        fs::write(home.join("files/a.txt"), "already there").unwrap();
        let file = dir.path().join("a.txt");
        fs::write(&file, "trashed").unwrap();
        put(&file, Some(&home)).unwrap();
        let read = |name: &str| fs::read_to_string(home.join("files").join(name)).unwrap();
        assert_eq!(
            (read("a.txt"), read("a.txt.2")),
            ("already there".into(), "trashed".into())
        );
        assert!(
            !home.join("info/a.txt.trashinfo").exists(),
            "its info taken back"
        );
        assert!(home.join("info/a.txt.2.trashinfo").exists());
    }

    #[test]
    fn a_trash_at_the_top_of_a_file_system_must_be_a_real_directory_its_user_owns() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path();
        let device = fs::metadata(top).unwrap().dev();
        let uid = rustix::process::getuid().as_raw();
        let elsewhere = top.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        // A link at .Trash, or at .Trash-UID, is never followed.
        std::os::unix::fs::symlink(&elsewhere, top.join(".Trash")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, top.join(format!(".Trash-{uid}"))).unwrap();
        assert_eq!(top_trash(top, device, uid), None);
        assert_eq!(
            fs::read_dir(&elsewhere).unwrap().count(),
            0,
            "nothing made there"
        );
        // .Trash that isn't sticky isn't used; .Trash-UID is made.
        fs::remove_file(top.join(".Trash")).unwrap();
        fs::remove_file(top.join(format!(".Trash-{uid}"))).unwrap();
        fs::create_dir(top.join(".Trash")).unwrap();
        let own = top.join(format!(".Trash-{uid}"));
        assert_eq!(top_trash(top, device, uid), Some(own.clone()));
        assert!(own.join("files").is_dir() && own.join("info").is_dir());
        // A sticky .Trash is used, with a directory of the user's own.
        let shared = top.join(".Trash");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        assert_eq!(
            top_trash(top, device, uid),
            Some(shared.join(uid.to_string()))
        );
        // Someone else's isn't.
        assert_eq!(top_trash(top, device, uid + 1), None);
        // Nor a link inside it.
        let files = shared.join(uid.to_string()).join("files");
        fs::remove_dir(&files).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &files).unwrap();
        assert_eq!(top_trash(top, device, uid), None);
    }

    #[test]
    fn what_is_trashed_goes_to_the_home_trash_with_its_info_under_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("Trash");
        let file = dir.path().join("a b.txt");
        for text in ["first", "second"] {
            fs::write(&file, text).unwrap();
            put(&file, Some(&home)).unwrap();
            assert!(!file.exists());
        }
        let trashed = |name: &str| fs::read_to_string(home.join("files").join(name)).unwrap();
        assert_eq!(
            (trashed("a b.txt").as_str(), trashed("a b.txt.2").as_str()),
            ("first", "second")
        );
        let info = fs::read_to_string(home.join("info/a b.txt.2.trashinfo")).unwrap();
        let path = encoded(&file);
        assert!(
            info.starts_with(&format!("[Trash Info]\nPath={path}\nDeletionDate=")),
            "{info}"
        );
        let mode = fs::metadata(&home).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(put(&file, Some(&home)).is_err(), "nothing there");
    }
}
