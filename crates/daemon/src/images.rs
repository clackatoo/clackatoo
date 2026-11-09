//! The images panes show, kept once each (CLACKA-239): the one store every
//! image goes through, whatever shows it, a chat's turn, a picture pane or
//! an editor's Markdown (and, to come, terminals' images).
//!
//! An image is kept as it was sent, PNG as PNG and JPEG as JPEG, by the ID
//! its bytes make ([`ImageId`]); pixels sent bare are kept as a PNG. All
//! that takes time, reading a file, hashing, looking at what the bytes are,
//! encoding and making copies at a smaller size, is done on a thread of the
//! store's own, never on the loop: the loop asks ([`Images::keep`]), and
//! takes what's done once the thread wakes it ([`Images::take`]).
//!
//! Each image belongs to one or more owners ([`ImageFrom`]), and is let go
//! once none holds it. Owners are kept within a limit each, and all images
//! within a limit in all ([`Limits`]): past either, the image used longest
//! ago that isn't pinned (as on screen) is let go, and a mark of it kept,
//! so what showed it can say it was dropped. An image bigger than an
//! owner's limit by itself isn't kept at all.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::io::{Cursor, Read as _};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use clackatoo_protocol::{
    BufferId, ImageAnswer, ImageFormat, ImageFrom, ImageId, ImageInfo, PictureId, TerminalId,
};

use crate::regular::{self, Links};

/// A mebibyte.
pub(crate) const MIB: u64 = 1024 * 1024;

/// How many images let go are remembered, to say they were dropped.
const DROPPED_KEPT: usize = 4096;

/// How many bytes of copies at a smaller size are kept.
const COPIES_BYTES: u64 = 32 * MIB;

/// How many sizes of images a copy couldn't be kept at are remembered, so
/// a copy isn't made again, and again, for each asking.
const UNCOPIED_KEPT: usize = 256;

/// How many copies too big to keep are held for those who asked, until
/// they've had them.
const ONCE_KEPT: usize = 2;

/// The most bytes an image is sent as it is, at a size it can't be made
/// smaller at: past this, asking for it fails.
const ORIGINAL_INSTEAD: u64 = 16 * MIB;

/// How many files are read at once, each on a thread of its own, so a
/// file that hangs, as on a mount gone away, holds up only its own.
const READERS: usize = 4;

/// How many files may wait to be read: past that, more are refused.
const READ_QUEUE: usize = 64;

/// How long a file is waited for, read or waiting to be, before it's
/// given up on: a reader that hangs past it keeps its place until it
/// comes back, but nothing waits for it.
const READ_WAIT: Duration = Duration::from_secs(10);

/// How many bytes sent to be kept may be on their way to the thread at
/// once: past that, more are refused until it catches up.
const ON_THE_WAY: u64 = 256 * MIB;

/// The sides copies are made at: a copy asked for at any size is made at
/// the next of these up, so copies are shared between sizes near it.
const SIDES: [u32; 12] = [
    64, 128, 192, 256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096,
];

/// How much memory images may take: each owner's, and all of them
/// together, in bytes as they're kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Each owner's: a terminal's, a chat's, a picture pane's or a
    /// buffer's.
    pub per_owner: u64,
    /// All images together.
    pub total: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_owner: 64 * MIB,
            total: 256 * MIB,
        }
    }
}

impl Limits {
    /// The limits `value` gives, as `$CLACKATOO_IMAGE_MEMORY` does: two
    /// numbers of mebibytes, each owner's and all together's, as `64,256`;
    /// or the defaults for none, or for anything else, which is logged.
    #[must_use]
    pub fn from_env(value: Option<&OsStr>) -> Self {
        let Some(value) = value else {
            return Self::default();
        };
        let parsed = value.to_str().and_then(|value| {
            let (each, all) = value.split_once(',')?;
            let each: u64 = each.trim().parse().ok()?;
            let all: u64 = all.trim().parse().ok()?;
            (each > 0 && each <= all && all <= 1 << 20).then_some(Self {
                per_owner: each * MIB,
                total: all * MIB,
            })
        });
        parsed.unwrap_or_else(|| {
            log::warn!(
                "CLACKATOO_IMAGE_MEMORY is two numbers of MiB, each owner's and all's, \
                 as 64,256: taking those"
            );
            Self::default()
        })
    }
}

/// Where an image's bytes come from.
#[derive(Debug)]
pub(crate) enum Source {
    /// Bytes as they were sent.
    Bytes(Vec<u8>),
    /// Bytes as standard base64, as agents and windows send them.
    Base64(String),
    /// A file the user opened, read whole as [`regular::open`] opens one,
    /// following a link at its path.
    File(PathBuf),
    /// A file at `path` below the folder `root`, as a Markdown file names
    /// its images: never through a link, at any part of the path, nor out
    /// of `root` (see [`regular::open_below`]).
    Below {
        /// The folder.
        root: PathBuf,
        /// The path below it, written plainly, with no `..`.
        path: PathBuf,
    },
    /// Pixels, four bytes each (red, green, blue, alpha), row by row:
    /// kept as a PNG. As terminals' programs send them (CLACKA-239, part
    /// 2).
    #[cfg_attr(not(test), expect(dead_code, reason = "terminals' images come next"))]
    Pixels {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
        /// The pixels.
        rgba: Vec<u8>,
    },
    /// For tests: a file that takes this long to read, as one on a mount
    /// gone away hangs, then isn't there.
    #[cfg(test)]
    Hang(Duration),
}

impl Source {
    /// Whether it's a file, read by a reader of its own.
    fn is_file(&self) -> bool {
        #[cfg(test)]
        if matches!(self, Self::Hang(_)) {
            return true;
        }
        matches!(self, Self::File(_) | Self::Below { .. })
    }

    /// About how many bytes it holds on its way to the thread.
    fn weight(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Base64(data) => data.len() as u64,
            Self::Pixels { rgba, .. } => rgba.len() as u64,
            Self::File(_) | Self::Below { .. } => 0,
            #[cfg(test)]
            Self::Hang(_) => 0,
        }
    }
}

/// What an image is kept for: what's done with it once it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// A picture pane's file.
    Picture(PictureId),
    /// An image a buffer's Markdown shows, at `path`, as it gives it.
    Markdown {
        /// The buffer.
        buffer: BufferId,
        /// The path.
        path: String,
    },
    /// The original of a chat's thumbnail with this handle.
    Chat {
        /// The chat.
        chat: TerminalId,
        /// The thumbnail's handle (see `ChatPreview::original`).
        original: u64,
    },
    /// For tests: a number to know it by.
    #[cfg(test)]
    Test(u32),
}

/// An image kept, or not, for the loop to act on.
#[derive(Debug)]
pub(crate) struct Kept {
    /// What it was for.
    pub(crate) why: Why,
    /// The image, or why it wasn't kept.
    pub(crate) kept: Result<ImageInfo, String>,
}

/// A copy at a smaller size that's ready, and who asked for it, to be
/// told.
#[derive(Debug)]
pub(crate) struct Ready<T> {
    /// The image.
    pub(crate) image: ImageId,
    /// Who asked: each with what they named as the image's owner, and
    /// the size they asked for.
    pub(crate) waiting: Vec<(T, ImageFrom, [u32; 2])>,
}

