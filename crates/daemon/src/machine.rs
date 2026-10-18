//! Which daemon this is: its [`Machine`], an ID, a name and a run.
//!
//! The ID is 16 random bytes, chosen the first time a daemon starts and
//! kept in a file in clackatoo's state directory
//! (`~/.local/state/clackatoo/machine`), so it's the same through
//! restarts, and through upgrades, which also hand it over. Every daemon
//! that keeps its state in that directory is the same machine. The name is
//! for people: `name` in `daemon/machine.toml` in the config directory, or
//! else the machine's hostname. The run is the daemon's own number, chosen
//! as it starts afresh and kept through upgrades, which tells a copy of
//! the state directory, elsewhere, from the machine itself.
//!
//! # The file
//!
//! JSON, one value: `{"clackatoo_machine": 1, "id": "<32 hex digits>"}`.
//! Frozen like the workspaces' file: a field this build doesn't know is
//! ignored, and the ID is read from a newer version's file too. Written
//! once, in a file made afresh beside it for the user alone, synced, and
//! linked into place, so that of two daemons starting at once, the first
//! to write is the machine, and the other reads what it wrote.
//!
//! One that isn't JSON, or holds no ID, is set aside as
//! `machine.unreadable`, and a new ID chosen: the daemon is a new machine
//! to everyone from then on, rather than not starting. One that can't be
//! read just now, as on a disk that's failing, is left alone, and the
//! daemon goes as a new machine for this run only.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use clackatoo_protocol::{Machine, MachineId};
use serde::{Deserialize, Serialize};

/// The file's version.
const VERSION: u32 = 1;

/// The most bytes the file is read at.
const MAX_FILE: u64 = 4 * 1024;

/// The longest name kept, in characters: a hostname is at most 64.
pub(crate) const LONGEST_NAME: usize = 64;

/// The file, as this build writes it.
#[derive(Debug, Serialize)]
struct Kept {
    /// The version, named so, first, so the file says what it is.
    clackatoo_machine: u32,
    id: String,
}

/// The file, as any version of it is read.
#[derive(Debug, Deserialize)]
struct Versioned {
    clackatoo_machine: u32,
    #[serde(default)]
    id: Option<String>,
}

/// What the file holds.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// There's no file.
    Nothing,
    /// The ID.
    Id(MachineId),
    /// Something else, which is why.
    Damaged(String),
    /// A newer version's file, with no ID this build reads.
    Newer(u32),
}

/// This daemon's identity, in the run numbered `run`: the ID handed over by
/// the build before, if this daemon carries on from an upgrade, or else
/// the one kept in `file`, or a new one, kept there for next time; and
/// `name`, or else the hostname. With no file, a new ID each time it
/// starts, as in tests.
pub(crate) fn identity(
    file: Option<&Path>,
    name: Option<&str>,
    handed: Option<MachineId>,
    run: u64,
) -> Machine {
    let id = match file {
        Some(file) => kept_id(file, handed),
        None => handed.unwrap_or_else(random),
    };
    let name = name
        .map(shown)
        .filter(|name| !name.is_empty())
        .or_else(hostname)
        .unwrap_or_else(|| "clackatoo".to_owned());
    Machine { id, name, run }
}

/// The ID `file` keeps, written there first if it keeps none: `handed`,
/// if the build before handed one over, which also wins over another in
/// the file, or else a new one.
fn kept_id(file: &Path, handed: Option<MachineId>) -> MachineId {
    match read(file) {
        Ok(Found::Id(kept)) => match handed {
            Some(id) if id != kept => {
                log::warn!(
                    "this machine's ID is {id}, as the build before handed it over, but {} \
                     says {kept}: it's written there again",
                    file.display()
                );
                if let Err(err) = write(file, id, true) {
                    log::error!("{} couldn't be written: {err}", file.display());
                }
                id
            }
            _ => kept,
        },
        Ok(Found::Nothing) => first_write(file, handed.unwrap_or_else(random)),
        Ok(Found::Damaged(why)) => {
            log::error!(
                "this machine's ID in {} can't be read, so it's a new machine from now on: {why}",
                file.display()
            );
            let mut aside = file.as_os_str().to_owned();
            aside.push(".unreadable");
            if let Err(err) = fs::rename(file, &aside) {
                log::error!("that file couldn't be set aside: {err}");
                return handed.unwrap_or_else(random);
            }
            first_write(file, handed.unwrap_or_else(random))
        }
        Ok(Found::Newer(version)) => {
            log::warn!(
                "{} is version {version}, with no ID this build can read: this run goes as a \
                 machine of its own, and leaves the file as it is",
                file.display()
            );
            handed.unwrap_or_else(random)
        }
        Err(err) => {
            log::error!(
                "this machine's ID in {} couldn't be read just now, so this run goes as a \
                 machine of its own, and leaves the file as it is: {err}",
                file.display()
            );
            handed.unwrap_or_else(random)
        }
    }
}

