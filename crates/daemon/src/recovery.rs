//! Recovery files: each buffer's changes that aren't saved, kept on disk
//! as they're made, so they outlive the daemon crashing, being killed or
//! stopping, or the machine going down, as Vim's swap files do. Written on
//! a thread of their own, as writing and syncing a file takes far too long
//! for the event loop.
//!
//! # The format
//!
//! Each buffer with changes has a file, `<daemon>-<buffer>.recover`, in
//! the recovery directory (`~/.local/state/clackatoo/recovery`), named by
//! the daemon's own number, which outlives upgrades, and the buffer's ID.
//! It starts with [`MAGIC`], then the format's version, [`VERSION`], as
//! four bytes, little-endian, then a [`Header`] in postcard, then the
//! buffer's text, as UTF-8, to the end of the file. The header says which
//! file the buffer edits, or none for a buffer with no file yet, and what
//! that file held when the buffer last read or wrote it, as a SHA-256
//! digest: if it still holds that, the changes can come back over it.
//!
//! Like the handover formats, the format is frozen: the types here are the
//! file's, not the daemon's, and change only with a new version, which a
//! newer build reads beside this one.
//!
//! Each file is written whole beside its name, synced, and renamed over
//! it, and the directory synced, so a crash leaves the last one written or
//! the one before, never half of one; only then are the files it takes the
//! place of removed. Files are for the user alone: the directory is made
//! 0700, and must be the user's own, and each file 0600. Nothing in it is
//! followed if it's a link, or opened if it isn't a regular file.
//!
//! Only one daemon at a time keeps the files in a directory: it holds a
//! lock on `.lock` in it for as long as it runs, and hands it to the next
//! build when it upgrades. Another daemon, such as one started by hand to
//! try something, finds it held and keeps none, rather than take the first
//! one's files for a crashed daemon's.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clackatoo_text::Rope;
use serde::{Deserialize, Serialize};

/// The start of every recovery file, to tell it from anything else.
const MAGIC: &[u8] = b"clackatoo recovery\n";

/// The format's version.
pub(crate) const VERSION: u32 = 1;

/// The most text a buffer can have and still be kept: a recovery file is
/// written whole every few seconds while it's typed in, and read whole as
/// the daemon starts. Far more than any file edited by hand.
pub(crate) const MOST_KEPT: usize = 64 * 1024 * 1024;

/// The most bytes a header can take: two paths, a digest and a time.
const MOST_HEADER: usize = 64 * 1024;

/// How many jobs wait for the thread before the event loop keeps the rest
/// itself, one for each file, rather than let what a stuck disk holds up
/// pile up.
const QUEUED: usize = 32;

/// What a recovery file's name ends with.
const EXTENSION: &str = "recover";

/// The file a daemon keeping the files holds a lock on.
const LOCK: &str = ".lock";

/// What a recovery file says of its buffer, before its text.
#[derive(Debug, Serialize, Deserialize)]
struct Header {
    /// The path of the file the buffer edits, absolute, or empty for one
    /// with no file yet.
    path: Vec<u8>,
    /// The directory paths opened beside it are from, and a buffer with no
    /// file yet's name, once it's given one.
    dir: Vec<u8>,
    /// The SHA-256 digest of the file's text as the buffer last read or
    /// wrote it, with its lines ending `\n`, as the buffer keeps them.
    base: [u8; 32],
    /// When it was written, in milliseconds since 1970.
    written: u64,
}

/// A buffer as a recovery file keeps it, taken from it on the event loop
/// at the cost of a few counts: ropes share what they hold.
#[derive(Debug)]
pub(crate) struct Keep {
    /// Its file's path, or an empty one for a buffer with no file yet.
    pub(crate) path: PathBuf,
    /// The directory paths opened beside it are from.
    pub(crate) dir: PathBuf,
    /// The text as last read from the file or written to it.
    pub(crate) saved: Rope,
    /// The text now.
    pub(crate) text: Rope,
}

/// A recovery file's header, read back: what its text is kept for, which
/// is read only when it's taken back (see [`Store::read_text`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    /// Its name, less its extension: see [`key`].
    pub(crate) key: String,
    /// The file its buffer edits, or `None` for one with no file yet.
    pub(crate) path: Option<PathBuf>,
    /// The directory paths opened beside it are from.
    pub(crate) dir: PathBuf,
    /// The digest of the file's text as the buffer last read or wrote it
    /// (see [`digest`]).
    pub(crate) base: [u8; 32],
    /// When it was written, in milliseconds since 1970.
    pub(crate) written: u64,
    /// How long its text is, in bytes.
    pub(crate) len: u64,
}

/// The name of the recovery file of buffer `buffer` of the daemon whose
/// own number is `daemon`, less its extension: what it's known by.
pub(crate) fn key(daemon: u64, buffer: u32) -> String {
    format!("{daemon:016x}-{buffer}")
}