/// Work for the store's thread.
enum Job {
    /// Read, look at and hash what `source` holds, of no more than `most`
    /// bytes.
    Keep {
        why: Why,
        owner: ImageFrom,
        source: Source,
        most: u64,
    },
    /// Make a copy of `bytes` no wider or taller than `side`.
    Scale {
        info: ImageInfo,
        bytes: Arc<[u8]>,
        side: u32,
    },
}

/// What the store's thread, or a reader, did.
enum Done {
    /// A reader read, looked at and hashed the file of read `read`.
    Read {
        read: u64,
        kept: Result<(ImageInfo, Arc<[u8]>), String>,
    },
    Kept {
        why: Why,
        owner: ImageFrom,
        weight: u64,
        kept: Result<(ImageInfo, Arc<[u8]>), String>,
    },
    Scaled {
        image: ImageId,
        side: u32,
        copy: Option<(ImageInfo, Arc<[u8]>)>,
    },
}

/// An image kept.
#[derive(Debug)]
struct Entry {
    info: ImageInfo,
    bytes: Arc<[u8]>,
    /// Who holds it.
    owners: Vec<ImageFrom>,
    /// How many times it's pinned, as on screen: while it is, it isn't
    /// let go.
    pins: u32,
    /// When it was last kept, shown or fetched, as a count of such.
    used: u64,
}

/// A copy of an image at a smaller size.
#[derive(Debug)]
struct Copy {
    info: ImageInfo,
    bytes: Arc<[u8]>,
    used: u64,
}

/// What a fetch found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found<'a> {
    /// The image, as it's kept.
    Kept {
        /// What it is.
        info: ImageInfo,
        /// Its bytes.
        bytes: &'a [u8],
    },
    /// It was let go to keep within this limit.
    Dropped(u64),
    /// There's none such, or not the owner's.
    Missing,
}

/// Who's waiting for a copy: each with what they named as the image's
/// owner, and the size they asked for.
type Waiting<T> = Vec<(T, ImageFrom, [u32; 2])>;

/// The store. `T` is who asks for copies, told when they're ready: a
/// client's token.
#[derive(Debug)]
pub(crate) struct Images<T> {
    limits: Limits,
    entries: HashMap<ImageId, Entry>,
    /// Images let go, with the limit each was let go for, oldest first.
    dropped: VecDeque<(ImageId, u64)>,
    /// How many bytes each owner holds.
    owners: HashMap<ImageFrom, u64>,
    total: u64,
    tick: u64,
    copies: HashMap<(ImageId, u32), Copy>,
    copies_bytes: u64,
    /// The most bytes of copies kept: [`COPIES_BYTES`], less in tests.
    copies_most: u64,
    /// The sizes of images a copy couldn't be made at, or kept at, oldest
    /// first: asked for again, the image as it is is sent, if it isn't
    /// too big, and else asking fails. Never made again.
    uncopied: VecDeque<(ImageId, u32)>,
    /// Copies too big to keep, held until those who asked have had them
    /// whole, oldest first.
    once: VecDeque<((ImageId, u32), Copy)>,
    /// Copies being made, and who's waiting for each.
    scaling: HashMap<(ImageId, u32), Waiting<T>>,
    /// Bytes on their way to the thread.
    on_the_way: u64,
    /// Jobs sent to the thread and not done yet.
    pending: usize,
    /// The store's thread, started when it's first needed.
    jobs: Option<mpsc::Sender<Job>>,
    /// What the thread and the readers have done.
    done: (mpsc::Sender<Done>, mpsc::Receiver<Done>),
    /// The files waiting to be read, oldest first, each with when it's
    /// given up on.
    to_read: VecDeque<(u64, ToRead)>,
    /// The files being read, each with when it's given up on.
    reading: HashMap<u64, (Why, ImageFrom, Instant)>,
    /// How many readers are out, those given up on among them.
    readers: usize,
    /// The number the next file read gets.
    next_read: u64,
    /// How long a file is waited for: [`READ_WAIT`], less in tests.
    read_wait: Duration,
    waker: Option<Arc<mio::Waker>>,
}

/// A file to read, as it waits.
#[derive(Debug)]
struct ToRead {
    why: Why,
    owner: ImageFrom,
    source: Source,
    due: Instant,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Job")
    }
}

impl std::fmt::Debug for Done {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Done")
    }
}

impl<T: Clone + PartialEq> Images<T> {
    /// A store within `limits`, whose thread wakes the loop with `waker`.
    pub(crate) fn new(limits: Limits, waker: Option<Arc<mio::Waker>>) -> Self {
        Self {
            limits,
            entries: HashMap::new(),
            dropped: VecDeque::new(),
            owners: HashMap::new(),
            total: 0,
            tick: 0,
            copies: HashMap::new(),
            copies_bytes: 0,
            copies_most: COPIES_BYTES,
            uncopied: VecDeque::new(),
            once: VecDeque::new(),
            scaling: HashMap::new(),
            on_the_way: 0,
            pending: 0,
            jobs: None,
            done: mpsc::channel(),
            to_read: VecDeque::new(),
            reading: HashMap::new(),
            readers: 0,
            next_read: 0,
            read_wait: READ_WAIT,
            waker,
        }
    }

    /// The limits it keeps within.
    pub(crate) fn limits(&self) -> Limits {
        self.limits
    }

    /// Has the image `source` holds kept for `owner`, for `why`: on the
    /// store's thread, and then, once [`take`](Self::take) takes it, here.
    /// Refused at once, saying why, if too much is on its way already.
    pub(crate) fn keep(
        &mut self,
        owner: ImageFrom,
        why: Why,
        source: Source,
    ) -> Result<(), String> {
        if source.is_file() {
            if self.to_read.len() >= READ_QUEUE {
                return Err("too many files to read at once: try again in a moment".into());
            }
            self.next_read += 1;
            // Waiting its turn, as long as three reads may take.
            let due = Instant::now() + 3 * self.read_wait;
            let read = ToRead {
                why,
                owner,
                source,
                due,
            };
            self.to_read.push_back((self.next_read, read));
            self.start_reading();
            return Ok(());
        }
        let weight = source.weight();
        if self.on_the_way.saturating_add(weight) > ON_THE_WAY {
            return Err("too many images at once: try again in a moment".into());
        }
        let most = self.limits.per_owner;
        self.send(Job::Keep {
            why,
            owner,
            source,
            most,
        })?;
        self.on_the_way += weight;
        Ok(())
    }

