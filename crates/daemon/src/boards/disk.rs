//! Boards on disk: the format of their files, reading them all when the
//! daemon starts, and writing what changes, on a thread of their own, as
//! fsync takes far too long for the event loop.
//!
//! # The format
//!
//! Each board is a file, `<id>.jsonl`, in the boards directory
//! (`~/.local/share/clackatoo/boards`): JSON, one value per line. The
//! first line is the board's [`Header`], which says the format's version,
//! [`VERSION`], under `clackatoo_board`. Each line after it is a
//! [`Record`]: one change to the board, in the order they were made. A
//! line is only ever added, never changed, so a write cut off by a crash
//! can only cut off the last line.
//!
//! Like the handover formats, the format is frozen: the types here are
//! the file's, not the daemon's, and change only with a new version,
//! which a newer build reads beside this one. A field missing reads as
//! its default, and one this build doesn't know is ignored, so a field can
//! be added to a record without a new version, as long as older builds
//! may ignore it. A new kind of record can't: it needs a new version.
//!
//! Why lines of JSON, not `SQLite`: boards only ever append and read back
//! whole, which a log of lines does with no dependency, and survives a
//! crash by its shape; a file of lines is read, grepped and mended with
//! the tools everyone has; and a board is small enough to read back in
//! milliseconds.
//!
//! # Reading them back
//!
//! - A last line that's cut off, or doesn't read, is a write a crash cut
//!   off: it's dropped, and cut from the file before anything is added.
//! - Any other line that doesn't read, or doesn't fit the board, means
//!   the file is damaged: it's moved to `quarantine/`, for the user to
//!   look at, and the daemon carries on without it.
//! - A file of a newer version than this build knows, as after going back
//!   to an older build, is left alone, and isn't shown.
//!
//! Only one daemon at a time keeps the boards in a directory: it holds a
//! lock on `.lock` in it for as long as its thread runs. Another daemon,
//! such as one started by hand to try something, finds it held and goes
//! without boards, rather than write over the first one's.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clackatoo_protocol::{BoardAuthor, BoardLink, BoardScope, PostKind, TerminalId};
use serde::{Deserialize, Serialize};

use super::{Board, Boards};

/// The format's version.
pub(crate) const VERSION: u32 = 1;

/// The most bytes a board's file is read at: past what a board grows to,
/// with room for archiving it.
const MAX_FILE: u64 = super::MAX_BYTES + 1024 * 1024;

/// The name of the lock file in the boards directory.
const LOCK: &str = ".lock";

/// Where damaged files go, in the boards directory.
const QUARANTINE: &str = "quarantine";

/// A board file's first line: what the board is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Header {
    /// The format's version, [`VERSION`]; named so, first, so the file
    /// says what it is.
    pub(crate) clackatoo_board: u32,
    /// The board's ID, which its file is named by.
    pub(crate) id: u32,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) purpose: String,
    pub(crate) scope: Scope,
    /// When it was made, in milliseconds since the Unix epoch.
    pub(crate) created: u64,
    /// Who made it.
    pub(crate) by: Author,
}

/// Which workspaces see a board.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    Global,
    /// The workspace with this name.
    Workspace(String),
}

/// Who made a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Author {
    User,
    /// A chat's agent: the chat's ID then, its agent as shown, and its
    /// title.
    Chat {
        chat: u32,
        agent: String,
        title: String,
    },
}

/// One change to a board: a line of its file after the first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// When, in milliseconds since the Unix epoch.
    pub(crate) at: u64,
    /// Who made it.
    pub(crate) by: Author,
    #[serde(flatten)]
    pub(crate) event: Event,
}

/// What a change was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(crate) enum Event {
    /// An entry posted, numbered one past the last. A claim has the work
    /// it claims, and when it expires.
    Post {
        entry: u32,
        kind: Kind,
        text: String,
        #[serde(default)]
        links: Vec<Link>,
        #[serde(default)]
        subject: Option<String>,
        #[serde(default)]
        expires: Option<u64>,
    },
    /// An entry's text changed.
    Edit {
        entry: u32,
        text: String,
    },
    /// A claim made to last until `expires`.
    Renew {
        entry: u32,
        expires: u64,
    },
    /// A claim released.
    Release {
        entry: u32,
    },
    Archive,
    Unarchive,
}

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Note,
    Claim,
    Result,
}

