//! The display: which desktop the programs clackatoo starts open their
//! windows on, following the desktop's login.
//!
//! Every program inherits its environment from whatever started it, so a
//! daemon started over ssh, before anyone logged in to the machine's
//! desktop, has no `WAYLAND_DISPLAY`, and nor would anything it starts,
//! however long after the login. A desktop that starts under systemd
//! (GNOME, KDE, and sway or Hyprland with their usual config) puts its
//! display's variables into the systemd user manager's environment as you
//! log in, and takes them out as you log out. So the daemon follows that.
//!
//! ## When it's read
//!
//! On a thread of its own, which runs `systemctl --user show-environment`
//! ([`DisplayConfig::source`]): once as the daemon starts, and again
//! whenever a Wayland socket (`wayland-*` in `$XDG_RUNTIME_DIR`) or an X
//! socket (`X*` in `/tmp/.X11-unix`) appears or goes, as inotify says, and
//! a few times in the seconds after ([`AGAIN_AFTER`]), as a desktop puts
//! the variables there a moment after it makes its socket. Nothing is read
//! on a timer otherwise. Without inotify, it's read again as each terminal
//! is made instead, which its shell picks up by its first prompt.
//!
//! ## What's taken
//!
//! Only [`VARIABLES`], each checked ([`valid`]):
//!
//! - **From systemd**, when it has a display (`WAYLAND_DISPLAY` or
//!   `DISPLAY`): its values, and none of the daemon's own, but for
//!   `XDG_SESSION_TYPE` when systemd has none.
//! - **Else the daemon's own**, as it was started with, if one of their
//!   displays' sockets is still there, so a desktop that tells systemd
//!   nothing, as sway started by hand, keeps working when the daemon was
//!   started from it. A display whose socket has gone is left out.
//! - **Else none**: the displays' variables are taken out of what the
//!   daemon starts, but `XDG_SESSION_TYPE`, which stays the daemon's own.
//!
//! `XDG_SESSION_TYPE` says what kind of session a program is in, and
//! toolkits read it to choose Wayland or X: Qt, Chromium and Electron
//! choose Wayland only when it says `wayland`. The daemon's own is
//! `tty` when ssh started it, which beside a `WAYLAND_DISPLAY` sends them
//! to X, or nowhere. GNOME puts `wayland` in systemd's environment as it
//! logs in (its shell's unit refuses to start without it), so it's taken
//! from there with the display. It isn't taken from the daemon's own
//! display, which already came with it, and it's never taken away: with
//! no display, `tty` is still true.
//!
//! ## Who gets it
//!
//! The daemon never changes its own environment: `setenv` isn't safe
//! while other threads may read it. Each program it starts that could
//! open a window (a terminal's program, an agent, a language server) is
//! given the values as they are when it starts ([`Env::apply`]). A program
//! that's already running keeps the environment it started with, as on any
//! Unix. The shells of terminals already open follow at their next
//! prompt, through their integration, from a small file the daemon writes
//! beside the integration's scripts ([`DisplayConfig::file`]), changing
//! only variables the user hasn't set in that shell themselves.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::inotify;
use rustix::io::Errno;

/// The variables that say which display programs open their windows on,
/// and what's showing it: the only ones followed. The shell integration's
/// scripts list them too, in this order.
pub const VARIABLES: [&str; 11] = [
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "XAUTHORITY",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "SWAYSOCK",
    "I3SOCK",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "NIRI_SOCKET",
];

/// Where `WAYLAND_DISPLAY` is in [`VARIABLES`].
const WAYLAND: usize = 0;

/// Where `DISPLAY` is in [`VARIABLES`].
const X: usize = 1;

/// Where `XDG_SESSION_TYPE` is in [`VARIABLES`].
const SESSION_TYPE: usize = 3;

/// The longest value taken, in bytes: far longer than any real one.
const LONGEST: usize = 1024;

/// How long after a socket appears or goes the environment is read again,
/// besides at once: the desktop puts its variables there a moment after
/// it makes its socket, and takes them out around when it removes it.
pub const AGAIN_AFTER: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// How long the command reading systemd's environment has to answer
/// before it's killed, and taken to have none.
const SOURCE_WAIT: Duration = Duration::from_secs(5);

/// The most of its answer read, in bytes.
const MOST_READ: usize = 1 << 20;