    /// Whether there's work on the thread not taken back yet.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.pending > 0
    }

    /// Starts reading the files waiting, as readers come free: each on a
    /// thread of its own, which wakes the loop once it's read.
    fn start_reading(&mut self) {
        while self.readers < READERS
            && let Some((read, to_read)) = self.to_read.pop_front()
        {
            let ToRead {
                why,
                owner,
                source,
                due: _,
            } = to_read;
            let (done, waker) = (self.done.0.clone(), self.waker.clone());
            let most = self.limits.per_owner;
            let spawned = std::thread::Builder::new()
                .name("image-reader".into())
                .spawn(move || {
                    let kept = read_source(source, most).and_then(|bytes| examine(bytes, most));
                    let _ = done.send(Done::Read { read, kept });
                    if let Some(waker) = &waker {
                        let _ = waker.wake();
                    }
                });
            match spawned {
                Ok(_) => {
                    self.readers += 1;
                    let due = Instant::now() + self.read_wait;
                    self.reading.insert(read, (why, owner, due));
                }
                Err(err) => {
                    let why_not = format!("couldn't start reading it: {err}");
                    self.reading.insert(read, (why, owner, Instant::now()));
                    let _ = self.done.0.send(Done::Read {
                        read,
                        kept: Err(why_not),
                    });
                    self.readers += 1;
                }
            }
        }
    }

    /// When the soonest file read, or waiting to be, is to be given up on.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        (self.reading.values().map(|&(.., due)| due))
            .chain(self.to_read.iter().map(|(_, to_read)| to_read.due))
            .min()
    }

    fn send(&mut self, job: Job) -> Result<(), String> {
        if self.jobs.is_none() {
            let (jobs, receive) = mpsc::channel::<Job>();
            let done = self.done.0.clone();
            let waker = self.waker.clone();
            std::thread::Builder::new()
                .name("images".into())
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        if done.send(work(job)).is_err() {
                            break;
                        }
                        if let Some(waker) = &waker {
                            let _ = waker.wake();
                        }
                    }
                })
                .map_err(|err| format!("couldn't start the images' thread: {err}"))?;
            self.jobs = Some(jobs);
        }
        let jobs = self.jobs.as_ref().expect("started above");
        jobs.send(job)
            .map_err(|_| "the images' thread has stopped".to_owned())?;
        self.pending += 1;
        Ok(())
    }

    /// Takes in what the thread and the readers have done: the images
    /// kept, or not, for their owners to act on, and the copies made, for
    /// who asked to be told; and gives up on the files waited for too long.
    /// With `wait`, waits that long at most for everything to be done, as
    /// before a handover: what isn't done by then is given up on.
    pub(crate) fn take(&mut self, wait: Option<Duration>) -> (Vec<Kept>, Vec<Ready<T>>) {
        let mut kept = Vec::new();
        let mut ready = Vec::new();
        let until = wait.map(|wait| Instant::now() + wait);
        while let Some(done) = self.next_done(until) {
            match done {
                Done::Read { read, kept: result } => {
                    self.readers = self.readers.saturating_sub(1);
                    // Given up on meanwhile: nothing waits for it.
                    if let Some((why, owner, _)) = self.reading.remove(&read) {
                        let result =
                            result.and_then(|(info, bytes)| self.insert(owner, info, bytes));
                        kept.push(Kept { why, kept: result });
                    }
                    self.start_reading();
                    continue;
                }
                _ => self.pending = self.pending.saturating_sub(1),
            }
            match done {
                Done::Read { .. } => {}
                Done::Kept {
                    why,
                    owner,
                    weight,
                    kept: result,
                } => {
                    self.on_the_way = self.on_the_way.saturating_sub(weight);
                    let result = result.and_then(|(info, bytes)| self.insert(owner, info, bytes));
                    kept.push(Kept { why, kept: result });
                }
                Done::Scaled { image, side, copy } => {
                    let waiting = self.scaling.remove(&(image, side)).unwrap_or_default();
                    match copy {
                        Some((info, bytes)) if info.bytes <= self.copies_most => {
                            self.keep_copy(image, side, info, bytes);
                        }
                        // Too big to keep: held for those waiting, until
                        // they've had it, and never made again.
                        Some((info, bytes)) => {
                            self.uncopied_at(image, side);
                            if self.once.len() >= ONCE_KEPT {
                                self.once.pop_front();
                            }
                            let copy = Copy {
                                info,
                                bytes,
                                used: self.tick,
                            };
                            self.once.push_back(((image, side), copy));
                        }
                        // It couldn't be made: those waiting, asking again,
                        // are sent the image as it is, or told it fails.
                        None => self.uncopied_at(image, side),
                    }
                    if !waiting.is_empty() {
                        ready.push(Ready { image, waiting });
                    }
                }
            }
        }
        self.give_up_reading(wait.is_some(), &mut kept);
        (kept, ready)
    }

    /// What the thread or a reader has done next, if anything: waiting
    /// for it until `until`, if there's work out.
    fn next_done(&self, until: Option<Instant>) -> Option<Done> {
        let working = self.pending > 0 || !self.reading.is_empty() || !self.to_read.is_empty();
        match until {
            Some(until) if working => {
                let left = until.saturating_duration_since(Instant::now());
                self.done.1.recv_timeout(left).ok()
            }
            _ => self.done.1.try_recv().ok(),
        }
    }

    /// Gives up on the files waited for past their time, and, if `all`, on
    /// every one: said as not kept, for their owners to act on.
    fn give_up_reading(&mut self, all: bool, kept: &mut Vec<Kept>) {
        let now = Instant::now();
        let late: Vec<u64> = (self.reading.iter())
            .filter(|&(_, &(.., due))| all || due <= now)
            .map(|(&read, _)| read)
            .collect();
        for read in late {
            if let Some((why, ..)) = self.reading.remove(&read) {
                let kept_not = Err("its file took too long to read".to_owned());
                kept.push(Kept {
                    why,
                    kept: kept_not,
                });
            }
        }
        while let Some((_, to_read)) = self.to_read.front() {
            if !all && to_read.due > now {
                break;
            }
            let (_, to_read) = self.to_read.pop_front().expect("looked at above");
            let kept_not = Err("too many files to read at once".to_owned());
            kept.push(Kept {
                why: to_read.why,
                kept: kept_not,
            });
        }
    }

    /// Keeps `bytes`, an image `info` says, for `owner`, letting older
    /// ones go as the limits say: refused if it's bigger than an owner's
    /// limit by itself.
    pub(crate) fn insert(
        &mut self,
        owner: ImageFrom,
        info: ImageInfo,
        bytes: Arc<[u8]>,
    ) -> Result<ImageInfo, String> {
        if info.bytes > self.limits.per_owner || info.bytes > self.limits.total {
            return Err(format!(
                "it's over the {} MB limit for images",
                self.limits.per_owner / MIB
            ));
        }
        self.tick += 1;
        let tick = self.tick;
        let entry = self.entries.entry(info.id).or_insert_with(|| {
            self.total += info.bytes;
            Entry {
                info,
                bytes,
                owners: Vec::new(),
                pins: 0,
                used: tick,
            }
        });
        entry.used = tick;
        let added = !entry.owners.contains(&owner);
        if added {
            entry.owners.push(owner);
            *self.owners.entry(owner).or_default() += info.bytes;
        }
        self.dropped.retain(|(id, _)| *id != info.id);
        // What's just kept is in use: only older images go for it.
        entry.pins += 1;
        self.make_room();
        if let Some(entry) = self.entries.get_mut(&info.id) {
            entry.pins -= 1;
        }
        // The limits hold, whatever's pinned: past them, it isn't kept.
        let over =
            self.total > self.limits.total || self.bytes(Some(owner)) > self.limits.per_owner;
        if over && added {
            self.let_go_of_quietly(info.id, owner);
            return Err(format!(
                "it's over the {} MB limit for images, with those shown",
                self.limits.total / MIB
            ));
        }
        Ok(info)
    }

    /// Lets images go until every owner, and all together, are within
    /// the limits, oldest first, but for those pinned.
    fn make_room(&mut self) {
        let over: Vec<ImageFrom> = (self.owners.iter())
            .filter(|&(_, &bytes)| bytes > self.limits.per_owner)
            .map(|(&owner, _)| owner)
            .collect();
        for owner in over {
            while self.owners.get(&owner).copied().unwrap_or(0) > self.limits.per_owner {
                let oldest = (self.entries.iter())
                    .filter(|(_, entry)| entry.pins == 0 && entry.owners.contains(&owner))
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(&id, _)| id);
                let Some(id) = oldest else {
                    break;
                };
                self.let_go_of(id, owner, self.limits.per_owner);
            }
        }
        while self.total > self.limits.total {
            let oldest = (self.entries.iter())
                .filter(|(_, entry)| entry.pins == 0)
                .min_by_key(|(_, entry)| entry.used)
                .map(|(&id, _)| id);
            let Some(id) = oldest else {
                break;
            };
            self.drop_image(id, self.limits.total);
        }
    }

    /// Lets `owner` go of image `id`, which goes if nobody else holds it,
    /// remembered as dropped for `limit`.
    fn let_go_of(&mut self, id: ImageId, owner: ImageFrom, limit: u64) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        entry.owners.retain(|held| *held != owner);
        let bytes = entry.info.bytes;
        self.lose_bytes(owner, bytes);
        if self.entries[&id].owners.is_empty() {
            self.forget(id);
            self.remember_dropped(id, limit);
        }
    }

    /// Lets `owner` go of image `id`, which goes if nobody else holds it,
    /// not remembered as dropped, as it was never shown.
    fn let_go_of_quietly(&mut self, id: ImageId, owner: ImageFrom) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        entry.owners.retain(|held| *held != owner);
        let bytes = entry.info.bytes;
        self.lose_bytes(owner, bytes);
        if self.entries[&id].owners.is_empty() {
            self.forget(id);
        }
    }

    /// Lets image `id` go for every owner, remembered as dropped for
    /// `limit`.
    fn drop_image(&mut self, id: ImageId, limit: u64) {
        let Some(entry) = self.entries.get(&id) else {
            return;
        };
        let (owners, bytes) = (entry.owners.clone(), entry.info.bytes);
        for owner in owners {
            self.lose_bytes(owner, bytes);
        }
        self.forget(id);
        self.remember_dropped(id, limit);
    }

    fn lose_bytes(&mut self, owner: ImageFrom, bytes: u64) {
        if let Some(held) = self.owners.get_mut(&owner) {
            *held = held.saturating_sub(bytes);
            if *held == 0 {
                self.owners.remove(&owner);
            }
        }
    }

    /// Takes image `id` out, and its copies.
    fn forget(&mut self, id: ImageId) {
        if let Some(entry) = self.entries.remove(&id) {
            self.total = self.total.saturating_sub(entry.info.bytes);
        }
        let copies: Vec<_> = (self.copies.keys())
            .filter(|(image, _)| *image == id)
            .copied()
            .collect();
        for key in copies {
            if let Some(copy) = self.copies.remove(&key) {
                self.copies_bytes -= copy.info.bytes;
            }
        }
    }

    fn remember_dropped(&mut self, id: ImageId, limit: u64) {
        if self.dropped.len() >= DROPPED_KEPT {
            self.dropped.pop_front();
        }
        self.dropped.push_back((id, limit));
    }

    /// Lets every image `owner` holds go, as it's closed: those nobody
    /// else holds go for good, not remembered as dropped, as nothing shows
    /// them now.
    pub(crate) fn release(&mut self, owner: ImageFrom) {
        if self.owners.remove(&owner).is_none() {
            return;
        }
        let gone: Vec<ImageId> = (self.entries.iter_mut())
            .filter_map(|(&id, entry)| {
                entry.owners.retain(|held| *held != owner);
                entry.owners.is_empty().then_some(id)
            })
            .collect();
        for id in gone {
            self.forget(id);
        }
    }

    /// Pins image `id`, as shown: it isn't let go while it's pinned.
    pub(crate) fn pin(&mut self, id: ImageId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.pins += 1;
        }
    }

    /// Takes a pin [`pin`](Self::pin) put on image `id` off again.
    pub(crate) fn unpin(&mut self, id: ImageId) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.pins = entry.pins.saturating_sub(1);
        }
        self.make_room();
    }

    /// Image `id`, if `owner` holds it, as it's kept: counted as used.
    pub(crate) fn get(&mut self, owner: ImageFrom, id: ImageId) -> Found<'_> {
        self.tick += 1;
        let tick = self.tick;
        match self.entries.get_mut(&id) {
            Some(entry) if entry.owners.contains(&owner) => {
                entry.used = tick;
                Found::Kept {
                    info: entry.info,
                    bytes: &entry.bytes,
                }
            }
            Some(_) => Found::Missing,
            None => match self
                .dropped
                .iter()
                .rev()
                .find(|(dropped, _)| *dropped == id)
            {
                Some(&(_, limit)) => Found::Dropped(limit),
                None => Found::Missing,
            },
        }
    }

    /// What's known of image `id`, if it's kept.
    pub(crate) fn info(&self, id: ImageId) -> Option<ImageInfo> {
        self.entries.get(&id).map(|entry| entry.info)
    }

    /// Whether `owner` holds image `id`.
    pub(crate) fn holds(&self, owner: ImageFrom, id: ImageId) -> bool {
        (self.entries.get(&id)).is_some_and(|entry| entry.owners.contains(&owner))
    }

    /// How many bytes all images take, and how many `owner`'s do.
    pub(crate) fn bytes(&self, owner: Option<ImageFrom>) -> u64 {
        match owner {
            Some(owner) => self.owners.get(&owner).copied().unwrap_or(0),
            None => self.total,
        }
    }

    /// Answers `asker`'s [`Request::Image`](clackatoo_protocol::Request::Image)
    /// for image `id` of `owner`'s: the bytes from `at`, of the image, or
    /// of a copy no bigger than `fit`, which, if it isn't made yet, is
    /// started, and `asker` told when it's ready.
    pub(crate) fn fetch(
        &mut self,
        asker: T,
        owner: ImageFrom,
        id: ImageId,
        fit: Option<[u32; 2]>,
        at: u64,
    ) -> ImageAnswer {
        let (info, bytes) = match self.get(owner, id) {
            Found::Kept { info, .. } => (info, Arc::clone(&self.entries[&id].bytes)),
            Found::Dropped(limit) => return ImageAnswer::Dropped { limit },
            Found::Missing => return ImageAnswer::Missing,
        };
        let Some(side) = fit.and_then(|fit| side_for(info, fit)) else {
            return part(info, &bytes, at);
        };
        self.tick += 1;
        let tick = self.tick;
        if let Some(copy) = self.copies.get_mut(&(id, side)) {
            copy.used = tick;
            return part(copy.info, &copy.bytes, at);
        }
        if let Some(held) = self.once.iter().position(|(key, _)| *key == (id, side)) {
            let copy = &self.once[held].1;
            let answer = part(copy.info, &copy.bytes, at);
            // Had whole: let go.
            if let ImageAnswer::Part {
                at, bytes, total, ..
            } = &answer
                && at + bytes.len() as u64 >= *total
            {
                self.once.remove(held);
            }
            return answer;
        }
        if self.uncopied.contains(&(id, side)) {
            return if info.bytes <= ORIGINAL_INSTEAD {
                part(info, &bytes, at)
            } else {
                ImageAnswer::Failed("it can't be made smaller to send".into())
            };
        }
        let fit = fit.unwrap_or_default();
        if let Some(waiting) = self.scaling.get_mut(&(id, side)) {
            if !waiting.contains(&(asker.clone(), owner, fit)) {
                waiting.push((asker, owner, fit));
            }
            return ImageAnswer::Scaling;
        }
        if self
            .send(Job::Scale {
                info,
                bytes: Arc::clone(&bytes),
                side,
            })
            .is_err()
        {
            // No copy can be made: the image as it is, then.
            return part(info, &bytes, at);
        }
        self.scaling.insert((id, side), vec![(asker, owner, fit)]);
        ImageAnswer::Scaling
    }

    /// Remembers image `image` can't be had at `side`.
    fn uncopied_at(&mut self, image: ImageId, side: u32) {
        self.copies.remove(&(image, side));
        if self.uncopied.contains(&(image, side)) {
            return;
        }
        if self.uncopied.len() >= UNCOPIED_KEPT {
            self.uncopied.pop_front();
        }
        self.uncopied.push_back((image, side));
    }

    /// Keeps at most `most` bytes of copies, as tests do.
    #[cfg(test)]
    pub(crate) fn keep_copies_within(&mut self, most: u64) {
        self.copies_most = most;
    }

    fn keep_copy(&mut self, image: ImageId, side: u32, info: ImageInfo, bytes: Arc<[u8]>) {
        if !self.entries.contains_key(&image) || info.bytes > self.copies_most {
            return;
        }
        self.tick += 1;
        self.copies_bytes += info.bytes;
        if let Some(old) = self.copies.insert(
            (image, side),
            Copy {
                info,
                bytes,
                used: self.tick,
            },
        ) {
            self.copies_bytes -= old.info.bytes;
        }
        while self.copies_bytes > self.copies_most {
            let Some(oldest) = (self.copies.iter())
                .min_by_key(|(_, copy)| copy.used)
                .map(|(&key, _)| key)
            else {
                break;
            };
            if let Some(copy) = self.copies.remove(&oldest) {
                self.copies_bytes -= copy.info.bytes;
            }
        }
    }

    /// Forgets `asker`, gone, as waiting for copies.
    pub(crate) fn forget_asker(&mut self, asker: &T) {
        for waiting in self.scaling.values_mut() {
            waiting.retain(|(waiter, ..)| waiter != asker);
        }
    }

    /// Every image kept, for the handover: each with its bytes, owners
    /// and pins.
    pub(crate) fn all(&self) -> impl Iterator<Item = (ImageInfo, &[u8], &[ImageFrom], u32, u64)> {
        (self.entries.values()).map(|entry| {
            (
                entry.info,
                &*entry.bytes,
                entry.owners.as_slice(),
                entry.pins,
                entry.used,
            )
        })
    }

    /// The images let go and remembered as such, oldest first.
    pub(crate) fn dropped(&self) -> impl Iterator<Item = (ImageId, u64)> + '_ {
        self.dropped.iter().copied()
    }

    /// Takes back an image handed over: kept as it was, with its owners
    /// and pins, if its bytes are what `info` says. Refused if it would
    /// go past the limits.
    pub(crate) fn resume(
        &mut self,
        info: ImageInfo,
        bytes: Vec<u8>,
        owners: &[ImageFrom],
        pins: u32,
    ) -> bool {
        if bytes.len() as u64 != info.bytes
            || owners.is_empty()
            || self.entries.contains_key(&info.id)
            || self.total + info.bytes > self.limits.total
            || owners
                .iter()
                .any(|owner| self.bytes(Some(*owner)) + info.bytes > self.limits.per_owner)
        {
            return false;
        }
        self.tick += 1;
        self.total += info.bytes;
        for owner in owners {
            *self.owners.entry(*owner).or_default() += info.bytes;
        }
        self.entries.insert(
            info.id,
            Entry {
                info,
                bytes: bytes.into(),
                owners: owners.to_vec(),
                pins,
                used: self.tick,
            },
        );
        true
    }

    /// Takes back the mark of an image let go, handed over.
    pub(crate) fn resume_dropped(&mut self, id: ImageId, limit: u64) {
        if !self.entries.contains_key(&id) {
            self.remember_dropped(id, limit);
        }
    }
}

