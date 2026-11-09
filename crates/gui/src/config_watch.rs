//! The window's config, watched as the windows run: `client/settings.lua`,
//! `client/init.lua` and the modules they require, which are run again,
//! as `:reload` runs them, a while after one changes, so the windows go by
//! what the files say without being told to. One thread for the process
//! of windows watches them, with inotify, on an instance of its own, never
//! the windows' event loop. A user has few instances: if one can't be
//! made, nothing's watched, which is logged once, and `:reload` still runs
//! the config again.
//!
//! What a window wrote itself, as the settings menu and `:setpersist`
//! save `client/settings.lua`, is no change: each file's text is
//! remembered, as the config last read it or a window last wrote it, and
//! only a file whose text differs from that is a change. A module the
//! config hasn't required is no change of its own, until the config that
//! requires it is.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;

/// How long after a file last changed the config is run again: an editor
/// saving, or a checkout, changes several at once.
const GATHER: Duration = Duration::from_millis(250);

/// How often the folders watched are looked at again, for those the
/// config's modules have come to be in.
const LOOK_AGAIN: Duration = Duration::from_secs(1);

/// What's heard of in a folder watched: a file written, or written by
/// renaming a new one over it, as editors do, made, or taken away.
const HEAR: WatchFlags = WatchFlags::CLOSE_WRITE
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::ONLYDIR);

/// Each file of the config's text, by its path, as a hash: as the config
/// last read it, or a window last wrote it; `None` for one that wasn't
/// there.
fn known() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Option<u64>>> {
    static KNOWN: OnceLock<Mutex<HashMap<PathBuf, Option<u64>>>> = OnceLock::new();
    (KNOWN.get_or_init(Mutex::default).lock()).unwrap_or_else(PoisonError::into_inner)
}

fn hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Notes that the config read the file at `path` as `text`, or found
/// nothing there.
pub(crate) fn read(path: &Path, text: Option<&str>) {
    known().insert(path.to_owned(), text.map(hash));
}

/// Notes that a window wrote `text` to the file at `path`, over `before`,
/// which is no change of the config's: the windows go by it already. But
/// if `before` wasn't what the config read, what changed it meanwhile,
/// which the write kept, is still a change.
pub(crate) fn wrote(path: &Path, before: Option<&str>, text: &str) {
    let mut known = known();
    if (known.get(path)).is_none_or(|was| *was == before.map(hash)) {
        known.insert(path.to_owned(), Some(hash(text)));
    }
}

/// Whether the file at `path` says something other than what the config
/// last read, or a window last wrote. One in `client` the config has never
/// read is, once it's there: `init.lua` made.
fn changed(path: &Path, client: &Path) -> bool {
    // Read as the config reads it: never from a FIFO, which would hold the
    // thread up.
    let now = match clackatoo_daemon::settings::read_text(path) {
        Ok(text) => text.as_deref().map(hash),
        // One that can't be read is run again, to say why.
        Err(_) => Some(0),
    };
    match known().get(path) {
        Some(was) => *was != now,
        None => path.parent() == Some(client) && now.is_some(),
    }
}

/// Watches the config in `dir`, calling `wake` once it's changed, a while
/// after its files last did. The watching is done on a thread of its own,
/// one for the process, whose first config it watches: inotify instances
/// are few.
pub(crate) fn watch(dir: PathBuf, wake: Box<dyn Fn() + Send>) {
    static WATCHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if WATCHING.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || run(&dir, &*wake));
    if let Err(err) = spawned {
        log::warn!("the config won't be watched, so :reload runs it again: {err}");
    }
}

/// The folders to watch for the config in `dir`: `dir` itself, for its
/// folders made, the config's own, and those its modules are in.
fn folders(dir: &Path) -> HashSet<PathBuf> {
    let mut folders: HashSet<PathBuf> = ["", "client", "client/lua", "lua"]
        .iter()
        .map(|folder| dir.join(folder))
        .collect();
    folders.extend(
        (known().keys())
            .filter(|path| path.starts_with(dir))
            .filter_map(|path| path.parent().map(Path::to_owned)),
    );
    folders
}