/// How the display is followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplayConfig {
    /// The command, and its arguments, that prints the systemd user
    /// manager's environment, a `NAME=VALUE` line each:
    /// `systemctl --user show-environment`.
    pub source: Vec<OsString>,
    /// Where Wayland's sockets are, watched for them: `$XDG_RUNTIME_DIR`.
    /// `None` watches none, and takes no Wayland display of the daemon's
    /// own.
    pub runtime_dir: Option<PathBuf>,
    /// Where X's sockets are, watched for them: `/tmp/.X11-unix`. `None`
    /// watches none.
    pub x11_dir: Option<PathBuf>,
    /// The daemon's own values of [`VARIABLES`], as it was started with
    /// them.
    pub inherited: Vec<(String, String)>,
    /// The file the values are written to for shells already running to
    /// follow (see the [module docs](self)): `display` beside the shell
    /// integration's scripts. `None` writes none.
    pub file: Option<PathBuf>,
    /// How long after a socket appears or goes the environment is read
    /// again ([`AGAIN_AFTER`]).
    pub again_after: Vec<Duration>,
}

impl DisplayConfig {
    /// Following the systemd user manager's environment, as the daemon
    /// does, with the daemon's own values as it was started, writing the
    /// values for shells to `file`.
    #[must_use]
    pub fn from_env(file: Option<PathBuf>) -> Self {
        let inherited = VARIABLES
            .iter()
            .filter_map(|&name| {
                let value = std::env::var(name).ok()?;
                Some((name.to_owned(), value))
            })
            .collect();
        Self {
            source: ["systemctl", "--user", "show-environment"]
                .map(OsString::from)
                .to_vec(),
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
            x11_dir: Some(PathBuf::from("/tmp/.X11-unix")),
            inherited,
            file,
            again_after: AGAIN_AFTER.to_vec(),
        }
    }
}

/// Where the values came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    /// The systemd user manager's environment: the desktop logged in.
    Systemd,
    /// The daemon's own environment, as it was started.
    Inherited,
    /// Nowhere: there's no display.
    None,
}

impl Origin {
    /// What it's called in the file and the log.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Systemd => "systemd",
            Self::Inherited => "inherited",
            Self::None => "none",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [Self::Systemd, Self::Inherited, Self::None]
            .into_iter()
            .find(|origin| origin.name() == name)
    }
}

/// A value for each of [`VARIABLES`], in its order, or none.
type Values = [Option<String>; VARIABLES.len()];

/// The values programs are started with, and where they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Env {
    origin: Origin,
    values: Values,
}

impl Env {
    /// Gives `command` these values, taking those there are none of out
    /// of what it would inherit.
    pub(crate) fn apply(&self, command: &mut Command) {
        for (name, value) in VARIABLES.iter().zip(&self.values) {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
    }

    /// The file's text: where they came from, on a line starting `#`,
    /// then a `NAME=VALUE` line for each there is. Values have no line
    /// breaks ([`valid`]).
    fn file(&self) -> String {
        let mut text = format!("#{}\n", self.origin.name());
        for (name, value) in VARIABLES.iter().zip(&self.values) {
            if let Some(value) = value {
                text.push_str(name);
                text.push('=');
                text.push_str(value);
                text.push('\n');
            }
        }
        text
    }

    /// How the log says it.
    fn said(&self) -> String {
        let set: Vec<String> = (VARIABLES.iter().zip(&self.values))
            .filter_map(|(name, value)| Some(format!("{name}={}", value.as_ref()?)))
            .collect();
        let origin = match self.origin {
            Origin::Systemd => "from systemd's user manager",
            Origin::Inherited => "the daemon's own",
            Origin::None => "none",
        };
        if set.is_empty() {
            origin.to_owned()
        } else {
            format!("{origin}: {}", set.join(" "))
        }
    }
}

/// What the file the daemon writes for shells says (see
/// [`DisplayConfig::file`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// Where the values came from.
    pub origin: Origin,
    /// Each variable there's a value of, and its value, in
    /// [`VARIABLES`]' order. The others are unset.
    pub values: Vec<(String, String)>,
}

/// Reads the file the daemon writes at `path` (see
/// [`DisplayConfig::file`]). `None` if there's no such file, or it isn't
/// one the daemon wrote.
///
/// # Errors
///
/// If it's there but can't be read.
pub fn read_file(path: &Path) -> io::Result<Option<Written>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut lines = text.lines();
    let Some(origin) = (lines.next())
        .and_then(|line| line.strip_prefix('#'))
        .and_then(Origin::named)
    else {
        return Ok(None);
    };
    let values = lines
        .filter_map(|line| line.split_once('='))
        .filter(|(name, value)| VARIABLES.contains(name) && valid(value))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
    Ok(Some(Written { origin, values }))
}