/// The side a copy of the image `info` describes no bigger than `fit` is
/// made at, if one is smaller than the image.
fn side_for(info: ImageInfo, fit: [u32; 2]) -> Option<u32> {
    let [width, height] = fit;
    if width == 0 || height == 0 {
        return None;
    }
    // How long the image's longer side is, fitted.
    let scale =
        (f64::from(width) / f64::from(info.width)).min(f64::from(height) / f64::from(info.height));
    let longer = info.width.max(info.height);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let wanted = (f64::from(longer) * scale).ceil().max(1.0) as u32;
    let side = SIDES.iter().copied().find(|&side| side >= wanted)?;
    (side < longer).then_some(side)
}

/// The part of `bytes`, an image `info` describes, from `at`.
fn part(info: ImageInfo, bytes: &[u8], at: u64) -> ImageAnswer {
    let total = bytes.len() as u64;
    let start = at.min(total);
    let end = start.saturating_add(ImageAnswer::PART).min(total);
    #[allow(clippy::cast_possible_truncation, reason = "within the bytes' length")]
    let bytes = bytes[start as usize..end as usize].to_vec();
    ImageAnswer::Part {
        info,
        total,
        at: start,
        bytes,
    }
}

/// Does `job`, on the store's thread.
fn work(job: Job) -> Done {
    match job {
        Job::Keep {
            why,
            owner,
            source,
            most,
        } => {
            let weight = source.weight();
            let kept = read_source(source, most).and_then(|bytes| examine(bytes, most));
            Done::Kept {
                why,
                owner,
                weight,
                kept,
            }
        }
        Job::Scale { info, bytes, side } => Done::Scaled {
            image: info.id,
            side,
            copy: scaled(info, &bytes, side),
        },
    }
}

