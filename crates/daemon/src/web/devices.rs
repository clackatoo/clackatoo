//! The devices paired with the web app, kept on disk from one run of the
//! daemon to the next, and written on a thread of their own, as fsync
//! takes far too long for the event loop.
//!
//! # The format
//!
//! One file, `devices.json`, in the web app's directory
//! (`~/.local/share/clackatoo/web`), only the user may read: a JSON object
//! saying the format's version, [`VERSION`], under `clackatoo_devices`,
//! the number the next device gets, and the devices. It's written whole,
//! beside the old one, and renamed over it, so a crash leaves one or the
//! other. Like the boards' format, it's frozen: a field may be added, which
//! older builds ignore, but anything else needs a new version, which a
//! newer build reads beside this one. A file of a newer version than this
//! build knows is left alone, and the web app stays off.
//!
//! Only one daemon at a time keeps the devices in a directory: it holds a
//! lock on `.lock` in it, and another daemon finds it held and goes
//! without the web app.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use super::auth;

/// The format's version.
pub(crate) const VERSION: u32 = 1;

/// The file's name in the web app's directory.
const FILE: &str = "devices.json";

/// The most devices kept: past this, pairing is refused until some are
/// revoked.
pub(super) const MOST: usize = 64;

/// How long a device's last use is kept only in memory before it's
/// written, so connecting doesn't write the file each time.
const USED_EVERY: Duration = Duration::from_hours(1);

/// A paired device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Device {
    pub(crate) id: u64,
    pub(crate) name: String,
    /// The hash of its token.
    pub(crate) token: [u8; 32],
    /// When it paired, in milliseconds since the Unix epoch.
    pub(crate) paired: u64,
    /// When it last connected.
    pub(crate) used: u64,
    /// When its password was last given, if it ever was.
    pub(crate) password: Option<u64>,
    /// Who Tailscale said it was when it paired, if it paired through
    /// Tailscale: it's let in as no one else.
    pub(crate) login: Option<String>,
    /// Where its notifications go, if it asked for them.
    pub(crate) push: Option<Subscription>,
}

/// Where a device's notifications go: its browser's push service, and
/// the keys they're sealed with for it (RFC 8291).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Subscription {
    /// The push service's address for the device.
    pub(crate) endpoint: String,
    /// The browser's public key, uncompressed: 65 bytes.
    pub(crate) p256dh: Vec<u8>,
    /// The secret the browser gave, 16 bytes.
    pub(crate) auth: Vec<u8>,
}

/// Every device, and the file they're kept in.
#[derive(Debug)]
pub(crate) struct Devices {
    pub(crate) list: Vec<Device>,
    next: u64,
    /// When each device's last use was last written, by its ID.
    written_used: Vec<(u64, u64)>,
    disk: Disk,
}

