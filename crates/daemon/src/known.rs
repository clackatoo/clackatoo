//! The identity keys this daemon knows other machines by (see
//! `crate::machine`), as ssh's `known_hosts` keeps hosts' keys: each
//! machine's ID, its key, and whether it was met straight, with ssh, or
//! only vouched for by a machine passing it on.
//!
//! They're kept in a file of their own, `known_machines`, beside the
//! `machine` file in clackatoo's state directory, apart from the saved
//! workspaces, so that starting afresh, keeping no workspaces, a
//! workspaces file that can't be read, or an older build writing its own
//! never loses one. JSON, one value: `{"clackatoo_known_machines": 1,
//! "machines": [{"machine": "<32 hex digits>", "key": "<64 hex digits>",
//! "met": true}]}`, the oldest learned first. Written as the `machine`
//! file is: in full beside it, in a file made afresh for the user alone,
//! synced, and renamed over it. Writing happens on a thread of its own, as
//! a key is learned, which writes what's known by then, so the last write
//! is always the newest. One that can't be read is set aside as
//! `known_machines.unreadable`, and nothing is known: each machine is met
//! again.
//!
//! # So many and no more
//!
//! A machine passing others on vouches for their keys, so a daemon that
//! isn't what it should be could have this one learn a key for machine
//! after machine. At most [`MOST_KEYS`] are known: past it, a new key
//! takes the place of the one only vouched for that was learned or used
//! longest ago, but for the keys of the machines the daemon is connected
//! to, is passed on or keeps a connection to (see
//! [`in_use`](Known::in_use)), and one met straight is never forgotten to
//! make room. (How many keys one daemon may vouch for in a while is the
//! daemon's to limit, as it knows which vouched.) So the file never grows past
//! [`MAX_FILE`]; one bigger, as an older build without the cap could
//! write, is read all the same, its keys met kept and the newest vouched
//! for, never set aside for its size.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::MachineId;
use serde::{Deserialize, Serialize};

/// The file's version.
const VERSION: u32 = 1;

/// The most keys known: far more than any number of machines a person
/// reaches. See the module docs.
const MOST_KEYS: usize = 1024;

/// The most bytes one machine's key takes in the file, with room to
/// spare: about 160.
const LONGEST_KEPT: usize = 512;

/// The most bytes the file this build writes takes.
const MAX_FILE: usize = MOST_KEYS * LONGEST_KEPT;

/// The identity key a machine is known by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    pub(crate) key: [u8; 32],
    /// Met straight, with ssh, which logged in to it, rather than only
    /// vouched for by a daemon passing it on.
    pub(crate) met: bool,
}

/// The keys known, and where they're kept, if anywhere.
#[derive(Debug, Default)]
pub(crate) struct Known {
    /// The file, or `None` to keep them for this run only, as in tests.
    file: Option<PathBuf>,
    keys: Arc<Mutex<Keys>>,
    /// Held while the file is written, so writes never cross.
    writing: Arc<Mutex<()>>,
    /// The threads writing, or about to, so `kept` can wait for them: one
    /// that hasn't taken `writing` yet would otherwise write after it.
    writers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    /// Something known isn't in the file yet: a change no thread has
    /// written, as none could be started or its write failed.
    unwritten: Arc<AtomicBool>,
}

/// The keys known, each with when it was learned or last used, counted.
#[derive(Debug, Default)]
struct Keys {
    pins: BTreeMap<MachineId, (Pin, u64)>,
    /// What the next key learned or used is counted as.
    learned: u64,
    /// The machines whose keys aren't forgotten to make room, as they're
    /// in use.
    in_use: BTreeSet<MachineId>,
}

impl Keys {
    /// Knows `machine` by `pin`, unless it knows it already and not
    /// `replace`, making room for it if it must: returns whether that
    /// changed what's known. Replacing counts as using it, and only ever
    /// takes the same key, met now if it wasn't: a key that differs from
    /// the one known is kept only once that's forgotten, never in its
    /// place, whatever a check made meanwhile, elsewhere, found.
    fn learn(&mut self, machine: MachineId, pin: Pin, replace: bool) -> bool {
        if let Some((known, learned)) = self.pins.get_mut(&machine) {
            if !replace {
                return false;
            }
            if known.key != pin.key {
                log::error!(
                    "{machine} is known by another key than the one just checked: it's kept, \
                     and the new one isn't"
                );
                return false;
            }
            *learned = self.learned;
            self.learned += 1;
            let met = known.met || pin.met;
            let changed = known.met != met;
            known.met = met;
            return changed;
        }
        if self.pins.len() >= MOST_KEYS && !self.make_room(pin.met) {
            log::error!(
                "no key known can be forgotten to make room for {machine}'s, so it isn't kept, \
                 and it's checked again next time: every one known was met straight, or is in \
                 use"
            );
            return false;
        }
        self.pins.insert(machine, (pin, self.learned));
        self.learned += 1;
        true
    }

