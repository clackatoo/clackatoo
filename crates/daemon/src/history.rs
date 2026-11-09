//! Terminals' history kept from one run of the daemon to the next: each
//! terminal's newest lines, with their colours and attributes, written to
//! a file of its own as they change, so they come back above a new shell
//! after the daemon restarts, or the machine does, even after a crash or
//! the daemon being killed (see `server/history.rs`). Written on a thread
//! of its own, as making the lines plain data, compressing and syncing
//! them all take far too long for the event loop.
//!
//! # The format
//!
//! Each terminal kept has a file, `<daemon>-<terminal>.history`, in the
//! history directory (`~/.local/state/clackatoo/history`), named by the
//! daemon's own number, which outlives upgrades, and the terminal's ID. It
//! starts with [`MAGIC`], then the format's version, [`VERSION`], as four
//! bytes, little-endian, then, compressed with deflate, a [`Body`] in
//! postcard: when it was written, and the lines, as
//! [`clackatoo_term::KeptLines`] keeps them.
//!
//! Like the handover formats, the format is frozen: the types here, and
//! the lines', are the file's, not the daemon's, and change only with a
//! new version, which a newer build reads beside this one.
//!
//! Each file is written whole beside its name, synced, and renamed over
//! it, and the directory synced, so a crash leaves the last one written or
//! the one before, never half of one. Files are for the user alone: the
//! directory is made 0700, and must be the user's own, and each file 0600.
//! Nothing in it is followed if it's a link, or read if it isn't a
//! regular file, or bigger than this build writes.
//!
//! Only the daemon keeping the workspaces keeps their terminals' history,
//! under the workspaces' lock (see `saved`), so two daemons never share
//! the folder.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use clackatoo_term::{Keeping, KeptCopy, KeptLines};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde::{Deserialize, Serialize};

/// The start of every history file, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo history\n";

/// The format's version.
pub(crate) const VERSION: u32 = 1;

/// What a history file's name ends with.
const EXTENSION: &str = "history";

/// The most bytes a history file can be, compressed: more than the most
/// this build writes ever compresses to.
const MOST_FILE: u64 = 16 * 1024 * 1024;

/// The most bytes a history file's body can be once inflated, so a file
/// that inflates without end is refused, and reading one at a start costs
/// no more than this: the most this build writes, [`MOST_WRITTEN`], and
/// room for its framing.
const MOST_INFLATED: u64 = 24 * 1024 * 1024;

/// The most bytes of lines this build writes to a file, framing included,
/// whatever it's asked to keep: always read back.
const MOST_WRITTEN: usize = 20 * 1024 * 1024;

/// What a history file holds, after its version.
#[derive(Debug, Serialize, Deserialize)]
struct Body {
    /// When it was written, in milliseconds since 1970.
    written: u64,
    lines: KeptLines,
}

/// The name of the history file of terminal `terminal` of the daemon
/// whose own number is `daemon`, less its extension.
pub(crate) fn key(daemon: u64, terminal: u32) -> String {
    format!("{daemon:016x}-{terminal}")
}

/// Whether `key` is one [`key`] makes: a file name, never a path.
pub(crate) fn is_key(key: &str) -> bool {
    key.split_once('-').is_some_and(|(daemon, terminal)| {
        daemon.len() == 16
            && daemon.bytes().all(|byte| byte.is_ascii_hexdigit())
            && !terminal.is_empty()
            && terminal.len() <= 10
            && terminal.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// The path of the history file `key` names, in `dir`.
fn path_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXTENSION}"))
}

/// The path a history file is written at before it's renamed into place:
/// hidden, and never taken for one.
fn temporary_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!(".{key}.tmp"))
}

/// `lines` as a history file written at `written`.
fn encode(lines: KeptLines, written: u64) -> Result<Vec<u8>, String> {
    let body = postcard::to_allocvec(&Body { written, lines }).map_err(|err| err.to_string())?;
    let mut file = Vec::with_capacity(MAGIC.len() + 4 + body.len() / 4);
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&VERSION.to_le_bytes());
    let mut deflate = DeflateEncoder::new(file, Compression::fast());
    deflate.write_all(&body).map_err(|err| err.to_string())?;
    deflate.finish().map_err(|err| err.to_string())
}

