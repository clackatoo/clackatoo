//! The folders trusted to run their language servers (CLACKA-224).
//!
//! A language server runs the code of the project it's started for:
//! rust-analyzer builds and runs build scripts and proc-macros, tsserver
//! loads plugins from `node_modules`, gopls runs the `go` command, which
//! may fetch and run another toolchain. So none is started for a folder
//! until the user has said it may be, by a key pressed in a window or with
//! the `clackatoo` command. A folder is trusted with everything inside it,
//! but for a folder inside it marked not trusted: the nearest folder
//! marked decides. A folder marked neither is asked about.
//!
//! They're kept in a file of their own, `trusted`, in clackatoo's state
//! directory, as JSON: `{"clackatoo_trusted": 1, "folders": [{"folder":
//! "/home/jack/projects", "trusted": true}]}`. Written as `known_machines`
//! is: in full beside it, in a file made afresh for the user alone,
//! synced, and renamed over it, on a thread of its own, the newest last.
//! One that can't be read is set aside as `trusted.unreadable`, and no
//! folder is trusted: each is asked about again. A folder whose name isn't
//! UTF-8 is trusted for this run only.
//!
//! The file is written as soon as a build that keeps it starts, so its
//! absence says the build before kept none: from such a build, the folders
//! of the files open as it handed over, whose servers it ran, are trusted
//! (see [`Trust::fresh`]).

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

/// The file's version.
const VERSION: u32 = 1;

/// The most bytes the file is read at: far more than anyone marks.
const MAX_FILE: u64 = 1024 * 1024;

/// Where the folders trusted are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trusting {
    /// In this file.
    File(PathBuf),
    /// For this run only: each folder is asked about in each run.
    ThisRun,
    /// Nowhere, as every folder is trusted and none asked about: for
    /// tests of what isn't about trust.
    All,
}

/// The folders marked trusted or not, and where they're kept.
#[derive(Debug)]
pub(crate) struct Trust {
    /// The file, if they're kept in one.
    file: Option<PathBuf>,
    /// Every folder trusted, none asked about.
    all: bool,
    folders: BTreeMap<PathBuf, bool>,
    /// The file wasn't there as the daemon started.
    fresh: bool,
    /// It's to be written as the daemon starts: it wasn't there, or was
    /// set aside.
    unwritten: bool,
    /// Counts the changes, so a write of what's older than what's been
    /// written already is passed over.
    changes: u64,
    /// The change last written, held while the file is written, so writes
    /// never cross.
    written: Arc<Mutex<u64>>,
    /// The threads writing, so [`kept`](Self::kept) can wait for them.
    writers: Vec<std::thread::JoinHandle<()>>,
}

/// The file, as it's written and read.
#[derive(Serialize, Deserialize)]
struct Kept {
    clackatoo_trusted: u32,
    #[serde(default)]
    folders: Vec<KeptFolder>,
}

#[derive(Serialize, Deserialize)]
struct KeptFolder {
    folder: String,
    trusted: bool,
}

impl Trust {
    /// The folders kept as `trusting` says.
    pub(crate) fn load(trusting: &Trusting) -> Self {
        let (file, all) = match trusting {
            Trusting::File(file) => (Some(file.clone()), false),
            Trusting::ThisRun => (None, false),
            Trusting::All => (None, true),
        };
        let (folders, fresh, unwritten) = match &file {
            Some(file) => read(file),
            None => (BTreeMap::new(), !all, false),
        };
        Self {
            file,
            all,
            folders,
            fresh,
            unwritten,
            changes: 0,
            written: Arc::default(),
            writers: Vec::new(),
        }
    }

    /// Whether the folder `root` is trusted to run its language servers:
    /// `None` if neither it nor any folder it's in is marked, so the user
    /// is asked.
    pub(crate) fn trusts(&self, root: &Path) -> Option<bool> {
        if self.all {
            return Some(true);
        }
        (root.ancestors()).find_map(|folder| self.folders.get(folder).copied())
    }

    /// Marks `folder` trusted or not, as `trusted` says, keeping it, and
    /// returns whether that changed anything.
    pub(crate) fn mark(&mut self, folder: PathBuf, trusted: bool) -> bool {
        if self.folders.insert(folder, trusted) == Some(trusted) {
            return false;
        }
        self.keep();
        true
    }

    /// Every folder marked, in order, and whether it's trusted.
    pub(crate) fn marked(&self) -> impl Iterator<Item = (&Path, bool)> {
        (self.folders.iter()).map(|(folder, &trusted)| (folder.as_path(), trusted))
    }

    /// Whether every folder is trusted, so none is asked about.
    pub(crate) fn trusts_all(&self) -> bool {
        self.all
    }

    /// Whether the file wasn't there as the daemon started, so the build
    /// before kept no folders: [`started`](Self::started) ends that.
    pub(crate) fn fresh(&self) -> bool {
        self.fresh
    }

    /// The daemon has started, and trusted what it carried on from: the
    /// file's written, if it wasn't there, so the next build knows this
    /// one kept it, even with nothing marked.
    pub(crate) fn started(&mut self) {
        self.fresh = false;
        if std::mem::take(&mut self.unwritten) {
            self.keep();
        }
    }