/// What an entry points to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Link {
    /// A file, by its absolute path, and a line in it, from 1.
    File { path: String, line: Option<u32> },
    /// A chat's turn, from 1.
    Turn { chat: u32, turn: u32 },
}

impl From<Scope> for BoardScope {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Global => Self::Global,
            Scope::Workspace(name) => Self::Workspace(name),
        }
    }
}

impl From<BoardScope> for Scope {
    fn from(scope: BoardScope) -> Self {
        match scope {
            BoardScope::Global => Self::Global,
            BoardScope::Workspace(name) => Self::Workspace(name),
        }
    }
}

impl From<Author> for BoardAuthor {
    fn from(author: Author) -> Self {
        match author {
            Author::User => Self::User,
            Author::Chat { chat, agent, title } => Self::Chat {
                chat: TerminalId(chat),
                agent,
                title,
            },
        }
    }
}

impl From<BoardAuthor> for Author {
    fn from(author: BoardAuthor) -> Self {
        match author {
            BoardAuthor::User => Self::User,
            BoardAuthor::Chat { chat, agent, title } => Self::Chat {
                chat: chat.0,
                agent,
                title,
            },
        }
    }
}

impl From<Kind> for PostKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Note => Self::Note,
            Kind::Claim => Self::Claim,
            Kind::Result => Self::Result,
        }
    }
}

impl From<PostKind> for Kind {
    fn from(kind: PostKind) -> Self {
        match kind {
            PostKind::Note => Self::Note,
            PostKind::Claim => Self::Claim,
            PostKind::Result => Self::Result,
        }
    }
}

impl From<Link> for BoardLink {
    fn from(link: Link) -> Self {
        match link {
            Link::File { path, line } => Self::File { path, line },
            Link::Turn { chat, turn } => Self::Turn {
                chat: TerminalId(chat),
                turn,
            },
        }
    }
}

impl From<BoardLink> for Link {
    fn from(link: BoardLink) -> Self {
        match link {
            BoardLink::File { path, line } => Self::File { path, line },
            BoardLink::Turn { chat, turn } => Self::Turn { chat: chat.0, turn },
        }
    }
}

/// What reading the boards directory found.
#[derive(Debug, Default)]
pub(crate) struct Loaded {
    pub(crate) boards: Boards,
    /// The files that were damaged, as they're named in `quarantine/` now.
    pub(crate) quarantined: Vec<PathBuf>,
    /// The files of a newer version, or too big, left alone.
    pub(crate) newer: Vec<PathBuf>,
}

/// Reads every board in `dir`, making it if it isn't there. Damaged files
/// are quarantined, and torn last lines cut off, as the module says.
///
/// # Errors
///
/// If `dir` can't be made or listed.
pub(crate) fn load(dir: &Path) -> io::Result<Loaded> {
    make_dir(dir)?;
    let mut loaded = Loaded::default();
    let mut highest = 0;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // A new board not yet in place when the daemon stopped.
        if name.ends_with(".jsonl.tmp") {
            let _ = fs::remove_file(&path);
            continue;
        }
        let Some(id) = (name.strip_suffix(".jsonl")).and_then(|id| id.parse::<u32>().ok()) else {
            continue;
        };
        highest = highest.max(id);
        match read_board(&path, id) {
            Ok(Read::Board(board)) => {
                loaded.boards.list.insert(board.id(), *board);
            }
            Ok(Read::Newer(version)) => {
                log::warn!(
                    "board file {} is of version {version}, newer than this build's: it's left \
                     alone",
                    path.display()
                );
                loaded.newer.push(path);
            }
            Ok(Read::TooBig(size)) => {
                log::warn!(
                    "board file {} is {size} bytes, more than a board can be: it's left alone",
                    path.display()
                );
                loaded.newer.push(path);
            }
            Err(why) => {
                log::error!("board file {} is damaged: {why}", path.display());
                match quarantine(dir, &path) {
                    Ok(moved) => loaded.quarantined.push(moved),
                    Err(err) => log::error!("…and couldn't be moved aside: {err}"),
                }
            }
        }
    }
    // Quarantined boards' IDs aren't used again either.
    if let Ok(quarantined) = fs::read_dir(dir.join(QUARANTINE)) {
        for entry in quarantined.flatten() {
            let name = entry.file_name();
            let id = (name.to_str())
                .and_then(|name| name.split('.').next())
                .and_then(|id| id.parse::<u32>().ok());
            highest = highest.max(id.unwrap_or(0));
        }
    }
    loaded.boards.next = highest.saturating_add(1);
    Ok(loaded)
}