/// Whether `value` is one to hand on: some text, no longer than
/// [`LONGEST`], with no control characters, line breaks included, so
/// each is one line of the file.
fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= LONGEST && !value.chars().any(char::is_control)
}

/// Where `name` is in [`VARIABLES`], if it's there.
fn index(name: &[u8]) -> Option<usize> {
    VARIABLES.iter().position(|known| known.as_bytes() == name)
}

/// The values of [`VARIABLES`] in `pairs`, those that are [`valid`].
fn values_of<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Values {
    let mut values = Values::default();
    for (name, value) in pairs {
        if let Some(at) = index(name.as_bytes())
            && valid(value)
        {
            values[at] = Some(value.to_owned());
        }
    }
    values
}

/// The values of [`VARIABLES`] in what `systemctl show-environment`
/// printed: a `NAME=VALUE` line each, a value with characters a shell
/// would take specially quoted as `$'…'`, or by older versions `"…"`.
/// Lines of other variables, and values that can't be read, are passed
/// over.
fn parse_systemd(text: &[u8]) -> Values {
    let mut values = Values::default();
    for line in text.split(|&byte| byte == b'\n') {
        let Some(eq) = line.iter().position(|&byte| byte == b'=') else {
            continue;
        };
        let Some(at) = index(&line[..eq]) else {
            continue;
        };
        let value = unquote(&line[eq + 1..]).and_then(|value| String::from_utf8(value).ok());
        values[at] = value.filter(|value| valid(value));
    }
    values
}

/// `raw` as systemd quoted it: as it is, unless it's in `$'…'`, with C's
/// escapes, or `"…"`, with `\\`, `\"`, `` \` `` and `\$`. `None` if it
/// can't be read.
fn unquote(raw: &[u8]) -> Option<Vec<u8>> {
    if let Some(inner) = raw.strip_prefix(b"$'") {
        let inner = inner.strip_suffix(b"'")?;
        let mut value = Vec::with_capacity(inner.len());
        let mut bytes = inner.iter().copied().peekable();
        while let Some(byte) = bytes.next() {
            if byte == b'\'' {
                return None;
            }
            if byte != b'\\' {
                value.push(byte);
                continue;
            }
            let escaped = bytes.next()?;
            value.push(match escaped {
                b'\\' | b'\'' | b'"' | b'?' => escaped,
                b'a' => 0x07,
                b'b' => 0x08,
                b'e' => 0x1b,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'v' => 0x0b,
                b'x' => {
                    let mut code = 0u8;
                    for _ in 0..2 {
                        let digit = bytes.peek().and_then(|&byte| (byte as char).to_digit(16));
                        let Some(digit) = digit else { break };
                        bytes.next();
                        code = code * 16 + u8::try_from(digit).ok()?;
                    }
                    code
                }
                b'0'..=b'7' => {
                    let mut code = u32::from(escaped - b'0');
                    for _ in 0..2 {
                        let digit = bytes.peek().and_then(|&byte| (byte as char).to_digit(8));
                        let Some(digit) = digit else { break };
                        bytes.next();
                        code = code * 8 + digit;
                    }
                    u8::try_from(code).ok()?
                }
                _ => return None,
            });
        }
        return Some(value);
    }
    if let Some(inner) = raw.strip_prefix(b"\"") {
        let inner = inner.strip_suffix(b"\"")?;
        let mut value = Vec::with_capacity(inner.len());
        let mut bytes = inner.iter().copied();
        while let Some(byte) = bytes.next() {
            match byte {
                b'"' => return None,
                b'\\' => match bytes.next()? {
                    escaped @ (b'\\' | b'"' | b'`' | b'$') => value.push(escaped),
                    _ => return None,
                },
                _ => value.push(byte),
            }
        }
        return Some(value);
    }
    Some(raw.to_owned())
}

/// Where displays' sockets are, to tell whether a display is still there.
#[derive(Debug)]
struct Sockets<'a> {
    runtime_dir: Option<&'a Path>,
    x11_dir: Option<&'a Path>,
}