impl Devices {
    /// The devices kept in `dir`, made if it isn't there, with a thread to
    /// write them as they change.
    ///
    /// # Errors
    ///
    /// If the directory can't be made or locked, as when another daemon
    /// keeps it, or its file can't be read or is of a newer version.
    pub(crate) fn open(dir: &Path) -> Result<Self, String> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|err| format!("{} couldn't be made: {err}", dir.display()))?;
        let lock = lock(dir)?;
        let (next, list) = read(&dir.join(FILE))?;
        let written_used = list.iter().map(|device| (device.id, device.used)).collect();
        Ok(Self {
            list,
            next,
            written_used,
            disk: Disk::start(dir.to_owned(), lock),
        })
    }

    /// Adds a device, called `name`, with the token whose hash is `token`,
    /// paired now, by `login` through Tailscale, with its password given
    /// now if `password`. Returns its ID.
    pub(crate) fn add(
        &mut self,
        name: String,
        token: [u8; 32],
        login: Option<String>,
        password: bool,
        now: u64,
    ) -> u64 {
        let id = self.next;
        self.next += 1;
        self.list.push(Device {
            id,
            name,
            token,
            paired: now,
            used: now,
            password: password.then_some(now),
            login,
            push: None,
        });
        self.written_used.push((id, now));
        self.save();
        id
    }

    /// The device whose token hashes to `token`.
    pub(crate) fn by_token(&self, token: &[u8; 32]) -> Option<&Device> {
        // Each is compared whole, so how long it takes says nothing of
        // how much of any matched.
        let mut found = None;
        for device in &self.list {
            if auth::same_hash(&device.token, token) {
                found = Some(device);
            }
        }
        found
    }

    /// The device with `id`, to change.
    pub(crate) fn get_mut(&mut self, id: u64) -> Option<&mut Device> {
        self.list.iter_mut().find(|device| device.id == id)
    }

    /// Takes away the device with `id`. Returns whether there was one.
    pub(crate) fn remove(&mut self, id: u64) -> bool {
        let before = self.list.len();
        self.list.retain(|device| device.id != id);
        self.written_used.retain(|&(device, _)| device != id);
        let removed = self.list.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    /// Takes away the devices unused since `before`, in milliseconds since
    /// the Unix epoch. Returns their IDs.
    pub(crate) fn expire(&mut self, before: u64) -> Vec<u64> {
        let gone: Vec<u64> = (self.list.iter())
            .filter(|device| device.used < before)
            .map(|device| device.id)
            .collect();
        for &id in &gone {
            log::info!("device {id} expired, unused");
            self.remove(id);
        }
        gone
    }

    /// Notes that the device with `id` connected `now`: kept in memory,
    /// and written only if it's been a while since it last was.
    pub(crate) fn used(&mut self, id: u64, now: u64) {
        let Some(device) = self.get_mut(id) else {
            return;
        };
        device.used = now;
        let written = (self.written_used.iter_mut()).find(|(device, _)| *device == id);
        match written {
            Some((_, at)) if now.saturating_sub(*at) < millis(USED_EVERY) => {}
            Some((_, at)) => {
                *at = now;
                self.save();
            }
            None => {
                self.written_used.push((id, now));
                self.save();
            }
        }
    }

    /// Writes the devices as they are now, on the thread.
    pub(crate) fn save(&self) {
        self.disk.write(encode(self.next, &self.list));
    }

    /// Waits up to `limit` for every change to be on disk.
    ///
    /// # Errors
    ///
    /// If they aren't by then.
    pub(crate) fn flush(&self, limit: Duration) -> Result<(), String> {
        self.disk.flush(limit)
    }
}

/// Milliseconds in `duration`.
pub(crate) fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The file: what it is, the next ID, and the devices.
#[derive(Debug, Serialize, Deserialize)]
struct FileV1 {
    /// The format's version, [`VERSION`], first, so the file says what it
    /// is.
    clackatoo_devices: u32,
    next: u64,
    devices: Vec<FileDevice>,
}

/// A device, as the file keeps it.
#[derive(Debug, Serialize, Deserialize)]
struct FileDevice {
    id: u64,
    name: String,
    /// The SHA-256 of its token, in hex.
    token: String,
    paired: u64,
    used: u64,
    #[serde(default)]
    password: Option<u64>,
    #[serde(default)]
    login: Option<String>,
    #[serde(default)]
    push: Option<FilePush>,
}