/// The bytes `source` holds, of no more than `most`.
fn read_source(source: Source, most: u64) -> Result<Vec<u8>, String> {
    let too_big = || format!("it's over the {} MB limit for images", most / MIB);
    let whole = |file: std::fs::File| -> Result<Vec<u8>, String> {
        let len = file.metadata().map_err(|err| err.to_string())?.len();
        if len > most {
            return Err(too_big());
        }
        let mut bytes = Vec::new();
        file.take(most + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| err.to_string())?;
        if bytes.len() as u64 > most {
            return Err(too_big());
        }
        Ok(bytes)
    };
    let bytes = match source {
        Source::Bytes(bytes) => bytes,
        Source::Base64(data) => base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| "it isn't base64".to_owned())?,
        Source::File(path) => whole(opened(regular::open(&path, Links::Follow))?)?,
        Source::Below { root, path } => whole(opened(regular::open_below(&root, &path))?)?,
        Source::Pixels {
            width,
            height,
            rgba,
        } => png_of(width, height, &rgba)?,
        #[cfg(test)]
        Source::Hang(wait) => {
            std::thread::sleep(wait);
            return Err("there's no such file".into());
        }
    };
    if bytes.len() as u64 > most {
        return Err(too_big());
    }
    Ok(bytes)
}

