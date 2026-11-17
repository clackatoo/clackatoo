//! Bounded directory watches. Missing watches and overflow require a full
//! metadata reconciliation; they never silently claim complete live coverage.
//! A folder that came, went or moved needs only what's under it walked
//! again.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

/// The most folders changed since the last scan that are each walked
/// again: past that, the whole project is.
const MOST_DIRS: usize = 256;

/// What has changed in a project since its index was brought up to date.
#[derive(Debug, Default)]
pub(super) struct Dirty {
    /// Anything may have: events were lost, or ignore rules changed.
    pub all: bool,
    /// The watch has more to read.
    pub more: bool,
    /// Files that changed, as the project names them.
    pub paths: BTreeSet<PathBuf>,
    /// Folders that came, went or moved: what's under each may have
    /// changed.
    pub dirs: BTreeSet<PathBuf>,
}

impl Dirty {
    /// Notes that what's under the folder `dir` may have changed: the
    /// whole project, if it's the root or too many folders have.
    pub(super) fn under(&mut self, dir: PathBuf) {
        if dir.as_os_str().is_empty() || self.dirs.len() >= MOST_DIRS {
            self.all = true;
        } else {
            self.dirs.insert(dir);
        }
    }
}

/// The folders watched: by the kernel's number for each watch, and by
/// path.
#[derive(Debug, Default)]
struct Dirs {
    ids: BTreeMap<i32, PathBuf>,
    paths: HashSet<PathBuf>,
    /// Watches let go of here, whose last events may still be queued: the
    /// kernel ends each with one saying it's gone. What they report was
    /// accounted for when they were let go.
    forgotten: HashSet<i32>,
}

impl Dirs {
    /// Stops watching the folder `dir` and every folder under it: it was
    /// deleted or moved away, and a watch follows the folder wherever it
    /// went. Were its path left here, a folder made in its place later
    /// would pass for watched.
    fn forget_under(&mut self, fd: &OwnedFd, dir: &Path) {
        let gone: Vec<i32> = (self.ids.iter())
            .filter(|(_, path)| path.starts_with(dir))
            .map(|(&id, _)| id)
            .collect();
        for id in gone {
            if let Some(path) = self.ids.remove(&id) {
                self.paths.remove(&path);
            }
            // Fails if the kernel dropped it already, with the folder: its
            // last word is queued either way.
            let _ = inotify::remove_watch(fd, id);
            self.forgotten.insert(id);
        }
    }
}

/// One project's inotify instance, and what it watches.
#[derive(Debug)]
pub(crate) struct Watch {
    pub fd: OwnedFd,
    dirs: Mutex<Dirs>,
}

impl Watch {
    /// A watch of nothing yet.
    ///
    /// # Errors
    /// If the user has no inotify instances left.
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self {
            fd: inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?,
            dirs: Mutex::new(Dirs::default()),
        })
    }

    /// Watches the folder `path` under `root`, if it isn't watched
    /// already. Returns whether it's watched now: not past 8,192 folders,
    /// or if the kernel refuses.
    pub(super) fn add(&self, root: &Path, path: &Path) -> bool {
        let mut dirs = self
            .dirs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if dirs.paths.contains(path) {
            return true;
        }
        if dirs.ids.len() >= 8192 {
            return false;
        }
        let flags = WatchFlags::CLOSE_WRITE
            | WatchFlags::CREATE
            | WatchFlags::DELETE
            | WatchFlags::MOVED_FROM
            | WatchFlags::MOVED_TO
            | WatchFlags::DELETE_SELF
            | WatchFlags::MOVE_SELF
            | WatchFlags::ONLYDIR
            | WatchFlags::DONT_FOLLOW;
        match inotify::add_watch(&self.fd, root.join(path), flags) {
            Ok(wd) => {
                if let Some(old) = dirs.ids.insert(wd, path.to_owned()) {
                    dirs.paths.remove(&old);
                }
                dirs.paths.insert(path.to_owned());
                true
            }
            Err(_) => false,
        }
    }

    /// Whether the project's root itself is watched. If it isn't (it was
    /// deleted, or couldn't be watched), nothing under it is heard of.
    pub(super) fn watches_root(&self) -> bool {
        let dirs = self
            .dirs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dirs.paths.contains(Path::new(""))
    }

    /// Reads what the kernel has reported, up to 4,096 events, as what
    /// changed. Anything it can't place, or more than it read, is reported
    /// as everything having changed.
    pub(super) fn read(&self) -> Dirty {
        let mut result = Dirty::default();
        let mut bytes = [MaybeUninit::uninit(); 16384];
        let mut reader = inotify::Reader::new(&self.fd, &mut bytes);
        let mut dirs = self
            .dirs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut exhausted = true;
        for _ in 0..4096 {
            let event = match reader.next() {
                Ok(event) => event,
                Err(rustix::io::Errno::INTR) => continue,
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    exhausted = false;
                    break;
                }
                Err(_) => {
                    exhausted = false;
                    result.all = true;
                    break;
                }
            };
            let flags = event.events();
            if flags.contains(ReadFlags::IGNORED) {
                // The folder went, and its watch with it. One let go of
                // here was accounted for then, and has no more to say.
                if dirs.forgotten.remove(&event.wd()) {
                    continue;
                }
                if let Some(path) = dirs.ids.remove(&event.wd()) {
                    dirs.paths.remove(&path);
                    result.under(path);
                }
                continue;
            }
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                result.all = true;
                continue;
            }
            if dirs.forgotten.contains(&event.wd()) {
                // From a folder that left: a rename tells its old parent,
                // then the folder itself, and what's under it is already
                // to be walked again.
                continue;
            }
            let Some(dir) = dirs.ids.get(&event.wd()).cloned() else {
                result.all = true;
                continue;
            };
            let Some(name) = event.file_name() else {
                // The watched folder itself was deleted or moved.
                dirs.forget_under(&self.fd, &dir);
                result.under(dir);
                continue;
            };
            let path = {
                use std::os::unix::ffi::OsStrExt as _;
                dir.join(std::ffi::OsStr::from_bytes(name.to_bytes()))
            };
            if matches!(name.to_bytes(), b".gitignore" | b".ignore" | b"exclude") {
                // What's left out may have changed anywhere below.
                result.all = true;
            } else if flags.contains(ReadFlags::ISDIR) {
                if flags.intersects(ReadFlags::MOVED_FROM | ReadFlags::DELETE) {
                    dirs.forget_under(&self.fd, &path);
                }
                result.under(path);
            } else if result.paths.len() < 4096 {
                result.paths.insert(path);
            } else {
                result.all = true;
            }
        }
        result.more = exhausted;
        result.all |= exhausted; // A reader buffer may contain events not yet visited.
        result
    }
}