/// Keeps `id` in `file`, where there's none yet, unless another daemon
/// gets there first: returns the ID that's kept, `id` or the other's. If
/// it can't be kept, says so in the log, and goes on as `id` all the
/// same.
fn first_write(file: &Path, id: MachineId) -> MachineId {
    match write(file, id, false) {
        Ok(()) => id,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => match read(file) {
            Ok(Found::Id(first)) => first,
            _ => id,
        },
        Err(err) => {
            log::error!(
                "this machine's ID couldn't be kept in {}, so the next start is a new machine: \
                 {err}",
                file.display()
            );
            id
        }
    }
}

/// `name` as it's shown: one line, without control or bidi characters,
/// spaces at either end, or runs of spaces, at most [`LONGEST_NAME`]
/// characters. For a name from another machine too, which may be anything.
pub(crate) fn shown(name: &str) -> String {
    let mut shown = String::new();
    for ch in name.chars() {
        let ch = if ch.is_whitespace() { ' ' } else { ch };
        if ch.is_control() || is_bidi(ch) || (ch == ' ' && shown.ends_with(' ')) {
            continue;
        }
        shown.push(ch);
    }
    shown
        .trim()
        .chars()
        .take(LONGEST_NAME)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Whether `ch` changes which way text runs, which could make a name read
/// as another.
pub(crate) fn is_bidi(ch: char) -> bool {
    matches!(ch, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// The machine's hostname, if it has one.
fn hostname() -> Option<String> {
    let name = fs::read_to_string("/proc/sys/kernel/hostname").ok()?;
    Some(shown(&name)).filter(|name| !name.is_empty())
}

/// A new ID, from the system's randomness.
fn random() -> MachineId {
    use ring::rand::SecureRandom;
    let mut id = [0; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut id)
        .expect("the system's randomness can be read");
    MachineId(id)
}

/// What `file` holds. Fails only if it can't be read.
fn read(file: &Path) -> io::Result<Found> {
    let opened = match File::open(file) {
        Ok(opened) => opened,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    opened.take(MAX_FILE).read_to_end(&mut bytes)?;
    let versioned: Versioned = match serde_json::from_slice(&bytes) {
        Ok(versioned) => versioned,
        Err(err) => return Ok(Found::Damaged(err.to_string())),
    };
    let id = versioned.id.as_deref().map(str::parse::<MachineId>);
    Ok(match (versioned.clackatoo_machine, id) {
        (0, _) => Found::Damaged("it says it's version 0".to_owned()),
        (_, Some(Ok(id))) => Found::Id(id),
        (VERSION, Some(Err(err))) => Found::Damaged(err.to_string()),
        (VERSION, None) => Found::Damaged("it holds no ID".to_owned()),
        (version, _) => Found::Newer(version),
    })
}

/// Writes `id` as the file at `path`: in full beside it, in a file made
/// afresh for the user alone, never through a link, and synced; then
/// renamed over it with `replace`, or else linked into place, which
/// fails with `AlreadyExists` if there's a file there already; and the
/// directory synced.
fn write(path: &Path, id: MachineId, replace: bool) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // A name of its own, as another daemon may be writing beside it.
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".new-{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    match fs::remove_file(&temporary) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let kept = Kept {
        clackatoo_machine: VERSION,
        id: id.to_string(),
    };
    let mut bytes = serde_json::to_vec(&kept).map_err(io::Error::other)?;
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
        if replace {
            fs::rename(&temporary, path)
        } else {
            fs::hard_link(&temporary, path)
        }
    })();
    let _ = fs::remove_file(&temporary);
    written?;
    File::open(dir)?.sync_all()
}