impl Sockets<'_> {
    /// Whether the socket of the Wayland display `name` is there: a path,
    /// or a name in the runtime directory.
    fn wayland(&self, name: &str) -> bool {
        let path = Path::new(name);
        if path.is_absolute() {
            return path.exists();
        }
        self.runtime_dir.is_some_and(|dir| dir.join(path).exists())
    }

    /// Whether the socket of the X display `display` is there: `:N`, or
    /// `:N.S`, or the same after `unix`, is `XN` in the X sockets'
    /// directory. One on another host, as `localhost:10.0` (ssh's X
    /// forwarding), can't be checked, and is taken to be there.
    fn x(&self, display: &str) -> bool {
        let local = display.strip_prefix("unix").unwrap_or(display);
        let Some(number) = local.strip_prefix(':') else {
            return true;
        };
        let number = number.split('.').next().unwrap_or_default();
        if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        (self.x11_dir).is_some_and(|dir| dir.join(format!("X{number}")).exists())
    }
}

/// Whether `values` have a display.
fn has_display(values: &Values) -> bool {
    values[WAYLAND].is_some() || values[X].is_some()
}

/// The values programs are started with: `systemd`'s, if it was read and
/// has a display; else `inherited`, the daemon's own, if one of their
/// displays' sockets is still there; else none (see the [module
/// docs](self)).
fn effective(systemd: Option<&Values>, inherited: &Values, sockets: &Sockets<'_>) -> Env {
    if let Some(systemd) = systemd.filter(|values| has_display(values)) {
        let mut values = systemd.clone();
        if values[SESSION_TYPE].is_none() {
            values[SESSION_TYPE].clone_from(&inherited[SESSION_TYPE]);
        }
        return Env {
            origin: Origin::Systemd,
            values,
        };
    }
    let mut values = inherited.clone();
    if values[WAYLAND]
        .as_deref()
        .is_some_and(|name| !sockets.wayland(name))
    {
        values[WAYLAND] = None;
    }
    if values[X]
        .as_deref()
        .is_some_and(|display| !sockets.x(display))
    {
        values[X] = None;
    }
    if has_display(&values) {
        return Env {
            origin: Origin::Inherited,
            values,
        };
    }
    let mut none = Values::default();
    none[SESSION_TYPE].clone_from(&inherited[SESSION_TYPE]);
    Env {
        origin: Origin::None,
        values: none,
    }
}

/// The display followed, by a thread of its own (see the [module
/// docs](self)). The thread ends once this is dropped.
#[derive(Debug)]
pub(crate) struct Display {
    current: Arc<Mutex<Arc<Env>>>,
    /// Whether socket changes are watched. If not, the environment is
    /// read again as each terminal is made.
    watching: Arc<AtomicBool>,
    /// Written to, to have the thread read the environment again; closed,
    /// to have it end. `None` if there's no thread.
    wake: Option<OwnedFd>,
}

impl Display {
    /// Follows the display as `config` says, with the daemon's own values
    /// to begin with, until systemd's have been read.
    pub(crate) fn start(config: DisplayConfig) -> Self {
        let inherited = values_of(
            (config.inherited.iter()).map(|(name, value)| (name.as_str(), value.as_str())),
        );
        let sockets = Sockets {
            runtime_dir: config.runtime_dir.as_deref(),
            x11_dir: config.x11_dir.as_deref(),
        };
        let first = effective(None, &inherited, &sockets);
        let current = Arc::new(Mutex::new(Arc::new(first)));
        let watching = Arc::new(AtomicBool::new(false));
        let pipe = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC);
        let (read, write) = match pipe {
            Ok(pipe) => pipe,
            Err(err) => {
                log::warn!("the display won't be followed: {err}");
                // The daemon's own values stay.
                return Self {
                    current,
                    watching,
                    wake: None,
                };
            }
        };
        // Never held up writing: a read is coming anyway if it's full.
        if let Err(err) = rustix::fs::fcntl_setfl(&write, rustix::fs::OFlags::NONBLOCK) {
            log::debug!("the display's pipe blocks: {err}");
        }
        let follower = Follower {
            config,
            inherited,
            current: Arc::clone(&current),
            watching: Arc::clone(&watching),
            wake: read,
            inotify: None,
            watches: HashMap::new(),
            written: false,
            systemd_failed: false,
        };
        let started = std::thread::Builder::new()
            .name("display".to_owned())
            .spawn(move || follower.run());
        if let Err(err) = started {
            log::warn!("the display won't be followed: {err}");
        }
        Self {
            current,
            watching,
            wake: Some(write),
        }
    }

    /// The values programs are started with now.
    pub(crate) fn current(&self) -> Arc<Env> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Has the environment read again if socket changes aren't watched, as
    /// a terminal is made: it's read by the time its shell first prompts.
    pub(crate) fn terminal_made(&self) {
        if let Some(wake) = &self.wake
            && !self.watching.load(Ordering::Relaxed)
        {
            // Full means a read is coming anyway.
            let _ = rustix::io::write(wake, &[0]);
        }
    }
}