/// The file at `path`, of no more than `most` bytes, read as a picture
/// pane's is, by a reader of its own: given up on past [`READ_WAIT`], as a
/// file on a mount gone away would hang, leaving its reader to come back
/// when it may.
pub(crate) fn read_in_time(path: PathBuf, most: u64) -> Result<Vec<u8>, String> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("image-reader".into())
        .spawn(move || {
            let _ = send.send(read_source(Source::File(path), most));
        })
        .map_err(|err| format!("couldn't start reading it: {err}"))?;
    receive
        .recv_timeout(READ_WAIT)
        .map_err(|_| "its file took too long to read".to_owned())?
}

/// What opening a file came to, said as a picture pane says it.
fn opened<F>(result: std::io::Result<F>) -> Result<F, String> {
    result.map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => "there's no such file".to_owned(),
        std::io::ErrorKind::IsADirectory => "it's a folder".to_owned(),
        _ => err.to_string(),
    })
}

/// `rgba`, `width` by `height` pixels, as a PNG, quickly compressed.
pub(crate) fn png_of(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    if !ImageInfo::sized(width, height)
        || rgba.len() as u64 != u64::from(width) * u64::from(height) * 4
    {
        return Err("its pixels don't fit its size".into());
    }
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut bytes,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
    .map_err(|err| err.to_string())?;
    Ok(bytes)
}

/// What `bytes` are, if an image of a format and size kept, with the ID
/// they make.
fn examine(bytes: Vec<u8>, most: u64) -> Result<(ImageInfo, Arc<[u8]>), String> {
    let (format, width, height) = look_at(&bytes)?;
    if bytes.len() as u64 > most {
        return Err(format!("it's over the {} MB limit for images", most / MIB));
    }
    let info = ImageInfo {
        id: id_of(&bytes),
        width,
        height,
        format,
        bytes: bytes.len() as u64,
    };
    Ok((info, bytes.into()))
}

/// The format and size of the image `bytes` are, by their header: only
/// what's kept, of a size that may be.
pub(crate) fn look_at(bytes: &[u8]) -> Result<(ImageFormat, u32, u32), String> {
    let (format, ours) = match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => (image::ImageFormat::Png, ImageFormat::Png),
        Ok(image::ImageFormat::Jpeg) => (image::ImageFormat::Jpeg, ImageFormat::Jpeg),
        Ok(image::ImageFormat::Gif) => (image::ImageFormat::Gif, ImageFormat::Gif),
        Ok(image::ImageFormat::WebP) => (image::ImageFormat::WebP, ImageFormat::Webp),
        Ok(_) => return Err("it's an image of a kind clackatoo doesn't show".into()),
        Err(_) => return Err("it isn't an image".into()),
    };
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| "it isn't an image that can be read".to_owned())?;
    if !ImageInfo::sized(width, height) {
        return Err(format!("it's too big to show, at {width} × {height}"));
    }
    Ok((ours, width, height))
}

/// The ID `bytes` make.
pub(crate) fn id_of(bytes: &[u8]) -> ImageId {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut id = [0; 16];
    id.copy_from_slice(&digest.as_ref()[..16]);
    ImageId(id)
}

/// The image `bytes` decoded, within its size, as an image whose header
/// `info` read.
pub(crate) fn decode(info: ImageInfo, bytes: &[u8]) -> Option<image::DynamicImage> {
    let format = match info.format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Webp => image::ImageFormat::WebP,
    };
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(ImageInfo::MOST_PIXELS * 4 + 64 * MIB);
    limits.max_image_width = Some(ImageInfo::MOST_SIDE);
    limits.max_image_height = Some(ImageInfo::MOST_SIDE);
    reader.limits(limits);
    reader.decode().ok()
}

