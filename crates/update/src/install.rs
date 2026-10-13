//! Putting a new build in place of the running one, and back.
//!
//! The new build is written beside the old, in the same directory, so
//! renaming it over the old is atomic: the path always holds a whole
//! program, old or new, never half of one and never nothing. The old one
//! is kept beside it as `clackatoo.previous`, for `--rollback`.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Version};

/// What's added to a program's name for the build it replaced.
const PREVIOUS: &str = ".previous";

/// The path of the build `program` replaced: `clackatoo.previous`.
#[must_use]
pub fn previous(program: &Path) -> PathBuf {
    with_suffix(program, PREVIOUS)
}

/// The program to update, from the path of the one running: itself, or if
/// it's the build an update replaced, the one that replaced it.
#[must_use]
pub fn installed(running: &Path) -> PathBuf {
    match running.file_name().and_then(|name| name.to_str()) {
        Some(name) if name.len() > PREVIOUS.len() && name.ends_with(PREVIOUS) => {
            running.with_file_name(&name[..name.len() - PREVIOUS.len()])
        }
        _ => running.to_owned(),
    }
}

/// Installs `program`, a build of clackatoo `version`, at `target`,
/// keeping what was there as [`previous`]. First runs it, to be sure it's
/// a program this machine can run, and that it's the version it should be.
///
/// # Errors
///
/// If the directory can't be written to, or the new build won't run or
/// says it's another version. Nothing at `target` changes then.
pub fn install(target: &Path, program: &[u8], version: Version) -> Result<(), Error> {
    let staged = Staged::write(target, program)?;
    check_runs(&staged.path, version)?;
    if target.exists() {
        // A link, renamed over the last previous build, so there's always
        // one.
        let link = sibling(target, ".clackatoo-previous");
        let _ = fs::remove_file(&link);
        fs::hard_link(target, &link).map_err(|err| Error::Write(link.clone(), err))?;
        let kept = previous(target);
        fs::rename(&link, &kept).map_err(|err| {
            let _ = fs::remove_file(&link);
            Error::Write(kept, err)
        })?;
    }
    staged.put_at(target)
}

/// Swaps the build at `target` with the one it replaced, so running
/// `--rollback` again goes forward again.
///
/// # Errors
///
/// [`Error::NoPrevious`] if there's no previous build, or if they can't be
/// swapped.
pub fn rollback(target: &Path) -> Result<(), Error> {
    let kept = previous(target);
    if !kept.exists() {
        return Err(Error::NoPrevious(kept));
    }
    let swapped = rustix::fs::renameat_with(
        rustix::fs::CWD,
        &kept,
        rustix::fs::CWD,
        target,
        rustix::fs::RenameFlags::EXCHANGE,
    );
    match swapped {
        Ok(()) => sync_dir(target),
        // A file system that can't swap two files at once.
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS | rustix::io::Errno::OPNOTSUPP) => {
            swap_by_renames(target, &kept)
        }
        Err(err) => Err(Error::Write(target.to_owned(), err.into())),
    }
}

/// Swaps `target` and `kept` in three steps, with `target` never missing:
/// a link to it is made, `kept` is renamed over it, then the link over
/// `kept`.
fn swap_by_renames(target: &Path, kept: &Path) -> Result<(), Error> {
    let link = sibling(target, ".clackatoo-rollback");
    let _ = fs::remove_file(&link);
    fs::hard_link(target, &link).map_err(|err| Error::Write(link.clone(), err))?;
    fs::rename(kept, target).map_err(|err| {
        let _ = fs::remove_file(&link);
        Error::Write(target.to_owned(), err)
    })?;
    fs::rename(&link, kept).map_err(|err| Error::Write(kept.to_owned(), err))?;
    sync_dir(target)
}

/// Runs the program at `path` with `--version`, and checks it says it's
/// `version`. A program built for another machine, or against a newer C
/// library than this one has, fails here, before it replaces anything.
fn check_runs(path: &Path, version: Version) -> Result<(), Error> {
    let mut tries = 0;
    let output = loop {
        match Command::new(path).arg("--version").output() {
            // Another thread forked while the file was open for writing,
            // and the child hasn't exec'd yet: it will in a moment.
            Err(err)
                if err.raw_os_error() == Some(rustix::io::Errno::TXTBSY.raw_os_error())
                    && tries < 50 =>
            {
                tries += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => break result.map_err(|err| Error::WontRun(err.to_string()))?,
        }
    };
    let said = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::WontRun(stderr.trim().to_owned()));
    }
    // `clackatoo 2026.10.0 (hash date)`.
    let says = said
        .split_whitespace()
        .nth(1)
        .and_then(|word| word.parse().ok());
    if says == Some(version) {
        Ok(())
    } else {
        Err(Error::OtherVersion {
            expected: version,
            said: said.trim().to_owned(),
        })
    }
}

/// A new build, written beside the one it will replace. Deleted if it's
/// dropped before it's put in place.
struct Staged {
    path: PathBuf,
}

impl Staged {
    fn write(target: &Path, program: &[u8]) -> Result<Self, Error> {
        let path = sibling(target, &format!(".clackatoo-update-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(&path)
            .map_err(|err| Error::Write(path.clone(), err))?;
        let staged = Self { path };
        (file.write_all(program).and_then(|()| file.sync_all()))
            .map_err(|err| Error::Write(staged.path.clone(), err))?;
        Ok(staged)
    }

    /// Renames the build over `target`.
    fn put_at(self, target: &Path) -> Result<(), Error> {
        fs::rename(&self.path, target).map_err(|err| Error::Write(target.to_owned(), err))?;
        std::mem::forget(self);
        sync_dir(target)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Makes renames in `path`'s directory last through a crash.
fn sync_dir(path: &Path) -> Result<(), Error> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    (File::open(dir).and_then(|dir| dir.sync_all())).map_err(|err| Error::Write(dir.into(), err))
}

/// A file called `name` beside `path`.
fn sibling(path: &Path, name: &str) -> PathBuf {
    path.with_file_name(name)
}

/// `path` with `suffix` after its name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(OsString::from).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_previous_build_is_beside_the_installed_one() {
        let program = Path::new("/opt/bin/clackatoo");
        assert_eq!(previous(program), Path::new("/opt/bin/clackatoo.previous"));
        assert_eq!(installed(program), program);
        assert_eq!(installed(&previous(program)), program);
        assert_eq!(
            installed(Path::new("/x/.previous")),
            Path::new("/x/.previous")
        );
    }
}
