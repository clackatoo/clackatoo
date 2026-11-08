//! Images pasted into terminals (CLACKA-227): [`Request::PasteImage`].
//!
//! A window sends the image on its clipboard to the daemon whose terminal
//! it is, this machine's or, through a relay, another's. That daemon keeps
//! it in a file of its own, and types its path for the program as a
//! paste, as a file dropped on the terminal is: quoted, for a shell at its
//! prompt, so it reads as one word. Claude Code and Codex take a pasted
//! path of an image as the image. Or, as `pasteimage=clipboard` asks, it
//! puts the image on its own machine's clipboard, in the desktop session
//! its programs follow (see [`crate::display`]), and types Ctrl+V, for a
//! program that reads the clipboard itself. With no desktop there, or no
//! `wl-copy` or `xclip`, it types the path instead, and says why.
//!
//! What touches the disk or runs a program is done by a thread of its own,
//! one paste at a time, so the loop never waits for it. The client is
//! answered in its turn among its answers, once that's done and what's to
//! be typed is typed ([`Daemon::take_pastes`]); a paste the thread hasn't
//! finished within [`PATIENCE`] is answered with an error, and typed never.
//!
//! # The folder
//!
//! [`PasteConfig::dir`], `~/.cache/clackatoo/pastes`, is made for the
//! user alone (0700) if it isn't there, and put back so if it's open to
//! others; it's refused if it's a link, not a folder, or another user's.
//! Each file is made new there, never through a link, for the user alone
//! (0600), named for when it was pasted, as `20261108-224501-1.png`. At
//! each paste, files of such names older than [`KEPT_FOR`] are deleted,
//! and the oldest past [`MOST_KEPT`] bytes; nothing else there is touched.
//!
//! # Web devices
//!
//! A phone may paste an image as a path, within the limits of the photos
//! it sends chats ([`ChatImage`]'s), but never onto the clipboard: that's
//! the desktop's, not the phone's.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::{self, Seek, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use clackatoo_protocol::{ChatImage, Error, ImagePaste, Response, TerminalId, TerminalStatus};
use mio::{Token, Waker};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::{Daemon, Source};
use crate::PasteConfig;
use crate::display::{Desktop, Env};

/// How long a pasted image is kept.
pub(crate) const KEPT_FOR: Duration = Duration::from_hours(72);

/// The most the pasted images kept may take together, in bytes: the
/// oldest go first.
pub(crate) const MOST_KEPT: u64 = 256 * 1024 * 1024;

/// The most files looked at in the folder at a paste: one with more is
/// pruned a part at a time.
const MOST_LOOKED: usize = 4096;

/// The most pastes under way at once: more are refused.
const MOST_UNDER_WAY: usize = 4;

/// How long a paste may take, from when it's asked, before its client is
/// told it failed.
pub(crate) const PATIENCE: Duration = Duration::from_secs(10);

/// How long `wl-copy` or `xclip` has to take the image: each goes on in
/// the background once it has it.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(3);

/// Ctrl+V, as a terminal sends it.
const CTRL_V: u8 = 0x16;

/// The pastes under way, and the thread doing them.
#[derive(Debug, Default)]
pub(super) struct Pastes {
    /// The thread's queue, once it has started.
    jobs: Option<mpsc::Sender<Job>>,
    /// What it has done, by the paste's number.
    done: Arc<Mutex<Vec<Finished>>>,
    /// Those under way, by number.
    under_way: HashMap<u64, Pasting>,
    next: u64,
}

/// A paste the thread finished, by its number: what it did, or why not.
type Finished = (u64, Result<Done, String>);

/// A paste under way.
#[derive(Debug)]
struct Pasting {
    /// The client that asked, and its answer's place.
    token: Token,
    ticket: u64,
    terminal: TerminalId,
    /// When it's given up on.
    due: Instant,
}

/// What the thread is asked to do.
struct Job {
    number: u64,
    image: ChatImage,
    how: ImagePaste,
    config: PasteConfig,
    /// The desktop's display the daemon's programs follow, if it's on
    /// the clipboard it's put.
    display: Option<Env>,
    /// What this machine is called, to say so.
    machine: String,
}

/// What the thread did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Done {
    /// The file it kept.
    path: PathBuf,
    /// Whether the image is on the clipboard, for Ctrl+V to paste.
    on_clipboard: bool,
    /// What the bar says, if anything.
    said: String,
}