/// The buffer whose recovery file `key` names, if it's this daemon's,
/// whose own number is `daemon`.
pub(crate) fn buffer_of(key: &str, daemon: u64) -> Option<u32> {
    let (owner, buffer) = key.split_once('-')?;
    (u64::from_str_radix(owner, 16).ok()? == daemon && owner.len() == 16)
        .then(|| buffer.parse().ok())
        .flatten()
}

/// The SHA-256 digest of `text`.
pub(crate) fn digest(text: &Rope) -> [u8; 32] {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    for chunk in text.chunks() {
        context.update(chunk.as_bytes());
    }
    let mut digest = [0; 32];
    digest.copy_from_slice(context.finish().as_ref());
    digest
}

/// The SHA-256 digest of `text`, as [`digest`] gives a rope's.
#[cfg(test)]
pub(crate) fn digest_of(text: &str) -> [u8; 32] {
    let mut digest = [0; 32];
    digest.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, text.as_bytes()).as_ref());
    digest
}

/// The path of the recovery file `key` names, in `dir`.
fn path_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{EXTENSION}"))
}

/// The path a recovery file is written at before it's renamed into
/// place: hidden, and never taken for one.
fn temporary_of(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!(".{key}.tmp"))
}

/// The start of a recovery file of `keep` as it was at `written`: all
/// but its text, which follows.
fn start_of(keep: &Keep, written: u64) -> Result<Vec<u8>, String> {
    let header = Header {
        path: keep.path.as_os_str().as_bytes().to_vec(),
        dir: keep.dir.as_os_str().as_bytes().to_vec(),
        base: digest(&keep.saved),
        written,
    };
    let header = postcard::to_allocvec(&header).map_err(|err| err.to_string())?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + header.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&header);
    Ok(bytes)
}

/// The header of a recovery file named `key` that starts with `start`,
/// and where its text starts. Says why not if it isn't one this build
/// reads.
fn decode_start(key: &str, start: &[u8], len: u64) -> Result<(Kept, usize), String> {
    let rest = start
        .strip_prefix(MAGIC)
        .ok_or("it isn't a recovery file")?;
    let (version, rest) = rest.split_at_checked(4).ok_or("it's cut short")?;
    let version = u32::from_le_bytes(version.try_into().map_err(|_| "it's cut short")?);
    if version != VERSION {
        return Err(format!(
            "it's of version {version}, which this build can't read"
        ));
    }
    let (header, after) = postcard::take_from_bytes::<Header>(&rest[..rest.len().min(MOST_HEADER)])
        .map_err(|err| format!("its header doesn't read: {err}"))?;
    let text_at = start.len() - rest.len() + (rest.len().min(MOST_HEADER) - after.len());
    let path = PathBuf::from(std::ffi::OsString::from_vec(header.path));
    let dir = PathBuf::from(std::ffi::OsString::from_vec(header.dir));
    // Only an absolute path is a file's: anything else could be taken from
    // wherever the daemon happened to be.
    if !(path.as_os_str().is_empty() || path.is_absolute()) || !dir.is_absolute() {
        return Err("its paths aren't absolute".to_owned());
    }
    let kept = Kept {
        key: key.to_owned(),
        path: (!path.as_os_str().is_empty()).then_some(path),
        dir,
        base: header.base,
        written: header.written,
        len: len.saturating_sub(text_at as u64),
    };
    Ok((kept, text_at))
}

/// Makes the directory `dir` for the user alone, and the directories it's
/// in as directories are usually made, then checks it's a directory, not
/// a link to one, and the user's own, making it the user's alone if it
/// was there already and wasn't: it holds what the user typed.
fn make_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        made => made?,
    }
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() {
        return Err(io::Error::other("it isn't a folder"));
    }
    if metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::other("it's another user's"));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Locks the recovery files in `dir` for this daemon, making `dir` if it
/// isn't there, or takes `handed`, the lock the build before held, if it's
/// still the lock on them. Returns the lock, held while the file is open,
/// or why another daemon has it.
fn lock(dir: &Path, handed: Option<OwnedFd>) -> Result<File, String> {
    let failed = |err: io::Error| {
        format!(
            "changes not saved can't be kept in {}: {err}",
            dir.display()
        )
    };
    make_dir(dir).map_err(failed)?;
    let path = dir.join(LOCK);
    let file = match handed.map(File::from) {
        Some(handed) if is_same_file(&handed, &path) => {
            log::info!("the recovery files' lock taken over from the build before");
            handed
        }
        _ => OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(failed)?,
    };
    // Held already by a lock that was handed over, it's held again. Held
    // by a program a daemon was starting as it went, it's let go of as
    // soon as that runs, so it's asked again for a moment.
    let deadline = Instant::now() + LOCK_PATIENCE;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(format!(
                    "another clackatoo daemon keeps changes not saved in {}: this one keeps none",
                    dir.display()
                ));
            }
            Err(err) => return Err(failed(err.into())),
        }
    }
}

/// How long the lock is asked for before it's taken that another daemon
/// holds it: a program a daemon forked holds a copy of every file it had
/// open, the lock too, until it runs what it was started for.
const LOCK_PATIENCE: Duration = Duration::from_millis(250);