/// What a watch is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Watched {
    /// The runtime directory, for Wayland's sockets.
    Wayland,
    /// X's sockets' directory.
    X,
}

/// The thread following the display.
struct Follower {
    config: DisplayConfig,
    inherited: Values,
    current: Arc<Mutex<Arc<Env>>>,
    watching: Arc<AtomicBool>,
    wake: OwnedFd,
    inotify: Option<OwnedFd>,
    watches: HashMap<i32, Watched>,
    /// Whether the file has been written since the daemon started.
    written: bool,
    /// Whether reading systemd's environment failed last time, so it's
    /// said again only once it has worked.
    systemd_failed: bool,
}

impl Follower {
    fn run(mut self) {
        self.watch();
        self.read();
        // When to read again, soonest last.
        let mut again: Vec<Instant> = Vec::new();
        loop {
            let timeout = again.last().map(|&when| {
                let left = when.saturating_duration_since(Instant::now());
                Timespec {
                    tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
                    tv_nsec: i64::from(left.subsec_nanos()),
                }
            });
            let mut fds = vec![PollFd::new(&self.wake, PollFlags::IN)];
            if let Some(inotify) = &self.inotify {
                fds.push(PollFd::new(inotify, PollFlags::IN));
            }
            match rustix::event::poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(err) => {
                    log::warn!("the display is no longer followed: {err}");
                    return;
                }
            }
            let woken = fds[0].revents();
            let changed = fds.get(1).is_some_and(|fd| !fd.revents().is_empty());
            if woken.intersects(PollFlags::HUP | PollFlags::ERR) {
                return;
            }
            let mut read = false;
            if woken.contains(PollFlags::IN) {
                let mut bytes = [0; 64];
                let _ = rustix::io::read(&self.wake, &mut bytes);
                read = true;
            }
            if changed && self.socket_changed() {
                let now = Instant::now();
                again = (self.config.again_after.iter())
                    .map(|&after| now + after)
                    .collect();
                again.sort_unstable_by(|one, other| other.cmp(one));
                read = true;
            }
            let now = Instant::now();
            while again.last().is_some_and(|&when| when <= now) {
                again.pop();
                read = true;
            }
            if read {
                self.read();
            }
        }
    }

    /// Watches the runtime directory for Wayland's sockets, and X's
    /// sockets' directory. If that can't be done, says so: then the
    /// environment is read again as terminals are made.
    fn watch(&mut self) {
        let flags = inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK;
        let inotify = match inotify::init(flags) {
            Ok(inotify) => inotify,
            Err(err) => {
                log::info!(
                    "display: sockets can't be watched ({err}), so the display is read again \
                     as each terminal is made"
                );
                return;
            }
        };
        let flags = inotify::WatchFlags::CREATE
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::ONLYDIR;
        let dirs = [
            (&self.config.runtime_dir, Watched::Wayland),
            (&self.config.x11_dir, Watched::X),
        ];
        for (dir, watched) in dirs {
            let Some(dir) = dir else { continue };
            match inotify::add_watch(&inotify, dir, flags) {
                Ok(watch) => {
                    self.watches.insert(watch, watched);
                }
                // No X sockets yet: a Wayland desktop makes its own.
                Err(Errno::NOENT) if watched == Watched::X => {
                    log::debug!("display: {} isn't there to watch", dir.display());
                }
                Err(err) => log::info!(
                    "display: {} can't be watched ({err}), so the display is read again as \
                     each terminal is made",
                    dir.display()
                ),
            }
        }
        let wayland = self
            .watches
            .values()
            .any(|&watched| watched == Watched::Wayland);
        self.watching.store(wayland, Ordering::Relaxed);
        self.inotify = Some(inotify);
    }

    /// Reads what inotify says, and whether a display's socket appeared or
    /// went.
    fn socket_changed(&mut self) -> bool {
        let Some(inotify) = &self.inotify else {
            return false;
        };
        let mut changed = false;
        let mut gone = Vec::new();
        let mut buffer = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(inotify, &mut buffer);
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::INTR) => continue,
                Err(Errno::WOULDBLOCK) => break,
                Err(err) => {
                    log::warn!("display: reading what changed failed: {err}");
                    break;
                }
            };
            if event.events().contains(inotify::ReadFlags::QUEUE_OVERFLOW) {
                changed = true;
                continue;
            }
            if event.events().contains(inotify::ReadFlags::IGNORED) {
                gone.push(event.wd());
                continue;
            }
            let Some(&watched) = self.watches.get(&event.wd()) else {
                continue;
            };
            let name = event.file_name().map(std::ffi::CStr::to_bytes);
            changed |= name.is_some_and(|name| socket(watched, name));
        }
        for watch in gone {
            if self.watches.remove(&watch) == Some(Watched::Wayland) {
                log::info!(
                    "display: the runtime directory is no longer watched, so the display is read \
                     again as each terminal is made"
                );
                self.watching.store(false, Ordering::Relaxed);
            }
        }
        changed
    }

    /// Reads systemd's environment, and if what programs are given has
    /// changed, or it's the first time, says so, and writes the file.
    fn read(&mut self) {
        let systemd = match read_source(&self.config.source) {
            Ok(text) => {
                if std::mem::take(&mut self.systemd_failed) {
                    log::info!("display: systemd's user manager can be read again");
                }
                Some(parse_systemd(&text))
            }
            Err(err) => {
                if !std::mem::replace(&mut self.systemd_failed, true) {
                    log::info!(
                        "display: systemd's user manager can't be read ({}: {err}), so only \
                         the daemon's own display is followed",
                        self.config.source.join(std::ffi::OsStr::new(" ")).display()
                    );
                }
                None
            }
        };
        let sockets = Sockets {
            runtime_dir: self.config.runtime_dir.as_deref(),
            x11_dir: self.config.x11_dir.as_deref(),
        };
        let env = effective(systemd.as_ref(), &self.inherited, &sockets);
        let changed = {
            let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = **current != env;
            if changed {
                *current = Arc::new(env.clone());
            }
            changed
        };
        if changed || !self.written {
            log::info!("display: {}", env.said());
            if let Some(file) = &self.config.file
                && let Err(err) = write_file(file, &env.file())
            {
                log::warn!("display: {} couldn't be written: {err}", file.display());
            }
            self.written = true;
        }
    }
}