/// The lines a history file holds.
fn decode(bytes: &[u8]) -> Result<KeptLines, String> {
    let rest = bytes.strip_prefix(MAGIC).ok_or("it isn't a history file")?;
    let (version, rest) = rest.split_at_checked(4).ok_or("it's cut short")?;
    let version = u32::from_le_bytes(version.try_into().expect("four bytes"));
    if version != VERSION {
        return Err(format!(
            "it's in version {version}, which this build doesn't read"
        ));
    }
    let mut body = Vec::new();
    DeflateDecoder::new(rest)
        .take(MOST_INFLATED + 1)
        .read_to_end(&mut body)
        .map_err(|err| err.to_string())?;
    if body.len() as u64 > MOST_INFLATED {
        return Err("it's bigger than this build writes".into());
    }
    let body: Body = postcard::from_bytes(&body).map_err(|err| err.to_string())?;
    Ok(body.lines)
}

/// Whether `dir` is a folder history can be read from: a folder, not a
/// link to one, the user's own, and theirs alone, as the daemon makes it.
/// Anything else might hold what someone else put there.
pub(crate) fn readable_dir(dir: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(dir).map_err(|err| err.to_string())?;
    if !metadata.is_dir() {
        return Err(format!("{} isn't a folder", dir.display()));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!("{} is another user's", dir.display()));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!("others can get into {}", dir.display()));
    }
    Ok(())
}

/// Reads the history file `key` names, in `dir`: `None` if there's none.
/// Only from a folder [`readable_dir`] takes, and only a file of the
/// user's own.
pub(crate) fn read(dir: &Path, key: &str) -> Result<Option<KeptLines>, String> {
    if !is_key(key) {
        return Err(format!("{key:?} isn't the name of a history file"));
    }
    readable_dir(dir)?;
    let path = path_of(dir, key);
    let file = match crate::regular::open(&path, crate::regular::Links::Refuse) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };
    let metadata = file.metadata().map_err(|err| err.to_string())?;
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err("it's another user's".into());
    }
    let len = metadata.len();
    if len > MOST_FILE {
        return Err("it's bigger than this build writes".into());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MOST_FILE)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    decode(&bytes).map(Some)
}

/// Writes `bytes` as the history file `key` names, in `dir`: whole,
/// beside it, synced, then renamed over it, for the user alone, then the
/// directory synced.
fn write_one(dir: &Path, key: &str, bytes: &[u8]) -> Result<(), String> {
    let temporary = temporary_of(dir, key);
    let written = (|| {
        // Never through a link, or into a file someone else made there.
        match fs::remove_file(&temporary) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        fs::rename(&temporary, path_of(dir, key))?;
        File::open(dir)?.sync_all()
    })();
    written.map_err(|err| {
        let _ = fs::remove_file(&temporary);
        err.to_string()
    })
}