/// Whether `file` is the file at `path`.
fn is_same_file(file: &File, path: &Path) -> bool {
    match (file.metadata(), fs::symlink_metadata(path)) {
        (Ok(open), Ok(there)) => (open.dev(), open.ino()) == (there.dev(), there.ino()),
        _ => false,
    }
}

/// What reading the recovery directory found.
#[derive(Debug, Default)]
struct Found {
    /// Other daemons' files, newest first.
    others: Vec<Kept>,
    /// This daemon's own, as their buffers and names: carried through an
    /// upgrade, they're its buffers', and aren't read.
    own: Vec<(u32, String)>,
}

/// Reads the header of every recovery file in `dir` but those of the
/// daemon whose number is `daemon`, newest first. A file that doesn't read
/// is renamed to end `.damaged`, for the user to look at, and isn't read
/// again; one bigger than any written, or that isn't a regular file, is
/// left alone. A file a crash left half written, under its temporary name,
/// is removed.
fn read_all(dir: &Path, daemon: u64) -> io::Result<Found> {
    let mut found = Found::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // Only this build's own names, which are lowercase.
        #[allow(
            clippy::case_sensitive_file_extension_comparisons,
            reason = "only the names this build writes"
        )]
        let temporary = name.starts_with('.') && name.ends_with(".tmp");
        if temporary {
            if let Err(err) = fs::remove_file(&path) {
                log::warn!("{}: {err}", path.display());
            }
            continue;
        }
        let Some(key) = name.strip_suffix(&format!(".{EXTENSION}")) else {
            continue;
        };
        if let Some(buffer) = buffer_of(key, daemon) {
            found.own.push((buffer, key.to_owned()));
            continue;
        }
        // Not followed, if it's a link.
        if !entry.file_type()?.is_file() {
            log::warn!("{}: left alone, as it isn't a file", path.display());
            continue;
        }
        match read_start(&path, key) {
            Ok(Some((one, _))) => found.others.push(one),
            Ok(None) => {}
            Err(why) => {
                let mut damaged = path.clone().into_os_string();
                damaged.push(".damaged");
                log::error!("{}: {why}: moved aside", path.display());
                if let Err(err) = fs::rename(&path, &damaged) {
                    log::error!("{}: {err}", path.display());
                }
            }
        }
    }
    (found.others).sort_by(|a, b| b.written.cmp(&a.written).then_with(|| a.key.cmp(&b.key)));
    Ok(found)
}

/// Opens the recovery file at `path` to read, never following a link, or
/// waiting on a pipe: `None` if it's too big to be one this build wrote,
/// or isn't a regular file.
fn open_kept(path: &Path) -> Result<Option<(File, u64)>, String> {
    let most = (MAGIC.len() + 4 + MOST_HEADER + MOST_KEPT) as u64;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|err| err.to_string())?;
    let metadata = file.metadata().map_err(|err| err.to_string())?;
    if !metadata.is_file() || metadata.len() > most {
        log::warn!("{}: left alone, as no recovery file is so", path.display());
        return Ok(None);
    }
    Ok(Some((file, metadata.len())))
}

/// Reads the header of the recovery file at `path`, named `key`, and
/// where its text starts.
fn read_start(path: &Path, key: &str) -> Result<Option<(Kept, usize)>, String> {
    let Some((file, len)) = open_kept(path)? else {
        return Ok(None);
    };
    let mut start = Vec::new();
    file.take((MAGIC.len() + 4 + MOST_HEADER) as u64)
        .read_to_end(&mut start)
        .map_err(|err| err.to_string())?;
    decode_start(key, &start, len).map(Some)
}

/// Writes `keep` as the recovery file `key` names, in `dir`: whole, beside
/// it, synced, then renamed over it, for the user alone, then the directory
/// synced, so it's there for good before anything it replaces goes.
fn write_one(dir: &Path, key: &str, keep: &Keep) -> Result<(), String> {
    let start = start_of(keep, now())?;
    let temporary = temporary_of(dir, key);
    let written = (|| {
        // Never through a link, or into a file someone else made there.
        match fs::remove_file(&temporary) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)?;
        // The text a chunk at a time, never copied whole.
        let mut file = io::BufWriter::with_capacity(64 * 1024, file);
        file.write_all(&start)?;
        for chunk in keep.text.chunks() {
            file.write_all(chunk.as_bytes())?;
        }
        let file = file.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_data()?;
        fs::rename(&temporary, path_of(dir, key))?;
        File::open(dir)?.sync_all()
    })();
    written.map_err(|err| {
        let _ = fs::remove_file(&temporary);
        err.to_string()
    })
}

/// Removes the recovery file `key` names, in `dir`, if it's there.
fn remove_one(dir: &Path, key: &str) -> Result<(), String> {
    match fs::remove_file(path_of(dir, key)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err.to_string()),
        _ => Ok(()),
    }
}

/// The time now, in milliseconds since 1970.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// What the thread is asked to do with one recovery file.
#[derive(Debug)]
enum Change {
    /// Write it.
    Write(Keep),
    /// Remove it.
    Remove,
}

