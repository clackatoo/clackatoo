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
//! "met": true}]}`. Written as the `machine` file is: in full beside it,
//! in a file made afresh for the user alone, synced, and renamed over it.
//! Writing happens on a thread of its own, as a key is learned, which
//! writes what's known by then, so the last write is always the newest.
//! One that can't be read is set aside as `known_machines.unreadable`,
//! and nothing is known: each machine is met again.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use clackatoo_protocol::MachineId;
use serde::{Deserialize, Serialize};

/// The file's version.
const VERSION: u32 = 1;

/// The most bytes the file is read at: far more than any number of
/// machines a person reaches.
const MAX_FILE: u64 = 1024 * 1024;

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
    keys: Arc<Mutex<BTreeMap<MachineId, Pin>>>,
    /// Held while the file is written, so writes never cross.
    writing: Arc<Mutex<()>>,
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
        }
    }

    /// Where the keys are kept for the daemon whose `machine` file is
    /// `machine`: beside it.
    pub(crate) fn beside(machine: &Path) -> PathBuf {
        machine.with_file_name("known_machines")
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, BTreeMap<MachineId, Pin>> {
        self.keys.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The key `machine` is known by, if it is.
    pub(crate) fn get(&self, machine: MachineId) -> Option<Pin> {
        self.keys().get(&machine).copied()
    }

    /// Every key known, by machine.
    pub(crate) fn all(&self) -> Vec<(MachineId, Pin)> {
        self.keys().iter().map(|(&id, &pin)| (id, pin)).collect()
    }

    /// Knows `machine` by `pin` from now on, keeping it, and returns
    /// whether that's new.
    pub(crate) fn insert(&self, machine: MachineId, pin: Pin) -> bool {
        let new = self.keys().insert(machine, pin) != Some(pin);
        if new {
            self.keep();
        }
        new
    }

    /// Knows `machine` by `pin`, if it knows it by none, as from a build
    /// that kept keys elsewhere.
    pub(crate) fn learn(&self, machine: MachineId, pin: Pin) {
        let new = {
            let mut keys = self.keys();
            let new = !keys.contains_key(&machine);
            keys.entry(machine).or_insert(pin);
            new
        };
        if new {
            self.keep();
        }
    }

    /// Forgets the key `machine` is known by, and returns whether it was.
    pub(crate) fn remove(&self, machine: MachineId) -> bool {
        let was = self.keys().remove(&machine).is_some();
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
        let writing = Arc::clone(&self.writing);
        let keys = Arc::clone(&self.keys);
        let spawned = std::thread::Builder::new()
            .name("known-machines".into())
            .spawn(move || {
                let _held = writing.lock().unwrap_or_else(PoisonError::into_inner);
                let keys: Vec<(MachineId, Pin)> = (keys.lock())
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .map(|(&id, &pin)| (id, pin))
                    .collect();
                if let Err(err) = write(&file, &keys) {
                    log::error!("{} couldn't be written: {err}", file.display());
                }
            });
        if let Err(err) = spawned {
            log::error!("there's no thread to keep the keys known on: {err}");
        }
    }

    /// Waits for what's being written to the file to be written, as the
    /// daemon stops or upgrades.
    pub(crate) fn kept(&self) {
        if let Some(file) = &self.file {
            let _held = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(err) = write(file, &self.all()) {
                log::error!("{} couldn't be written: {err}", file.display());
            }
        }
    }
}

/// The keys `file` keeps: none if there's no file, and none, with the file
/// set aside, if it can't be read as one.
fn read(file: &Path) -> BTreeMap<MachineId, Pin> {
    let mut bytes = Vec::new();
    match File::open(file) {
        Ok(opened) => {
            if let Err(err) = opened.take(MAX_FILE).read_to_end(&mut bytes) {
                log::error!("{} couldn't be read: {err}", file.display());
                return BTreeMap::new();
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => {
            log::error!("{} couldn't be read: {err}", file.display());
            return BTreeMap::new();
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
            return BTreeMap::new();
        }
    };
    (kept.machines.iter())
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
        .collect()
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
    fn keys_are_kept_for_the_user_alone_and_read_again() {
        use std::os::unix::fs::PermissionsExt;

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
        assert_eq!(Known::load(Some(file.clone())).get(server), Some(pin));
        // Learned from elsewhere, a key known already stays.
        let other = Pin {
            key: [8; 32],
            met: false,
        };
        known.learn(server, other);
        assert_eq!(known.get(server), Some(pin));
        assert!(known.remove(server));
        known.kept();
        assert_eq!(Known::load(Some(file.clone())).get(server), None);
        // One that can't be read is set aside, and nothing is known.
        fs::write(&file, "not json").unwrap();
        assert!(Known::load(Some(file.clone())).all().is_empty());
        assert!(dir.path().join("known_machines.unreadable").exists());
    }
}