/// Whether `name`, a file in a directory `watched`, is a display's
/// socket: `wayland-N` in the runtime directory, not its `.lock`; `XN` in
/// X's.
fn socket(watched: Watched, name: &[u8]) -> bool {
    match watched {
        Watched::Wayland => name.starts_with(b"wayland-") && !name.ends_with(b".lock"),
        Watched::X => name.first() == Some(&b'X'),
    }
}

/// Runs `command` and returns what it printed, if it exits with 0 within
/// [`SOURCE_WAIT`], printing at most [`MOST_READ`] bytes.
fn read_source(command: &[OsString]) -> io::Result<Vec<u8>> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no command"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut text = Vec::new();
    let read = read_until(&mut stdout, &mut text, Instant::now() + SOURCE_WAIT);
    if read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    read?;
    if !status.success() {
        return Err(io::Error::other(format!("it {status}")));
    }
    Ok(text)
}

/// Reads `pipe` to its end into `text`, giving up at `deadline`, or past
/// [`MOST_READ`] bytes.
fn read_until(
    pipe: &mut std::process::ChildStdout,
    text: &mut Vec<u8>,
    deadline: Instant,
) -> io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "it didn't answer"));
        }
        let timeout = Timespec {
            tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&*pipe, PollFlags::IN)];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => return Err(err.into()),
        }
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                text.extend_from_slice(&buffer[..read]);
                if text.len() > MOST_READ {
                    return Err(io::Error::other("it said too much"));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// Writes `text` to `path`, whole or not at all, as shells may be reading
/// it, for the user alone, never through a link.
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let partial = dir.join(format!(".{name}.{}", std::process::id()));
    let open = || {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
            .open(&partial)
    };
    // One left by a daemon that crashed is no one's now.
    let mut file = match open() {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(&partial)?;
            open()?
        }
        opened => opened?,
    };
    let written = file.write_all(text.as_bytes());
    drop(file);
    if let Err(err) = written.and_then(|()| std::fs::rename(&partial, path)) {
        let _ = std::fs::remove_file(&partial);
        return Err(err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> Values {
        values_of(pairs.iter().copied())
    }

    #[test]
    fn systemds_environment_is_read_for_the_variables_followed_alone() {
        let text = b"HOME=/home/jack\nWAYLAND_DISPLAY=wayland-0\nDISPLAY=:0\n\
            XDG_CURRENT_DESKTOP=ubuntu:GNOME\nXAUTHORITY=$'/run/user/1000/.mutter-Xwaylandauth.\\x41B'\n\
            DESKTOP_SESSION=\"a \\\"b\\\"\"\nXDG_SESSION_TYPE=$'bad\\nline'\nSWAYSOCK=$'no end\nI3SOCK\n";
        let read = parse_systemd(text);
        assert_eq!(read[WAYLAND].as_deref(), Some("wayland-0"));
        assert_eq!(read[X].as_deref(), Some(":0"));
        assert_eq!(read[4].as_deref(), Some("ubuntu:GNOME"));
        assert_eq!(
            read[2].as_deref(),
            Some("/run/user/1000/.mutter-Xwaylandauth.AB")
        );
        assert_eq!(read[6].as_deref(), Some("a \"b\""));
        // A line break, quoting that doesn't end, and no value at all are
        // passed over.
        assert_eq!(read[SESSION_TYPE], None);
        assert_eq!(read[7], None);
        assert_eq!(read[8], None);
    }

    #[test]
    fn systemds_quoting_is_undone_or_the_value_passed_over() {
        assert_eq!(unquote(b"plain").as_deref(), Some(&b"plain"[..]));
        assert_eq!(unquote(b"$'a b'").as_deref(), Some(&b"a b"[..]));
        assert_eq!(unquote(b"$'it\\'s'").as_deref(), Some(&b"it's"[..]));
        assert_eq!(unquote(b"$'\\101\\x42\\\\'").as_deref(), Some(&b"AB\\"[..]));
        assert_eq!(unquote(b"$'\\q'"), None);
        assert_eq!(unquote(b"$'a'b'"), None);
        assert_eq!(unquote(b"\"a \\$b\"").as_deref(), Some(&b"a $b"[..]));
        assert_eq!(unquote(b"\"a\\n\""), None);
        assert_eq!(unquote(b"\"open"), None);
    }

    #[test]
    fn values_are_one_line_of_text_of_a_sane_length() {
        assert!(valid("wayland-0"));
        assert!(valid("ubuntu:GNOME"));
        assert!(!valid(""));
        assert!(!valid("a\nb"));
        assert!(!valid("a\0b"));
        assert!(!valid("a\x1b[31m"));
        assert!(valid(&"x".repeat(LONGEST)));
        assert!(!valid(&"x".repeat(LONGEST + 1)));
        // The daemon's own aren't taken unchecked either.
        let own = values(&[
            ("DISPLAY", "a\nb"),
            ("WAYLAND_DISPLAY", "w"),
            ("PATH", "/bin"),
        ]);
        assert_eq!(own[X], None);
        assert_eq!(own[WAYLAND].as_deref(), Some("w"));
    }

    #[test]
    fn systemds_display_wins_whole_and_the_daemons_own_is_kept_only_while_its_socket_is() {
        let dir = tempfile::tempdir().unwrap();
        let (runtime, x11) = (dir.path().join("run"), dir.path().join("x11"));
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(&x11).unwrap();
        let sockets = Sockets {
            runtime_dir: Some(&runtime),
            x11_dir: Some(&x11),
        };
        // Started over ssh, with sway's display of its own.
        let own = values(&[
            ("XDG_SESSION_TYPE", "tty"),
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("SWAYSOCK", "/run/sway.sock"),
            ("DISPLAY", ":3.0"),
        ]);
        let desktop = values(&[
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("XDG_CURRENT_DESKTOP", "GNOME"),
        ]);

        // No sockets, and systemd has no display: none, but the session
        // type, which is still true.
        let none = effective(
            Some(&values(&[("XDG_CURRENT_DESKTOP", "GNOME")])),
            &own,
            &sockets,
        );
        assert_eq!(none.origin, Origin::None);
        assert_eq!(none.file(), "#none\nXDG_SESSION_TYPE=tty\n");
        assert_eq!(effective(None, &own, &sockets), none);

        // Its X display's socket is there: the daemon's own, without the
        // Wayland display whose socket isn't.
        std::fs::write(x11.join("X3"), "").unwrap();
        let inherited = effective(None, &own, &sockets);
        assert_eq!(inherited.origin, Origin::Inherited);
        assert_eq!(
            inherited.file(),
            "#inherited\nDISPLAY=:3.0\nXDG_SESSION_TYPE=tty\nSWAYSOCK=/run/sway.sock\n"
        );
        std::fs::write(runtime.join("wayland-1"), "").unwrap();
        assert_eq!(
            effective(None, &own, &sockets).values[WAYLAND].as_deref(),
            Some("wayland-1")
        );

        // The desktop logged in: systemd's, whole, with the daemon's own
        // session type only as systemd has none.
        let logged_in = effective(Some(&desktop), &own, &sockets);
        assert_eq!(logged_in.origin, Origin::Systemd);
        assert_eq!(
            logged_in.file(),
            "#systemd\nWAYLAND_DISPLAY=wayland-0\nXDG_SESSION_TYPE=tty\nXDG_CURRENT_DESKTOP=GNOME\n"
        );
        let mut gnome = desktop.clone();
        gnome[SESSION_TYPE] = Some("wayland".to_owned());
        assert_eq!(
            effective(Some(&gnome), &own, &sockets).values[SESSION_TYPE].as_deref(),
            Some("wayland")
        );
    }

    #[test]
    fn a_displays_socket_is_where_its_name_says() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wayland-0"), "").unwrap();
        std::fs::write(dir.path().join("X0"), "").unwrap();
        let sockets = Sockets {
            runtime_dir: Some(dir.path()),
            x11_dir: Some(dir.path()),
        };
        assert!(sockets.wayland("wayland-0"));
        assert!(!sockets.wayland("wayland-1"));
        let absolute = dir.path().join("wayland-0");
        assert!(sockets.wayland(absolute.to_str().unwrap()));
        assert!(sockets.x(":0"));
        assert!(sockets.x(":0.1"));
        assert!(sockets.x("unix:0"));
        assert!(!sockets.x(":1"));
        assert!(!sockets.x(":"));
        assert!(!sockets.x(":x"));
        // Another host's can't be checked.
        assert!(sockets.x("localhost:10.0"));
        let nowhere = Sockets {
            runtime_dir: None,
            x11_dir: None,
        };
        assert!(!nowhere.wayland("wayland-0"));
        assert!(!nowhere.x(":0"));
    }

    #[test]
    fn only_displays_sockets_are_watched_for() {
        assert!(socket(Watched::Wayland, b"wayland-0"));
        assert!(!socket(Watched::Wayland, b"wayland-0.lock"));
        assert!(!socket(Watched::Wayland, b"bus"));
        assert!(socket(Watched::X, b"X0"));
        assert!(!socket(Watched::X, b".X0-lock"));
    }

    #[test]
    fn the_file_says_where_the_values_came_from_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell/display");
        let env = Env {
            origin: Origin::Systemd,
            values: values(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
        };
        write_file(&path, &env.file()).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&path.metadata().unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let read = read_file(&path).unwrap().unwrap();
        assert_eq!(read.origin, Origin::Systemd);
        assert_eq!(
            read.values,
            [
                ("WAYLAND_DISPLAY".to_owned(), "wayland-0".to_owned()),
                ("DISPLAY".to_owned(), ":0".to_owned())
            ]
        );
        // Written again, it's replaced whole, leaving nothing beside it.
        write_file(
            &path,
            &Env {
                origin: Origin::None,
                values: Values::default(),
            }
            .file(),
        )
        .unwrap();
        let none = Written {
            origin: Origin::None,
            values: Vec::new(),
        };
        assert_eq!(read_file(&path).unwrap(), Some(none));
        assert_eq!(
            std::fs::read_dir(dir.path().join("shell")).unwrap().count(),
            1
        );
        assert_eq!(read_file(&dir.path().join("nothing")).unwrap(), None);
        std::fs::write(&path, "WAYLAND_DISPLAY=x\n").unwrap();
        assert_eq!(read_file(&path).unwrap(), None, "not one the daemon wrote");
    }

    #[test]
    fn a_command_is_given_the_values_and_none_of_the_rest() {
        let env = Env {
            origin: Origin::Systemd,
            values: values(&[("WAYLAND_DISPLAY", "wayland-0")]),
        };
        let mut command = Command::new("true");
        command.env("DISPLAY", ":9");
        env.apply(&mut command);
        let given: HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            given.get(std::ffi::OsStr::new("WAYLAND_DISPLAY")),
            Some(&Some(std::ffi::OsStr::new("wayland-0")))
        );
        for name in &VARIABLES[1..] {
            assert_eq!(given.get(std::ffi::OsStr::new(name)), Some(&None), "{name}");
        }
    }

    #[test]
    fn a_source_that_fails_gives_nothing() {
        let said = read_source(&["sh".into(), "-c".into(), "printf 'A=1\\n'".into()]).unwrap();
        assert_eq!(said, b"A=1\n");
        assert!(read_source(&["sh".into(), "-c".into(), "exit 1".into()]).is_err());
        assert!(read_source(&["/nonexistent/systemctl".into()]).is_err());
        assert!(read_source(&[]).is_err());
    }
}