/// What the thread is asked to do.
#[derive(Debug)]
enum Job {
    /// Write or remove the recovery file `key` names, then, if that went
    /// well, remove those `replaces` names: files the buffer's text came
    /// back from, which this one now keeps.
    File {
        key: String,
        change: Change,
        replaces: Vec<String>,
    },
    /// For tests: do what's waiting, then say so.
    #[cfg(test)]
    Flush(mpsc::Sender<()>),
    /// Do what's waiting, and stop.
    Stop,
}

/// A recovery file the thread couldn't write or remove, or a file it
/// replaces that it couldn't remove.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    /// The file's name.
    pub(crate) key: String,
    /// Why.
    pub(crate) why: String,
    /// The files it was to replace that are still there, to be removed
    /// once it's written next.
    pub(crate) replaces: Vec<String>,
    /// Whether it's the first of a run of failures for the file, which
    /// is said.
    pub(crate) first: bool,
}

/// What [`Store::open`] found.
#[derive(Debug)]
pub(crate) struct Opened {
    pub(crate) store: Store,
    /// Other daemons' files, newest first, as their headers.
    pub(crate) others: Vec<Kept>,
    /// This daemon's own, as their buffers and names.
    pub(crate) own: Vec<(u32, String)>,
}

/// The thread that writes recovery files, and the lock on their directory.
#[derive(Debug)]
pub(crate) struct Store {
    dir: PathBuf,
    /// The lock on the directory, held while it's open.
    lock: File,
    jobs: SyncSender<Job>,
    /// Jobs the thread hasn't room for yet, one for each file, sent as it
    /// makes room.
    waiting: VecDeque<Job>,
    /// Set while jobs wait, for the thread to wake the event loop once it
    /// makes room.
    backlog: Arc<AtomicBool>,
    /// Says when the thread has stopped.
    stopped: Receiver<()>,
    /// Set as it's asked to stop, which ends a wait for the gate.
    stopping: Arc<AtomicBool>,
    /// What the thread couldn't do, for the event loop.
    failed: Arc<Mutex<Vec<Failure>>>,
}

impl Store {
    /// Locks the recovery files in `dir`, making it if it isn't there, or
    /// takes `lock`, the lock the build before held; reads the headers of
    /// every file but those of the daemon whose number is `daemon`, newest
    /// first; and starts the thread that writes them from then on, calling
    /// `wake` when one couldn't be written, or once it has room for jobs
    /// waiting. For tests, the thread waits before each write until `gate`
    /// exists.
    ///
    /// # Errors
    ///
    /// If another daemon keeps the files, or they couldn't be read, or the
    /// thread couldn't start.
    pub(crate) fn open(
        dir: PathBuf,
        daemon: u64,
        lock: Option<OwnedFd>,
        gate: Option<PathBuf>,
        wake: impl Fn() + Send + 'static,
    ) -> Result<Opened, String> {
        let lock = self::lock(&dir, lock)?;
        let found = read_all(&dir, daemon).map_err(|err| {
            format!(
                "changes not saved in {} couldn't be read: {err}",
                dir.display()
            )
        })?;
        let (jobs, receive) = mpsc::sync_channel(QUEUED);
        let (stop, stopped) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let backlog = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(Mutex::new(Vec::new()));
        let (failures, asked_to_stop, room) = (
            Arc::clone(&failed),
            Arc::clone(&stopping),
            Arc::clone(&backlog),
        );
        let writing = dir.clone();
        std::thread::Builder::new()
            .name("recovery".into())
            .spawn(move || {
                let writer = Writer {
                    dir: &writing,
                    gate: gate.as_deref(),
                    stopping: &asked_to_stop,
                    backlog: &room,
                    failed: &failures,
                    wake: &wake,
                };
                writer.run(&receive);
                let _ = stop.send(());
            })
            .map_err(|err| format!("changes not saved can't be kept: {err}"))?;
        let store = Self {
            dir,
            lock,
            jobs,
            waiting: VecDeque::new(),
            backlog,
            stopped,
            stopping,
            failed,
        };
        Ok(Opened {
            store,
            others: found.others,
            own: found.own,
        })
    }

    /// The text of the recovery file `kept` is the header of, read now,
    /// if it's still the file read before.
    pub(crate) fn read_text(&self, kept: &Kept) -> Result<String, String> {
        let path = path_of(&self.dir, &kept.key);
        let Some((mut file, len)) = open_kept(&path)? else {
            return Err("it isn't a recovery file".to_owned());
        };
        let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        (&mut file)
            .take(len)
            .read_to_end(&mut bytes)
            .map_err(|err| err.to_string())?;
        let (now, text_at) = decode_start(&kept.key, &bytes, len)?;
        if now != *kept {
            return Err("it has changed since it was read".to_owned());
        }
        String::from_utf8(bytes.split_off(text_at)).map_err(|_| "its text isn't UTF-8".to_owned())
    }