    /// Forgets the key only vouched for, and not in use, that was learned
    /// or used longest ago, returning whether there was one. For a key
    /// `met` straight, which is never turned away for room, one in use
    /// goes if no other can.
    fn make_room(&mut self, met: bool) -> bool {
        let oldest = |in_use: bool| {
            (self.pins.iter())
                .filter(|(machine, (pin, _))| {
                    !pin.met && (in_use || !self.in_use.contains(machine))
                })
                .min_by_key(|(_, (_, learned))| *learned)
                .map(|(&machine, _)| machine)
        };
        let Some(oldest) = oldest(false).or_else(|| if met { oldest(true) } else { None }) else {
            return false;
        };
        log::warn!(
            "{MOST_KEYS} machines' keys are known: {oldest}'s, only vouched for, is forgotten \
             to make room"
        );
        self.pins.remove(&oldest);
        true
    }

    /// Every key known, by machine, the oldest learned first.
    fn all(&self) -> Vec<(MachineId, Pin)> {
        let mut all: Vec<(MachineId, Pin, u64)> = (self.pins.iter())
            .map(|(&machine, &(pin, learned))| (machine, pin, learned))
            .collect();
        all.sort_by_key(|&(_, _, learned)| learned);
        all.into_iter()
            .map(|(machine, pin, _)| (machine, pin))
            .collect()
    }
}

/// The file, as it's written and read.
#[derive(Serialize, Deserialize)]
struct Kept {
    clackatoo_known_machines: u32,
    #[serde(default)]
    machines: Vec<KeptMachine>,
}

#[derive(Serialize, Deserialize)]
struct KeptMachine {
    machine: String,
    key: String,
    #[serde(default)]
    met: bool,
}

impl Known {
    /// The keys `file` keeps, kept there from now on; with no file, none,
    /// kept for this run only.
    pub(crate) fn load(file: Option<PathBuf>) -> Self {
        let keys = file.as_deref().map(read).unwrap_or_default();
        Self {
            file,
            keys: Arc::new(Mutex::new(keys)),
            writing: Arc::default(),
            writers: Mutex::default(),
            unwritten: Arc::default(),
        }
    }

