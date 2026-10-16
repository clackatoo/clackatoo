//! Recovery in the event loop: having each buffer's changes that aren't
//! saved written to its recovery file a moment after they're made, and
//! the file removed once there are none; and, as the daemon starts, taking
//! back what the files kept, from a daemon that crashed, was killed or
//! stopped (see [`crate::recovery`]).
//!
//! The loop only notes which buffers changed and when. Once a buffer has
//! rested for [`RecoveryConfig::after`](crate::RecoveryConfig), or changed
//! for [`most`](crate::RecoveryConfig) without one, longer for a bigger
//! buffer, its text and the text it was saved as go to the thread, as
//! ropes, which share what they hold: the copying, writing and syncing are
//! all the thread's.
//!
//! As the daemon starts, each file's buffer comes back, unseen, as a
//! buffer kept with changes after `:q` waits: any window opening its file
//! gets it. If its file still holds what the buffer last read or wrote, it
//! comes back with its changes, as changes not saved. If the file has
//! changed since, it's opened as it is now, and the changes are set aside,
//! for `:recover` to take or drop. A buffer that was open already, as one
//! an upgrade carried, never has its text replaced: what came back for it
//! is set aside. The file is never written by any of this. A buffer with
//! no file yet comes back with no file, shown by `:recover`. Past
//! [`MOST_TAKEN_FILES`] files or [`MOST_TAKEN_BYTES`] bytes, the rest wait
//! for `:recover`, so a full folder never holds up the start.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt::Write;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    Author, BufferId, BufferUpdate, Content, Edit, Error, PROTOCOL_VERSION, PaneId, Response, Side,
    wire,
};
use mio::Token;

use super::buffers::{BufferSlot, blank_kept, resolve};
use super::{Daemon, Source};
use crate::buffer::Buffer;
use crate::recovery::{self, Keep, Kept, MOST_KEPT, Opened, Store};

/// How long the daemon waits, as it upgrades, for the last changes to be
/// written.
const STOP_WAIT: Duration = Duration::from_secs(1);

/// The most recovery files taken back as the daemon starts: the rest wait
/// for `:recover`.
const MOST_TAKEN_FILES: usize = 64;

/// The most bytes of text taken back as the daemon starts: the rest wait
/// for `:recover`.
const MOST_TAKEN_BYTES: u64 = 256 * 1024 * 1024;

/// How many bytes of text lengthen the time a buffer's file waits by as
/// much again: writing a big buffer whole costs as much as its size.
const SCALE_STEP: usize = 4 * 1024 * 1024;

/// The most the time a buffer's file waits is lengthened by.
const MOST_SCALE: u32 = 16;

/// What came back to a buffer from recovery files, while it's still to be
/// dealt with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Recovered {
    /// Its changes came back, over its file's text, and haven't been
    /// saved or thrown away since.
    pub(super) restored: bool,
    /// Changes that couldn't come back over its text, set aside until
    /// `:recover` takes or drops them: its file had changed since they
    /// were kept, or it had changes of its own.
    pub(super) aside: Option<SetAside>,
}

impl Recovered {
    /// Whether anything is still to be dealt with.
    pub(super) fn pending(&self) -> bool {
        self.restored || self.aside.is_some()
    }
}

/// Changes set aside: the recovery file that keeps them, as its header,
/// its text read only once `:recover` takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SetAside {
    /// The recovery file's header.
    pub(super) kept: Kept,
}

/// Buffers' recovery files, as the event loop keeps track of them.
#[derive(Debug)]
pub(super) struct Recovery {
    /// The thread writing them, if they're kept.
    store: Option<Store>,
    /// Why they aren't kept, if they're to be and couldn't be, as when
    /// another daemon keeps the folder: said to windows.
    off: Option<String>,
    /// Buffers whose windows have been told why their changes aren't kept.
    warned: BTreeSet<BufferId>,
    /// How long after its last change a buffer's file is written.
    after: Duration,
    /// The longest a change waits while changes keep coming.
    most: Duration,
    /// Buffers changed since their file was last asked to be written.
    changed: BTreeMap<BufferId, Changed>,
    /// Buffers with a file written, or asked to be.
    kept: BTreeSet<BufferId>,
    /// The files each buffer's text came back from, which its own next
    /// file takes the place of: removed once it's written.
    replaces: HashMap<BufferId, Vec<String>>,
    /// Buffers too big to keep whose windows have been told so.
    too_big: BTreeSet<BufferId>,
    /// The buffer each recovery file read as the daemon started came back
    /// as, by the file's name.
    from: HashMap<String, BufferId>,
    /// Files left as the daemon started, past what's taken back at once,
    /// newest first, for `:recover`.
    waiting: VecDeque<Kept>,
    /// How many files, and bytes of text, are taken back at once.
    most_taken: (usize, u64),
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            store: None,
            off: None,
            warned: BTreeSet::new(),
            after: crate::DEFAULT_RECOVERY_AFTER,
            most: crate::DEFAULT_RECOVERY_MOST,
            changed: BTreeMap::new(),
            kept: BTreeSet::new(),
            replaces: HashMap::new(),
            too_big: BTreeSet::new(),
            from: HashMap::new(),
            waiting: VecDeque::new(),
            most_taken: (MOST_TAKEN_FILES, MOST_TAKEN_BYTES),
        }
    }
}

/// When a buffer's changes not yet written were made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Changed {
    first: Instant,
    last: Instant,
    /// How many times longer than usual its file waits, for its size.
    scale: u32,
}

impl Changed {
    /// When the buffer's file is to be written.
    fn due(self, after: Duration, most: Duration) -> Instant {
        (self.last + after * self.scale).min(self.first + most * self.scale)
    }
}

/// How many times longer than usual a buffer of `len` bytes waits to be
/// written.
fn scale(len: usize) -> u32 {
    u32::try_from(1 + len / SCALE_STEP).map_or(MOST_SCALE, |scale| scale.min(MOST_SCALE))
}

