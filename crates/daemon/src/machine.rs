//! Which daemon this is: its [`Machine`], an ID, a name and a run; and the
//! key it proves it by.
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
//! # The identity key
//!
//! Beside the ID, the file keeps an Ed25519 key, chosen with it: the
//! machine's identity. Each run's QUIC certificate is signed by it (see
//! [`Signer`]), so a daemon reaching this one through another machine,
//! which carries its connection on and could put a certificate of its own
//! in place of this one's, can tell. The other machine knows the key from
//! when it last reached this one, straight with ssh, or as the machine
//! carrying it on vouched for it the first time (see the server's
//! `chain`). A file from before keys gets one, written back.
//!
//! # The file
//!
//! JSON, one value: `{"clackatoo_machine": 1, "id": "<32 hex digits>",
//! "key": "<64 hex digits>"}`, the key's seed: the file is the user's
//! alone, as it holds what proves the machine.
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
#[derive(Serialize)]
struct Kept {
    /// The version, named so, first, so the file says what it is.
    clackatoo_machine: u32,
    id: String,
    /// The identity key's seed.
    key: String,
}

/// The file, as any version of it is read.
#[derive(Deserialize)]
struct Versioned {
    clackatoo_machine: u32,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    key: Option<String>,
}

/// The seed of a machine's identity key: 32 random bytes. Never shown in
/// debug output.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seed(pub(crate) [u8; 32]);

impl std::fmt::Debug for Seed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seed(..)")
    }
}

/// A machine's identity key, which signs its QUIC certificates.
pub(crate) struct Signer {
    seed: Seed,
    pair: ring::signature::Ed25519KeyPair,
}

impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("key", &hex(&self.key()))
            .finish_non_exhaustive()
    }
}

/// What a machine's identity key signs, before the machine's ID and the
/// certificate.
const SIGNED: &[u8] = b"clackatoo carries QUIC with this certificate\0";

impl Signer {
    fn new(seed: Seed) -> Self {
        let pair = ring::signature::Ed25519KeyPair::from_seed_unchecked(&seed.0)
            .expect("any 32 bytes are an Ed25519 seed");
        Self { seed, pair }
    }

    /// The public key, which others know the machine by.
    pub(crate) fn key(&self) -> [u8; 32] {
        use ring::signature::KeyPair;
        let mut key = [0; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        key
    }

    /// The seed, for the next build to sign with as this one did.
    pub(crate) fn seed(&self) -> Seed {
        self.seed
    }

    /// `certificate`, the QUIC certificate of this run of `machine`'s
    /// daemon, signed.
    pub(crate) fn sign(&self, machine: MachineId, certificate: &[u8]) -> Vec<u8> {
        self.pair
            .sign(&signed(machine, certificate))
            .as_ref()
            .to_vec()
    }
}

/// Whether `signature` is `certificate`, of `machine`'s daemon, signed by
/// the identity key `key`.
pub(crate) fn verify(key: &[u8], machine: MachineId, certificate: &[u8], signature: &[u8]) -> bool {
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&signed(machine, certificate), signature)
        .is_ok()
}

/// What's signed for `certificate` of `machine`'s daemon.
fn signed(machine: MachineId, certificate: &[u8]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(SIGNED.len() + 16 + certificate.len());
    signed.extend_from_slice(SIGNED);
    signed.extend_from_slice(&machine.0);
    signed.extend_from_slice(certificate);
    signed
}