/// Where a device's notifications go, as the file keeps it: the keys in
/// base64url, as browsers give them.
#[derive(Debug, Serialize, Deserialize)]
struct FilePush {
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// Just the version, to read before the rest.
#[derive(Deserialize)]
struct Version {
    clackatoo_devices: u32,
}

/// The file's text, for `next` and `devices`.
fn encode(next: u64, devices: &[Device]) -> Vec<u8> {
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let file = FileV1 {
        clackatoo_devices: VERSION,
        next,
        devices: (devices.iter())
            .map(|device| FileDevice {
                id: device.id,
                name: device.name.clone(),
                token: hex(&device.token),
                paired: device.paired,
                used: device.used,
                password: device.password,
                login: device.login.clone(),
                push: device.push.as_ref().map(|push| FilePush {
                    endpoint: push.endpoint.clone(),
                    p256dh: b64.encode(&push.p256dh),
                    auth: b64.encode(&push.auth),
                }),
            })
            .collect(),
    };
    let mut text = serde_json::to_vec_pretty(&file).expect("devices are JSON");
    text.push(b'\n');
    text
}

/// The next ID and the devices in the file at `path`: none if it isn't
/// there.
fn read(path: &Path) -> Result<(u64, Vec<Device>), String> {
    let text = match fs::read(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok((1, Vec::new())),
        Err(err) => return Err(format!("{} couldn't be read: {err}", path.display())),
    };
    decode(&text).map_err(|err| format!("{}: {err}", path.display()))
}

/// The next ID and the devices `text` holds.
fn decode(text: &[u8]) -> Result<(u64, Vec<Device>), String> {
    let version: Version =
        serde_json::from_slice(text).map_err(|err| format!("isn't a devices file: {err}"))?;
    if version.clackatoo_devices != VERSION {
        return Err(format!(
            "is of version {}, which this build doesn't know",
            version.clackatoo_devices
        ));
    }
    let file: FileV1 = serde_json::from_slice(text).map_err(|err| err.to_string())?;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut devices = Vec::with_capacity(file.devices.len());
    for device in file.devices {
        let token = unhex(&device.token).ok_or_else(|| format!("device {}'s token", device.id))?;
        let push = device.push.and_then(|push| {
            Some(Subscription {
                endpoint: push.endpoint,
                p256dh: b64.decode(push.p256dh).ok()?,
                auth: b64.decode(push.auth).ok()?,
            })
        });
        devices.push(Device {
            id: device.id,
            name: device.name,
            token,
            paired: device.paired,
            used: device.used,
            password: device.password,
            login: device.login,
            push,
        });
    }
    let next = (devices.iter().map(|device| device.id + 1))
        .chain([file.next, 1])
        .max()
        .expect("there's always 1");
    Ok((next, devices))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn unhex(text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Locks `dir` for this daemon, or says another has it.
fn lock(dir: &Path) -> Result<File, String> {
    let path = dir.join(".lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|err| format!("{} couldn't be opened: {err}", path.display()))?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(file),
        Err(rustix::io::Errno::WOULDBLOCK) => Err(format!(
            "another daemon keeps the devices in {}",
            dir.display()
        )),
        Err(err) => Err(format!("{} couldn't be locked: {err}", path.display())),
    }
}

/// What the writing thread is asked.
enum Job {
    /// Write this text as the file.
    Write(Vec<u8>),
    /// Say so once everything asked before is written.
    Flush(Sender<()>),
}

/// The thread writing the file.
#[derive(Debug)]
struct Disk {
    jobs: Option<Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Disk {
    /// Waits for the last changes to be written, and the lock let go of.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            log::error!("the thread writing devices panicked");
        }
    }
}

impl Disk {
    /// Starts the thread writing the file in `dir`, holding `lock` for as
    /// long as it runs.
    fn start(dir: PathBuf, lock: File) -> Self {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("devices".to_owned())
            .spawn(move || {
                let _lock = lock;
                while let Ok(job) = inbox.recv() {
                    let mut text = None;
                    let mut flushed = Vec::new();
                    for job in std::iter::once(job).chain(inbox.try_iter()) {
                        match job {
                            // Only the latest is worth writing.
                            Job::Write(latest) => text = Some(latest),
                            Job::Flush(done) => flushed.push(done),
                        }
                    }
                    if let Some(text) = text
                        && let Err(err) = write_file(&dir, &text)
                    {
                        log::error!("the devices couldn't be written: {err}");
                    }
                    for done in flushed {
                        let _ = done.send(());
                    }
                }
            });
        let thread = thread
            .inspect_err(|err| log::error!("the thread writing devices couldn't start: {err}"))
            .ok();
        Self {
            jobs: Some(jobs),
            thread,
        }
    }

    fn write(&self, text: Vec<u8>) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Write(text));
        }
    }

    fn flush(&self, limit: Duration) -> Result<(), String> {
        let (done, wait) = mpsc::channel();
        (self.jobs.as_ref())
            .and_then(|jobs| jobs.send(Job::Flush(done)).ok())
            .ok_or_else(|| "the thread writing devices has stopped".to_owned())?;
        wait.recv_timeout(limit)
            .map_err(|_| "the devices are taking too long to write".to_owned())
    }
}