fn run(dir: &Path, wake: &dyn Fn()) {
    let flags = CreateFlags::CLOEXEC | CreateFlags::NONBLOCK;
    let instance = match inotify::init(flags) {
        Ok(instance) => instance,
        Err(err) => {
            log::warn!("the config won't be watched, so :reload runs it again: {err}");
            return;
        }
    };
    let client = dir.join("client");
    // The folders watched, by their watch.
    let mut watched: HashMap<i32, PathBuf> = HashMap::new();
    let mut looked = None;
    let mut due: Option<Instant> = None;
    let mut touched: HashSet<PathBuf> = HashSet::new();
    let mut buffer = [MaybeUninit::uninit(); 4096];
    loop {
        let now = Instant::now();
        if looked.is_none_or(|looked: Instant| now >= looked + LOOK_AGAIN) {
            looked = Some(now);
            for folder in folders(dir) {
                if watched.values().any(|each| *each == folder) {
                    continue;
                }
                // One that isn't there yet is watched once it's made.
                if let Ok(watch) = inotify::add_watch(&instance, &folder, HEAR) {
                    watched.insert(watch, folder);
                }
            }
        }
        let wait = due.map_or(LOOK_AGAIN, |due| due.saturating_duration_since(now));
        let timeout = Timespec::try_from(wait).ok();
        let mut fds = [PollFd::new(&instance, PollFlags::IN)];
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(err) => {
                log::warn!("watching the config stopped: {err}");
                return;
            }
        }
        let mut reader = inotify::Reader::new(&instance, &mut buffer);
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::INTR) => continue,
                Err(_) => break,
            };
            if event.events().contains(ReadFlags::IGNORED) {
                watched.remove(&event.wd());
                continue;
            }
            // Changes were lost: any file may have changed.
            if event.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                touched.extend(known().keys().cloned());
                due = Some(Instant::now() + GATHER);
                continue;
            }
            let (Some(folder), Some(name)) = (watched.get(&event.wd()), event.file_name()) else {
                continue;
            };
            let name = name.to_string_lossy();
            if name.ends_with(".lua") {
                touched.insert(folder.join(&*name));
                due = Some(Instant::now() + GATHER);
            } else {
                // A folder made, as `client/` or `lua/`: watched at once.
                looked = None;
            }
        }
        if due.is_some_and(|due| due <= Instant::now()) {
            due = None;
            if touched.drain().any(|path| changed(&path, &client)) {
                wake();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the config read, or a window wrote, is no change; what the
    /// files say otherwise is, and `init.lua` made is.
    #[test]
    fn only_what_differs_from_what_was_read_or_written_is_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let client = dir.path().join("client");
        std::fs::create_dir_all(&client).unwrap();
        let settings = client.join("settings.lua");
        let init = client.join("init.lua");
        let module = dir.path().join("lua/mine.lua");
        std::fs::write(&settings, "client.opt.number = false\n").unwrap();
        read(&settings, Some("client.opt.number = false\n"));
        read(&init, None);
        assert!(!changed(&settings, &client));
        assert!(!changed(&init, &client));
        // A window saves: no change.
        std::fs::write(&settings, "client.opt.number = true\n").unwrap();
        let before = Some("client.opt.number = false\n");
        wrote(&settings, before, "client.opt.number = true\n");
        assert!(!changed(&settings, &client));
        // Someone else edits it, or makes init.lua: a change, even once a
        // window has saved over it, keeping it.
        std::fs::write(&settings, "client.opt.wrap = false\n").unwrap();
        assert!(changed(&settings, &client));
        let before = Some("client.opt.wrap = false\n");
        let kept = "client.opt.wrap = false\nclient.opt.number = true\n";
        std::fs::write(&settings, kept).unwrap();
        wrote(&settings, before, kept);
        assert!(changed(&settings, &client));
        std::fs::write(&init, "client.opt.wrap = true\n").unwrap();
        assert!(changed(&init, &client));
        // A module the config never required isn't, until it is.
        std::fs::create_dir_all(module.parent().unwrap()).unwrap();
        std::fs::write(&module, "return 1\n").unwrap();
        assert!(!changed(&module, &client));
        read(&module, Some("return 1\n"));
        std::fs::write(&module, "return 2\n").unwrap();
        assert!(changed(&module, &client));
        assert!(folders(dir.path()).contains(&dir.path().join("lua")));
    }

    /// The thread wakes the windows once init.lua is edited, a while
    /// after, but not for what a window saved.
    #[test]
    fn an_edit_wakes_the_windows_and_a_windows_own_save_doesnt() {
        let dir = tempfile::tempdir().unwrap();
        let client = dir.path().join("client");
        std::fs::create_dir_all(&client).unwrap();
        let (init, settings) = (client.join("init.lua"), client.join("settings.lua"));
        std::fs::write(&init, "client.opt.wrap = true\n").unwrap();
        read(&init, Some("client.opt.wrap = true\n"));
        read(&settings, None);
        let (send, woken) = std::sync::mpsc::channel();
        let watched = dir.path().to_owned();
        // The thread's own, past the one the process watches with.
        std::thread::spawn(move || {
            let wake = move || {
                let _ = send.send(());
            };
            run(&watched, &wake);
        });
        // Its folders watched.
        std::thread::sleep(Duration::from_millis(300));
        let saved = "client.opt.number = false\n";
        std::fs::write(&settings, saved).unwrap();
        wrote(&settings, None, saved);
        assert!(woken.recv_timeout(Duration::from_secs(1)).is_err());
        std::fs::write(&init, "client.opt.wrap = false\n").unwrap();
        assert!(woken.recv_timeout(Duration::from_secs(10)).is_ok());
    }
}