    /// Writes `keep` as the recovery file `key` names, then removes those
    /// `replaces` names, which it takes the place of.
    pub(crate) fn write(&mut self, key: String, keep: Keep, replaces: Vec<String>) {
        let change = Change::Write(keep);
        self.send(Job::File {
            key,
            change,
            replaces,
        });
    }

    /// Removes the recovery file `key` names, and those `replaces` names.
    pub(crate) fn remove(&mut self, key: String, replaces: Vec<String>) {
        let change = Change::Remove;
        self.send(Job::File {
            key,
            change,
            replaces,
        });
    }

    /// Sends `job` to the thread, never waiting: if the thread has no room
    /// for it, it waits here, in place of any job for the same file
    /// waiting, whose files to replace it takes on.
    fn send(&mut self, job: Job) {
        if let Job::File { key, replaces, .. } = &job
            && let Some(at) = (self.waiting.iter())
                .position(|waiting| matches!(waiting, Job::File { key: other, .. } if other == key))
        {
            let mut replaces = replaces.clone();
            if let Some(Job::File {
                replaces: before, ..
            }) = self.waiting.remove(at)
            {
                replaces.extend(before);
            }
            let Job::File { key, change, .. } = job else {
                unreachable!("just matched")
            };
            self.waiting.push_back(Job::File {
                key,
                change,
                replaces,
            });
        } else {
            self.waiting.push_back(job);
        }
        self.send_waiting();
    }

    /// Sends the thread the jobs waiting, as many as it has room for.
    pub(crate) fn send_waiting(&mut self) {
        // Said before trying, not after: the thread clears it as it takes
        // in what's queued, so set after a try that found no room, it
        // could be set just after the thread last looked, and never wake
        // the loop.
        if !self.waiting.is_empty() {
            self.backlog.store(true, Ordering::Release);
        }
        while let Some(job) = self.waiting.pop_front() {
            match self.jobs.try_send(job) {
                Ok(()) => {}
                Err(TrySendError::Full(job)) => {
                    self.waiting.push_front(job);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.waiting.clear();
                    break;
                }
            }
        }
        if self.waiting.is_empty() {
            self.backlog.store(false, Ordering::Release);
        }
    }

    /// Whether jobs wait for room.
    pub(crate) fn is_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// What the thread couldn't do since last asked.
    pub(crate) fn take_failed(&self) -> Vec<Failure> {
        std::mem::take(&mut *self.failed.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// For tests: waits up to `wait` for everything asked for so far to be
    /// done. Returns whether it was in time.
    #[cfg(test)]
    pub(crate) fn flush(&mut self, wait: Duration) -> bool {
        let (reply, flushed) = mpsc::channel();
        self.waiting.push_back(Job::Flush(reply));
        let deadline = Instant::now() + wait;
        while self.is_waiting() && Instant::now() < deadline {
            self.send_waiting();
            std::thread::sleep(Duration::from_millis(1));
        }
        flushed
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }

    /// Has the thread do what's waiting and stop, without waiting for it:
    /// [`stop`](Self::stop) does.
    pub(crate) fn begin_stop(&mut self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.waiting.push_back(Job::Stop);
        self.send_waiting();
    }

    /// Has the thread do what's waiting and stop, if it hasn't been told
    /// to already, waiting up to `wait` for it. Returns the lock, for the
    /// next build to take, and whether the thread stopped in time.
    pub(crate) fn stop(mut self, wait: Duration) -> (OwnedFd, bool) {
        if !self.stopping.load(Ordering::Relaxed) {
            self.begin_stop();
        }
        let deadline = Instant::now() + wait;
        while self.is_waiting() && Instant::now() < deadline {
            self.send_waiting();
            std::thread::sleep(Duration::from_millis(1));
        }
        let stopped = !self.is_waiting()
            && !matches!(
                (self.stopped).recv_timeout(deadline.saturating_duration_since(Instant::now())),
                Err(RecvTimeoutError::Timeout)
            );
        (self.lock.into(), stopped)
    }
}

/// The thread writing recovery files, as it runs.
struct Writer<'a, F> {
    dir: &'a Path,
    gate: Option<&'a Path>,
    stopping: &'a AtomicBool,
    backlog: &'a AtomicBool,
    failed: &'a Mutex<Vec<Failure>>,
    wake: &'a F,
}

impl<F: Fn()> Writer<'_, F> {
    /// Does what it's asked to until it's asked to stop. Of several
    /// changes to one file waiting, only the last is made, with every
    /// file any of them replaces removed after it.
    fn run(&self, jobs: &Receiver<Job>) {
        // Files whose last change failed, so a run of failures is said
        // once.
        let mut failing: HashSet<String> = HashSet::new();
        let mut stop = false;
        while !stop {
            let Ok(first) = jobs.recv() else { break };
            let mut order: Vec<String> = Vec::new();
            let mut changes: HashMap<String, (Change, Vec<String>)> = HashMap::new();
            #[cfg(test)]
            let mut flushes = Vec::new();
            for job in std::iter::once(first).chain(jobs.try_iter()) {
                match job {
                    Job::File {
                        key,
                        change,
                        replaces,
                    } => {
                        if let Some((last, all)) = changes.get_mut(&key) {
                            *last = change;
                            all.extend(replaces);
                        } else {
                            order.push(key.clone());
                            changes.insert(key, (change, replaces));
                        }
                    }
                    #[cfg(test)]
                    Job::Flush(reply) => flushes.push(reply),
                    Job::Stop => stop = true,
                }
            }
            // Room for more: the loop sends what waited for it.
            if self.backlog.swap(false, Ordering::AcqRel) {
                (self.wake)();
            }
            let mut woke = false;
            for key in order {
                let Some((change, replaces)) = changes.remove(&key) else {
                    continue;
                };
                if let Some(failure) = self.change(&key, change, replaces, &mut failing) {
                    log::error!("{}: {}", path_of(self.dir, &key).display(), failure.why);
                    (self.failed.lock().unwrap_or_else(PoisonError::into_inner)).push(failure);
                    woke = true;
                }
            }
            if woke {
                (self.wake)();
            }
            #[cfg(test)]
            for reply in flushes {
                let _ = reply.send(());
            }
        }
    }