/// What a board file held.
enum Read {
    Board(Box<Board>),
    /// A file of this newer version.
    Newer(u32),
    /// A file this many bytes long, far more than a board can be.
    TooBig(u64),
}

/// Reads the board in the file at `path`, which should be board `id`'s,
/// cutting off a torn last line.
fn read_board(path: &Path, id: u32) -> Result<Read, String> {
    // Looked at before it's read: a board can't grow this big, so one
    // that has is left alone rather than read into memory.
    let size = fs::metadata(path).map_err(|err| err.to_string())?.len();
    if size > MAX_FILE {
        return Ok(Read::TooBig(size));
    }
    let bytes = fs::read(path).map_err(|err| err.to_string())?;
    let mut lines = bytes.split_inclusive(|&byte| byte == b'\n').peekable();
    let first = lines.next().unwrap_or_default();
    if !first.ends_with(b"\n") {
        return Err("it has no whole first line".to_owned());
    }
    // The version first, so a newer file isn't taken for a damaged one.
    let value: serde_json::Value =
        serde_json::from_slice(first).map_err(|err| format!("its first line: {err}"))?;
    let version = value["clackatoo_board"]
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .ok_or("its first line isn't a board's")?;
    if version > VERSION {
        return Ok(Read::Newer(version));
    }
    let header: Header =
        serde_json::from_value(value).map_err(|err| format!("its first line: {err}"))?;
    if header.id != id {
        return Err(format!("it says it's board {}", header.id));
    }
    let mut board = Board::new(&header);
    let mut good = first.len();
    let mut number = 1;
    while let Some(line) = lines.next() {
        number += 1;
        let last = lines.peek().is_none();
        match serde_json::from_slice::<Record>(line) {
            Ok(record) if line.ends_with(b"\n") => {
                board
                    .apply(&record)
                    .map_err(|why| format!("line {number}: {why}"))?;
                good += line.len();
            }
            // The last line, cut off by a crash, or left as nothing but
            // zeros, as some file systems leave one.
            _ if last => {
                log::warn!(
                    "board file {}: its last line was cut off, and is dropped",
                    path.display()
                );
                let file = OpenOptions::new()
                    .write(true)
                    .open(path)
                    .map_err(|err| err.to_string())?;
                file.set_len(good as u64).map_err(|err| err.to_string())?;
                file.sync_all().map_err(|err| err.to_string())?;
            }
            Ok(_) => unreachable!("only the last line can lack its end"),
            Err(err) => return Err(format!("line {number}: {err}")),
        }
    }
    board.bytes = good as u64;
    Ok(Read::Board(Box::new(board)))
}

/// Moves the damaged file at `path` into `dir`'s quarantine, named with
/// when, so a second one by the same name doesn't replace it. Returns
/// where it went.
fn quarantine(dir: &Path, path: &Path) -> io::Result<PathBuf> {
    let to = dir.join(QUARANTINE);
    make_dir(&to)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let moved = to.join(format!("{name}.{}", super::now()));
    fs::rename(path, &moved)?;
    Ok(moved)
}

