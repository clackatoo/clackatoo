//! Bounded directory watches. Missing watches and overflow require a full
//! metadata reconciliation; they never silently claim complete live coverage.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

#[derive(Debug, Default)]
pub(super) struct Dirty {
    pub all: bool,
    pub more: bool,
    pub paths: BTreeSet<PathBuf>,
}

#[derive(Debug, Default)]
struct Dirs {
    ids: BTreeMap<i32, PathBuf>,
    paths: HashSet<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct Watch {
    pub fd: OwnedFd,
    dirs: Mutex<Dirs>,
}

impl Watch {
    pub(crate) fn new() -> std::io::Result<Self> {
        Ok(Self {
            fd: inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)?,
            dirs: Mutex::new(Dirs::default()),
        })
    }

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
                if let Some(path) = dirs.ids.remove(&event.wd()) {
                    dirs.paths.remove(&path);
                }
                result.all = true;
                continue;
            }
            if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
                result.all = true;
                continue;
            }
            let Some(dir) = dirs.ids.get(&event.wd()) else {
                result.all = true;
                continue;
            };
            let Some(name) = event.file_name() else {
                result.all = true;
                continue;
            };
            if flags.contains(ReadFlags::ISDIR)
                || matches!(name.to_bytes(), b".gitignore" | b".ignore" | b"exclude")
            {
                result.all = true;
            } else if result.paths.len() < 4096 {
                use std::os::unix::ffi::OsStrExt as _;
                result
                    .paths
                    .insert(dir.join(std::ffi::OsStr::from_bytes(name.to_bytes())));
            } else {
                result.all = true;
            }
        }
        result.more = exhausted;
        result.all |= exhausted; // A reader buffer may contain events not yet visited.
        result
    }
}