/// A copy of `bytes`, the image `info` describes, no wider or taller than
/// `side`: a JPEG as a JPEG, anything else as a PNG.
fn scaled(info: ImageInfo, bytes: &[u8], side: u32) -> Option<(ImageInfo, Arc<[u8]>)> {
    let image = decode(info, bytes)?.thumbnail(side, side);
    let mut out = Vec::new();
    let format = if info.format == ImageFormat::Jpeg {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
        encoder.encode_image(&image.to_rgb8()).ok()?;
        ImageFormat::Jpeg
    } else {
        let rgba = image.to_rgba8();
        out = png_of(rgba.width(), rgba.height(), rgba.as_raw()).ok()?;
        ImageFormat::Png
    };
    let copy = ImageInfo {
        id: info.id,
        width: image.width(),
        height: image.height(),
        format,
        bytes: out.len() as u64,
    };
    Some((copy, out.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG `width` by `height`, each pixel `shade`.
    pub(crate) fn png(width: u32, height: u32, shade: u8) -> Vec<u8> {
        let rgba = vec![shade; (width * height * 4) as usize];
        png_of(width, height, &rgba).unwrap()
    }

    fn kept(
        store: &mut Images<u32>,
        owner: ImageFrom,
        bytes: Vec<u8>,
    ) -> Result<ImageInfo, String> {
        let (info, bytes) = examine(bytes, store.limits.per_owner)?;
        store.insert(owner, info, bytes)
    }

    const CHAT: ImageFrom = ImageFrom::Terminal(TerminalId(1));

    /// How long tests wait for the store's thread and readers.
    const WAIT: Duration = Duration::from_mins(1);
    const OTHER: ImageFrom = ImageFrom::Terminal(TerminalId(2));

    #[test]
    fn an_image_is_kept_once_by_its_bytes_and_found_only_by_its_owners() {
        let mut store = Images::<u32>::new(Limits::default(), None);
        let a = kept(&mut store, CHAT, png(4, 3, 9)).unwrap();
        assert_eq!((a.width, a.height, a.format), (4, 3, ImageFormat::Png));
        let again = kept(&mut store, OTHER, png(4, 3, 9)).unwrap();
        assert_eq!(a, again);
        assert_eq!(store.bytes(None), a.bytes, "kept once");
        assert_eq!(store.bytes(Some(CHAT)), a.bytes);
        assert_eq!(store.bytes(Some(OTHER)), a.bytes);
        let stranger = ImageFrom::Buffer(BufferId(5));
        assert_eq!(store.get(stranger, a.id), Found::Missing);
        assert!(matches!(store.get(CHAT, a.id), Found::Kept { .. }));
        // Its owners let go, it goes, not as dropped: nothing shows it.
        store.release(CHAT);
        assert!(matches!(store.get(OTHER, a.id), Found::Kept { .. }));
        store.release(OTHER);
        assert_eq!(store.get(OTHER, a.id), Found::Missing);
        assert_eq!(store.bytes(None), 0);
    }

    #[test]
    fn what_isnt_an_image_of_a_kind_shown_is_refused_saying_so() {
        assert_eq!(
            examine(b"hello".to_vec(), MIB).unwrap_err(),
            "it isn't an image"
        );
        let mut bmp = Vec::new();
        image::RgbImage::new(2, 2)
            .write_to(&mut Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .unwrap();
        assert!(examine(bmp, MIB).unwrap_err().contains("kind"));
        // A header claiming more pixels than may be is refused before
        // anything is decoded.
        let mut huge = png(1, 1, 0);
        huge[16..20].copy_from_slice(&100_000u32.to_be_bytes());
        huge[20..24].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(examine(huge, MIB).is_err());
    }

    #[test]
    fn past_an_owners_limit_its_oldest_image_not_shown_goes_and_is_said_dropped() {
        let one = png(64, 64, 1).len() as u64;
        let limits = Limits {
            per_owner: one * 2 + one / 2,
            total: one * 100,
        };
        let mut store = Images::<u32>::new(limits, None);
        let first = kept(&mut store, CHAT, png(64, 64, 1)).unwrap();
        let second = kept(&mut store, CHAT, png(64, 64, 2)).unwrap();
        // The first is on screen: pinned, it stays, and the second goes.
        store.pin(first.id);
        let third = kept(&mut store, CHAT, png(64, 64, 3)).unwrap();
        assert!(matches!(store.get(CHAT, first.id), Found::Kept { .. }));
        assert_eq!(store.get(CHAT, second.id), Found::Dropped(limits.per_owner));
        assert!(matches!(store.get(CHAT, third.id), Found::Kept { .. }));
        assert!(store.bytes(Some(CHAT)) <= limits.per_owner);
        // Another owner's images are its own.
        kept(&mut store, OTHER, png(64, 64, 4)).unwrap();
        assert!(matches!(store.get(CHAT, third.id), Found::Kept { .. }));
        // Unpinned, the first is the oldest, and goes next.
        store.unpin(first.id);
        let fourth = kept(&mut store, CHAT, png(64, 64, 5)).unwrap();
        assert_eq!(store.get(CHAT, first.id), Found::Dropped(limits.per_owner));
        assert!(matches!(store.get(CHAT, fourth.id), Found::Kept { .. }));
        // An image bigger than an owner's limit by itself is refused.
        let big = png(512, 512, 7);
        assert!(kept(&mut store, CHAT, big).unwrap_err().contains("limit"));
    }

    /// The limits hold even when what's shown fills them: a new image past
    /// them isn't kept, and what's shown stays.
    #[test]
    fn past_the_limits_with_what_is_shown_a_new_image_is_refused() {
        let one = png(64, 64, 1).len() as u64;
        let limits = Limits {
            per_owner: one * 10,
            total: one * 2 + one / 2,
        };
        let mut store = Images::<u32>::new(limits, None);
        let a = kept(&mut store, CHAT, png(64, 64, 1)).unwrap();
        let b = kept(&mut store, OTHER, png(64, 64, 2)).unwrap();
        store.pin(a.id);
        store.pin(b.id);
        let refused = kept(&mut store, CHAT, png(64, 64, 3));
        assert!(refused.unwrap_err().contains("limit"));
        assert!(store.bytes(None) <= limits.total);
        assert!(matches!(store.get(CHAT, a.id), Found::Kept { .. }));
        assert!(matches!(store.get(OTHER, b.id), Found::Kept { .. }));
        // Unpinned, they can go for it.
        store.unpin(b.id);
        assert!(kept(&mut store, CHAT, png(64, 64, 3)).is_ok());
    }

    #[test]
    fn past_the_limit_in_all_the_oldest_image_anyones_goes() {
        let one = png(64, 64, 1).len() as u64;
        let limits = Limits {
            per_owner: one * 10,
            total: one * 2 + one / 2,
        };
        let mut store = Images::<u32>::new(limits, None);
        let a = kept(&mut store, CHAT, png(64, 64, 1)).unwrap();
        let b = kept(&mut store, OTHER, png(64, 64, 2)).unwrap();
        // Used since, `a` is newer than `b`.
        assert!(matches!(store.get(CHAT, a.id), Found::Kept { .. }));
        let c = kept(&mut store, ImageFrom::Picture(PictureId(1)), png(64, 64, 3)).unwrap();
        assert_eq!(store.get(OTHER, b.id), Found::Dropped(limits.total));
        assert!(matches!(store.get(CHAT, a.id), Found::Kept { .. }));
        assert!(store.holds(ImageFrom::Picture(PictureId(1)), c.id));
        assert!(store.bytes(None) <= limits.total);
        // Kept again, it's no longer dropped.
        kept(&mut store, OTHER, png(64, 64, 2)).unwrap();
        assert!(matches!(store.get(OTHER, b.id), Found::Kept { .. }));
    }

    #[test]
    fn an_image_comes_in_parts_and_a_copy_at_a_size_is_made_on_the_thread() {
        let mut store = Images::<u32>::new(Limits::default(), None);
        let info = kept(&mut store, CHAT, png(1000, 500, 3)).unwrap();
        let ImageAnswer::Part {
            info: sent,
            total,
            at,
            bytes,
        } = store.fetch(1, CHAT, info.id, None, 5)
        else {
            panic!("not a part");
        };
        assert_eq!((sent, total, at), (info, info.bytes, 5));
        assert_eq!(bytes.len() as u64, info.bytes - 5);
        // Past the end is an empty part, never a panic.
        assert!(matches!(
            store.fetch(1, CHAT, info.id, None, u64::MAX),
            ImageAnswer::Part { bytes, .. } if bytes.is_empty()
        ));
        // As big as it is, or bigger, a copy is the image.
        assert!(matches!(
            store.fetch(1, CHAT, info.id, Some([4000, 4000]), 0),
            ImageAnswer::Part { info: sent, .. } if sent == info
        ));
        // Smaller, a copy is made, and those who asked told.
        assert_eq!(
            store.fetch(1, CHAT, info.id, Some([300, 300]), 0),
            ImageAnswer::Scaling
        );
        assert_eq!(
            store.fetch(2, CHAT, info.id, Some([290, 300]), 0),
            ImageAnswer::Scaling
        );
        assert!(store.busy());
        let (_, ready) = store.take(Some(WAIT));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].image, info.id);
        assert_eq!(
            ready[0].waiting,
            vec![(1, CHAT, [300, 300]), (2, CHAT, [290, 300])]
        );
        let ImageAnswer::Part { info: copy, .. } =
            store.fetch(1, CHAT, info.id, Some([300, 300]), 0)
        else {
            panic!("no copy");
        };
        assert_eq!(copy.id, info.id);
        assert_eq!((copy.width, copy.height), (384, 192));
        // Another's image isn't.
        assert_eq!(
            store.fetch(1, OTHER, info.id, None, 0),
            ImageAnswer::Missing
        );
    }

    /// A copy that can't be made, or kept, is made once: those asking again
    /// get the image as it is, or are told it fails, and no copy is made
    /// again.
    #[test]
    fn a_copy_that_cant_be_made_or_kept_is_made_once_then_answered_for_good() {
        let mut store = Images::<u32>::new(Limits::default(), None);
        // Its header says 1000 × 500, but its pixels are cut short.
        let mut cut = png(1000, 500, 3);
        cut.truncate(cut.len() - 20);
        let (info, bytes) = examine(cut.clone(), MIB).unwrap();
        store.insert(CHAT, info, bytes).unwrap();
        let fit = Some([300, 300]);
        assert_eq!(store.fetch(1, CHAT, info.id, fit, 0), ImageAnswer::Scaling);
        let (_, ready) = store.take(Some(WAIT));
        assert_eq!(ready.len(), 1, "those waiting are told");
        for _ in 0..3 {
            assert!(matches!(
                store.fetch(1, CHAT, info.id, fit, 0),
                ImageAnswer::Part { info: sent, .. } if sent == info
            ));
            assert!(!store.busy(), "no copy is made again");
        }
        // One too big to keep is held until it's had whole, then let go.
        let mut store = Images::<u32>::new(Limits::default(), None);
        store.keep_copies_within(10);
        let info = kept(&mut store, CHAT, png(1000, 500, 4)).unwrap();
        assert_eq!(store.fetch(1, CHAT, info.id, fit, 0), ImageAnswer::Scaling);
        let (_, ready) = store.take(Some(WAIT));
        assert_eq!(ready.len(), 1);
        let ImageAnswer::Part {
            info: copy, total, ..
        } = store.fetch(1, CHAT, info.id, fit, 0)
        else {
            panic!("not held");
        };
        assert_eq!((copy.width, total), (384, copy.bytes));
        assert!(store.once.is_empty(), "had whole, so let go");
        assert!(matches!(
            store.fetch(1, CHAT, info.id, fit, 0),
            ImageAnswer::Part { info: sent, .. } if sent == info
        ));
        assert!(!store.busy(), "no copy is made again");
        // Too big to send as it is, asking fails.
        let mut store = Images::<u32>::new(Limits::default(), None);
        let big = ImageInfo {
            bytes: ORIGINAL_INSTEAD + 1,
            ..info
        };
        store
            .insert(
                CHAT,
                big,
                vec![0; usize::try_from(ORIGINAL_INSTEAD + 1).unwrap()].into(),
            )
            .unwrap();
        store.uncopied_at(info.id, 384);
        assert!(matches!(
            store.fetch(1, CHAT, info.id, fit, 0),
            ImageAnswer::Failed(_)
        ));
    }

    #[test]
    fn files_are_read_on_the_thread_as_regular_files_capped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.png"), png(3, 2, 8)).unwrap();
        std::fs::write(dir.path().join("big.png"), vec![0; 2048]).unwrap();
        std::os::unix::fs::symlink(dir.path().join("a.png"), dir.path().join("link.png")).unwrap();
        let limits = Limits {
            per_owner: 1024,
            total: 4096,
        };
        let mut store = Images::<u32>::new(limits, None);
        let owner = ImageFrom::Buffer(BufferId(1));
        let below = |path: &str| Source::Below {
            root: dir.path().to_owned(),
            path: path.into(),
        };
        for (n, source) in [
            Source::File(dir.path().join("a.png")),
            below("a.png"),
            below("link.png"),
            below("big.png"),
            below("/etc/passwd"),
            below("nothing.png"),
            Source::File(dir.path().to_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            let n = u32::try_from(n).unwrap();
            store.keep(owner, Why::Test(n), source).unwrap();
        }
        let (kept, _) = store.take(Some(WAIT));
        let mut said: Vec<_> = (kept.iter())
            .map(|kept| (kept.why.clone(), kept.kept.clone().map(|info| info.width)))
            .collect();
        // Read each by a reader of its own, in whatever order.
        said.sort_by_key(|(why, _)| match why {
            Why::Test(n) => *n,
            _ => u32::MAX,
        });
        assert_eq!(said[0], (Why::Test(0), Ok(3)));
        assert_eq!(said[1], (Why::Test(1), Ok(3)));
        assert!(said[2].1.as_ref().unwrap_err().contains("link"), "{said:?}");
        assert!(
            said[3].1.as_ref().unwrap_err().contains("limit"),
            "{said:?}"
        );
        assert!(
            said[4].1.as_ref().unwrap_err().contains("folder"),
            "{said:?}"
        );
        assert_eq!(said[5].1, Err("there's no such file".into()));
        assert_eq!(said[6].1, Err("it's a folder".into()));
    }

    /// A file that hangs, as on a mount gone away, holds up neither the
    /// store's thread nor anything waiting for it: it's given up on in
    /// its time, or at once before a handover; and only so many wait.
    #[test]
    fn a_file_that_hangs_is_given_up_on_and_holds_nothing_up() {
        let mut store = Images::<u32>::new(Limits::default(), None);
        store.read_wait = Duration::from_millis(50);
        store
            .keep(CHAT, Why::Test(1), Source::Hang(Duration::from_secs(2)))
            .unwrap();
        // The thread still keeps what it's sent meanwhile.
        store
            .keep(CHAT, Why::Test(2), Source::Bytes(png(2, 2, 1)))
            .unwrap();
        let (kept, _) = store.take(Some(Duration::from_millis(500)));
        let said: Vec<_> = (kept.iter())
            .map(|kept| (kept.why.clone(), kept.kept.clone().err()))
            .collect();
        assert!(said.contains(&(Why::Test(2), None)), "{said:?}");
        assert!(
            said.contains(&(Why::Test(1), Some("its file took too long to read".into()))),
            "{said:?}"
        );
        assert!(store.next_due().is_none());
        // Its time passed, it's given up on as the loop takes what's done.
        store
            .keep(CHAT, Why::Test(3), Source::Hang(Duration::from_secs(2)))
            .unwrap();
        assert!(store.next_due().is_some());
        std::thread::sleep(Duration::from_millis(100));
        let (kept, _) = store.take(None);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].kept.is_err());
        // Only so many wait their turn.
        let mut store = Images::<u32>::new(Limits::default(), None);
        for n in 0..(READERS + READ_QUEUE) {
            let n = u32::try_from(n).unwrap();
            let hang = Source::Hang(Duration::from_millis(200));
            store.keep(CHAT, Why::Test(n), hang).unwrap();
        }
        let hang = Source::Hang(Duration::from_millis(200));
        assert!(store.keep(CHAT, Why::Test(999), hang).is_err());
    }

    #[test]
    fn a_fifo_is_never_waited_on() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.png");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        let (send, receive) = mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = send.send(read_source(Source::File(path), MIB));
        });
        let read = receive
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("a FIFO was waited on");
        assert!(read.is_err());
        // Unblock a regression's open, if any.
        let _ = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags_nonblock()
            .open(&fifo);
    }

    trait NonBlock {
        fn custom_flags_nonblock(&mut self) -> &mut Self;
    }

    impl NonBlock for std::fs::OpenOptions {
        fn custom_flags_nonblock(&mut self) -> &mut Self {
            use std::os::unix::fs::OpenOptionsExt as _;
            self.custom_flags(libc::O_NONBLOCK)
        }
    }

    #[test]
    fn pixels_are_kept_as_a_png_and_the_limits_read_from_the_environment() {
        let mut store = Images::<u32>::new(Limits::default(), None);
        store
            .keep(
                CHAT,
                Why::Test(1),
                Source::Pixels {
                    width: 2,
                    height: 1,
                    rgba: vec![1, 2, 3, 255, 4, 5, 6, 255],
                },
            )
            .unwrap();
        let (kept, _) = store.take(Some(WAIT));
        let info = kept[0].kept.clone().unwrap();
        assert_eq!(
            (info.format, info.width, info.height),
            (ImageFormat::Png, 2, 1)
        );
        let Found::Kept { bytes, .. } = store.get(CHAT, info.id) else {
            panic!("not kept");
        };
        let decoded = decode(info, bytes).unwrap().to_rgba8();
        assert_eq!(decoded.as_raw(), &[1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(
            Limits::from_env(Some(OsStr::new("8, 32"))),
            Limits {
                per_owner: 8 * MIB,
                total: 32 * MIB
            }
        );
        assert_eq!(Limits::from_env(Some(OsStr::new("64"))), Limits::default());
        assert_eq!(Limits::from_env(Some(OsStr::new("9,8"))), Limits::default());
    }
}