/// Makes the directory `dir`, and those it's in, for the user alone.
fn make_dir(dir: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// `value` as a line of a file, its end included.
pub(crate) fn line_of(value: &impl Serialize) -> Vec<u8> {
    let mut line = serde_json::to_vec(value).expect("the format's types always serialize");
    line.push(b'\n');
    line
}

/// What the thread is asked to do.
enum Job {
    /// Make the file for a new board, with its header.
    Create { id: u32, header: Vec<u8> },
    /// Add a record to a board's file.
    Append { id: u32, line: Vec<u8> },
    /// Write what's waiting, then say whether everything asked for has
    /// been written, or why not.
    Flush(Sender<Result<(), String>>),
    /// Write what's waiting, and stop.
    Stop,
    /// For tests: be busy for this long.
    #[cfg(test)]
    Stall(Duration),
}

/// How the boards were read, put here by the thread for the event loop.
pub(crate) type Reading = Arc<Mutex<Option<Result<Loaded, String>>>>;

/// The thread that reads and writes the boards.
#[derive(Debug)]
pub(crate) struct Disk {
    jobs: Sender<Job>,
    /// Says when the thread has stopped.
    stopped: Receiver<()>,
    /// Writes that failed, as the thread says why, for the event loop.
    pub(crate) failed: Arc<Mutex<Vec<String>>>,
}

impl Disk {
    /// Starts the thread on the boards in `dir`. It locks them and reads
    /// them, putting what it found in `reading` and calling `wake`, then
    /// writes what it's asked to.
    ///
    /// # Errors
    ///
    /// If the thread can't start.
    pub(crate) fn start(
        dir: PathBuf,
        reading: Reading,
        wake: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let (jobs, receive) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let failed = Arc::new(Mutex::new(Vec::new()));
        let failures = Arc::clone(&failed);
        std::thread::Builder::new()
            .name("boards".into())
            .spawn(move || {
                let outcome = lock(&dir).and_then(|lock| {
                    let loaded = load(&dir).map_err(|err| {
                        format!("the boards in {} couldn't be read: {err}", dir.display())
                    })?;
                    Ok((lock, loaded))
                });
                let lock = match outcome {
                    Ok((lock, loaded)) => {
                        *reading.lock().unwrap_or_else(PoisonError::into_inner) = Some(Ok(loaded));
                        Some(lock)
                    }
                    Err(why) => {
                        log::warn!("{why}");
                        *reading.lock().unwrap_or_else(PoisonError::into_inner) = Some(Err(why));
                        None
                    }
                };
                wake();
                if lock.is_some() {
                    write(&dir, &receive, &failures, &wake);
                }
                drop(lock);
                let _ = stop.send(());
            })?;
        Ok(Self {
            jobs,
            stopped,
            failed,
        })
    }

    /// Makes the file of the board `header` makes.
    pub(crate) fn create(&self, header: &Header) {
        let job = Job::Create {
            id: header.id,
            header: line_of(header),
        };
        let _ = self.jobs.send(job);
    }

    /// Adds `line`, a record as [`line_of`] makes it, to board `id`'s
    /// file.
    pub(crate) fn append(&self, id: u32, line: Vec<u8>) {
        let _ = self.jobs.send(Job::Append { id, line });
    }

    /// Waits up to `wait` for everything asked for so far to be written
    /// and synced. Fails if it isn't in time, or a write failed, which
    /// left a board's file behind what was said of it.
    pub(crate) fn flush(&self, wait: Duration) -> Result<(), String> {
        let (reply, flushed) = mpsc::channel();
        let _ = self.jobs.send(Job::Flush(reply));
        match flushed.recv_timeout(wait) {
            Ok(outcome) => outcome,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "the boards' last changes weren't written within {} s",
                wait.as_secs_f64()
            )),
            Err(RecvTimeoutError::Disconnected) => Err("the boards' thread has stopped".to_owned()),
        }
    }

    /// For tests: keeps the thread busy for `time`.
    #[cfg(test)]
    pub(crate) fn stall(&self, time: Duration) {
        let _ = self.jobs.send(Job::Stall(time));
    }

    /// Has the thread write what's waiting and stop, without waiting for
    /// it: [`stop`](Self::stop) does.
    pub(crate) fn begin_stop(&self) {
        let _ = self.jobs.send(Job::Stop);
    }

    /// Has the thread write what's waiting and stop, if it hasn't been
    /// told to already, waiting up to `wait` for it. Returns whether it
    /// stopped in time.
    pub(crate) fn stop(self, wait: Duration) -> bool {
        self.begin_stop();
        !matches!(
            self.stopped.recv_timeout(wait),
            Err(RecvTimeoutError::Timeout)
        )
    }
}