    /// Where the keys are kept for the daemon whose `machine` file is
    /// `machine`: beside it.
    pub(crate) fn beside(machine: &Path) -> PathBuf {
        machine.with_file_name("known_machines")
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, Keys> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The key `machine` is known by, if it is.
    pub(crate) fn get(&self, machine: MachineId) -> Option<Pin> {
        self.keys().pins.get(&machine).map(|&(pin, _)| pin)
    }

    /// Every key known, by machine, the oldest learned first.
    pub(crate) fn all(&self) -> Vec<(MachineId, Pin)> {
        self.keys().all()
    }

    /// Knows `machine` by `pin` from now on, keeping it, as just used, and
    /// returns whether that's new: past [`MOST_KEYS`], in place of one
    /// only vouched for (see the module docs), or not at all if every one
    /// known was met or is in use.
    pub(crate) fn insert(&self, machine: MachineId, pin: Pin) -> bool {
        let new = self.keys().learn(machine, pin, true);
        if new {
            self.keep();
        }
        new
    }

    /// Knows each of `known` by its key, if it knows it by none, as from a
    /// build that kept keys elsewhere, or handed them over, the oldest
    /// learned first: as [`insert`](Self::insert) makes room.
    pub(crate) fn learn(&self, known: impl IntoIterator<Item = (MachineId, Pin)>) {
        let new = {
            let mut keys = self.keys();
            let mut new = false;
            for (machine, pin) in known {
                new |= keys.learn(machine, pin, false);
            }
            new
        };
        if new {
            self.keep();
        }
    }

    /// Notes that the keys of `machines` are in use, as the machines the
    /// daemon is connected to, passes on or keeps connections to: none of
    /// them is forgotten to make room.
    pub(crate) fn in_use(&self, machines: BTreeSet<MachineId>) {
        self.keys().in_use = machines;
    }

    /// Forgets the key `machine` is known by, and returns whether it was.
    pub(crate) fn remove(&self, machine: MachineId) -> bool {
        let was = self.keys().pins.remove(&machine).is_some();
        if was {
            self.keep();
        }
        was
    }

    /// Writes what's known to the file, on a thread of its own: what's
    /// known when it writes, so the last to write writes the newest.
    fn keep(&self) {
        let Some(file) = self.file.clone() else {
            return;
        };
        self.unwritten.store(true, Ordering::SeqCst);
        let writing = Arc::clone(&self.writing);
        let keys = Arc::clone(&self.keys);
        let unwritten = Arc::clone(&self.unwritten);
        let spawned = std::thread::Builder::new()
            .name("known-machines".into())
            .spawn(move || {
                let _held = writing.lock().unwrap_or_else(PoisonError::into_inner);
                // Before the keys are read: a change after it is seen.
                unwritten.store(false, Ordering::SeqCst);
                let keys = keys.lock().unwrap_or_else(PoisonError::into_inner).all();
                if let Err(err) = write(&file, &keys) {
                    unwritten.store(true, Ordering::SeqCst);
                    log::error!("{} couldn't be written: {err}", file.display());
                }
            });
        match spawned {
            Ok(writer) => {
                let mut writers = self.writers.lock().unwrap_or_else(PoisonError::into_inner);
                writers.retain(|writer| !writer.is_finished());
                writers.push(writer);
            }
            Err(err) => log::error!("there's no thread to keep the keys known on: {err}"),
        }
    }

    /// Waits for what's being written to the file to be written, as the
    /// daemon stops or upgrades: every thread writing, and then once more,
    /// if something known still isn't in the file. A file that has it all
    /// already isn't written again: an fsync can take seconds on a busy
    /// disk, and the daemon has only a few to stop in.
    pub(crate) fn kept(&self) {
        let writers =
            std::mem::take(&mut *self.writers.lock().unwrap_or_else(PoisonError::into_inner));
        for writer in writers {
            let _ = writer.join();
        }
        if let Some(file) = &self.file
            && self.unwritten.swap(false, Ordering::SeqCst)
        {
            let _held = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(err) = write(file, &self.all()) {
                log::error!("{} couldn't be written: {err}", file.display());
            }
        }
    }
}

/// The keys `file` keeps, the oldest learned first: none if there's no
/// file, and none, with the file set aside, if it can't be read as one.
/// Past [`MOST_KEYS`], the oldest only vouched for are left out.
fn read(file: &Path) -> Keys {
    let mut bytes = Vec::new();
    match File::open(file) {
        Ok(mut opened) => {
            if let Err(err) = opened.read_to_end(&mut bytes) {
                log::error!("{} couldn't be read: {err}", file.display());
                return Keys::default();
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Keys::default(),
        Err(err) => {
            log::error!("{} couldn't be read: {err}", file.display());
            return Keys::default();
        }
    }
    let kept = match serde_json::from_slice::<Kept>(&bytes) {
        Ok(kept) if kept.clackatoo_known_machines != 0 => kept,
        Ok(_) | Err(_) => {
            log::error!(
                "{} can't be read: it's set aside, and each machine is met again",
                file.display()
            );
            let mut aside = file.as_os_str().to_owned();
            aside.push(".unreadable");
            let _ = fs::rename(file, aside);
            return Keys::default();
        }
    };
    if bytes.len() > MAX_FILE {
        log::warn!(
            "{} is bigger than this build writes it: its keys met are kept, and the newest only \
             vouched for",
            file.display()
        );
    }
    let known: Vec<(MachineId, Pin)> = (kept.machines.iter())
        .filter_map(|known| {
            let machine = known.machine.parse().ok()?;
            let key = crate::machine::unhex(&known.key)?;
            Some((
                machine,
                Pin {
                    key,
                    met: known.met,
                },
            ))
        })
        .collect();
    // Room for every key met, as none is ever forgotten to make room, and
    // then for the newest only vouched for.
    let met = known.iter().filter(|(_, pin)| pin.met).count();
    let mut vouched = (known.iter())
        .filter(|(_, pin)| !pin.met)
        .count()
        .saturating_sub(MOST_KEYS.saturating_sub(met));
    let mut keys = Keys::default();
    for (machine, pin) in known {
        if !pin.met && vouched > 0 {
            vouched -= 1;
            continue;
        }
        keys.pins.insert(machine, (pin, keys.learned));
        keys.learned += 1;
    }
    keys
}

/// Writes `keys` as the file at `path`: in full beside it, in a file made
/// afresh for the user alone, never through a link, and synced; then
/// renamed over it, and the directory synced.
fn write(path: &Path, keys: &[(MachineId, Pin)]) -> io::Result<()> {
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
    let mut bytes = written(keys)?;
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

/// `keys`, as the file holds them.
fn written(keys: &[(MachineId, Pin)]) -> io::Result<Vec<u8>> {
    let kept = Kept {
        clackatoo_known_machines: VERSION,
        machines: (keys.iter())
            .map(|(machine, pin)| KeptMachine {
                machine: machine.to_string(),
                key: crate::machine::hex(&pin.key),
                met: pin.met,
            })
            .collect(),
    };
    serde_json::to_vec_pretty(&kept).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_kept_for_the_user_alone_and_read_again() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let file = Known::beside(&dir.path().join("machine"));
        let known = Known::load(Some(file.clone()));
        let server = MachineId([5; 16]);
        let pin = Pin {
            key: [7; 32],
            met: true,
        };
        assert!(known.insert(server, pin));
        assert!(!known.insert(server, pin), "nothing new");
        known.kept();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // With nothing new since, the file isn't written again.
        let written = fs::metadata(&file).unwrap().ino();
        known.kept();
        assert_eq!(fs::metadata(&file).unwrap().ino(), written);
        assert_eq!(Known::load(Some(file.clone())).get(server), Some(pin));
        // Learned from elsewhere, a key known already stays.
        let other = Pin {
            key: [8; 32],
            met: false,
        };
        known.learn([(server, other)]);
        assert_eq!(known.get(server), Some(pin));
        assert!(known.remove(server));
        known.kept();
        assert_eq!(Known::load(Some(file.clone())).get(server), None);
        // One that can't be read is set aside, and nothing is known.
        fs::write(&file, "not json").unwrap();
        assert!(Known::load(Some(file.clone())).all().is_empty());
        assert!(dir.path().join("known_machines.unreadable").exists());
    }

    /// The `n`th machine, and a key for it, met or not.
    fn nth(n: usize, met: bool) -> (MachineId, Pin) {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&(n as u64).to_be_bytes());
        let mut key = [0; 32];
        key[..8].copy_from_slice(&(n as u64).to_be_bytes());
        (MachineId(id), Pin { key, met })
    }

    #[test]
    fn so_many_keys_are_known_and_a_met_one_is_never_forgotten_for_room() {
        // Kept for this run only: each key a file is written for is
        // synced, which takes long by the thousand.
        let known = Known::load(None);
        let (first, met) = nth(0, true);
        assert!(known.insert(first, met));
        // Vouched for, one machine after another, far past the cap.
        for n in 1..=MOST_KEYS * 3 {
            let (machine, pin) = nth(n, false);
            known.insert(machine, pin);
        }
        assert_eq!(known.all().len(), MOST_KEYS);
        assert_eq!(known.get(first), Some(met), "the one met stays");
        // The newest stay, the oldest went.
        assert!(known.get(nth(MOST_KEYS * 3, false).0).is_some());
        assert!(known.get(nth(1, false).0).is_none());
        // Kept, it fits, and read again, it's as it was, in the order
        // learned.
        let dir = tempfile::tempdir().unwrap();
        let file = Known::beside(&dir.path().join("machine"));
        let kept = Known::load(Some(file.clone()));
        kept.learn(known.all());
        kept.kept();
        let bytes = fs::read(&file).unwrap();
        assert!(bytes.len() <= MAX_FILE, "{}", bytes.len());
        let again = Known::load(Some(file.clone()));
        assert_eq!(again.all(), known.all());
        // With every one met, a new one isn't kept.
        let full = Known::load(None);
        for n in 0..MOST_KEYS {
            let (machine, pin) = nth(n, true);
            assert!(full.insert(machine, pin));
        }
        let (late, pin) = nth(MOST_KEYS, false);
        assert!(!full.insert(late, pin));
        assert_eq!(full.get(late), None);
        assert_eq!(full.get(nth(0, true).0), Some(nth(0, true).1));
        // Handed over, as many again: no more are known.
        let handed = Known::load(None);
        handed.learn((0..MOST_KEYS * 2).map(|n| nth(n, n == 0)));
        assert_eq!(handed.all().len(), MOST_KEYS);
        assert!(handed.get(nth(0, true).0).is_some());
    }

    #[test]
    fn keys_in_use_or_used_lately_are_never_forgotten_for_room() {
        let known = Known::load(None);
        for n in 0..MOST_KEYS {
            let (machine, pin) = nth(n, false);
            assert!(known.insert(machine, pin));
        }
        // The oldest two: one passed on now, one just used again.
        let (passed, used) = (nth(0, false), nth(1, false));
        known.in_use(BTreeSet::from([passed.0]));
        assert!(!known.insert(used.0, used.1), "nothing new");
        // Vouched for, one machine after another: room is made for as many
        // as were learned before it was used, and it stays.
        for n in MOST_KEYS..MOST_KEYS * 2 - 2 {
            let (machine, pin) = nth(n, false);
            assert!(known.insert(machine, pin));
        }
        assert_eq!(known.get(passed.0), Some(passed.1));
        assert_eq!(known.get(used.0), Some(used.1));
        assert_eq!(known.get(nth(MOST_KEYS - 1, false).0), None);
        // It goes next, before those learned since.
        let (next, pin) = nth(MOST_KEYS * 2, false);
        assert!(known.insert(next, pin));
        assert_eq!(known.get(used.0), None);
        assert!(known.get(nth(MOST_KEYS, false).0).is_some());
        assert_eq!(known.all().len(), MOST_KEYS);
        // A key met straight is never turned away while one only vouched
        // for is known, even one in use.
        let vouched = Known::load(None);
        for n in 0..MOST_KEYS {
            let (machine, pin) = nth(n, false);
            assert!(vouched.insert(machine, pin));
        }
        vouched.in_use((0..MOST_KEYS).map(|n| nth(n, false).0).collect());
        let (late, pin) = nth(MOST_KEYS, false);
        assert!(
            !vouched.insert(late, pin),
            "no room for one only vouched for"
        );
        let (met, pin) = nth(MOST_KEYS + 1, true);
        assert!(vouched.insert(met, pin));
        assert_eq!(vouched.get(nth(0, false).0), None);
    }

    #[test]
    fn a_key_known_is_never_replaced_by_another() {
        let known = Known::load(None);
        let (machine, met) = nth(0, true);
        assert!(known.insert(machine, met));
        let other = Pin {
            key: [0xee; 32],
            met: true,
        };
        assert!(!known.insert(machine, other));
        assert_eq!(known.get(machine), Some(met));
        // Nor is one met only vouched for again.
        assert!(!known.insert(machine, Pin { met: false, ..met }));
        assert_eq!(known.get(machine), Some(met));
        // Forgotten, another is taken.
        assert!(known.remove(machine));
        assert!(known.insert(machine, other));
    }

    #[test]
    fn a_file_too_big_for_this_build_is_read_all_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let file = Known::beside(&dir.path().join("machine"));
        // As a build without the cap could write: the keys met among
        // many only vouched for.
        let keys: Vec<(MachineId, Pin)> =
            (0..MOST_KEYS * 8).map(|n| nth(n, n % 1000 == 0)).collect();
        let bytes = written(&keys).unwrap();
        assert!(bytes.len() > MAX_FILE);
        fs::write(&file, bytes).unwrap();
        let known = Known::load(Some(file.clone()));
        assert!(file.exists(), "never set aside");
        assert_eq!(known.all().len(), MOST_KEYS);
        for n in (0..MOST_KEYS * 8).step_by(1000) {
            assert_eq!(known.get(nth(n, true).0), Some(nth(n, true).1));
        }
        assert!(known.get(nth(MOST_KEYS * 8 - 1, false).0).is_some());
        assert!(known.get(nth(1, false).0).is_none());
    }
}