impl Daemon {
    /// Starts pasting `image` into `terminal`, this daemon's, for the
    /// client at `token`, as `how` says: answered once it's done (see the
    /// [module docs](self)), or at once, with an error, if it can't be.
    pub(super) fn paste_image(
        &mut self,
        token: Token,
        terminal: TerminalId,
        image: ChatImage,
        how: ImagePaste,
    ) -> Result<(), Error> {
        let slot = (self.terminals.get(&terminal))
            .filter(|_| self.may_see_terminal(token, terminal))
            .ok_or(Error::NoSuchTerminal(terminal))?;
        if slot.terminal.is_chat() {
            return Err(Error::IsAChat(terminal));
        }
        if slot.terminal.status() != TerminalStatus::Running {
            return Err(Error::TerminalExited(terminal));
        }
        let Some(config) = self.config.pastes.clone() else {
            return Err(Error::Other(format!(
                "‘{}’ keeps no pasted images (CLACKATOO_PASTES=off)",
                self.machine.name
            )));
        };
        if !image.valid() {
            return Err(Error::Other(
                "the image is too big: at most 16 million pixels, 6 MiB as PNG".into(),
            ));
        }
        // The clipboard is the desktop's, not a phone's.
        if how == ImagePaste::Clipboard && self.device_of(token).is_some() {
            return Err(Error::Other(
                "a phone pastes an image as its path, never onto the clipboard".into(),
            ));
        }
        if self.pastes.under_way.len() >= MOST_UNDER_WAY {
            return Err(Error::Other(
                "images are being pasted already: try again in a moment".into(),
            ));
        }
        let display = match how {
            ImagePaste::Path => None,
            ImagePaste::Clipboard => Some(
                self.display_env()
                    .map_or_else(Env::own, |env| Env::clone(&env)),
            ),
        };
        // Its client has gone, if there's no place for its answer.
        let Some(ticket) = self.take_place(token) else {
            return Ok(());
        };
        let number = self.pastes.next;
        self.pastes.next += 1;
        let job = Job {
            number,
            image,
            how,
            config,
            display,
            machine: self.machine.name.clone(),
        };
        if let Err(err) = self.pastes.start(job, &self.waker) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return Ok(());
        }
        self.pastes.under_way.insert(
            number,
            Pasting {
                token,
                ticket,
                terminal,
                due: Instant::now() + PATIENCE,
            },
        );
        Ok(())
    }

    /// Types what the pastes the thread has finished are to type, and
    /// answers their clients, in their turn.
    pub(super) fn take_pastes(&mut self) {
        if self.pastes.jobs.is_none() {
            return;
        }
        let done =
            std::mem::take(&mut *(self.pastes.done.lock()).unwrap_or_else(PoisonError::into_inner));
        for (number, done) in done {
            // Given up on already.
            let Some(pasting) = self.pastes.under_way.remove(&number) else {
                continue;
            };
            let answer = match done {
                Ok(done) => self.type_paste(&pasting, done),
                Err(why) => Response::Error(Error::Other(why)),
            };
            self.hand_answer_with(pasting.token, pasting.ticket, answer);
        }
    }

    /// Types for `pasting`'s program what the thread `done`: the image's
    /// path, or Ctrl+V, if it's on the clipboard. Only for a client still
    /// waiting for it, and still allowed the terminal.
    fn type_paste(&mut self, pasting: &Pasting, done: Done) -> Response {
        let Pasting {
            token, terminal, ..
        } = *pasting;
        if !self.waits_for(token, pasting.ticket) || !self.may_see_terminal(token, terminal) {
            return Response::Error(Error::NoSuchTerminal(terminal));
        }
        let bytes = if done.on_clipboard {
            vec![CTRL_V]
        } else {
            let Some(slot) = self.terminals.get(&terminal) else {
                return Response::Error(Error::NoSuchTerminal(terminal));
            };
            let Some(path) = done.path.to_str() else {
                return Response::Error(Error::Other("the image's path isn't text".into()));
            };
            // Quoted for a shell at its prompt, with a space after, as a
            // file dropped on it is; as it is for anything else.
            let shell = slot.terminal.shell_at_prompt();
            let text = if shell {
                format!("{} ", clackatoo_term::shell_quoted(path))
            } else {
                path.to_owned()
            };
            let modes = (slot.terminal.grid()).map(clackatoo_term::Terminal::input_modes);
            clackatoo_term::paste(&text, modes.unwrap_or_default())
        };
        match self.send_input(token, terminal, &bytes) {
            Ok(()) => Response::Text(done.said),
            Err(err) => Response::Error(err),
        }
    }

    /// Whether the client at `token` still waits for its answer numbered
    /// `ticket`.
    fn waits_for(&self, token: Token, ticket: u64) -> bool {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) => (client.held.iter())
                .any(|held| matches!(held, super::Held::Waiting(waiting) if *waiting == ticket)),
            _ => false,
        }
    }

    /// When the first paste under way is to be given up on.
    pub(super) fn next_paste_due(&self) -> Option<Instant> {
        self.pastes
            .under_way
            .values()
            .map(|pasting| pasting.due)
            .min()
    }

    /// Gives up on the pastes the thread hasn't finished in time: each
    /// client is told, and what the thread does later is dropped.
    pub(super) fn time_out_pastes(&mut self) {
        let now = Instant::now();
        let late: Vec<u64> = (self.pastes.under_way.iter())
            .filter(|(_, pasting)| pasting.due <= now)
            .map(|(&number, _)| number)
            .collect();
        for number in late {
            if let Some(pasting) = self.pastes.under_way.remove(&number) {
                log::warn!(
                    "pasting an image into terminal {} took too long",
                    pasting.terminal
                );
                let why = Error::Other("pasting the image took too long".into());
                self.hand_answer_with(pasting.token, pasting.ticket, Response::Error(why));
            }
        }
    }
}