/// The edit that makes `now` into `text`, changing only what's between
/// what they start and end with alike, so places outside it stay put.
fn trimmed(now: &str, text: &str) -> Edit {
    let mut start = (now.bytes().zip(text.bytes()))
        .take_while(|(a, b)| a == b)
        .count();
    while !(now.is_char_boundary(start) && text.is_char_boundary(start)) {
        start -= 1;
    }
    let most = now.len().min(text.len()) - start;
    let mut end = (now.bytes().rev().zip(text.bytes().rev()))
        .take(most)
        .take_while(|(a, b)| a == b)
        .count();
    while !(now.is_char_boundary(now.len() - end) && text.is_char_boundary(text.len() - end)) {
        end -= 1;
    }
    Edit::replace(
        now.len(),
        start,
        &now[start..now.len() - end],
        &text[start..text.len() - end],
    )
}

impl Daemon {
    /// Starts keeping buffers' changes not saved, if they're to be kept,
    /// with `lock`, the lock on their folder an upgrade handed over, and
    /// takes back what was kept before. As the daemon starts afresh, every
    /// file is another daemon's, as each has a number of its own; carrying
    /// on after an upgrade, its own are its buffers', whose changes it has,
    /// and the rest are those still set aside or waiting.
    pub(super) fn start_recovery(&mut self, lock: Option<OwnedFd>) {
        let Some(config) = self.config.recovery.clone() else {
            return;
        };
        self.recovery.after = config.after;
        self.recovery.most = config.most.max(config.after);
        let waker = Arc::clone(&self.waker);
        let wake = move || {
            if let Err(err) = waker.wake() {
                log::warn!("waking the event loop for recovery files failed: {err}");
            }
        };
        let opened = Store::open(config.dir.clone(), self.id, lock, config.gate, wake);
        let Opened { store, others, own } = match opened {
            Ok(opened) => opened,
            Err(why) => {
                log::warn!("{why}");
                self.recovery.off = Some(why);
                return;
            }
        };
        self.recovery.store = Some(store);
        for (buffer, key) in own {
            self.keep_own(BufferId(buffer), key);
        }
        let (most_files, most_bytes) = self.recovery.most_taken;
        let (mut files, mut bytes) = (0, 0);
        for kept in others {
            if files < most_files && bytes + kept.len <= most_bytes {
                files += 1;
                bytes += kept.len;
                self.take_back(kept);
            } else {
                self.recovery.waiting.push_back(kept);
            }
        }
        if files > 0 {
            log::info!("{files} buffers' changes not saved taken back");
        }
        if !self.recovery.waiting.is_empty() {
            log::info!("{} more wait for :recover", self.recovery.waiting.len());
        }
        // Any buffer an upgrade carried whose file wasn't written in time.
        let unkept: Vec<BufferId> = (self.buffers.iter())
            .filter(|(id, slot)| slot.buffer.modified() && !self.recovery.kept.contains(id))
            .map(|(&id, _)| id)
            .collect();
        for id in unkept {
            self.write_soon(id);
        }
    }

    /// This daemon's file `key`, of `buffer`, found as it carries on
    /// after an upgrade: kept if the buffer still has changes, rewritten
    /// in case the last didn't get written before the upgrade, and removed
    /// if it hasn't.
    fn keep_own(&mut self, buffer: BufferId, key: String) {
        let modified = (self.buffers.get(&buffer)).is_some_and(|slot| slot.buffer.modified());
        if modified {
            self.recovery.kept.insert(buffer);
            self.write_soon(buffer);
        } else if let Some(store) = &mut self.recovery.store {
            store.remove(key, Vec::new());
        }
    }

    /// Takes back the buffer the recovery file `kept` kept, as the
    /// module's comment says. Returns the buffer it came back to, if it
    /// came back to one.
    fn take_back(&mut self, kept: Kept) -> Option<BufferId> {
        let Some(path) = kept.path.as_deref() else {
            let text = self.kept_text(&kept)?;
            return Some(self.take_back_blank(kept.key, &text, kept.dir));
        };
        // Made real as a file opened is, so the buffer of a file it names
        // through a link is found.
        let path = match path.to_str() {
            Some(path) => resolve(path, &kept.dir, &self.config.home),
            None => path.to_owned(),
        };
        let open = (self.buffers.iter()).find(|(_, slot)| slot.buffer.path() == path);
        let (id, opened) = match open {
            Some((&id, _)) => (id, false),
            None => match Buffer::open(path.clone(), kept.dir.clone()) {
                Ok(buffer) => {
                    let id = BufferId(self.next_buffer);
                    self.next_buffer += 1;
                    log::info!("buffer {id}: {}, taken back", path.display());
                    self.buffers.insert(id, BufferSlot::new(buffer));
                    self.find_words(id);
                    self.watch_buffer(id);
                    (id, true)
                }
                Err(err) => {
                    // Kept with no file rather than lost: `:w NAME` writes
                    // it where it can be.
                    log::warn!("{}: taken back with no file: {err}", path.display());
                    let text = self.kept_text(&kept)?;
                    let dir = path.parent().map_or(kept.dir, PathBuf::from);
                    return Some(self.take_back_blank(kept.key, &text, dir));
                }
            },
        };
        let slot = self.buffers.get_mut(&id)?;
        let unchanged = recovery::digest(slot.buffer.saved()) == kept.base;
        if opened && unchanged {
            let Some(text) = self.kept_text(&kept) else {
                self.close_if_unwanted(id);
                return None;
            };
            if *self.buffers[&id].buffer.text() == text.as_str() {
                log::info!("buffer {id}: has the changes {} kept", kept.key);
                if let Some(store) = &mut self.recovery.store {
                    store.remove(kept.key, Vec::new());
                }
                // Opened only for them, it isn't wanted.
                self.close_if_unwanted(id);
                return None;
            }
            self.recovery.from.insert(kept.key.clone(), id);
            self.restore(id, &text, kept.key);
        } else if slot.buffer.text().len_bytes() as u64 == kept.len
            && self.kept_text(&kept).as_deref()
                == Some(&self.buffers[&id].buffer.text().to_string())
        {
            // What the file holds, or the buffer has, already: nothing to
            // set aside. Only read if it's as long.
            log::info!("buffer {id}: has the changes {} kept", kept.key);
            if let Some(store) = &mut self.recovery.store {
                store.remove(kept.key, Vec::new());
            }
            if opened {
                self.close_if_unwanted(id);
                return None;
            }
        } else if let Some(slot) = self.buffers.get_mut(&id)
            && slot.recovered.aside.is_none()
        {
            // Its text stays on disk until `:recover` takes it.
            log::info!("buffer {id}: {} set aside", kept.key);
            self.recovery.from.insert(kept.key.clone(), id);
            slot.recovered.aside = Some(SetAside { kept });
            self.tell_viewers_recovered(id);
        } else {
            // A second set of changes to one file: kept with no file
            // rather than lost.
            let text = self.kept_text(&kept)?;
            let dir = path.parent().map_or(kept.dir, PathBuf::from);
            return Some(self.take_back_blank(kept.key, &text, dir));
        }
        Some(id)
    }