/// Removes the history file `key` names, in `dir`, if it's there.
fn remove_one(dir: &Path, key: &str) -> io::Result<()> {
    match fs::remove_file(path_of(dir, key)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Removes every history file in `dir` but those `keep` names, and any
/// left half written: those of terminals not made again.
fn sweep(dir: &Path, keep: &HashSet<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let stale = match name.strip_suffix(&format!(".{EXTENSION}")) {
            Some(key) => is_key(key) && !keep.contains(key),
            None => name
                .strip_prefix('.')
                .and_then(|name| name.strip_suffix(".tmp"))
                .is_some_and(is_key),
        };
        if stale {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// The time now, in milliseconds since 1970.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// What the thread is asked to do with a terminal's file.
pub(crate) enum Job {
    /// Writes the lines of `copy` that `keeping` says, or removes the
    /// file if none are kept.
    Write {
        copy: Box<KeptCopy>,
        keeping: Keeping,
    },
    /// Removes the file.
    Remove,
}

/// What waits for the thread, in the order it was asked.
enum Item {
    /// A terminal's file, by its key, whose job waits in [`Queue::jobs`].
    File(String),
    /// Removing every file but those named.
    Sweep(HashSet<String>),
}

/// What the thread has yet to do. Each terminal has one job waiting at
/// most: a newer one takes its place, where it waits, so a slow disk
/// holds no more than a copy of each terminal's lines.
#[derive(Default)]
struct Queue {
    order: VecDeque<Item>,
    /// The job waiting for each terminal's file, by its key.
    jobs: HashMap<String, Job>,
    /// logind's locks, let go once nothing waits and the thread is idle.
    locks: Vec<OwnedFd>,
    /// The daemon let the thread go: it ends once nothing waits.
    stopping: bool,
}

/// The queue, shared with the thread, which the daemon's loop holds only
/// to put a job in.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The thread writing history files.
pub(crate) struct Writer {
    shared: Arc<Shared>,
    /// Hears from the thread once it has done every job and ended.
    done: Receiver<()>,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Writer")
    }
}

impl Writer {
    /// Starts the thread, writing in `dir`, which is made for the user
    /// alone if it isn't there, no more than `bytes` of text a terminal.
    pub(crate) fn start(dir: PathBuf, bytes: usize) -> io::Result<Self> {
        crate::recovery::make_dir(&dir)?;
        let bytes = bytes.min(MOST_WRITTEN);
        let shared = Arc::new(Shared::default());
        let (finished, done) = mpsc::channel();
        let theirs = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("history".into())
            .spawn(move || {
                run(&dir, bytes, &theirs);
                let _ = finished.send(());
            })?;
        Ok(Self { shared, done })
    }

    /// Asks the thread to do `job` to the file `key` names: in place of
    /// the one waiting for it, if one is.
    pub(crate) fn send(&self, key: String, job: Job) {
        let mut queue = self.shared.queue();
        if queue.jobs.insert(key.clone(), job).is_none() {
            queue.order.push_back(Item::File(key));
        }
        drop(queue);
        self.shared.ready.notify_one();
    }

    /// Asks the thread to remove every file but those `keep` names, once
    /// it has done what it was asked before.
    pub(crate) fn sweep(&self, keep: HashSet<String>) {
        self.shared.queue().order.push_back(Item::Sweep(keep));
        self.shared.ready.notify_one();
    }

    /// Has the thread let go of `lock`, logind's, once nothing waits for
    /// it: whatever was asked before, and after, is on disk.
    pub(crate) fn let_go(&self, lock: OwnedFd) {
        self.shared.queue().locks.push(lock);
        self.shared.ready.notify_one();
    }

    /// How many terminals' files have a job waiting.
    #[cfg(test)]
    fn waiting(&self) -> usize {
        self.shared.queue().jobs.len()
    }

    /// Lets the thread finish what it was asked, waiting up to `wait` for
    /// it to. Says whether it did.
    pub(crate) fn stop(self, wait: Duration) -> bool {
        self.shared.queue().stopping = true;
        self.shared.ready.notify_one();
        self.done.recv_timeout(wait).is_ok()
    }
}

/// The thread: each job in turn, until the daemon lets it go.
fn run(dir: &Path, bytes: usize, shared: &Shared) {
    loop {
        let mut queue = shared.queue();
        let (key, job) = loop {
            match queue.order.pop_front() {
                Some(Item::File(key)) => {
                    let Some(job) = queue.jobs.remove(&key) else {
                        continue;
                    };
                    break (key, job);
                }
                Some(Item::Sweep(keep)) => {
                    drop(queue);
                    if let Err(err) = sweep(dir, &keep) {
                        log::warn!(
                            "terminals' history no longer needed couldn't be removed: {err}"
                        );
                    }
                    queue = shared.queue();
                }
                None => {
                    // Everything asked is on disk.
                    queue.locks.clear();
                    if queue.stopping {
                        return;
                    }
                    queue = shared
                        .ready
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        };
        drop(queue);
        let done = match job {
            Job::Write { copy, keeping } => {
                let lines = copy.lines(keeping, bytes);
                drop(copy);
                if lines.is_empty() {
                    remove_one(dir, &key).map_err(|err| err.to_string())
                } else {
                    encode(lines, now()).and_then(|file| write_one(dir, &key, &file))
                }
            }
            Job::Remove => remove_one(dir, &key).map_err(|err| err.to_string()),
        };
        if let Err(err) = done {
            log::warn!("a terminal's history couldn't be kept or removed ({key}): {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{Config, Size, Terminal};

    use super::*;

    fn copy(output: &str) -> Box<KeptCopy> {
        let mut terminal = Terminal::new(Size::new(20, 3).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        Box::new(terminal.copy_to_keep(100))
    }

    #[test]
    fn keys_name_files_and_nothing_else() {
        let key = key(0xabc, 7);
        assert_eq!(key, "0000000000000abc-7");
        assert!(is_key(&key));
        for bad in [
            "../x-1",
            "0000000000000abc-",
            "0000000000000abc-1/2",
            "abc-1",
            "",
        ] {
            assert!(!is_key(bad), "{bad}");
        }
    }

    #[test]
    fn files_are_written_whole_for_the_user_alone_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().join("history");
        let writer = Writer::start(dir.clone(), usize::MAX).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let key = key(1, 2);
        writer.send(
            key.clone(),
            Job::Write {
                copy: copy("one\r\n\x1b[32mtwo\x1b[m\r\nthree"),
                keeping: Keeping::default(),
            },
        );
        assert!(writer.stop(Duration::from_secs(10)));
        let path = path_of(&dir, &key);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let lines = read(&dir, &key).unwrap().unwrap();
        assert_eq!(lines.texts(), ["one", "two", "three"]);
        // Nothing half written is left beside it.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        // Damaged, or not one at all, it isn't read.
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() / 2);
        fs::write(&path, &bytes).unwrap();
        assert!(read(&dir, &key).is_err());
        fs::write(&path, b"hello").unwrap();
        assert!(read(&dir, &key).is_err());
        assert_eq!(read(&dir, &self::key(1, 3)), Ok(None));
    }

    /// Lines a program said never to write to disk aren't, in any form:
    /// a placeholder stands for them.
    #[test]
    fn lines_never_to_be_written_never_are() {
        let dir = tempfile::tempdir().unwrap();
        let mut terminal = Terminal::new(Size::new(20, 3).unwrap(), &Config::default());
        terminal.advance(b"user\r\nhunter2\r\ndone\r\n");
        terminal.add_hints(1..2, clackatoo_term::Hints::NOSAVE);
        let writer = Writer::start(dir.path().to_owned(), usize::MAX).unwrap();
        let key = key(1, 2);
        writer.send(
            key.clone(),
            Job::Write {
                copy: Box::new(terminal.copy_to_keep(100)),
                keeping: Keeping::default(),
            },
        );
        assert!(writer.stop(Duration::from_secs(10)));
        let lines = read(dir.path(), &key).unwrap().unwrap();
        assert_eq!(
            lines.texts(),
            ["user", clackatoo_term::hints::NOT_KEPT, "done"]
        );
        let file = fs::read(path_of(dir.path(), &key)).unwrap();
        let mut body = Vec::new();
        DeflateDecoder::new(&file[MAGIC.len() + 4..])
            .read_to_end(&mut body)
            .unwrap();
        assert!(!body.windows(7).any(|bytes| bytes == b"hunter2"));
    }

    /// Nothing is read from a folder others can get into, or that's a
    /// link to one.
    #[test]
    fn nothing_is_read_from_a_folder_others_can_get_into_or_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("history");
        let writer = Writer::start(history.clone(), usize::MAX).unwrap();
        let key = key(1, 2);
        writer.send(
            key.clone(),
            Job::Write {
                copy: copy("mine"),
                keeping: Keeping::default(),
            },
        );
        assert!(writer.stop(Duration::from_secs(10)));
        assert!(read(&history, &key).unwrap().is_some());
        fs::set_permissions(&history, fs::Permissions::from_mode(0o755)).unwrap();
        let err = read(&history, &key).unwrap_err();
        assert!(err.contains("others can get into"), "{err}");
        fs::set_permissions(&history, fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&history, &link).unwrap();
        let err = read(&link, &key).unwrap_err();
        assert!(err.contains("isn't a folder"), "{err}");
    }

    /// A file bigger than this build writes isn't read, nor one whose
    /// body inflates past what it writes.
    #[test]
    fn files_too_big_or_inflating_too_far_arent_read() {
        let temporary = tempfile::tempdir().unwrap();
        let dir = temporary.path().join("history");
        crate::recovery::make_dir(&dir).unwrap();
        let key = key(1, 2);
        let path = path_of(&dir, &key);
        let mut big = MAGIC.to_vec();
        big.extend_from_slice(&VERSION.to_le_bytes());
        big.resize(usize::try_from(MOST_FILE).unwrap() + 1, 0);
        fs::write(&path, &big).unwrap();
        let err = read(&dir, &key).unwrap_err();
        assert!(err.contains("bigger"), "{err}");
        let mut bomb = MAGIC.to_vec();
        bomb.extend_from_slice(&VERSION.to_le_bytes());
        let mut deflate = DeflateEncoder::new(bomb, Compression::best());
        let zeros = vec![0; 1024 * 1024];
        for _ in 0..25 {
            deflate.write_all(&zeros).unwrap();
        }
        fs::write(&path, deflate.finish().unwrap()).unwrap();
        let err = read(&dir, &key).unwrap_err();
        assert!(err.contains("bigger"), "{err}");
    }

    /// No more than the bytes of text allowed are written: the newest.
    #[test]
    fn no_more_is_written_than_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::start(dir.path().to_owned(), 40).unwrap();
        let key = key(1, 2);
        writer.send(
            key.clone(),
            Job::Write {
                copy: copy("one\r\ntwo\r\nthree\r\nfour\r\nfive"),
                keeping: Keeping::default(),
            },
        );
        assert!(writer.stop(Duration::from_secs(10)));
        let lines = read(dir.path(), &key).unwrap().unwrap();
        assert_eq!(lines.texts(), ["four", "five"]);
    }

    #[test]
    fn a_link_where_a_file_is_read_or_written_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"theirs").unwrap();
        let key = key(1, 2);
        std::os::unix::fs::symlink(&target, temporary_of(dir.path(), &key)).unwrap();
        let writer = Writer::start(dir.path().to_owned(), usize::MAX).unwrap();
        writer.send(
            key.clone(),
            Job::Write {
                copy: copy("mine"),
                keeping: Keeping::default(),
            },
        );
        assert!(writer.stop(Duration::from_secs(10)));
        assert_eq!(fs::read(&target).unwrap(), b"theirs");
        assert_eq!(read(dir.path(), &key).unwrap().unwrap().texts(), ["mine"]);
        fs::remove_file(path_of(dir.path(), &key)).unwrap();
        std::os::unix::fs::symlink(&target, path_of(dir.path(), &key)).unwrap();
        assert!(read(dir.path(), &key).is_err());
    }

    /// logind's lock is let go only once nothing waits: what was asked
    /// before it, however much there was, and anything asked after while
    /// that was being done.
    #[test]
    fn the_lock_waits_for_everything_asked_of_the_thread() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::start(dir.path().to_owned(), usize::MAX).unwrap();
        let (lock, held) = rustix::pipe::pipe().unwrap();
        let keys: Vec<String> = (0..64).map(|terminal| key(1, terminal)).collect();
        for key in &keys[..32] {
            writer.send(
                key.clone(),
                Job::Write {
                    copy: copy("before"),
                    keeping: Keeping::default(),
                },
            );
        }
        writer.let_go(held);
        for key in &keys[32..] {
            writer.send(
                key.clone(),
                Job::Write {
                    copy: copy("after"),
                    keeping: Keeping::default(),
                },
            );
        }
        let mut byte = [0];
        assert_eq!(rustix::io::read(&lock, &mut byte).unwrap(), 0);
        for key in &keys[..32] {
            assert!(path_of(dir.path(), key).exists(), "{key}");
        }
        assert!(writer.stop(Duration::from_secs(10)));
    }

    /// Each terminal has one job waiting at most, a newer one in the
    /// older's place, so a stuck disk holds one copy of each terminal's
    /// lines, not one for every time they were written.
    #[test]
    fn a_terminal_has_one_job_waiting_at_most() {
        let (_, done) = mpsc::channel();
        let writer = Writer {
            shared: Arc::new(Shared::default()),
            done,
        };
        for _ in 0..3 {
            writer.send(
                key(1, 1),
                Job::Write {
                    copy: copy("text"),
                    keeping: Keeping::default(),
                },
            );
        }
        writer.send(key(1, 2), Job::Remove);
        writer.send(key(1, 1), Job::Remove);
        assert_eq!(writer.waiting(), 2);
        let queue = writer.shared.queue();
        assert_eq!(queue.order.len(), 2);
        assert!(matches!(queue.jobs[&key(1, 1)], Job::Remove));
    }

    #[test]
    fn files_go_when_removed_swept_or_left_with_nothing_and_the_lock_waits() {
        let dir = tempfile::tempdir().unwrap();
        let writer = Writer::start(dir.path().to_owned(), usize::MAX).unwrap();
        let (one, two, three) = (key(1, 1), key(1, 2), key(1, 3));
        for key in [&one, &two, &three] {
            writer.send(
                key.clone(),
                Job::Write {
                    copy: copy("text"),
                    keeping: Keeping::default(),
                },
            );
        }
        let (lock, held) = rustix::pipe::pipe().unwrap();
        writer.send(one.clone(), Job::Remove);
        writer.send(
            two.clone(),
            Job::Write {
                copy: copy("text"),
                keeping: Keeping {
                    from: u64::MAX,
                    marked_only: false,
                },
            },
        );
        writer.sweep(HashSet::from([one.clone()]));
        writer.let_go(held);
        // The lock is let go once all that's done.
        let mut byte = [0];
        assert_eq!(rustix::io::read(&lock, &mut byte).unwrap(), 0);
        for key in [&one, &two, &three] {
            assert!(!path_of(dir.path(), key).exists(), "{key}");
        }
        assert!(writer.stop(Duration::from_secs(10)));
    }
}