impl Pastes {
    /// Gives the thread `job`, starting it first if it hasn't.
    fn start(&mut self, job: Job, waker: &Arc<Waker>) -> Result<(), Error> {
        if self.jobs.is_none() {
            let (jobs, queue) = mpsc::channel::<Job>();
            let done = Arc::clone(&self.done);
            let waker = Arc::clone(waker);
            std::thread::Builder::new()
                .name("pastes".into())
                .spawn(move || {
                    for job in queue {
                        let number = job.number;
                        let result = std::panic::catch_unwind(|| paste(&job))
                            .unwrap_or_else(|_| Err("pasting the image failed".into()));
                        (done.lock().unwrap_or_else(PoisonError::into_inner))
                            .push((number, result));
                        if let Err(err) = waker.wake() {
                            log::warn!("waking the event loop for a paste failed: {err}");
                        }
                    }
                })
                .map_err(|err| Error::Other(format!("the image couldn't be pasted: {err}")))?;
            self.jobs = Some(jobs);
        }
        let jobs = self.jobs.as_ref().expect("started");
        jobs.send(job)
            .map_err(|_| Error::Other("the image couldn't be pasted".into()))
    }
}

/// Keeps `job`'s image in a file, and puts it on the clipboard if asked:
/// on the thread.
fn paste(job: &Job) -> Result<Done, String> {
    let png = png_of(&job.image)?;
    let dir = &job.config.dir;
    let folder = open_folder(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    prune(&folder, SystemTime::now());
    let (name, mut file) = make_file(&folder, &png)
        .map_err(|err| format!("the image couldn't be kept in {}: {err}", dir.display()))?;
    let path = dir.join(name);
    let mut done = Done {
        path,
        on_clipboard: false,
        said: String::new(),
    };
    if job.how == ImagePaste::Clipboard {
        let put = file
            .rewind()
            .map_err(|err| err.to_string())
            .and_then(|()| put_on_clipboard(job, file));
        match put {
            Ok(()) => done.on_clipboard = true,
            Err(why) => done.said = format!("{why}: pasted the image's path instead"),
        }
    }
    Ok(done)
}

/// The PNG `image` holds, checked to be one of the size it says.
pub(crate) fn png_of(image: &ChatImage) -> Result<Vec<u8>, String> {
    let png = (base64::engine::general_purpose::STANDARD.decode(&image.data))
        .map_err(|_| "the image isn't base64".to_owned())?;
    // The signature, then IHDR's length and name, then its width and
    // height.
    let header = png.get(..24).ok_or("the image isn't a PNG")?;
    let size = |at: usize| {
        u32::from_be_bytes([header[at], header[at + 1], header[at + 2], header[at + 3]])
    };
    if header[..16] != *b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR" {
        return Err("the image isn't a PNG".into());
    }
    if (size(16), size(20)) != (image.width, image.height) {
        return Err("the image isn't the size it says".into());
    }
    Ok(png)
}