    /// The text of the recovery file `kept` is the header of, read now,
    /// or `None`, said in the log, if it can't be.
    fn kept_text(&self, kept: &Kept) -> Option<String> {
        match self.recovery.store.as_ref()?.read_text(kept) {
            Ok(text) => Some(text),
            Err(why) => {
                log::error!("{}: {why}", kept.key);
                None
            }
        }
    }

    /// Takes back a buffer with no file yet, with `text`, which the
    /// recovery file `key` kept, its name to be from `dir`.
    fn take_back_blank(&mut self, key: String, text: &str, dir: PathBuf) -> BufferId {
        let id = BufferId(self.next_buffer);
        self.next_buffer += 1;
        log::info!("buffer {id}: blank, in {}, taken back", dir.display());
        self.buffers.insert(id, BufferSlot::new(Buffer::blank(dir)));
        self.find_words(id);
        self.recovery.from.insert(key.clone(), id);
        self.restore(id, text, key);
        id
    }

    /// Gives buffer `id`, which has no changes, the text `text` came back
    /// with, from the recovery file `key` names, as an edit by the user,
    /// which can be undone: its file's text comes back with `u`. Its own
    /// file is written at once, then `key`'s removed. Never over changes:
    /// returns whether it was.
    fn restore(&mut self, id: BufferId, text: &str, key: String) -> bool {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return false;
        };
        if slot.buffer.modified() {
            log::error!("buffer {id}: {key} not taken back over changes not saved");
            return false;
        }
        let edit = trimmed(&slot.buffer.text().to_string(), text);
        let base = slot.buffer.version();
        match slot.buffer.edit(edit, base, true, Author::User) {
            Ok(made) => {
                slot.recovered.restored = true;
                self.recovery.replaces.entry(id).or_default().push(key);
                self.made_for_user(id, made);
                self.write_now(id);
                true
            }
            Err(err) => {
                // Its file stays, to be taken back next time.
                log::error!("buffer {id}: {key} couldn't be taken back: {err}");
                self.close_if_unwanted(id);
                false
            }
        }
    }

    /// Notes that buffer `id` has changed, or been saved, or whether it's
    /// modified may have: a buffer with changes has its file written soon,
    /// and one with none has its file removed.
    pub(super) fn recovery_touched(&mut self, id: BufferId) {
        let Some(slot) = self.buffers.get_mut(&id) else {
            return;
        };
        if slot.buffer.modified() {
            if self.recovery.store.is_none() {
                self.warn_unkept(id);
            } else {
                self.write_soon(id);
            }
            return;
        }
        slot.recovered.restored = false;
        self.recovery.changed.remove(&id);
        self.recovery.too_big.remove(&id);
        if self.recovery.kept.remove(&id) || self.recovery.replaces.contains_key(&id) {
            let replaces = self.recovery.replaces.remove(&id).unwrap_or_default();
            let key = recovery::key(self.id, id.0);
            if let Some(store) = &mut self.recovery.store {
                store.remove(key, replaces);
            }
        }
    }

    /// Buffer `id`'s file has moved: if it has changes, its recovery file,
    /// which names the file, is written again at once.
    pub(super) fn recovery_moved(&mut self, id: BufferId) {
        if (self.buffers.get(&id)).is_some_and(|slot| slot.buffer.modified()) {
            self.write_now(id);
        } else {
            self.recovery_touched(id);
        }
    }

    /// Tells the windows following buffer `id`, which has just come to have
    /// changes, why they aren't kept through a crash, if they're to be and
    /// can't be: once for each buffer.
    fn warn_unkept(&mut self, id: BufferId) {
        let Some(why) = &self.recovery.off else {
            return;
        };
        if !self.recovery.warned.insert(id) {
            return;
        }
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        let notice = format!(
            "{}: changes not saved won't outlive a crash: {why}",
            slot_name(slot)
        );
        self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
    }

    /// Buffer `id` is gone: its file goes too, with any it took the
    /// place of.
    pub(super) fn recovery_forget(&mut self, id: BufferId) {
        self.recovery.changed.remove(&id);
        self.recovery.too_big.remove(&id);
        self.recovery.warned.remove(&id);
        let kept = self.recovery.kept.remove(&id);
        let replaces = self.recovery.replaces.remove(&id).unwrap_or_default();
        let key = recovery::key(self.id, id.0);
        if (kept || !replaces.is_empty())
            && let Some(store) = &mut self.recovery.store
        {
            store.remove(key, replaces);
        }
    }

    /// Has buffer `id`'s file written once it has rested, or changes have
    /// waited long enough, the longer the bigger it is.
    fn write_soon(&mut self, id: BufferId) {
        let (Some(_), Some(slot)) = (&self.recovery.store, self.buffers.get(&id)) else {
            return;
        };
        let scale = scale(slot.buffer.text().len_bytes());
        let now = Instant::now();
        let changed = self.recovery.changed.entry(id).or_insert(Changed {
            first: now,
            last: now,
            scale,
        });
        changed.last = now;
        changed.scale = scale;
    }

    /// Has buffer `id`'s file written as soon as the loop goes round.
    fn write_now(&mut self, id: BufferId) {
        if self.recovery.store.is_none() {
            return;
        }
        let past = Instant::now()
            .checked_sub(self.recovery.most)
            .unwrap_or_else(Instant::now);
        let changed = Changed {
            first: past,
            last: past,
            scale: 1,
        };
        self.recovery.changed.insert(id, changed);
    }

    /// When the next buffer's file is to be written, if one is.
    pub(super) fn next_recovery_due(&self) -> Option<Instant> {
        let (after, most) = (self.recovery.after, self.recovery.most);
        (self.recovery.changed.values())
            .map(|changed| changed.due(after, most))
            .min()
    }

    /// Sends the thread what waited for room, has the files written of
    /// every buffer whose time has come, and says what the thread couldn't
    /// write.
    pub(super) fn keep_for_recovery(&mut self) {
        let Some(store) = &mut self.recovery.store else {
            return;
        };
        if store.is_waiting() {
            store.send_waiting();
        }
        self.take_recovery_failures();
        if self.recovery.changed.is_empty() {
            return;
        }
        let now = Instant::now();
        let (after, most) = (self.recovery.after, self.recovery.most);
        let due: Vec<BufferId> = (self.recovery.changed.iter())
            .filter(|(_, changed)| changed.due(after, most) <= now)
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            self.recovery.changed.remove(&id);
            self.write_recovery(id);
        }
    }

    /// Has buffer `id`'s file written now, if it has changes, or says once
    /// that it's too big to keep.
    fn write_recovery(&mut self, id: BufferId) {
        let (Some(store), Some(slot)) = (&mut self.recovery.store, self.buffers.get(&id)) else {
            return;
        };
        let key = recovery::key(self.id, id.0);
        if !slot.buffer.modified() {
            return;
        }
        if slot.buffer.text().len_bytes() > MOST_KEPT {
            // What it last kept is older than what it has: gone, rather
            // than taken back for what it has. What it came back from
            // stays, until it's saved.
            if self.recovery.kept.remove(&id) {
                store.remove(key, Vec::new());
            }
            if self.recovery.too_big.insert(id) {
                let notice = format!(
                    "{}: too big to keep its changes not saved through a crash (over {} MiB)",
                    slot_name(slot),
                    MOST_KEPT / (1024 * 1024)
                );
                log::warn!("buffer {id}: {notice}");
                self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
            }
            return;
        }
        self.recovery.too_big.remove(&id);
        let keep = Keep {
            path: slot.buffer.path().to_owned(),
            dir: slot.buffer.dir().to_owned(),
            saved: slot.buffer.saved().clone(),
            text: slot.buffer.text().clone(),
        };
        let replaces = self.recovery.replaces.remove(&id).unwrap_or_default();
        store.write(key, keep, replaces);
        self.recovery.kept.insert(id);
    }

    /// Takes in what the thread couldn't do: the files a buffer's file
    /// was to replace go back to wait for its next one, which is tried
    /// again soon, and its windows are told why, once for each run of
    /// failures.
    fn take_recovery_failures(&mut self) {
        let Some(store) = &self.recovery.store else {
            return;
        };
        for failure in store.take_failed() {
            let buffer = recovery::buffer_of(&failure.key, self.id).map(BufferId);
            let Some(id) = buffer.filter(|id| self.buffers.contains_key(id)) else {
                if !failure.replaces.is_empty() {
                    log::warn!("{:?} are left, their buffer gone", failure.replaces);
                }
                continue;
            };
            if !failure.replaces.is_empty() {
                (self.recovery.replaces.entry(id).or_default()).extend(failure.replaces);
            }
            self.write_soon(id);
            if failure.first {
                let notice = format!(
                    "{}: its changes not saved couldn't be kept through a crash: {}",
                    slot_name(&self.buffers[&id]),
                    failure.why
                );
                self.tell_viewers(id, None, &BufferUpdate::Notice(notice));
            }
        }
    }

    /// As the daemon stops: has every buffer's changes written now, and
    /// the thread stop once they are.
    pub(super) fn begin_stopping_recovery(&mut self) {
        let changed: Vec<BufferId> = self.recovery.changed.keys().copied().collect();
        self.recovery.changed.clear();
        for id in changed {
            self.write_recovery(id);
        }
        if let Some(store) = &mut self.recovery.store {
            store.begin_stop();
        }
    }

    /// As the daemon stops: waits for the thread, told to stop already, to
    /// write what was waiting and stop, until `until` at most. Returns the
    /// lock on the folder.
    pub(super) fn finish_stopping_recovery(&mut self, until: Instant) -> Option<OwnedFd> {
        let store = self.recovery.store.take()?;
        let (lock, stopped) = store.stop(until.saturating_duration_since(Instant::now()));
        if !stopped {
            log::error!("the last changes not saved may not have been kept");
        }
        Some(lock)
    }

    /// Before an upgrade: has every buffer's changes written, and the
    /// thread stop. Returns the lock on the folder, for the next build to
    /// hold: no other daemon takes it meanwhile.
    pub(super) fn stop_recovery(&mut self) -> Option<OwnedFd> {
        self.begin_stopping_recovery();
        self.finish_stopping_recovery(Instant::now() + STOP_WAIT)
    }

    /// Tells the client at `token`, attaching to buffer `id`, what came
    /// back to it from recovery files, if that's still to be dealt with.
    pub(super) fn tell_recovered(&mut self, token: Token, id: BufferId) {
        let Some(slot) = self.buffers.get(&id) else {
            return;
        };
        if !slot.recovered.pending() {
            return;
        }
        let knows = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.wire >= wire::RECOVER,
            _ => true,
        };
        let more = self.unseen_recovered().filter(|&other| other != id).count()
            + self.recovery.waiting.len();
        let notice = recovered_notice(&slot_name(slot), slot, more, knows);
        self.tell(token, id, BufferUpdate::Notice(notice));
    }

    /// Tells every client following buffer `id` what came back to it.
    fn tell_viewers_recovered(&mut self, id: BufferId) {
        let viewers = (self.buffers.get(&id)).map_or_else(Vec::new, |slot| slot.viewers.clone());
        for viewer in viewers {
            self.tell_recovered(viewer, id);
        }
    }

    /// The buffers that came back from recovery files, still to be dealt
    /// with, that no pane shows, oldest first. Any buffer with no file yet
    /// that has changes counts, as nothing else could show it: one that
    /// came back before an upgrade, which doesn't carry what came back.
    fn unseen_recovered(&self) -> impl Iterator<Item = BufferId> + '_ {
        (self.buffers.iter())
            .filter(|(_, slot)| {
                slot.recovered.pending() || (slot.buffer.is_blank() && slot.buffer.modified())
            })
            .map(|(&id, _)| id)
            .filter(|&id| !self.workspaces.shows(id))
    }

    /// Answers [`Request::Recover`](clackatoo_protocol::Request::Recover):
    /// if `pane` shows a buffer with changes set aside, takes them, or with
    /// `drop`, throws them away. One with no changes of its own takes them
    /// as an edit by the user, which can be undone; one with changes gets
    /// them beside it, in a new buffer with no file yet, never over its
    /// own. Otherwise shows the next buffer that came back from a recovery
    /// file that no pane shows, taking back the next file left waiting if
    /// there's none: in `pane`, if it shows a file or a folder, as
    /// [`Request::OpenHere`](clackatoo_protocol::Request::OpenHere) would,
    /// or else in a new tab beside it.
    pub(super) fn recover_here(&mut self, pane: PaneId, drop: bool) -> Result<Response, Error> {
        let content = self.workspaces.check_pane(pane)?;
        if let Some(id) = content.buffer()
            && (self.buffers.get(&id)).is_some_and(|slot| slot.recovered.aside.is_some())
        {
            return self.deal_with_aside(pane, id, drop);
        }
        if drop {
            return Err(Error::Other(
                "no changes are set aside for this file".to_owned(),
            ));
        }
        // The next after what the pane shows, so `:recover` again goes on
        // to another; then those left waiting; then round again.
        let shown = content.buffer();
        let unseen: Vec<BufferId> = self.unseen_recovered().collect();
        let mut next = (unseen.iter().copied()).find(|&id| shown.is_none_or(|shown| id > shown));
        // One at a time, as each is read whole on the loop.
        if next.is_none()
            && let Some(kept) = self.recovery.waiting.pop_front()
        {
            next = self.take_back(kept);
            if next.is_none() && !self.recovery.waiting.is_empty() {
                return Err(Error::Other(format!(
                    "nothing came back from one kept from before: {} more waiting, `:recover` \
                     for the next",
                    self.recovery.waiting.len()
                )));
            }
        }
        let Some(id) = next.or_else(|| unseen.first().copied()) else {
            return Err(Error::Other(
                "no changes not saved are waiting to be recovered".to_owned(),
            ));
        };
        self.show_recovered(pane, id).map(|made| match made {
            Some(pane) => Response::PaneCreated(pane),
            None => Response::Done,
        })
    }

    /// Takes or drops buffer `id`'s changes set aside, as `pane` shows it:
    /// see [`recover_here`](Self::recover_here).
    fn deal_with_aside(
        &mut self,
        pane: PaneId,
        id: BufferId,
        drop: bool,
    ) -> Result<Response, Error> {
        let slot = self.buffers.get(&id).ok_or(Error::NoSuchBuffer(id))?;
        let modified = slot.buffer.modified();
        let Some(SetAside { kept }) = slot.recovered.aside.clone() else {
            return Err(Error::NoSuchBuffer(id));
        };
        let text = if drop {
            String::new()
        } else {
            if modified {
                self.workspaces.check_split(pane, Side::Right)?;
            }
            // Left set aside if it can't be read: `:recover!` drops it.
            let store = self.recovery.store.as_ref();
            let read = store.map_or_else(
                || Err("nothing is kept".to_owned()),
                |store| store.read_text(&kept),
            );
            read.map_err(|why| Error::Other(format!("the changes kept couldn't be read: {why}")))?
        };
        let slot = self.buffers.get_mut(&id).ok_or(Error::NoSuchBuffer(id))?;
        slot.recovered.aside = None;
        let key = kept.key;
        let name = slot_name(slot);
        let folder = match slot.buffer.path().parent() {
            Some(folder) if !slot.buffer.is_blank() => folder.to_owned(),
            _ => slot.buffer.dir().to_owned(),
        };
        if drop {
            log::info!("buffer {id}: the changes {key} kept dropped");
            if let Some(store) = &mut self.recovery.store {
                store.remove(key, Vec::new());
            }
            return Ok(Response::Text(format!(
                "{name}: the changes kept from before were dropped"
            )));
        }
        if modified {
            // Never over its own changes, nor its recovery file over theirs.
            log::info!("buffer {id}: the changes {key} kept opened beside it");
            let beside = self.take_back_blank(key, &text, folder);
            let made = (self.workspaces).split(pane, Side::Right, Content::Buffer(beside));
            self.workspaces_changed();
            return Ok(Response::PaneCreated(made));
        }
        log::info!("buffer {id}: the changes {key} kept taken");
        self.restore(id, &text, key);
        if let Some(slot) = self.buffers.get_mut(&id) {
            // Taken by choice, over the file as it is now: nothing more to
            // say of it.
            slot.recovered.restored = false;
        }
        Ok(Response::Text(format!(
            "{name}: the changes kept from before taken: `u` to put the file's text back"
        )))
    }

    /// Shows buffer `id`, which came back from a recovery file, in `pane`
    /// in place of what it shows, if that's a file or a folder, or else in
    /// a new tab in `pane`'s workspace, returning the new tab's pane. A
    /// saved session puts a recovered buffer back where it was with this.
    /// Refused if it would lose a buffer with no file yet that has changes.
    pub(super) fn show_recovered(
        &mut self,
        pane: PaneId,
        id: BufferId,
    ) -> Result<Option<PaneId>, Error> {
        if !self.buffers.contains_key(&id) {
            return Err(Error::NoSuchBuffer(id));
        }
        let old = self.workspaces.check_pane(pane)?;
        if !matches!(old, Content::Buffer(_) | Content::Folder(_)) {
            let workspace = (self.workspaces.get().find_pane(pane))
                .map(|(workspace, _, _)| workspace.id)
                .ok_or(Error::NoSuchPane(pane))?;
            let made = self.workspaces.new_tab(workspace, Content::Buffer(id));
            self.workspaces_changed();
            return Ok(Some(made));
        }
        if self.loses_blank(&[pane]) {
            return Err(blank_kept(PROTOCOL_VERSION));
        }
        self.workspaces.set_content(pane, Content::Buffer(id))?;
        self.workspaces_changed();
        match old {
            Content::Buffer(old) if old != id => self.close_if_unwanted(old),
            Content::Folder(files) => self.close_files(files),
            _ => {}
        }
        Ok(None)
    }

    /// The buffer the recovery file named `key` came back as, as the
    /// daemon started: how a saved session finds a buffer with no file
    /// yet again, having kept its [`recovery_key`](Self::recovery_key).
    #[allow(dead_code, reason = "for restoring saved sessions (CLACKA-114)")]
    pub(super) fn recovered_from(&self, key: &str) -> Option<BufferId> {
        self.recovery.from.get(key).copied()
    }

    /// The name buffer `id`'s recovery file has, or would have: what a
    /// saved session keeps to find it again after a crash, with
    /// [`recovered_from`](Self::recovered_from).
    #[allow(dead_code, reason = "for restoring saved sessions (CLACKA-114)")]
    pub(super) fn recovery_key(&self, id: BufferId) -> String {
        recovery::key(self.id, id.0)
    }
}