/// The name `daemon/machine.toml` in `config` gives this machine, if it
/// gives one: `name = "jack-desktop"`. A file that can't be read, or a
/// name that isn't text, is said in the log and passed over.
pub(crate) fn configured_name(config: &Path) -> Option<String> {
    let path = config.join("daemon/machine.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(err) => {
            log::warn!("{} couldn't be read: {err}", path.display());
            return None;
        }
    };
    let document = match text.parse::<toml_edit::DocumentMut>() {
        Ok(document) => document,
        Err(err) => {
            log::warn!("{} isn't TOML: {err}", path.display());
            return None;
        }
    };
    let name = document.get("name")?;
    let Some(name) = name.as_str() else {
        log::warn!("`name` in {} isn't text", path.display());
        return None;
    };
    Some(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity_in(file: &Path, handed: Option<MachineId>) -> MachineId {
        identity(Some(file), Some("desk"), handed, 1).id
    }

    #[test]
    fn the_id_is_kept_and_the_same_next_time() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/machine");
        let first = identity(Some(&file), Some("desk"), None, 7);
        assert_eq!((first.name.as_str(), first.run), ("desk", 7));
        assert_eq!(identity_in(&file, None), first.id);
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains(&first.id.to_string()), "{text}");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // Nothing left beside it.
        assert_eq!(fs::read_dir(dir.path().join("state")).unwrap().count(), 1);
        // Without a file, a new one each time.
        assert_ne!(
            identity(None, None, None, 1).id,
            identity(None, None, None, 1).id
        );
    }

    #[test]
    fn a_handed_over_id_wins_and_is_written_where_the_file_differs_or_went() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine");
        let handed = MachineId([9; 16]);
        assert_eq!(identity_in(&file, Some(handed)), handed);
        assert_eq!(identity_in(&file, None), handed);
        let other = MachineId([8; 16]);
        assert_eq!(identity_in(&file, Some(other)), other);
        assert_eq!(identity_in(&file, None), other, "written again");
    }

    #[test]
    fn the_first_daemon_to_write_the_file_is_the_machine() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine");
        let first = MachineId([1; 16]);
        write(&file, first, false).unwrap();
        // Another, which found no file a moment before, reads it.
        assert_eq!(first_write(&file, MachineId([2; 16])), first);
        assert_eq!(identity_in(&file, None), first);
    }

    #[test]
    fn a_damaged_file_is_set_aside_for_a_new_id() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine");
        fs::write(&file, "not json").unwrap();
        let id = identity_in(&file, None);
        assert_eq!(
            fs::read_to_string(dir.path().join("machine.unreadable")).unwrap(),
            "not json"
        );
        assert_eq!(identity_in(&file, None), id);
        fs::write(&file, r#"{"clackatoo_machine": 1, "id": "x"}"#).unwrap();
        assert_ne!(identity_in(&file, None), id);
    }

    #[test]
    fn a_newer_file_gives_its_id_or_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine");
        let id = MachineId([5; 16]);
        let newer = format!(r#"{{"clackatoo_machine": 2, "id": "{id}", "keys": []}}"#);
        fs::write(&file, &newer).unwrap();
        assert_eq!(identity_in(&file, None), id);
        // With no ID this build reads: a machine of its own this run, and
        // the file kept for the build that wrote it.
        fs::write(&file, r#"{"clackatoo_machine": 2, "ids": ["y"]}"#).unwrap();
        assert_ne!(identity_in(&file, None), id);
        assert!(fs::read_to_string(&file).unwrap().contains("ids"));
        assert!(!dir.path().join("machine.unreadable").exists());
    }

    #[test]
    fn a_file_that_cant_be_read_just_now_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file goes: reading it fails, but it isn't
        // damaged, so it's neither set aside nor written over.
        let file = dir.path().join("machine");
        fs::create_dir(&file).unwrap();
        let _ = identity_in(&file, None);
        assert!(file.is_dir());
        assert!(!dir.path().join("machine.unreadable").exists());
    }

    #[test]
    fn names_are_shown_safely() {
        assert_eq!(shown("  jack-desktop\n"), "jack-desktop");
        assert_eq!(shown("a\u{202e}b\x1b[31mc  d\te"), "ab[31mc d e");
        assert_eq!(shown(&"x".repeat(100)).len(), LONGEST_NAME);
        // A blank name is no name: the hostname is used.
        assert!(!identity(None, Some(" \n"), None, 1).name.trim().is_empty());
    }

    #[test]
    fn the_name_comes_from_the_config() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(configured_name(dir.path()), None);
        fs::create_dir(dir.path().join("daemon")).unwrap();
        let file = dir.path().join("daemon/machine.toml");
        fs::write(&file, "name = \"desk\"\n").unwrap();
        assert_eq!(configured_name(dir.path()).as_deref(), Some("desk"));
        fs::write(&file, "name = 3\n").unwrap();
        assert_eq!(configured_name(dir.path()), None);
    }
}