/// The folder at `dir`, made for the user alone if it isn't there, and
/// refused if it's a link, not a folder, or anyone else's.
fn open_folder(dir: &Path) -> Result<OwnedFd, String> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    match rustix::fs::mkdir(dir, Mode::RWXU) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(err) => return Err(err.to_string()),
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = rustix::fs::open(dir, flags, Mode::empty()).map_err(|err| match err {
        Errno::LOOP | Errno::NOTDIR => "it's a link or not a folder: it isn't used".to_owned(),
        err => err.to_string(),
    })?;
    let stat = rustix::fs::fstat(&folder).map_err(|err| err.to_string())?;
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err("it's another user's: it isn't used".into());
    }
    if stat.st_mode & 0o077 != 0 {
        rustix::fs::fchmod(&folder, Mode::RWXU).map_err(|err| err.to_string())?;
    }
    Ok(folder)
}

/// Whether `name` is one of a pasted image's: `YYYYMMDD-HHMMSS-N.png`.
fn is_pasted(name: &[u8]) -> bool {
    let Some(stem) = name.strip_suffix(b".png") else {
        return false;
    };
    let digits = |part: &[u8], exactly: Option<usize>| {
        !part.is_empty()
            && exactly.is_none_or(|length| part.len() == length)
            && part.iter().all(u8::is_ascii_digit)
    };
    let mut parts = stem.split(|&byte| byte == b'-');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(day), Some(time), Some(number), None)
            if digits(day, Some(8)) && digits(time, Some(6)) && digits(number, None) && number.len() <= 6
    )
}

/// Deletes the pasted images in `folder` older than [`KEPT_FOR`] at
/// `now`, and the oldest past [`MOST_KEPT`] bytes, of the first
/// [`MOST_LOOKED`] files.
fn prune(folder: &OwnedFd, now: SystemTime) {
    let Ok(dir) = Dir::read_from(folder) else {
        return;
    };
    let mut kept: Vec<(SystemTime, u64, CString)> = Vec::new();
    for entry in dir.take(MOST_LOOKED) {
        let Ok(entry) = entry else {
            break;
        };
        let name = entry.file_name();
        if !is_pasted(name.to_bytes()) {
            continue;
        }
        let Ok(stat) = rustix::fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) else {
            continue;
        };
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            continue;
        }
        let modified = SystemTime::UNIX_EPOCH
            + Duration::from_secs(u64::try_from(stat.st_mtime).unwrap_or_default());
        let old = now.duration_since(modified).is_ok_and(|age| age > KEPT_FOR);
        if old {
            let _ = rustix::fs::unlinkat(folder, name, AtFlags::empty());
        } else {
            let bytes = u64::try_from(stat.st_size).unwrap_or_default();
            kept.push((modified, bytes, name.to_owned()));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, bytes, _)| bytes).sum();
    kept.sort();
    for (_, bytes, name) in kept {
        if total <= MOST_KEPT {
            break;
        }
        if rustix::fs::unlinkat(folder, &name, AtFlags::empty()).is_ok() {
            total -= bytes;
        }
    }
}

/// Writes `png` to a new file in `folder`, for the user alone, never
/// through a link: named for now, the first number free after it.
/// Returns its name and the file, open to be read again.
fn make_file(folder: &OwnedFd, png: &[u8]) -> io::Result<(String, File)> {
    let now = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
    let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    for number in 1..=1000 {
        let name = format!("{now}-{number}.png");
        let made = rustix::fs::openat(folder, name.as_str(), flags, Mode::RUSR | Mode::WUSR);
        match made {
            Ok(fd) => {
                let mut file = File::from(fd);
                if let Err(err) = file.write_all(png) {
                    let _ = rustix::fs::unlinkat(folder, name.as_str(), AtFlags::empty());
                    return Err(err);
                }
                return Ok((name, file));
            }
            Err(Errno::EXIST) => {}
            Err(err) => return Err(err.into()),
        }
    }
    Err(io::Error::other(
        "a thousand images were pasted this second",
    ))
}