/// Locks the boards in `dir` for this daemon, making `dir` if it isn't
/// there. Returns the lock, held while the file is open, or why another
/// daemon has it.
fn lock(dir: &Path) -> Result<File, String> {
    let failed = |err: io::Error| format!("the boards in {} can't be kept: {err}", dir.display());
    make_dir(dir).map_err(failed)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join(LOCK))
        .map_err(failed)?;
    // A lock just let go of may still be held a moment by a copy of its
    // file a process starting meanwhile took, until it runs its program,
    // as the workspaces' may (see `saved::lock`): this is the boards'
    // thread, which may wait.
    let patient = std::time::Instant::now() + crate::saved::LOCK_PATIENCE;
    loop {
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(file),
            Err(rustix::io::Errno::WOULDBLOCK) if std::time::Instant::now() < patient => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(rustix::io::Errno::WOULDBLOCK) => {
                return Err(format!(
                    "another clackatoo daemon keeps the boards in {}: this one has none",
                    dir.display()
                ));
            }
            Err(err) => return Err(failed(err.into())),
        }
    }
}

/// Writes what it's asked to until it's asked to stop: each batch of
/// records waiting, then fsyncs the files written. What fails is said in
/// `failed`, with `wake` called.
fn write(dir: &Path, jobs: &Receiver<Job>, failed: &Mutex<Vec<String>>, wake: &impl Fn()) {
    let mut files: HashMap<u32, File> = HashMap::new();
    // Boards a write to failed: nothing more is added to their files,
    // whose records would no longer follow on from what's there.
    let mut broken: HashSet<u32> = HashSet::new();
    let mut stop = false;
    while !stop {
        let Ok(first) = jobs.recv() else { break };
        let mut written: Vec<u32> = Vec::new();
        let mut failures = Vec::new();
        let mut flushes = Vec::new();
        for job in std::iter::once(first).chain(jobs.try_iter()) {
            let outcome = match job {
                Job::Create { id, header } => match create(dir, id, &header) {
                    Ok(file) => {
                        files.insert(id, file);
                        Ok(())
                    }
                    Err(why) => {
                        broken.insert(id);
                        Err(why)
                    }
                },
                Job::Append { id, .. } if broken.contains(&id) => Err(format!(
                    "board {id}'s change isn't saved: an earlier one couldn't be written"
                )),
                Job::Append { id, line } => match append(dir, &mut files, id, &line) {
                    Ok(()) => {
                        written.push(id);
                        Ok(())
                    }
                    Err(why) => {
                        broken.insert(id);
                        Err(why)
                    }
                },
                Job::Flush(reply) => {
                    flushes.push(reply);
                    Ok(())
                }
                Job::Stop => {
                    stop = true;
                    Ok(())
                }
                #[cfg(test)]
                Job::Stall(time) => {
                    std::thread::sleep(time);
                    Ok(())
                }
            };
            if let Err(why) = outcome {
                failures.push(why);
            }
        }
        written.sort_unstable();
        written.dedup();
        for id in written {
            if let Some(file) = files.get(&id)
                && let Err(err) = file.sync_data()
            {
                failures.push(format!("board {id} couldn't be synced: {err}"));
            }
        }
        if !failures.is_empty() {
            for why in &failures {
                log::error!("{why}");
            }
            (failed.lock().unwrap_or_else(PoisonError::into_inner)).extend(failures);
            wake();
        }
        for reply in flushes {
            let mut unsaved: Vec<u32> = broken.iter().copied().collect();
            unsaved.sort_unstable();
            let outcome = if unsaved.is_empty() {
                Ok(())
            } else {
                Err(format!(
                    "boards {unsaved:?} have changes that couldn't be written, which would be lost"
                ))
            };
            let _ = reply.send(outcome);
        }
    }
}