/// Writes `text` as the file in `dir`, beside the old, renamed over it.
fn write_file(dir: &Path, text: &[u8]) -> io::Result<()> {
    let partial = dir.join(format!("{FILE}.partial"));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&partial)?;
    file.write_all(text)?;
    file.sync_all()?;
    fs::rename(&partial, dir.join(FILE))?;
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u64) -> Device {
        Device {
            id,
            name: format!("phone {id}"),
            token: [u8::try_from(id).unwrap(); 32],
            paired: 10,
            used: 20,
            password: Some(15),
            login: Some("jack@github".into()),
            push: Some(Subscription {
                endpoint: "https://web.push.apple.com/abc".into(),
                p256dh: vec![4; 65],
                auth: vec![9; 16],
            }),
        }
    }

    #[test]
    fn devices_come_back_as_they_were_written() {
        let devices = vec![device(3), device(7)];
        let (next, read) = decode(&encode(5, &devices)).unwrap();
        assert_eq!(read, devices);
        assert_eq!(next, 8, "never an ID a device has");
    }

    #[test]
    fn the_format_is_frozen() {
        // Version 1 as written: fields added later read as their defaults,
        // and fields this build doesn't know are ignored.
        let text = br#"{"clackatoo_devices":1,"next":2,"devices":[{"id":1,"name":"iPhone",
            "token":"0000000000000000000000000000000000000000000000000000000000000001",
            "paired":1,"used":2,"later":true}]}"#;
        let (next, devices) = decode(text).unwrap();
        assert_eq!(next, 2);
        assert_eq!(devices[0].name, "iPhone");
        assert_eq!(devices[0].token[31], 1);
        assert_eq!(devices[0].push, None);
        let newer = br#"{"clackatoo_devices":2,"devices":"elsewhere"}"#;
        assert!(decode(newer).unwrap_err().contains("version 2"));
        assert!(decode(b"[]").is_err());
    }

    #[test]
    fn devices_are_kept_on_disk_by_one_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut devices = Devices::open(dir.path()).unwrap();
        assert!(
            Devices::open(dir.path())
                .unwrap_err()
                .contains("another daemon"),
            "the first holds the lock"
        );
        let id = devices.add("iPhone".into(), [1; 32], None, true, 100);
        devices.add("iPad".into(), [2; 32], None, false, 100);
        assert!(devices.remove(id + 1));
        devices.flush(Duration::from_secs(5)).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &fs::metadata(dir.path().join(FILE)).unwrap().permissions(),
        );
        assert_eq!(mode & 0o077, 0, "only the user reads it");
        drop(devices);
        let devices = Devices::open(dir.path()).unwrap();
        assert_eq!(devices.list.len(), 1);
        assert_eq!(devices.list[0].name, "iPhone");
        assert!(devices.by_token(&[1; 32]).is_some());
        assert!(devices.by_token(&[2; 32]).is_none());
    }

    #[test]
    fn devices_unused_too_long_expire() {
        let dir = tempfile::tempdir().unwrap();
        let mut devices = Devices::open(dir.path()).unwrap();
        let old = devices.add("old".into(), [1; 32], None, false, 100);
        let new = devices.add("new".into(), [2; 32], None, false, 100);
        devices.used(new, 5000);
        assert_eq!(devices.expire(1000), [old]);
        assert_eq!(devices.list.len(), 1);
    }
}