/// What a buffer is called in a message: its file's name, or `[No Name]`.
fn slot_name(slot: &BufferSlot) -> String {
    if slot.buffer.is_blank() {
        return "[No Name]".to_owned();
    }
    let path = slot.buffer.path();
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What a window says of buffer `slot`, called `name`, which came back
/// from recovery files, and how many `more` are waiting, unseen. A window
/// that doesn't `know` `:recover`, from before it, is told it needs one
/// that does.
fn recovered_notice(name: &str, slot: &BufferSlot, more: usize, knows: bool) -> String {
    let (recover, drop) = if knows {
        ("`:recover`", "`:recover!`")
    } else {
        (
            "`:recover` in a window as new as the daemon",
            "`:recover!` there",
        )
    };
    let came_back = "changes not saved came back from before the daemon stopped";
    let recovered = &slot.recovered;
    let mut notice = match (recovered.restored, recovered.aside.is_some()) {
        (true, true) => format!(
            "{name}: {came_back}, with an older set beside them: `:w` to keep these, {recover} \
             to open the older beside them, {drop} to drop it"
        ),
        (true, false) if slot.buffer.is_blank() => {
            format!("{name}: {came_back}: `:w NAME` to keep them, `:q!` to drop them")
        }
        (true, false) => format!("{name}: {came_back}: `:w` to keep them, `:e!` to drop them"),
        (false, true) if slot.buffer.modified() => format!(
            "{name} has changes of its own, and older ones not saved came back: {recover} to \
             open them beside it, {drop} to drop them"
        ),
        (false, _) => format!(
            "{name} changed on disk since its changes not saved were kept: {recover} to take \
             them, {drop} to drop them"
        ),
    };
    if more > 0 {
        let _ = write!(notice, "; {more} more waiting: {recover} shows them");
    }
    notice
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::RecoveryConfig;
    use crate::server::agent_files::tests::daemon_with_chat;

    /// Keeps `text` for the file at `path` as a daemon before did for its
    /// buffer `buffer`, having read `saved` from it, a moment after the
    /// last.
    fn kept_before(store: &mut Store, path: &Path, buffer: u32, saved: &str, text: &str) {
        let keep = Keep {
            path: path.to_owned(),
            dir: path.parent().unwrap().to_owned(),
            saved: clackatoo_text::Rope::from_str(saved),
            text: clackatoo_text::Rope::from_str(text),
        };
        store.write(recovery::key(0xdead, buffer), keep, Vec::new());
        assert!(store.flush(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(3));
    }

    /// A store of recovery files in `dir`, as a daemon before kept them.
    fn store_before(dir: &Path) -> Store {
        Store::open(dir.to_owned(), 0xdead, None, None, || {})
            .unwrap()
            .store
    }

    /// The recovery files in `dir`, by name, in order.
    fn files_in(dir: &Path) -> Vec<String> {
        let mut files: Vec<String> = (std::fs::read_dir(dir).unwrap())
            .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
            .filter(|name| name.ends_with(".recover"))
            .collect();
        files.sort();
        files
    }

    /// A daemon in `root` keeping changes not saved in `kept`, having taken
    /// back what was kept there.
    fn recovering(root: &Path, kept: &Path) -> Daemon {
        let (mut daemon, _) = daemon_with_chat(root);
        daemon.config.recovery = Some(RecoveryConfig::in_dir(kept.to_owned()));
        daemon.start_recovery(None);
        daemon
    }

    /// The text of the changes set aside for buffer `slot`, read now.
    fn aside_text(daemon: &Daemon, slot: &BufferSlot) -> String {
        let aside = slot.recovered.aside.as_ref().expect("changes set aside");
        let store = daemon.recovery.store.as_ref().expect("a store");
        store.read_text(&aside.kept).unwrap()
    }

    fn slot_of<'a>(daemon: &'a Daemon, path: &Path) -> &'a BufferSlot {
        (daemon.buffers.values())
            .find(|slot| slot.buffer.path() == path)
            .unwrap()
    }

    #[test]
    fn every_set_of_changes_kept_comes_back_and_none_is_lost() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let (a, b) = (root.join("a.txt"), root.join("b.txt"));
        std::fs::write(&a, "disk\n").unwrap();
        std::fs::write(&b, "same\n").unwrap();
        let mut store = store_before(&kept);
        kept_before(&mut store, &a, 1, "disk\n", "oldest\n");
        kept_before(&mut store, &a, 2, "disk\n", "older\n");
        kept_before(&mut store, &a, 3, "disk\n", "newest\n");
        // Changes the file has come to hold since are nothing to take back.
        kept_before(&mut store, &b, 4, "before\n", "same\n");
        drop(store.stop(Duration::from_secs(5)));

        let mut daemon = recovering(&root, &kept);
        // The newest over the file, the next set aside beside it, and the
        // oldest in a buffer of its own with no file, in the file's folder.
        let a_slot = slot_of(&daemon, &a);
        assert_eq!(a_slot.buffer.text().to_string(), "newest\n");
        assert!(a_slot.buffer.modified());
        assert!(a_slot.recovered.restored);
        let aside = a_slot.recovered.aside.as_ref().unwrap();
        assert_eq!(aside.kept.key, recovery::key(0xdead, 2));
        assert_eq!(aside_text(&daemon, a_slot), "older\n");
        // Said as both.
        let notice = recovered_notice("a.txt", a_slot, 0, true);
        assert!(
            notice.contains("came back") && notice.contains("an older set beside"),
            "{notice}"
        );
        let blanks: Vec<&BufferSlot> = (daemon.buffers.values())
            .filter(|slot| slot.buffer.is_blank())
            .collect();
        assert_eq!(blanks.len(), 1);
        assert_eq!(blanks[0].buffer.text().to_string(), "oldest\n");
        assert_eq!(blanks[0].buffer.dir(), root.as_path());
        assert!(blanks[0].recovered.restored);
        assert_eq!(daemon.buffers.len(), 2, "b.txt isn't kept open");
        assert_eq!(daemon.unseen_recovered().count(), 2);
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "disk\n");

        // Written as the loop goes round, each in place of the file it came
        // back from; what's set aside stays.
        daemon.keep_for_recovery();
        daemon.stop_recovery();
        let mut expected: Vec<String> = (daemon.buffers.keys())
            .map(|id| format!("{}.recover", recovery::key(daemon.id, id.0)))
            .chain([format!("{}.recover", recovery::key(0xdead, 2))])
            .collect();
        expected.sort();
        assert_eq!(files_in(&kept), expected);
    }

    #[test]
    fn a_file_gone_from_disk_has_its_changes_set_aside_and_one_that_wont_open_comes_back_blank() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let (gone, folder) = (root.join("gone.txt"), root.join("now-a-folder"));
        let mut store = store_before(&kept);
        kept_before(&mut store, &gone, 1, "was\n", "was and mine\n");
        kept_before(&mut store, &folder, 2, "was\n", "mine\n");
        drop(store.stop(Duration::from_secs(5)));
        std::fs::create_dir(&folder).unwrap();

        let daemon = recovering(&root, &kept);
        // Gone, it opens empty, as a new file, and the changes wait.
        let slot = slot_of(&daemon, &gone);
        assert_eq!(slot.buffer.text().to_string(), "");
        assert!(!slot.buffer.modified());
        assert_eq!(aside_text(&daemon, slot), "was and mine\n");
        // A folder now, it can't open: the changes come back with no file,
        // in the folder it was in.
        let blank = (daemon.buffers.values())
            .find(|slot| slot.buffer.is_blank())
            .unwrap();
        assert_eq!(blank.buffer.text().to_string(), "mine\n");
        assert_eq!(blank.buffer.dir(), root.as_path());
    }

    #[test]
    fn past_so_many_files_the_rest_wait_for_recover() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let mut store = store_before(&kept);
        for (buffer, name) in [(1, "a"), (2, "b"), (3, "c")] {
            std::fs::write(root.join(name), "disk\n").unwrap();
            kept_before(&mut store, &root.join(name), buffer, "disk\n", name);
        }
        drop(store.stop(Duration::from_secs(5)));
        let (mut daemon, _) = daemon_with_chat(&root);
        daemon.config.recovery = Some(RecoveryConfig::in_dir(kept.clone()));
        daemon.recovery.most_taken = (1, u64::MAX);
        daemon.start_recovery(None);
        // The newest only, read whole.
        assert_eq!(daemon.buffers.len(), 1);
        assert_eq!(
            slot_of(&daemon, &root.join("c")).buffer.text().to_string(),
            "c"
        );
        assert_eq!(daemon.recovery.waiting.len(), 2);

        // Shown in turn by `:recover`, those waiting taken back as it goes.
        let name = daemon.workspaces.name_for_new(None).unwrap();
        let blank = daemon.open_blank(&clackatoo_protocol::OpenBlank { cwd: None }, None);
        let pane = daemon
            .workspaces
            .new_workspace(name, Content::Buffer(blank));
        let mut shown = Vec::new();
        for _ in 0..4 {
            assert_eq!(daemon.recover_here(pane, false).unwrap(), Response::Done);
            let id = daemon
                .workspaces
                .check_pane(pane)
                .unwrap()
                .buffer()
                .unwrap();
            shown.push(daemon.buffers[&id].buffer.text().to_string());
        }
        // Those waiting, then round again.
        assert_eq!(shown, ["b", "a", "c", "b"]);
        assert!(daemon.recovery.waiting.is_empty());
    }

    #[test]
    fn changes_set_aside_go_beside_a_buffer_with_changes_of_its_own_never_over_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let a = root.join("a.txt");
        std::fs::write(&a, "theirs\n").unwrap();
        let mut store = store_before(&kept);
        kept_before(&mut store, &a, 1, "mine before\n", "kept\n");
        drop(store.stop(Duration::from_secs(5)));
        let mut daemon = recovering(&root, &kept);
        let id = (daemon.buffers.keys().next().copied()).unwrap();
        let name = daemon.workspaces.name_for_new(None).unwrap();
        let pane = daemon.workspaces.new_workspace(name, Content::Buffer(id));
        // Changed since, by the user.
        let slot = daemon.buffers.get_mut(&id).unwrap();
        let edit = Edit::replace(7, 0, "", "new ");
        let made = slot.buffer.edit(edit, 0, true, Author::User).unwrap();
        daemon.made_for_user(id, made);
        daemon.write_now(id);
        daemon.keep_for_recovery();
        assert!(daemon.recovery.kept.contains(&id));

        let answer = daemon.recover_here(pane, false).unwrap();
        let Response::PaneCreated(beside) = answer else {
            panic!("{answer:?}");
        };
        let blank = daemon
            .workspaces
            .check_pane(beside)
            .unwrap()
            .buffer()
            .unwrap();
        assert_eq!(daemon.buffers[&blank].buffer.text().to_string(), "kept\n");
        assert!(daemon.buffers[&blank].buffer.is_blank());
        let slot = &daemon.buffers[&id];
        assert_eq!(slot.buffer.text().to_string(), "new theirs\n");
        assert_eq!(slot.recovered, Recovered::default());
        // Its own recovery file is still its own text's, and the set aside
        // file goes once the new buffer's is written.
        daemon.keep_for_recovery();
        daemon.stop_recovery();
        let opened = Store::open(kept.clone(), 0xfeed, None, None, || {}).unwrap();
        let texts: Vec<String> = (opened.others.iter())
            .map(|one| opened.store.read_text(one).unwrap())
            .collect();
        let mut texts = texts;
        texts.sort();
        assert_eq!(texts, ["kept\n", "new theirs\n"]);
    }

    #[test]
    fn an_upgrade_never_puts_what_came_back_over_a_buffer_it_carried() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let a = root.join("a.txt");
        std::fs::write(&a, "disk\n").unwrap();
        let (mut daemon, _) = daemon_with_chat(&root);
        let open = clackatoo_protocol::OpenFile {
            path: a.display().to_string(),
            cwd: None,
        };
        let id = daemon.open_buffer(open, None).unwrap();
        let name = daemon.workspaces.name_for_new(None).unwrap();
        daemon.workspaces.new_workspace(name, Content::Buffer(id));
        // Another daemon's file for it is there as the next build starts,
        // its file unchanged since.
        let mut store = store_before(&kept);
        kept_before(&mut store, &a, 1, "disk\n", "other\n");
        drop(store.stop(Duration::from_secs(5)));
        let mut config = daemon.config.clone();
        config.recovery = Some(RecoveryConfig::in_dir(kept.clone()));
        let handover = daemon.hand_over(Instant::now()).unwrap();
        drop(daemon);
        let mut resumed = Daemon::resume(handover, config.clone()).unwrap();
        let slot = &resumed.buffers[&id];
        assert_eq!(slot.buffer.text().to_string(), "disk\n");
        assert!(!slot.buffer.modified());
        assert_eq!(aside_text(&resumed, slot), "other\n");
        // The lock goes with the next upgrade: no other daemon takes the
        // folder meanwhile.
        let handover = resumed.hand_over(Instant::now()).unwrap();
        assert!(Store::open(kept.clone(), 1, None, None, || {}).is_err());
        drop(resumed);
        let again = Daemon::resume(handover, config).unwrap();
        assert!(again.recovery.store.is_some());
        assert!(again.buffers[&id].recovered.aside.is_some());
    }

    #[test]
    fn what_a_failed_write_was_to_replace_waits_for_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let kept = root.join("recovery");
        let mut store = store_before(&kept);
        kept_before(&mut store, &root.join("x"), 1, "", "draft\n");
        drop(store.stop(Duration::from_secs(5)));
        let mut daemon = recovering(&root, &kept);
        let id = (daemon.buffers.keys().next().copied()).unwrap();
        let own = kept.join(format!("{}.recover", recovery::key(daemon.id, id.0)));
        std::fs::create_dir(&own).unwrap();
        daemon.keep_for_recovery();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !daemon.recovery.replaces.contains_key(&id) {
            assert!(Instant::now() < deadline, "never put back");
            std::thread::sleep(Duration::from_millis(5));
            daemon.take_recovery_failures();
        }
        assert_eq!(daemon.recovery.replaces[&id], [recovery::key(0xdead, 1)]);
        std::fs::remove_dir(&own).unwrap();
        daemon.write_now(id);
        daemon.keep_for_recovery();
        daemon.stop_recovery();
        assert_eq!(
            files_in(&kept),
            [format!("{}.recover", recovery::key(daemon.id, id.0))]
        );
    }

    #[test]
    fn a_moved_buffer_is_kept_again_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let mut daemon = recovering(&root, &root.join("recovery"));
        let id = daemon.open_blank(&clackatoo_protocol::OpenBlank { cwd: None }, None);
        let slot = daemon.buffers.get_mut(&id).unwrap();
        let made = (slot.buffer)
            .edit(Edit::replace(0, 0, "", "x"), 0, true, Author::User)
            .unwrap();
        daemon.made_for_user(id, made);
        let soon = daemon.recovery.changed[&id];
        assert!(soon.due(daemon.recovery.after, daemon.recovery.most) > Instant::now());
        daemon.recovery_moved(id);
        let now = daemon.recovery.changed[&id];
        assert!(now.due(daemon.recovery.after, daemon.recovery.most) <= Instant::now());
    }

    #[test]
    fn a_bigger_buffer_waits_longer_to_be_written() {
        assert_eq!(scale(0), 1);
        assert_eq!(scale(SCALE_STEP), 2);
        assert_eq!(scale(usize::MAX), MOST_SCALE);
        let now = Instant::now();
        let changed = |scale| Changed {
            first: now,
            last: now,
            scale,
        };
        let (after, most) = (Duration::from_secs(1), Duration::from_secs(4));
        assert_eq!(changed(1).due(after, most), now + after);
        assert_eq!(changed(3).due(after, most), now + after * 3);
    }

    #[test]
    fn what_comes_back_changes_only_what_differs() {
        let edit = trimmed("one two three", "one 2 three");
        assert_eq!(edit, Edit::replace(13, 4, "two", "2"));
        assert_eq!(edit.apply_to("one two three").unwrap(), "one 2 three");
        // Never inside a character.
        let edit = trimmed("aéb", "aèb");
        assert_eq!(edit.apply_to("aéb").unwrap(), "aèb");
        assert_eq!(trimmed("", "new"), Edit::replace(0, 0, "", "new"));
        assert_eq!(trimmed("abab", "ab").apply_to("abab").unwrap(), "ab");
    }

    #[test]
    fn a_window_from_before_recover_is_told_it_needs_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut slot = BufferSlot::new(Buffer::blank(dir.path().to_owned()));
        let kept = Kept {
            key: "k".into(),
            path: None,
            dir: dir.path().to_owned(),
            base: [0; 32],
            written: 0,
            len: 1,
        };
        slot.recovered.aside = Some(SetAside { kept });
        let new = recovered_notice("a.txt", &slot, 2, true);
        assert!(
            new.contains("changed on disk") && new.contains("`:recover` to take"),
            "{new}"
        );
        assert!(
            new.ends_with("2 more waiting: `:recover` shows them"),
            "{new}"
        );
        let old = recovered_notice("a.txt", &slot, 0, false);
        assert!(old.contains("in a window as new as the daemon"), "{old}");
    }
}