/// Makes board `id`'s file with `header`, in full before it has its
/// name, so a crash never leaves a board without one. Returns it, open to
/// add to.
fn create(dir: &Path, id: u32, header: &[u8]) -> Result<File, String> {
    let failed = |err: io::Error| format!("board {id}'s file couldn't be made: {err}");
    let path = dir.join(format!("{id}.jsonl"));
    let temporary = dir.join(format!("{id}.jsonl.tmp"));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(failed)?;
    file.write_all(header).map_err(failed)?;
    file.sync_all().map_err(failed)?;
    // Never over another board.
    if path.exists() {
        let _ = fs::remove_file(&temporary);
        return Err(format!("board {id}'s file is there already"));
    }
    fs::rename(&temporary, &path).map_err(failed)?;
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(failed)?;
    OpenOptions::new().append(true).open(&path).map_err(failed)
}

/// Adds `line` to board `id`'s file, opening it if it isn't open. A
/// line only partly written, as when the disk is full, is cut off again,
/// so the file still reads.
fn append(dir: &Path, files: &mut HashMap<u32, File>, id: u32, line: &[u8]) -> Result<(), String> {
    let failed = |err: io::Error| format!("board {id}'s change couldn't be written: {err}");
    let file = match files.entry(id) {
        std::collections::hash_map::Entry::Occupied(file) => file.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let file = OpenOptions::new()
                .append(true)
                .open(dir.join(format!("{id}.jsonl")))
                .map_err(failed)?;
            entry.insert(file)
        }
    };
    let before = file.metadata().map_err(failed)?.len();
    file.write_all(line).map_err(|err| {
        if let Err(cut) = file.set_len(before) {
            log::error!("board {id}'s file couldn't be cut back to its last whole line: {cut}");
        }
        failed(err)
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::boards::DEFAULT_CLAIM;

    fn header(id: u32, name: &str) -> Header {
        Header {
            clackatoo_board: VERSION,
            id,
            name: name.to_owned(),
            purpose: String::new(),
            scope: Scope::Global,
            created: 1,
            by: Author::User,
        }
    }

    fn note(entry: u32, text: &str) -> Record {
        Record {
            at: 2,
            by: Author::Chat {
                chat: 3,
                agent: "Codex".into(),
                title: "t".into(),
            },
            event: Event::Post {
                entry,
                kind: Kind::Note,
                text: text.into(),
                links: vec![Link::File {
                    path: "/a".into(),
                    line: Some(1),
                }],
                subject: None,
                expires: None,
            },
        }
    }

    /// Writes a board file in `dir` of `lines`, each with its end.
    fn file(dir: &Path, id: u32, lines: &[Vec<u8>]) -> PathBuf {
        let path = dir.join(format!("{id}.jsonl"));
        fs::write(&path, lines.concat()).unwrap();
        path
    }

    #[test]
    fn the_format_is_as_frozen() {
        // Version 1's lines, exactly: changing them breaks every board on
        // disk.
        assert_eq!(
            String::from_utf8(line_of(&header(4, "plan"))).unwrap(),
            "{\"clackatoo_board\":1,\"id\":4,\"name\":\"plan\",\"purpose\":\"\",\"scope\":\"global\",\
             \"created\":1,\"by\":\"user\"}\n"
        );
        assert_eq!(
            String::from_utf8(line_of(&note(1, "hi"))).unwrap(),
            "{\"at\":2,\"by\":{\"chat\":{\"chat\":3,\"agent\":\"Codex\",\"title\":\"t\"}},\
             \"event\":\"post\",\"entry\":1,\"kind\":\"note\",\"text\":\"hi\",\
             \"links\":[{\"file\":{\"path\":\"/a\",\"line\":1}}],\"subject\":null,\"expires\":null}\n"
        );
        let release = Record {
            at: 5,
            by: Author::User,
            event: Event::Release { entry: 2 },
        };
        assert_eq!(
            String::from_utf8(line_of(&release)).unwrap(),
            "{\"at\":5,\"by\":\"user\",\"event\":\"release\",\"entry\":2}\n"
        );
        let scoped = Header {
            scope: Scope::Workspace("w".into()),
            ..header(1, "x")
        };
        assert!(
            String::from_utf8(line_of(&scoped))
                .unwrap()
                .contains("\"scope\":{\"workspace\":\"w\"}")
        );
        // A field a newer build adds is ignored; one missing is its
        // default.
        let read: Record = serde_json::from_str(
            "{\"at\":1,\"by\":\"user\",\"event\":\"post\",\"entry\":1,\"kind\":\"result\",\
             \"text\":\"x\",\"later\":true}",
        )
        .unwrap();
        assert!(
            matches!(read.event, Event::Post { kind: Kind::Result, ref links, .. } if links.is_empty())
        );
    }

    #[test]
    fn boards_are_read_back_and_a_torn_last_line_is_cut_off() {
        let dir = tempfile::tempdir().unwrap();
        let torn = line_of(&note(2, "cut"));
        let path = file(
            dir.path(),
            7,
            &[
                line_of(&header(7, "plan")),
                line_of(&note(1, "kept")),
                torn[..torn.len() / 2].to_vec(),
            ],
        );
        let whole = line_of(&header(7, "plan")).len() + line_of(&note(1, "kept")).len();
        let loaded = load(dir.path()).unwrap();
        let board = loaded.boards.get(clackatoo_protocol::BoardId(7)).unwrap();
        assert_eq!(board.entries.len(), 1);
        assert_eq!(board.entries[0].text, "kept");
        assert_eq!(loaded.boards.next, 8);
        assert!(loaded.quarantined.is_empty());
        // Cut from the file, so what's added next starts a line of its own.
        assert_eq!(fs::metadata(&path).unwrap().len(), whole as u64);
        // A last line of zeros, as a crash can leave, too.
        let mut zeros = fs::read(&path).unwrap();
        zeros.extend_from_slice(&[0; 40]);
        zeros.push(b'\n');
        fs::write(&path, &zeros).unwrap();
        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.boards.list.len(), 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), whole as u64);
    }

    #[test]
    fn a_damaged_board_is_quarantined_and_a_newer_one_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        file(
            dir.path(),
            1,
            &[
                line_of(&header(1, "damaged")),
                b"{not json\n".to_vec(),
                line_of(&note(1, "after")),
            ],
        );
        // A record that doesn't fit: an entry edited that isn't there.
        let edit = Record {
            at: 1,
            by: Author::User,
            event: Event::Edit {
                entry: 9,
                text: "x".into(),
            },
        };
        file(
            dir.path(),
            2,
            &[
                line_of(&header(2, "misfit")),
                line_of(&edit),
                line_of(&note(1, "x")),
            ],
        );
        // Named for another board.
        file(dir.path(), 3, &[line_of(&header(4, "liar"))]);
        let newer = file(
            dir.path(),
            5,
            &[b"{\"clackatoo_board\":2,\"anything\":true}\n".to_vec()],
        );
        file(dir.path(), 6, &[line_of(&header(6, "fine"))]);
        fs::write(dir.path().join("notes.txt"), "not a board").unwrap();
        let loaded = load(dir.path()).unwrap();
        let names: Vec<&str> = (loaded.boards.list.values())
            .map(|board| board.summary.name.as_str())
            .collect();
        assert_eq!(names, ["fine"]);
        assert_eq!(loaded.quarantined.len(), 3);
        assert!(
            loaded
                .quarantined
                .iter()
                .all(|path| path.starts_with(dir.path().join(QUARANTINE)))
        );
        assert_eq!(loaded.newer, std::slice::from_ref(&newer));
        assert!(newer.exists(), "left where it was");
        // No ID is used twice, a quarantined board's included.
        assert_eq!(loaded.boards.next, 7);
        fs::remove_file(dir.path().join("6.jsonl")).unwrap();
        fs::remove_file(&newer).unwrap();
        assert_eq!(load(dir.path()).unwrap().boards.next, 4);
    }

    /// Waits for `reading` to be filled.
    fn wait_for(reading: &Reading) -> Result<Loaded, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(read) = reading.lock().unwrap().take() {
                return read;
            }
            assert!(Instant::now() < deadline, "the boards were never read");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_file_bigger_than_a_board_can_be_is_left_alone_unread() {
        let dir = tempfile::tempdir().unwrap();
        let path = file(dir.path(), 3, &[line_of(&header(3, "huge"))]);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_FILE + 1)
            .unwrap();
        let loaded = load(dir.path()).unwrap();
        assert!(loaded.boards.list.is_empty());
        assert_eq!(loaded.newer, std::slice::from_ref(&path));
        assert!(loaded.quarantined.is_empty());
        assert!(path.exists());
    }

    #[test]
    fn a_flush_waits_for_what_was_asked_and_says_if_it_isnt_in_time() {
        let dir = tempfile::tempdir().unwrap();
        let reading = Reading::default();
        let disk = Disk::start(dir.path().to_owned(), Arc::clone(&reading), || {}).unwrap();
        let mut boards = wait_for(&reading).unwrap().boards;
        let header = boards
            .create("plan", "", BoardScope::Global, &BoardAuthor::User, 1)
            .unwrap();
        let id = boards.add(&header);
        disk.create(&header);
        disk.stall(Duration::from_millis(500));
        let post = boards.get(id).unwrap();
        let post = post
            .post(&BoardAuthor::User, 2, PostKind::Note, "x", Vec::new())
            .unwrap();
        disk.append(id.0, line_of(&post));
        let late = disk.flush(Duration::from_millis(50)).unwrap_err();
        assert!(late.contains("weren't written within"), "{late}");
        assert_eq!(disk.flush(Duration::from_secs(5)), Ok(()));
        let read = load(dir.path()).unwrap();
        assert_eq!(read.boards.get(id).unwrap().entries.len(), 1, "on disk");
        assert!(disk.stop(Duration::from_secs(5)));
    }

    #[test]
    fn the_thread_writes_boards_that_read_back_and_holds_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let reading = Reading::default();
        let disk = Disk::start(dir.path().to_owned(), Arc::clone(&reading), || {}).unwrap();
        let mut boards = wait_for(&reading).unwrap().boards;
        // Another daemon finds them held.
        let other = Reading::default();
        let second = Disk::start(dir.path().to_owned(), Arc::clone(&other), || {}).unwrap();
        let refused = wait_for(&other).unwrap_err();
        assert!(
            refused.contains("another clackatoo daemon keeps the boards"),
            "{refused}"
        );
        assert!(second.stop(Duration::from_secs(5)));

        let user = BoardAuthor::User;
        let header = boards
            .create("plan", "for the work", BoardScope::Global, &user, 1)
            .unwrap();
        let id = boards.add(&header);
        disk.create(&header);
        let board = boards.get_mut(id).unwrap();
        let claim = board
            .claim(&user, 2, "lexer", "mine", DEFAULT_CLAIM)
            .unwrap();
        board.apply(&claim).unwrap();
        disk.append(id.0, line_of(&claim));
        assert!(disk.stop(Duration::from_secs(5)));
        assert!(!dir.path().join(format!("{id}.jsonl.tmp")).exists());

        let again = load(dir.path()).unwrap();
        let read = again.boards.get(id).unwrap();
        assert_eq!(read.summary.purpose, "for the work");
        assert_eq!(read.entries, boards.get(id).unwrap().entries);
        assert_eq!(again.boards.next, id.0 + 1);
    }
}