    /// Writes the folders to the file, on a thread of its own.
    fn keep(&mut self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        self.changes += 1;
        let change = self.changes;
        let folders = self.folders.clone();
        let written = Arc::clone(&self.written);
        let spawned = std::thread::Builder::new()
            .name("trusted-folders".into())
            .spawn(move || {
                let mut written = written.lock().unwrap_or_else(PoisonError::into_inner);
                // A newer write got there first.
                if *written >= change {
                    return;
                }
                match write(&file, &folders) {
                    Ok(()) => *written = change,
                    Err(err) => log::error!("{} couldn't be written: {err}", file.display()),
                }
            });
        match spawned {
            Ok(writer) => {
                self.writers.retain(|writer| !writer.is_finished());
                self.writers.push(writer);
            }
            Err(err) => log::error!("there's no thread to keep the folders trusted on: {err}"),
        }
    }

    /// Waits for what's being written to the file to be written, as the
    /// daemon stops or upgrades.
    pub(crate) fn kept(&mut self) {
        for writer in std::mem::take(&mut self.writers) {
            let _ = writer.join();
        }
    }
}

/// The folders `file` keeps, whether it wasn't there, and whether it's to
/// be written as the daemon starts: none, with the file set aside, if it
/// can't be read as one.
fn read(file: &Path) -> (BTreeMap<PathBuf, bool>, bool, bool) {
    let mut bytes = Vec::new();
    match File::open(file) {
        Ok(opened) => {
            if let Err(err) = opened.take(MAX_FILE).read_to_end(&mut bytes) {
                log::error!("{} couldn't be read: {err}", file.display());
                return (BTreeMap::new(), false, false);
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return (BTreeMap::new(), true, true);
        }
        Err(err) => {
            log::error!("{} couldn't be read: {err}", file.display());
            return (BTreeMap::new(), false, false);
        }
    }
    let kept = match serde_json::from_slice::<Kept>(&bytes) {
        Ok(kept) if kept.clackatoo_trusted != 0 => kept,
        Ok(_) | Err(_) => {
            log::error!(
                "{} can't be read: it's set aside, and each folder is asked about again",
                file.display()
            );
            let mut aside = file.as_os_str().to_owned();
            aside.push(".unreadable");
            let _ = fs::rename(file, aside);
            return (BTreeMap::new(), false, true);
        }
    };
    let folders = (kept.folders.into_iter())
        .map(|kept| (PathBuf::from(kept.folder), kept.trusted))
        // Only a folder named from the root means one folder.
        .filter(|(folder, _)| folder.is_absolute())
        .collect();
    (folders, false, false)
}

/// Writes `folders` as the file at `path`: in full beside it, in a file
/// made afresh for the user alone, never through a link, and synced; then
/// renamed over it, and the directory synced.
fn write(path: &Path, folders: &BTreeMap<PathBuf, bool>) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".new-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    match fs::remove_file(&temporary) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let kept = Kept {
        clackatoo_trusted: VERSION,
        folders: (folders.iter())
            .filter_map(|(folder, &trusted)| {
                Some(KeptFolder {
                    folder: folder.to_str()?.to_owned(),
                    trusted,
                })
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec_pretty(&kept).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let written = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    let _ = fs::remove_file(&temporary);
    written?;
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nearest_folder_marked_decides() {
        let mut trust = Trust::load(&Trusting::ThisRun);
        assert!(trust.fresh(), "a run keeping nothing kept nothing before");
        let projects = Path::new("/home/u/projects");
        assert_eq!(trust.trusts(&projects.join("a")), None);
        assert!(trust.mark(projects.to_owned(), true));
        assert!(!trust.mark(projects.to_owned(), true), "nothing new");
        assert_eq!(trust.trusts(&projects.join("a/b")), Some(true));
        assert_eq!(trust.trusts(projects), Some(true));
        assert_eq!(trust.trusts(Path::new("/home/u/other")), None);
        assert!(trust.mark(projects.join("a"), false));
        assert_eq!(trust.trusts(&projects.join("a/b")), Some(false));
        assert_eq!(trust.trusts(&projects.join("ab")), Some(true));
        assert!(Trust::load(&Trusting::All).trusts(projects) == Some(true));
    }

    #[test]
    fn folders_are_kept_for_the_user_alone_and_read_again() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/trusted");
        let trusting = Trusting::File(file.clone());
        let mut trust = Trust::load(&trusting);
        assert!(trust.fresh());
        trust.started();
        trust.kept();
        assert!(!trust.fresh());
        let read = Trust::load(&trusting);
        assert!(!read.fresh(), "written as it started, with nothing marked");
        assert_eq!(read.marked().count(), 0);
        assert!(trust.mark("/src/good".into(), true));
        assert!(trust.mark("/src/good/bad".into(), false));
        trust.kept();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let read = Trust::load(&trusting);
        let marked: Vec<_> = read.marked().collect();
        assert_eq!(
            marked,
            [
                (Path::new("/src/good"), true),
                (Path::new("/src/good/bad"), false)
            ]
        );
        // One that can't be read is set aside, and nothing is trusted.
        fs::write(&file, "not json").unwrap();
        let read = Trust::load(&trusting);
        assert_eq!(read.trusts(Path::new("/src/good")), None);
        assert!(!read.fresh(), "it was there");
        assert!(dir.path().join("state/trusted.unreadable").exists());
        // And written afresh as the daemon starts.
        let mut read = read;
        read.started();
        read.kept();
        assert!(!Trust::load(&trusting).fresh());
    }
}