    /// Makes `change` to the file `key` names, then removes those
    /// `replaces` names if that went well. Returns what went wrong, if
    /// anything did, with the files it was to replace still there.
    fn change(
        &self,
        key: &str,
        change: Change,
        replaces: Vec<String>,
        failing: &mut HashSet<String>,
    ) -> Option<Failure> {
        let outcome = match change {
            Change::Write(keep) => {
                self.wait_for_gate();
                write_one(self.dir, key, &keep)
            }
            Change::Remove => remove_one(self.dir, key),
        };
        let (why, left) = if let Err(why) = outcome {
            (why, replaces)
        } else {
            let mut why = None;
            let left: Vec<String> = (replaces.into_iter())
                .filter(|old| old != key)
                .filter(|old| match remove_one(self.dir, old) {
                    Ok(()) => false,
                    Err(err) => {
                        why.get_or_insert(err);
                        true
                    }
                })
                .collect();
            let Some(why) = why else {
                failing.remove(key);
                return None;
            };
            (why, left)
        };
        let first = failing.insert(key.to_owned());
        Some(Failure {
            key: key.to_owned(),
            why,
            replaces: left,
            first,
        })
    }

    /// For tests: waits until the gate exists, unless the daemon is
    /// stopping.
    fn wait_for_gate(&self) {
        let Some(gate) = self.gate else { return };
        while !gate.exists() && !self.stopping.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A recovery file of `keep` as it was at `written`, as written.
    fn encode(keep: &Keep, written: u64) -> Result<Vec<u8>, String> {
        let mut bytes = start_of(keep, written)?;
        bytes.extend_from_slice(keep.text.to_string().as_bytes());
        Ok(bytes)
    }

    /// What the bytes of a recovery file named `k` say, text and all.
    fn decode(bytes: &[u8]) -> Result<(Kept, String), String> {
        let (kept, text_at) = decode_start("k", bytes, bytes.len() as u64)?;
        let text = std::str::from_utf8(&bytes[text_at..]).map_err(|_| "its text isn't UTF-8")?;
        Ok((kept, text.to_owned()))
    }

    fn keep(path: &str, saved: &str, text: &str) -> Keep {
        Keep {
            path: PathBuf::from(path),
            dir: PathBuf::from("/home/jack"),
            saved: Rope::from_str(saved),
            text: Rope::from_str(text),
        }
    }

    fn open(dir: &Path, gate: Option<PathBuf>) -> Result<Opened, String> {
        Store::open(dir.to_owned(), 0xabc, None, gate, || {})
    }

    #[test]
    fn the_format_is_as_frozen() {
        let bytes = encode(&keep("/home/jack/a.rs", "one\n", "one\ntwo\n"), 7).unwrap();
        let mut expected = b"clackatoo recovery\n\x01\x00\x00\x00".to_vec();
        expected.push(15);
        expected.extend_from_slice(b"/home/jack/a.rs");
        expected.push(10);
        expected.extend_from_slice(b"/home/jack");
        expected.extend_from_slice(&digest_of("one\n"));
        expected.push(7);
        expected.extend_from_slice(b"one\ntwo\n");
        assert_eq!(bytes, expected);
        let (kept, text) = decode(&bytes).unwrap();
        assert_eq!(
            kept,
            Kept {
                key: "k".to_owned(),
                path: Some(PathBuf::from("/home/jack/a.rs")),
                dir: PathBuf::from("/home/jack"),
                base: digest_of("one\n"),
                written: 7,
                len: 8,
            }
        );
        assert_eq!(text, "one\ntwo\n");
        // A buffer with no file yet has no path.
        let blank = encode(&keep("", "", "draft"), 1).unwrap();
        assert_eq!(decode(&blank).unwrap().0.path, None);
        assert_eq!(digest(&Rope::from_str("one\n")), digest_of("one\n"));
    }

    #[test]
    fn what_isnt_a_recovery_file_doesnt_read() {
        let good = encode(&keep("/a", "", "text"), 1).unwrap();
        let mut newer = good.clone();
        newer[MAGIC.len()] = 2;
        let mut not_utf8 = good.clone();
        not_utf8.push(0xff);
        let relative = encode(&keep("a", "", "text"), 1).unwrap();
        for (bytes, why) in [
            (&b"something else"[..], "it isn't a recovery file"),
            (&good[..MAGIC.len() + 2], "it's cut short"),
            (&newer[..], "it's of version 2, which this build can't read"),
            (&not_utf8[..], "its text isn't UTF-8"),
            (&relative[..], "its paths aren't absolute"),
        ] {
            assert_eq!(decode(bytes).unwrap_err(), why);
        }
        assert!(decode(&good[..MAGIC.len() + 6]).is_err());
    }

    #[test]
    fn keys_name_a_daemons_buffers() {
        let key = key(0xabc, 7);
        assert_eq!(key, "0000000000000abc-7");
        assert_eq!(buffer_of(&key, 0xabc), Some(7));
        assert_eq!(buffer_of(&key, 0xabd), None);
        assert_eq!(buffer_of("abc-7", 0xabc), None);
        assert_eq!(buffer_of("0000000000000abc-x", 0xabc), None);
    }

    fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn files_are_written_whole_for_the_user_alone_and_read_back_newest_first() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("state/recovery");
        let Opened {
            mut store, others, ..
        } = open(&dir, None).unwrap();
        assert!(others.is_empty());
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        // The folder's the user's alone; those it's in, as usual.
        assert_eq!(mode(&dir), 0o700);
        assert_ne!(mode(&home.path().join("state")), 0o700);
        store.write("a".into(), keep("/a", "", "first"), Vec::new());
        assert!(store.flush(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(5));
        store.write("b".into(), keep("/b", "", "second"), Vec::new());
        // This daemon's own aren't read again, as it has their buffers.
        let own = key(0xabc, 9);
        store.write(own.clone(), keep("/c", "", "mine"), Vec::new());
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(mode(&path_of(&dir, "a")), 0o600);
        // Another daemon can't keep them meanwhile.
        let refused = open(&dir, None).unwrap_err();
        assert!(refused.contains("another clackatoo daemon"), "{refused}");
        let (_, stopped) = store.stop(Duration::from_secs(5));
        assert!(stopped);

        // A file a crash left half written goes; a damaged one is moved
        // aside; a pipe or a link isn't opened; a loose directory is made
        // the user's alone.
        fs::write(temporary_of(&dir, "c"), b"half").unwrap();
        fs::write(path_of(&dir, "d"), b"damaged").unwrap();
        let fifo = path_of(&dir, "e");
        let piped = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(piped.success());
        std::os::unix::fs::symlink(path_of(&dir, "a"), path_of(&dir, "f")).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let Opened {
            store,
            others,
            own: mine,
            ..
        } = open(&dir, None).unwrap();
        let keys: Vec<_> = others.iter().map(|kept| kept.key.as_str()).collect();
        assert_eq!(keys, ["b", "a"]);
        assert_eq!(mine, [(9, own)]);
        assert_eq!(store.read_text(&others[1]).unwrap(), "first");
        assert!(!temporary_of(&dir, "c").exists());
        assert!(dir.join("d.recover.damaged").exists());
        assert_eq!(mode(&dir), 0o700);
        // A file changed since its header was read isn't read for it.
        fs::write(
            path_of(&dir, "b"),
            encode(&keep("/b", "", "x"), 99).unwrap(),
        )
        .unwrap();
        assert!(store.read_text(&others[0]).is_err());
    }

    #[test]
    fn a_folder_that_is_a_link_or_someone_elses_isnt_kept_in() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = home.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let refused = open(&link, None).unwrap_err();
        assert!(refused.contains("isn't a folder"), "{refused}");
        // Owned by root, as `/` is, it's someone else's.
        if rustix::process::geteuid().as_raw() != 0 {
            assert!(make_dir(Path::new("/")).is_err());
        }
    }