/// `bytes` as hex digits.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// The 32 bytes `text`, 64 hex digits, says.
pub(crate) fn unhex(text: &str) -> Option<[u8; 32]> {
    let digits = text.as_bytes();
    if digits.len() != 64 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let mut bytes = [0; 32];
    for (byte, pair) in bytes.iter_mut().zip(digits.chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

/// What the file holds.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    /// There's no file.
    Nothing,
    /// The ID, and the key's seed, if it keeps one.
    Id(MachineId, Option<Seed>),
    /// Something else, which is why.
    Damaged(String),
    /// A newer version's file, with no ID this build reads.
    Newer(u32),
}

/// What the build before handed over of this machine's identity: its ID,
/// and its key's seed, from a build that has one.
pub(crate) type Handed = (MachineId, Option<Seed>);

/// This daemon's identity, in the run numbered `run`, and its key: the ID
/// and key handed over by the build before, if this daemon carries on
/// from an upgrade, or else those kept in `file`, or new ones, kept there
/// for next time; and `name`, or else the hostname. With no file, new
/// ones each time it starts, as in tests.
pub(crate) fn identity(
    file: Option<&Path>,
    name: Option<&str>,
    handed: Option<Handed>,
    run: u64,
) -> (Machine, Signer) {
    let (id, seed) = match file {
        Some(file) => kept_id(file, handed),
        None => (
            handed.map_or_else(random, |(id, _)| id),
            handed
                .and_then(|(_, seed)| seed)
                .unwrap_or_else(random_seed),
        ),
    };
    let name = name
        .map(shown)
        .filter(|name| !name.is_empty())
        .or_else(hostname)
        .unwrap_or_else(|| "clackatoo".to_owned());
    (Machine { id, name, run }, Signer::new(seed))
}

/// The ID and key `file` keeps, written there first if it keeps none:
/// those `handed` over by the build before, which also win over others in
/// the file, or else new ones. A file from before keys gets one.
fn kept_id(file: &Path, handed: Option<Handed>) -> (MachineId, Seed) {
    let handed_id = handed.map(|(id, _)| id);
    let handed_seed = handed.and_then(|(_, seed)| seed);
    let new = || {
        (
            handed_id.unwrap_or_else(random),
            handed_seed.unwrap_or_else(random_seed),
        )
    };
    match read(file) {
        Ok(Found::Id(kept, kept_seed)) => {
            let id = handed_id.unwrap_or(kept);
            if id != kept {
                log::warn!(
                    "this machine's ID is {id}, as the build before handed it over, but {} \
                     says {kept}: it's written there again",
                    file.display()
                );
            }
            // A key kept for another ID is that one's.
            let seed = handed_seed
                .or(kept_seed.filter(|_| id == kept))
                .unwrap_or_else(random_seed);
            if (id != kept || Some(seed) != kept_seed)
                && let Err(err) = write(file, id, seed, true)
            {
                log::error!("{} couldn't be written: {err}", file.display());
            }
            (id, seed)
        }
        Ok(Found::Nothing) => {
            let (id, seed) = new();
            first_write(file, id, seed)
        }
        Ok(Found::Damaged(why)) => {
            log::error!(
                "this machine's ID in {} can't be read, so it's a new machine from now on: {why}",
                file.display()
            );
            let mut aside = file.as_os_str().to_owned();
            aside.push(".unreadable");
            if let Err(err) = fs::rename(file, &aside) {
                log::error!("that file couldn't be set aside: {err}");
                return new();
            }
            let (id, seed) = new();
            first_write(file, id, seed)
        }
        Ok(Found::Newer(version)) => {
            log::warn!(
                "{} is version {version}, with no ID this build can read: this run goes as a \
                 machine of its own, and leaves the file as it is",
                file.display()
            );
            new()
        }
        Err(err) => {
            log::error!(
                "this machine's ID in {} couldn't be read just now, so this run goes as a \
                 machine of its own, and leaves the file as it is: {err}",
                file.display()
            );
            new()
        }
    }
}

/// Keeps `id` and `seed` in `file`, where there's none yet, unless another
/// daemon gets there first: returns those that are kept, these or the
/// other's. If they can't be kept, says so in the log, and goes on as
/// these all the same.
fn first_write(file: &Path, id: MachineId, seed: Seed) -> (MachineId, Seed) {
    match write(file, id, seed, false) {
        Ok(()) => (id, seed),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => match read(file) {
            Ok(Found::Id(first, Some(kept))) => (first, kept),
            _ => (id, seed),
        },
        Err(err) => {
            log::error!(
                "this machine's ID couldn't be kept in {}, so the next start is a new machine: \
                 {err}",
                file.display()
            );
            (id, seed)
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

/// A new identity key's seed, from the system's randomness.
fn random_seed() -> Seed {
    use ring::rand::SecureRandom;
    let mut seed = [0; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut seed)
        .expect("the system's randomness can be read");
    Seed(seed)
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
    // A key that can't be read is as none: a new one is chosen.
    let seed = versioned.key.as_deref().and_then(unhex).map(Seed);
    Ok(match (versioned.clackatoo_machine, id) {
        (0, _) => Found::Damaged("it says it's version 0".to_owned()),
        (_, Some(Ok(id))) => Found::Id(id, seed),
        (VERSION, Some(Err(err))) => Found::Damaged(err.to_string()),
        (VERSION, None) => Found::Damaged("it holds no ID".to_owned()),
        (version, _) => Found::Newer(version),
    })
}

/// Writes `id` and `seed` as the file at `path`: in full beside it, in a
/// file made afresh for the user alone, never through a link, and synced;
/// then renamed over it with `replace`, or else linked into place, which
/// fails with `AlreadyExists` if there's a file there already; and the
/// directory synced.
fn write(path: &Path, id: MachineId, seed: Seed, replace: bool) -> io::Result<()> {
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
        key: hex(&seed.0),
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
        identity(Some(file), Some("desk"), handed.map(|id| (id, None)), 1)
            .0
            .id
    }

    /// The identity key the daemon keeping its identity in `file` has.
    fn key_in(file: &Path) -> [u8; 32] {
        identity(Some(file), None, None, 1).1.key()
    }

    #[test]
    fn the_key_is_kept_with_the_id_and_signs_what_only_it_signs() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("machine");
        let (machine, signer) = identity(Some(&file), None, None, 1);
        assert_eq!(key_in(&file), signer.key(), "kept");
        let signature = signer.sign(machine.id, b"certificate");
        assert!(verify(
            &signer.key(),
            machine.id,
            b"certificate",
            &signature
        ));
        // Not another certificate, nor another machine's, nor another key.
        assert!(!verify(
            &signer.key(),
            machine.id,
            b"certificat3",
            &signature
        ));
        assert!(!verify(
            &signer.key(),
            MachineId([3; 16]),
            b"certificate",
            &signature
        ));
        let other = identity(None, None, None, 1).1;
        assert!(!verify(
            &other.key(),
            machine.id,
            b"certificate",
            &signature
        ));
        // A file from before keys keeps its ID, and gets one, kept.
        fs::write(
            &file,
            format!(r#"{{"clackatoo_machine": 1, "id": "{}"}}"#, machine.id),
        )
        .unwrap();
        assert_eq!(identity_in(&file, None), machine.id);
        let new = key_in(&file);
        assert_ne!(new, signer.key());
        assert_eq!(key_in(&file), new);
        // A key handed over wins, and is written back.
        let handed = identity(
            Some(&file),
            None,
            Some((machine.id, Some(signer.seed()))),
            1,
        );
        assert_eq!(handed.1.key(), signer.key());
        assert_eq!(key_in(&file), signer.key());
        assert!(!format!("{:?}", signer.seed()).contains(&hex(&signer.seed().0)));
    }

    #[test]
    fn the_id_is_kept_and_the_same_next_time() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state/machine");
        let first = identity(Some(&file), Some("desk"), None, 7).0;
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
            identity(None, None, None, 1).0.id,
            identity(None, None, None, 1).0.id
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
        write(&file, first, Seed([1; 32]), false).unwrap();
        // Another, which found no file a moment before, reads it.
        assert_eq!(
            first_write(&file, MachineId([2; 16]), Seed([2; 32])),
            (first, Seed([1; 32]))
        );
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
        assert!(
            !identity(None, Some(" \n"), None, 1)
                .0
                .name
                .trim()
                .is_empty()
        );
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