/// Puts the PNG `file` holds on the clipboard of the desktop `job` says,
/// with `wl-copy` or `xclip`, given the desktop's display alone of the
/// daemon's own environment. Returns why not, if it can't be.
fn put_on_clipboard(job: &Job, file: File) -> Result<(), String> {
    let machine = &job.machine;
    let display = job.display.as_ref();
    let Some(desktop) = display.and_then(Env::desktop) else {
        return Err(format!("there's no desktop session on ‘{machine}’"));
    };
    let mut command = match desktop {
        Desktop::Wayland => Command::new(&job.config.wayland),
        Desktop::X => {
            let mut command = Command::new(&job.config.x);
            command.args(["-selection", "clipboard", "-t", "image/png"]);
            command
        }
    };
    if let Some(display) = display {
        display.apply(&mut command);
    }
    let program = command.get_program().to_owned();
    let named = Path::new(&program)
        .file_name()
        .map_or_else(|| program.to_string_lossy(), OsStr::to_string_lossy)
        .into_owned();
    // Its output goes nowhere: each forks a process that keeps the
    // clipboard, which would hold a pipe open.
    command
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().map_err(|err| match err.kind() {
        io::ErrorKind::NotFound => format!("‘{machine}’ has no {named}"),
        _ => format!("{named} couldn't start: {err}"),
    })?;
    let until = Instant::now() + CLIPBOARD_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(format!("{named} failed")),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{named} didn't finish"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(png: &[u8], width: u32, height: u32) -> ChatImage {
        ChatImage {
            data: base64::engine::general_purpose::STANDARD.encode(png),
            width,
            height,
            preview: None,
        }
    }

    /// A PNG's first 24 bytes, for an image `width` by `height`.
    fn header(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png
    }

    #[test]
    fn only_a_png_of_the_size_it_says_is_kept() {
        assert_eq!(png_of(&image(&header(3, 2), 3, 2)), Ok(header(3, 2)));
        assert!(png_of(&image(&header(3, 2), 2, 3)).is_err());
        assert!(png_of(&image(b"GIF89a and more besides that", 1, 1)).is_err());
        assert!(png_of(&image(b"\x89PNG", 1, 1)).is_err());
        let mut bad = image(&header(1, 1), 1, 1);
        bad.data.push('!');
        assert!(png_of(&bad).is_err());
    }

    #[test]
    fn only_names_of_pasted_images_are_pruned() {
        assert!(is_pasted(b"20261108-224501-1.png"));
        assert!(is_pasted(b"20261108-224501-12.png"));
        for name in [
            "notes.png",
            "20261108-224501.png",
            "20261108-224501-1.png.txt",
            "20261108-22450-1.png",
            "20261108-224501--1.png",
            "20261108-224501-x.png",
            "20261108-224501-1-2.png",
            ".20261108-224501-1.png",
        ] {
            assert!(!is_pasted(name.as_bytes()), "{name}");
        }
    }

    #[test]
    fn the_folder_is_the_users_alone_and_never_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("cache/clackatoo/pastes");
        open_folder(&folder).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700);
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        open_folder(&folder).unwrap();
        assert_eq!(mode(&folder), 0o700);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        assert!(open_folder(&link).unwrap_err().contains("link"));
    }

    #[test]
    fn each_image_is_a_new_file_for_the_user_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let folder = open_folder(dir.path()).unwrap();
        let (first, _) = make_file(&folder, b"one").unwrap();
        let (second, _) = make_file(&folder, b"two").unwrap();
        assert_ne!(first, second);
        assert!(is_pasted(first.as_bytes()), "{first}");
        let path = dir.path().join(&second);
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // A link where the next name would go is never written through.
        let now = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S").to_string();
        let target = dir.path().join("target");
        for number in 1..=3 {
            let _ =
                std::os::unix::fs::symlink(&target, dir.path().join(format!("{now}-{number}.png")));
        }
        let (third, _) = make_file(&folder, b"three").unwrap();
        assert!(!target.exists());
        assert_eq!(std::fs::read(dir.path().join(third)).unwrap(), b"three");
    }

    #[test]
    fn old_images_go_and_the_oldest_past_the_most_kept() {
        let dir = tempfile::tempdir().unwrap();
        let folder = open_folder(dir.path()).unwrap();
        let now = SystemTime::now();
        let made = |name: &str, age: Duration| {
            let path = dir.path().join(name);
            std::fs::write(&path, b"png").unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now - age)
                .unwrap();
            path
        };
        let old = made("20261101-000000-1.png", KEPT_FOR + Duration::from_mins(1));
        let new = made("20261108-000000-1.png", Duration::from_mins(1));
        let other = made("notes.png", KEPT_FOR * 2);
        prune(&folder, now);
        assert!(!old.exists());
        assert!(new.exists());
        assert!(other.exists(), "only pasted images are pruned");
        // Past the most kept, the oldest go first.
        let big = |name: &str, age: u64| {
            let path = dir.path().join(name);
            let file = File::create(&path).unwrap();
            file.set_len(MOST_KEPT / 2).unwrap();
            file.set_modified(now - Duration::from_secs(age)).unwrap();
            path
        };
        let oldest = big("20261107-000000-1.png", 300);
        let older = big("20261107-000000-2.png", 200);
        prune(&folder, now);
        assert!(!oldest.exists(), "the oldest went");
        assert!(older.exists() && new.exists());
    }
}