    #[test]
    fn a_link_where_a_file_is_written_is_never_followed() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("recovery");
        let Opened { mut store, .. } = open(&dir, None).unwrap();
        let victim = home.path().join("victim");
        fs::write(&victim, "mine").unwrap();
        std::os::unix::fs::symlink(&victim, temporary_of(&dir, "a")).unwrap();
        store.write("a".into(), keep("/a", "", "kept"), Vec::new());
        assert!(store.flush(Duration::from_secs(5)));
        assert_eq!(fs::read_to_string(&victim).unwrap(), "mine");
        assert!(fs::symlink_metadata(path_of(&dir, "a")).unwrap().is_file());
    }

    #[test]
    fn only_the_last_change_to_a_file_is_made_and_what_it_replaces_goes_after() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().to_owned();
        let gate = home.path().join("gate");
        let Opened { mut store, .. } = open(&dir, Some(gate.clone())).unwrap();
        fs::write(path_of(&dir, "old"), b"from before").unwrap();
        // Held at the gate, nothing's written, and the one it replaces is
        // still there.
        store.write("a".into(), keep("/a", "", "one"), vec!["old".into()]);
        store.write("a".into(), keep("/a", "", "two"), Vec::new());
        std::thread::sleep(Duration::from_millis(50));
        assert!(!path_of(&dir, "a").exists() && path_of(&dir, "old").exists());
        fs::write(&gate, b"").unwrap();
        assert!(store.flush(Duration::from_secs(5)));
        let (kept, _) = read_start(&path_of(&dir, "a"), "a").unwrap().unwrap();
        assert_eq!(store.read_text(&kept).unwrap(), "two");
        assert!(!path_of(&dir, "old").exists());
        // Written then removed, it's removed.
        store.write("b".into(), keep("/b", "", "one"), Vec::new());
        store.remove("b".into(), Vec::new());
        store.remove("a".into(), Vec::new());
        assert!(store.flush(Duration::from_secs(5)));
        assert!(!path_of(&dir, "a").exists() && !path_of(&dir, "b").exists());
    }

    #[test]
    fn a_stuck_thread_never_holds_up_the_sender_and_gets_everything_after() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("recovery");
        let gate = home.path().join("gate");
        let Opened { mut store, .. } = open(&dir, Some(gate.clone())).unwrap();
        // Far more than the thread has room for, many times over for each
        // file, never waiting.
        let started = Instant::now();
        for round in 0..20 {
            for file in 0..QUEUED * 2 {
                let text = format!("{round}");
                store.write(format!("{file}"), keep("/a", "", &text), Vec::new());
            }
        }
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(store.is_waiting());
        assert!(store.waiting.len() <= QUEUED * 2, "one waits for each file");
        fs::write(&gate, b"").unwrap();
        assert!(store.flush(Duration::from_secs(10)));
        for file in 0..QUEUED * 2 {
            let key = format!("{file}");
            let (kept, _) = read_start(&path_of(&dir, &key), &key).unwrap().unwrap();
            assert_eq!(store.read_text(&kept).unwrap(), "19");
        }
    }

    #[test]
    fn the_thread_wakes_the_sender_once_it_has_room_for_what_waits() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("recovery");
        let gate = home.path().join("gate");
        let woken = Arc::new(AtomicBool::new(false));
        let wake = {
            let woken = Arc::clone(&woken);
            move || woken.store(true, Ordering::Relaxed)
        };
        let Opened { mut store, .. } =
            Store::open(dir.clone(), 1, None, Some(gate.clone()), wake).unwrap();
        for file in 0..QUEUED * 3 {
            store.write(format!("{file}"), keep("/a", "", "x"), Vec::new());
        }
        assert!(store.is_waiting());
        woken.store(false, Ordering::Relaxed);
        // Let go, with no more asked of it, it says when it has room.
        fs::write(&gate, b"").unwrap();
        wait_for("woken for what waits", || woken.load(Ordering::Relaxed));
        assert!(store.flush(Duration::from_secs(10)));
        assert!(!store.is_waiting());
    }

    #[test]
    fn a_failure_is_said_once_and_says_what_it_didnt_replace() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("recovery");
        let woken = Arc::new(AtomicBool::new(false));
        let wake = {
            let woken = Arc::clone(&woken);
            move || woken.store(true, Ordering::Relaxed)
        };
        let Opened { mut store, .. } = Store::open(dir.clone(), 1, None, None, wake).unwrap();
        fs::write(path_of(&dir, "old"), b"from before").unwrap();
        // A directory where its file would go can't be written over.
        fs::create_dir(path_of(&dir, "a")).unwrap();
        for _ in 0..2 {
            store.write("a".into(), keep("/a", "", "one"), vec!["old".into()]);
            assert!(store.flush(Duration::from_secs(5)));
        }
        wait_for("woken", || woken.load(Ordering::Relaxed));
        let failed = store.take_failed();
        let firsts: Vec<bool> = failed.iter().map(|failure| failure.first).collect();
        assert_eq!(firsts, [true, false]);
        assert!(failed.iter().all(|failure| failure.replaces == ["old"]));
        assert!(path_of(&dir, "old").exists());
        fs::remove_dir(path_of(&dir, "a")).unwrap();
        store.write("a".into(), keep("/a", "", "one"), vec!["old".into()]);
        assert!(store.flush(Duration::from_secs(5)));
        assert!(store.take_failed().is_empty());
        assert!(!path_of(&dir, "old").exists());
    }

    #[test]
    fn the_lock_is_handed_to_the_next_build_and_held_meanwhile() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("recovery");
        let Opened { store, .. } = open(&dir, None).unwrap();
        let (lock, stopped) = store.stop(Duration::from_secs(5));
        assert!(stopped);
        // Still held, between the builds.
        assert!(open(&dir, None).is_err());
        let Opened { store, .. } =
            Store::open(dir.clone(), 0xabc, Some(lock), None, || {}).unwrap();
        let (lock, _) = store.stop(Duration::from_secs(5));
        drop(lock);
        // Let go of, it's free.
        assert!(open(&dir, None).is_ok());
    }
}
