//! Keeping the daemon, and the window, quick on a machine kept busy
//! (CLACKA-231): their own threads get more of the CPU than the rest of
//! the user's programs, and nothing they start does.
//!
//! Two things are asked for, over D-Bus (see [`crate::dbus`]), neither
//! needing privileges:
//!
//! - **A higher priority for the loop's thread.** Linux shares the CPU
//!   among the threads of a cgroup (below) by their nice: from 19, least,
//!   to -20, most. A user can only lower their own threads', so the daemon
//!   asks `RealtimeKit` (rtkit), the system service desktops run so audio
//!   keeps up, which raises a thread to the most it allows
//!   (`MinNiceLevel`, -15 as it comes). The window's thread asks too.
//!   Never real-time scheduling, though rtkit offers it: it's granted only
//!   to a process that sets `RLIMIT_RTTIME`, a limit on the CPU a
//!   real-time thread may take without waiting, past which the kernel
//!   kills it, and the loop can be busy for longer than rtkit allows
//!   (200 ms), taking in a huge paste or a flood: the daemon would die
//!   with every terminal in it. A real-time thread that's busy also holds
//!   off everything else on its CPU, which rtkit's watchdog answers by
//!   taking real-time scheduling from every thread on the machine.
//! - **A place of its own.** A cgroup, or control group, is a group of
//!   processes the kernel shares the CPU, memory and disk among by weight,
//!   group against group, before nice shares a group's out among its
//!   threads. systemd runs each user's programs in cgroups of its own, and
//!   the user's systemd (the user manager) makes new ones on request. The
//!   daemon asks it for a scope, a cgroup of programs it didn't start
//!   itself (`StartTransientUnit`), which the daemon may divide (it's
//!   delegated), and divides it into three leaves: `daemon`, itself alone,
//!   weighted ten times the usual and its memory protected from being
//!   swapped out first (`memory.low`); `terminals`, at the usual weight,
//!   for the user's shells and what the daemon runs for the user; and
//!   `agents`, at a fifth of it, for agents' adapters, language servers,
//!   the browser, installs, and the terminals and background tabs agents
//!   start. A build typed in a terminal can then take all the CPU the
//!   daemon doesn't need, but never what it does. Leaves, as the kernel
//!   lets only a cgroup with no processes of its own divide itself.
//!
//! Linux passes both on to children, which mustn't have either (jack's
//! rule: only the daemon itself). So the loop's thread is first set to
//! `SCHED_RESET_ON_FORK`, which starts every process and thread it starts
//! at the usual scheduling and nice 0; and every program the daemon
//! starts, from any thread, goes into its leaf: moved by the daemon as
//! soon as it has started, or, for those with a hook of their own between
//! fork and exec anyway (a terminal's, the browser's), moving itself
//! there, and putting a raised nice back, before that hook (see `spawn`
//! and `ready_first`). Those started before the daemon has leaves are
//! noted, and moved once it has.
//!
//! The scope ends with the user manager, which stops when the user's last
//! login does, unless they linger (`loginctl enable-linger`). A daemon
//! where a login put it, by contrast, outlives that login, as terminals
//! should. So the daemon takes a scope only while the user lingers, or if
//! it's under the user manager already.
//!
//! An upgrade execs the next build in place, which keeps the thread's
//! nice and the process's cgroup: the next build finds itself in its
//! scope's `daemon` leaf, or in the scope itself, if it came while the
//! scope was being divided, and finishes dividing it. Without
//! rtkit, a user manager, lingering, or cgroups as systemd delegates them,
//! as in tests (whose daemons never call [`enable`]), under
//! `CLACKATOO_DISPLAY=off` or `CLACKATOO_PRIORITY=off`, or with
//! `priority = false` in `daemon/machine.toml`, the daemon runs as it
//! always has, and says why in one line of its log.

#![allow(
    unsafe_code,
    reason = "a child's hook between fork and exec, and setting a scheduling flag, have no safe API"
)]

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::fd::{AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustix::process::Pid;

use crate::dbus::{self, Body, Bus, Call, Message};

/// Which leaf of the daemon's scope a program it starts goes in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Leaf {
    /// The user's: their shells, and what the daemon runs for them, such
    /// as git and ssh. At the usual weight.
    Terminals,
    /// Agents', and what serves them: adapters, language servers, the
    /// browser, installs, and terminals and background tabs agents start.
    /// At a fifth of the usual weight.
    Agents,
}

impl Leaf {
    /// Its cgroup's name, in the scope.
    fn name(self) -> &'static str {
        match self {
            Self::Terminals => "terminals",
            Self::Agents => "agents",
        }
    }
}

/// Where cgroup v2 is mounted, as systemd mounts it.
const CGROUPS: &str = "/sys/fs/cgroup";

/// The daemon's own leaf.
const DAEMON: &str = "daemon";

/// The start of the name of the daemon's scope, which ends with its pid:
/// `app-clackatoo-1234.scope`, as systemd names apps' scopes.
const SCOPE_PREFIX: &str = "app-clackatoo-";

/// The daemon's leaf's weight for the CPU and the disk: ten times the
/// usual (100), so it has its share at once whatever else runs.
const DAEMON_WEIGHT: &str = "1000";

/// The agents' leaf's weight, a fifth of the usual.
const AGENTS_WEIGHT: &str = "20";

/// How much of the daemon's memory is kept from being reclaimed or
/// swapped out while there's other memory to take: about what it uses
/// with a few dozen terminals and chats.
const MEMORY_LOW: u64 = 256 << 20;

/// How long each D-Bus call may take. rtkit asks polkit, which can be
/// slow on a busy machine; nothing waits for this but the setting up.
const WAIT: Duration = Duration::from_secs(10);

/// Whether this program is the daemon, which takes its priority and
/// place as it serves: see [`enable`].
static WANTED: AtomicBool = AtomicBool::new(false);

/// Whether [`begin_for`] has run, which it does once.
static BEGUN: AtomicBool = AtomicBool::new(false);

/// Whether the programs the daemon starts are noted, from [`early`] on.
static NOTING: AtomicBool = AtomicBool::new(false);

/// Whether a thread of this process was raised, so the programs it
/// starts are readied to put their nice back.
static RAISED: AtomicBool = AtomicBool::new(false);

/// Where the programs the daemon starts go.
static PLACE: Mutex<Place> = Mutex::new(Place::Nowhere);

/// What setting the daemon up came to, or the thread finding out.
static SETTLING: Mutex<Option<Settling>> = Mutex::new(None);

/// What setting the daemon up came to, or the thread finding out.
enum Settling {
    Going(JoinHandle<Settled>),
    Done(Settled),
}

/// Where the programs the daemon starts go.
#[derive(Debug)]
enum Place {
    /// Where the daemon is, as ever: it has no leaves.
    Nowhere,
    /// Not known yet, as the daemon is given its place: those started
    /// meanwhile, to be moved into their leaves once there are some.
    Pending(Vec<Early>),
    /// Into their leaves.
    Leaves(Leaves),
}

/// A program started before the daemon had leaves.
#[derive(Clone, Copy, Debug)]
struct Early {
    pid: Pid,
    leaf: Leaf,
    /// When it started, as `/proc/PID/stat` says, so another program
    /// given its pid once it has ended is never moved in its place.
    since: Option<u64>,
}

/// The leaves the daemon's children go in.
#[derive(Debug)]
struct Leaves {
    /// The scope's unit, for systemd.
    unit: String,
    /// Each leaf's `cgroup.procs`, open for writing, for the life of the
    /// process: a child writes `0` to its own to move itself in.
    terminals: OwnedFd,
    agents: OwnedFd,
}

impl Leaves {
    fn procs(&self, leaf: Leaf) -> &OwnedFd {
        match leaf {
            Leaf::Terminals => &self.terminals,
            Leaf::Agents => &self.agents,
        }
    }
}

/// What setting the daemon up came to.
#[doc(hidden)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// The loop thread's nice, if it was raised.
    pub nice: Option<i32>,
    /// The daemon's scope, if it has one, with its leaves.
    pub scope: Option<String>,
}

/// Has the daemon take its priority and place as it serves (see
/// [`crate::upgrade::serve`]): called by `clackatoo daemon`, never by
/// tests, whose daemons stay as they are.
pub fn enable() {
    WANTED.store(true, Ordering::SeqCst);
}

/// Before the daemon has its runtime directory's lock, and starts
/// anything, if [`enable`] asked: the programs it starts from now on are
/// noted, to be moved into its leaves once it has some (see [`begin`]).
/// Writes nothing and asks nothing, so a daemon that finds another
/// running changes nothing.
pub(crate) fn early() {
    if WANTED.load(Ordering::SeqCst) {
        note_early();
    }
}

/// Notes the programs the daemon starts from now on, installs included,
/// until it has its place, or is to have none. Once only.
fn note_early() {
    if NOTING.swap(true, Ordering::SeqCst) {
        return;
    }
    clackatoo_syntax::note_programs_with(|pid| started(pid, Leaf::Agents));
    *lock(&PLACE) = Place::Pending(Vec::new());
}

/// Once the daemon has its runtime directory's lock, if [`enable`]
/// asked: takes its priority and place, for the calling thread, the one
/// that runs its loop, saying how it went in `runtime`, the daemon's
/// runtime directory.
pub(crate) fn begin(runtime: &Path) {
    if !WANTED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(thread) = this_thread() {
        begin_for(thread, Some(runtime));
    } else {
        no_place();
        tell(Status::usual("its thread can't be found"), Some(runtime));
    }
}

/// Takes the daemon's priority, for `loop_thread`, and its place: at
/// once, if it's in a scope of its own already, as after an upgrade, or
/// else on a thread of its own, while the daemon starts. Its programs
/// started meanwhile are moved into their leaves once there are some.
/// How it went is said in the log, and in `runtime`, if given (see
/// [`STATUS_FILE`]). Once only.
#[doc(hidden)]
pub fn begin_for(loop_thread: Pid, runtime: Option<&Path>) {
    if BEGUN.swap(true, Ordering::SeqCst) {
        return;
    }
    note_early();
    let runtime = runtime.map(Path::to_owned);
    // In its scope already, divided or halfway, as an upgrade can come
    // while it's divided: it finishes, at once, even when switched off,
    // so its programs keep out of its own leaf.
    let scope = own_scope().and_then(|(dir, unit)| {
        divide(&dir, &unit, std::process::id())
            .map(|leaves| settle_into(leaves, None))
            .inspect_err(|err| log::warn!("{} couldn't be divided: {err}", dir.display()))
            .ok()
    });
    if let Some(why) = switched_off() {
        if scope.is_none() {
            no_place();
        }
        // Raised by the build before, which was on.
        if rustix::process::getpriority_process(Some(loop_thread)).is_ok_and(|nice| nice < 0) {
            let _ = rustix::process::setpriority_process(Some(loop_thread), 0);
        }
        let place = scope.map_or_else(|| "off".to_owned(), |scope| placed_in(&scope));
        let status = Status {
            priority: format!("off: {why}"),
            place,
            needs_linger: false,
        };
        tell(status, runtime.as_deref());
        return;
    }
    if let Some(scope) = &scope
        && let Ok(nice) = rustix::process::getpriority_process(Some(loop_thread))
        && nice < 0
    {
        // Upgraded in place: nothing to ask for.
        RAISED.store(true, Ordering::SeqCst);
        let status = Status {
            priority: raised_to(nice),
            place: placed_in(scope),
            needs_linger: false,
        };
        tell(status, runtime.as_deref());
        *lock(&SETTLING) = Some(Settling::Done(Settled {
            nice: Some(nice),
            scope: Some(scope.clone()),
        }));
        return;
    }
    let asking = runtime.clone();
    let settling = std::thread::Builder::new()
        .name("priority".into())
        .spawn(move || settle(loop_thread, scope, asking.as_deref()));
    match settling {
        Ok(settling) => *lock(&SETTLING) = Some(Settling::Going(settling)),
        Err(err) => {
            no_place();
            tell(
                Status::usual(&format!("no thread to ask from: {err}")),
                runtime.as_deref(),
            );
        }
    }
}

/// Waits for setting the daemon up to finish, if it's going on, and says
/// what it came to: for tests.
#[doc(hidden)]
#[must_use]
pub fn settled() -> Option<Settled> {
    let settling = lock(&SETTLING).take()?;
    match settling {
        Settling::Going(thread) => thread.join().ok(),
        Settling::Done(settled) => Some(settled),
    }
}

/// Raises the calling thread, the window's, as far as rtkit allows, on a
/// thread of its own, as asking takes a while: see the module's docs.
/// `what` names it in the one line it logs.
pub fn raise_this_thread(what: &'static str) {
    let Some(thread) = this_thread() else {
        log::info!("{what} runs at its usual priority: its thread can't be found");
        return;
    };
    let asking = std::thread::Builder::new()
        .name("priority".into())
        .spawn(move || match raise(thread) {
            Ok(nice) => log::info!("{what} runs at nice {nice}, through RealtimeKit"),
            Err(err) => log::info!("{what} runs at its usual priority: {err}"),
        });
    if let Err(err) = asking {
        log::info!("{what} runs at its usual priority: no thread to ask from: {err}");
    }
}

/// Raises `loop_thread`, then gives the daemon a scope, unless it has
/// `scope` already, and says how both went (see [`tell`]).
fn settle(loop_thread: Pid, scope: Option<String>, runtime: Option<&Path>) -> Settled {
    let nice = raise(loop_thread);
    let placed = if let Some(scope) = scope {
        Ok(scope)
    } else {
        match take_place() {
            Ok((leaves, mut user)) => Ok(settle_into(leaves, Some(&mut user))),
            Err(not) => {
                no_place();
                Err(not)
            }
        }
    };
    let status = Status {
        priority: match &nice {
            Ok(nice) => raised_to(*nice),
            Err(err) => format!("usual: {err}"),
        },
        place: match &placed {
            Ok(scope) => placed_in(scope),
            Err(not) => format!("where it started: {}", not.why),
        },
        needs_linger: placed.as_ref().is_err_and(|not| not.linger),
    };
    tell(status, runtime);
    Settled {
        nice: nice.ok(),
        scope: placed.ok(),
    }
}

/// Has the programs the daemon starts go into `leaves` from now on, and
/// moves those started before, through the user manager `user` for any
/// outside the scope. Returns the scope's unit.
fn settle_into(leaves: Leaves, user: Option<&mut Bus>) -> String {
    let unit = leaves.unit.clone();
    let early = match std::mem::replace(&mut *lock(&PLACE), Place::Leaves(leaves)) {
        Place::Pending(early) => early,
        _ => Vec::new(),
    };
    move_early(&early, &unit, user);
    unit
}

/// The daemon has no place of its own: the programs it started meanwhile
/// stay where they are, and the rest start where it is.
fn no_place() {
    let mut place = lock(&PLACE);
    if matches!(*place, Place::Pending(_)) {
        *place = Place::Nowhere;
    }
}

// --- What's said of it ------------------------------------------------------

/// The file in the daemon's runtime directory it says how its priority
/// and place went in, for `clackatoo priority` and windows: a line each,
/// `daemon: PID`, `priority: …` and `place: …`, then [`NEEDS_LINGER`] if
/// the daemon would have taken a place of its own, but the user doesn't
/// linger.
pub const STATUS_FILE: &str = "priority";

/// The line [`STATUS_FILE`] ends with when only lingering stands between
/// the daemon and a place of its own.
pub const NEEDS_LINGER: &str = "needs: linger";

/// Beside [`STATUS_FILE`]: made by the first window that tells the user
/// they don't linger, so no other window tells them again, until what the
/// daemon says changes.
const NOTED_FILE: &str = "priority.noted";

/// What a window says, once, of a daemon that could have taken a place of
/// its own but for lingering.
const LINGER_NOTE: &str = "clackatoo would keep agents' builds from slowing your typing with \
                           `loginctl enable-linger`: `clackatoo priority` says more";

/// How the daemon's priority and place went.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    /// Its loop thread's priority, and why, if it's the usual.
    priority: String,
    /// Where it is, and why, if where it started.
    place: String,
    /// Whether only lingering stood between it and a place of its own.
    needs_linger: bool,
}

impl Status {
    /// At the usual priority, where it started, as `why` says.
    fn usual(why: &str) -> Self {
        Self {
            priority: format!("usual: {why}"),
            place: "where it started".to_owned(),
            needs_linger: false,
        }
    }

    /// What [`STATUS_FILE`] holds after its `daemon` line.
    fn text(&self) -> String {
        let mut text = format!("priority: {}\nplace: {}\n", self.priority, self.place);
        if self.needs_linger {
            text.push_str(NEEDS_LINGER);
            text.push('\n');
        }
        text
    }
}

/// Why the daemon has no place of its own.
#[derive(Debug)]
struct NotPlaced {
    why: String,
    /// Whether it would have, if the user lingered.
    linger: bool,
}

impl From<String> for NotPlaced {
    fn from(why: String) -> Self {
        Self { why, linger: false }
    }
}

impl From<&str> for NotPlaced {
    fn from(why: &str) -> Self {
        why.to_owned().into()
    }
}

/// What it was, then, for the loop raised to `nice`.
fn raised_to(nice: i32) -> String {
    format!("nice {nice}, through RealtimeKit")
}

/// Where it is, in `scope`.
fn placed_in(scope: &str) -> String {
    format!("{scope}, with what it starts in leaves beside it")
}

/// How it went, kept for a chat's `:Capabilities`.
static TOLD: Mutex<Option<Status>> = Mutex::new(None);

/// Says how it went, in one line of the log, and in [`STATUS_FILE`] in
/// `runtime`, if given. A window tells the user they don't linger again
/// only if what's said changed (see [`linger_note`]).
fn tell(status: Status, runtime: Option<&Path>) {
    log::info!(
        "the daemon's priority: {}; its place: {}",
        status.priority,
        status.place
    );
    if let Some(runtime) = runtime {
        let file = runtime.join(STATUS_FILE);
        let text = status.text();
        let before = fs::read_to_string(&file).ok();
        let said_before = before.as_deref().map(without_daemon);
        if said_before != Some(text.as_str()) {
            let _ = fs::remove_file(runtime.join(NOTED_FILE));
        }
        let whole = format!("daemon: {}\n{text}", std::process::id());
        let partial = runtime.join(format!("{STATUS_FILE}.partial"));
        let written = fs::write(&partial, whole).and_then(|()| fs::rename(&partial, &file));
        if let Err(err) = written {
            log::warn!("{} couldn't be written: {err}", file.display());
        }
    }
    *lock(&TOLD) = Some(status);
}

/// What [`STATUS_FILE`]'s `text` says, without its first line, the
/// daemon's pid.
fn without_daemon(text: &str) -> &str {
    match text.split_once('\n') {
        Some((first, rest)) if first.starts_with("daemon: ") => rest,
        _ => text,
    }
}

/// How the daemon's priority and place went, on one line, for a chat's
/// `:Capabilities`.
pub(crate) fn status_line() -> String {
    match &*lock(&TOLD) {
        Some(status) if status.needs_linger => format!(
            "Priority {}; place {}. `loginctl enable-linger`, then `clackatoo upgrade`, gives it a place of its own",
            status.priority, status.place
        ),
        Some(status) => format!("Priority {}; place {}", status.priority, status.place),
        None => "Not asked for, or still being asked for".to_owned(),
    }
}

/// Whether the daemon whose runtime directory is `runtime` says only
/// lingering stands between it and a place of its own: waits up to
/// `patience` for a running daemon to say how its priority went, as it
/// does a moment after it starts. Blocks: for a thread of its own.
#[must_use]
pub fn needs_linger(runtime: &Path, patience: Duration) -> bool {
    let until = Instant::now() + patience;
    loop {
        if let Ok(text) = fs::read_to_string(runtime.join(STATUS_FILE))
            && let Some(pid) = (text.lines().next())
                .and_then(|line| line.strip_prefix("daemon: "))
            // What a daemon that has stopped said is no one's.
            && Path::new("/proc").join(pid).exists()
        {
            return text.lines().any(|line| line == NEEDS_LINGER);
        }
        if Instant::now() > until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// What a window tells the user, once, of the daemon whose runtime
/// directory is `runtime`, which [`needs_linger`]: `None` if another
/// window has told them since the daemon last said so.
#[must_use]
pub fn linger_note(runtime: &Path) -> Option<&'static str> {
    // The first window to make the mark says it; the rest find it made.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(runtime.join(NOTED_FILE))
        .ok()?;
    Some(LINGER_NOTE)
}

/// Why the daemon is to run as it always has, if it is: the environment
/// or `daemon/machine.toml` says so.
fn switched_off() -> Option<String> {
    let off = |name: &str| std::env::var_os(name).is_some_and(|value| value == "off");
    if off("CLACKATOO_PRIORITY") {
        return Some("CLACKATOO_PRIORITY is off".to_owned());
    }
    if off("CLACKATOO_DISPLAY") {
        return Some("CLACKATOO_DISPLAY is off".to_owned());
    }
    let config = crate::config_dir()?;
    (crate::machine::configured_priority(&config) == Some(false))
        .then(|| "daemon/machine.toml says priority = false".to_owned())
}

/// The calling thread's id, from `/proc/thread-self`, which links to
/// `PID/task/TID`.
fn this_thread() -> Option<Pid> {
    let link = fs::read_link("/proc/thread-self").ok()?;
    let tid = link.file_name()?.to_str()?.parse().ok()?;
    Pid::from_raw(tid)
}

/// The lock on `mutex`, whatever a thread that panicked holding it left.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

// --- What the daemon starts ----------------------------------------------
//
// Most programs are started the quick way, `posix_spawn`, and moved into
// their leaf by the daemon as soon as they have started: a hook between
// fork and exec makes std fork instead, which copies the daemon's page
// tables, about 2 ms at 50 MiB resident and 20 ms at 300 MiB, against a
// tenth of a millisecond. Their nice is put back by the kernel, as the
// raised thread has `SCHED_RESET_ON_FORK`. Only programs that fork anyway,
// for a hook of their own (a terminal's, the browser's), move themselves,
// before that hook runs.

/// Starts `command` as a child of the daemon's, the quick way, and puts
/// it in `leaf`: at once, or once the daemon has leaves.
///
/// # Errors
///
/// If it can't be started.
pub(crate) fn spawn(command: &mut Command, leaf: Leaf) -> io::Result<Child> {
    let child = command.spawn()?;
    started(child.id(), leaf);
    Ok(child)
}

/// Readies `command`, which has a hook of its own between fork and exec,
/// to start in `leaf`, and never at the raised priority of the thread
/// starting it: called before its own hook is added, so this one runs
/// first, before that hook moves any fds about. Returns what [`spawned`]
/// is to be told once it has.
pub(crate) fn ready_first(command: &mut Command, leaf: Leaf) -> Readied {
    let procs = match &*lock(&PLACE) {
        Place::Leaves(leaves) => Some(leaves.procs(leaf).as_raw_fd()),
        Place::Pending(_) => None,
        Place::Nowhere if RAISED.load(Ordering::SeqCst) => None,
        Place::Nowhere => return Readied(false),
    };
    ready(command, procs);
    Readied(procs.is_some())
}

/// Whether a program readied by [`ready_first`] moves itself into its
/// leaf as it starts.
#[derive(Clone, Copy, Debug)]
#[must_use]
pub(crate) struct Readied(bool);

/// Says that `child`, readied by [`ready_first`] as `readied`, has
/// started in `leaf`: one that didn't move itself is moved once there
/// are leaves.
pub(crate) fn spawned(child: &Child, leaf: Leaf, readied: Readied) {
    if !readied.0 {
        started(child.id(), leaf);
    }
}

/// Puts `pid`, a child that has just started and isn't reaped yet, in
/// `leaf`: now, if there are leaves, or once there are.
pub(crate) fn started(pid: u32, leaf: Leaf) {
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return;
    };
    match &mut *lock(&PLACE) {
        Place::Pending(early) => early.push(Early {
            pid,
            leaf,
            since: start_time(pid),
        }),
        // It started with the daemon, in its leaf, unless just before the
        // daemon went there, which is passed over.
        Place::Leaves(leaves) => {
            if let Err(err) = write_pid(leaves.procs(leaf), pid) {
                log::debug!("pid {pid} couldn't go in {}: {err}", leaf.name());
            }
        }
        Place::Nowhere => {}
    }
}

/// Has `command`, as it starts, put its nice back to 0 if it was raised,
/// and write `0` to `procs`, a leaf's `cgroup.procs`, if given, which
/// moves it there.
fn ready(command: &mut Command, procs: Option<RawFd>) {
    // SAFETY: the closure runs in the child between fork and exec, where
    // only async-signal-safe operations are allowed. It makes at most
    // three raw system calls through rustix, getpriority, setpriority and
    // write, with no allocation, locking or shared state. The fd it
    // writes to is never closed: `PLACE` keeps it, once set, for the life
    // of the process, at 64 or above, out of the way of the fds hooks
    // after it put in place.
    unsafe {
        command.pre_exec(move || {
            // A nice of 0 or more is the user's choice, kept.
            if rustix::process::getpriority_process(None).is_ok_and(|nice| nice < 0) {
                let _ = rustix::process::setpriority_process(None, 0);
            }
            if let Some(procs) = procs {
                // Failing leaves it with the daemon, rather than not
                // starting it at all.
                let _ = rustix::io::write(BorrowedFd::borrow_raw(procs), b"0");
            }
            Ok(())
        });
    }
}

/// Moves `pid` into the leaf whose `cgroup.procs` is `procs`.
fn write_pid(procs: &OwnedFd, pid: Pid) -> io::Result<()> {
    let text = pid.as_raw_pid().to_string();
    rustix::io::write(procs, text.as_bytes())?;
    Ok(())
}

/// When process `pid` started, in clock ticks since boot: field 22 of
/// `/proc/PID/stat`. `None` once it has gone.
fn start_time(pid: Pid) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    start_time_in(&stat)
}

/// Field 22, the start time, of `stat`, a `/proc/PID/stat`: counted from
/// the state, the 3rd, after the name in brackets, which may hold spaces.
fn start_time_in(stat: &str) -> Option<u64> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(22 - 3)?.parse().ok()
}

/// Moves the programs started before the daemon had leaves into theirs:
/// those it started once it was in its scope from there, the rest
/// through the user manager `user`, if given, as they're in the cgroup
/// where the daemon started, which isn't the user's to change. One that
/// has ended meanwhile, its pid perhaps another's by now, is passed over.
fn move_early(early: &[Early], unit: &str, user: Option<&mut Bus>) {
    let still = |early: &Early| early.since.is_some() && start_time(early.pid) == early.since;
    let place = lock(&PLACE);
    let Place::Leaves(leaves) = &*place else {
        return;
    };
    let mut elsewhere = Vec::new();
    for early in early.iter().filter(|early| still(early)) {
        if write_pid(leaves.procs(early.leaf), early.pid).is_err() {
            elsewhere.push(*early);
        }
    }
    drop(place);
    let Some(user) = user else {
        return;
    };
    for early in elsewhere.iter().filter(|early| still(early)) {
        if let Err(err) = attach(user, unit, early.pid, early.leaf) {
            log::debug!(
                "pid {} couldn't go in {}: {err}",
                early.pid,
                early.leaf.name()
            );
        }
    }
}

// --- Priority --------------------------------------------------------------

/// Raises `thread` as far as rtkit allows, first setting it to start what
/// it starts at the usual priority. Returns its nice.
fn raise(thread: Pid) -> Result<i32, String> {
    let nice = rustix::process::getpriority_process(Some(thread))
        .map_err(|err| format!("its nice can't be read: {err}"))?;
    // SAFETY: only reads the thread's policy.
    let policy = unsafe { libc::sched_getscheduler(thread.as_raw_pid()) };
    if let Some(chosen) = chosen_scheduling(policy, nice) {
        return Err(format!("kept as it was started, {chosen}"));
    }
    if nice < 0 {
        // Raised already, as it stays through an upgrade.
        RAISED.store(true, Ordering::SeqCst);
        reset_on_fork(thread)?;
        return Ok(nice);
    }
    reset_on_fork(thread)?;
    let mut bus =
        connect(&dbus::system_address()).map_err(|err| format!("the system bus: {err}"))?;
    let min = rtkit_min_nice(&mut bus).map_err(|err| format!("RealtimeKit: {err}"))?;
    let call = Call {
        destination: RTKIT,
        path: RTKIT_PATH,
        interface: RTKIT,
        member: "MakeThreadHighPriority",
        signature: "ti",
    };
    let mut body = Body::default();
    body.u64(u64::from(thread.as_raw_pid().cast_unsigned()));
    body.i32(min);
    bus.ask(&call, &body, WAIT).map_err(|err| {
        if err.to_string().contains("AccessDenied") {
            // polkit's rule for rtkit, as it comes: a local desktop login's
            // programs only.
            format!("RealtimeKit refused, as polkit allows it only from a local desktop login, not ssh ({err})")
        } else {
            format!("RealtimeKit: {err}")
        }
    })?;
    RAISED.store(true, Ordering::SeqCst);
    rustix::process::getpriority_process(Some(thread))
        .map_err(|err| format!("its nice can't be read: {err}"))
}

/// What the user chose for the thread's scheduling, if they chose
/// anything but the usual, which is then theirs and left alone: `policy`
/// other than `SCHED_OTHER`, as `chrt --idle` or `--batch` start one with,
/// or a `nice` above 0, as `nice` does. A negative nice is taken to be
/// rtkit's, raised before an upgrade.
fn chosen_scheduling(policy: libc::c_int, nice: i32) -> Option<String> {
    let policy = policy & !libc::SCHED_RESET_ON_FORK;
    if policy >= 0 && policy != libc::SCHED_OTHER {
        let named = match policy {
            libc::SCHED_BATCH => "SCHED_BATCH".to_owned(),
            libc::SCHED_IDLE => "SCHED_IDLE".to_owned(),
            libc::SCHED_FIFO => "SCHED_FIFO".to_owned(),
            libc::SCHED_RR => "SCHED_RR".to_owned(),
            other => format!("scheduling policy {other}"),
        };
        return Some(format!("with {named}"));
    }
    (nice > 0).then(|| format!("at nice {nice}"))
}

/// Sets `thread` to start every process and thread at the usual
/// scheduling and nice 0, as rtkit does too: setting the flag needs no
/// privilege, only clearing it does.
fn reset_on_fork(thread: Pid) -> Result<(), String> {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a valid `sched_param`, read during the call only.
    let set = unsafe {
        libc::sched_setscheduler(
            thread.as_raw_pid(),
            libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK,
            &raw const param,
        )
    };
    if set == 0 {
        Ok(())
    } else {
        Err(format!(
            "its scheduling can't be set: {}",
            io::Error::last_os_error()
        ))
    }
}

const RTKIT: &str = "org.freedesktop.RealtimeKit1";
const RTKIT_PATH: &str = "/org/freedesktop/RealtimeKit1";

/// The most rtkit raises a thread: its `MinNiceLevel`.
fn rtkit_min_nice(bus: &mut Bus) -> io::Result<i32> {
    let reply = property(bus, RTKIT, RTKIT_PATH, RTKIT, "MinNiceLevel")?;
    let mut args = reply.args("v").ok_or_else(|| unexpected("MinNiceLevel"))?;
    match args.variant() {
        Some("i") => args.i32().ok_or_else(|| unexpected("MinNiceLevel")),
        _ => Err(unexpected("MinNiceLevel")),
    }
}

// --- Place -----------------------------------------------------------------

const LOGIN: &str = "org.freedesktop.login1";
const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const MANAGER: &str = "org.freedesktop.systemd1.Manager";

/// Gives the daemon a scope of its own, divided into leaves, with itself
/// in its own: see the module's docs. Returns the leaves, and the
/// connection to the user manager, for moving the programs started
/// meanwhile. Or why not, and whether lingering would change that.
fn take_place() -> Result<(Leaves, Bus), NotPlaced> {
    if !Path::new(CGROUPS).join("cgroup.controllers").exists() {
        return Err("cgroups aren't mounted as systemd mounts them, all in one tree".into());
    }
    let started = own_cgroup().map_err(|err| format!("its cgroup can't be read: {err}"))?;
    let uid = rustix::process::getuid().as_raw();
    if !under_user_manager(&started, uid) {
        let lingering = connect(&dbus::system_address())
            .and_then(|mut bus| lingers(&mut bus, uid))
            .map_err(|err| format!("whether you linger can't be told: {err}"))?;
        if !lingering {
            return Err(NotPlaced {
                why: "a scope of its own would end with your last login, as you don't linger \
                      (`loginctl enable-linger` lets it)"
                    .to_owned(),
                linger: true,
            });
        }
    }
    let address = dbus::session_address().ok_or("there's no session bus")?;
    let mut user = connect(&address).map_err(|err| format!("the session bus: {err}"))?;
    let pid = std::process::id();
    let unit = format!("{SCOPE_PREFIX}{pid}.scope");
    match start_scope(&mut user, &unit, pid) {
        // Asked for by the build before, which upgraded before systemd
        // moved it in: it's moved in still.
        Err(err) if err.to_string().contains("UnitExists") => {
            log::debug!("{unit} is there already: {err}");
        }
        started => started.map_err(|err| format!("systemd: {err}"))?,
    }
    let scope = wait_to_be_in(&unit)?;
    let leaves = divide(&scope, &unit, pid)
        .map_err(|err| format!("{} couldn't be divided: {err}", scope.display()))?;
    Ok((leaves, user))
}

/// Whether `cgroup` is under the user manager of `uid` already, so a
/// scope of the daemon's own ends no sooner than it would.
fn under_user_manager(cgroup: &Path, uid: u32) -> bool {
    let manager = format!("user@{uid}.service");
    cgroup
        .components()
        .any(|part| part.as_os_str() == manager.as_str())
}

/// Whether the user `uid` lingers, as logind says.
fn lingers(bus: &mut Bus, uid: u32) -> io::Result<bool> {
    let path = format!("/org/freedesktop/login1/user/_{uid}");
    let reply = property(bus, LOGIN, &path, "org.freedesktop.login1.User", "Linger")?;
    let mut args = reply.args("v").ok_or_else(|| unexpected("Linger"))?;
    match args.variant() {
        Some("b") => args.boolean().ok_or_else(|| unexpected("Linger")),
        _ => Err(unexpected("Linger")),
    }
}

/// Asks the user manager for the scope `unit`, with the process `pid` in
/// it, delegated, so the daemon may divide it.
fn start_scope(user: &mut Bus, unit: &str, pid: u32) -> io::Result<()> {
    let call = Call {
        destination: SYSTEMD,
        path: SYSTEMD_PATH,
        interface: MANAGER,
        member: "StartTransientUnit",
        signature: "ssa(sv)a(sa(sv))",
    };
    user.ask(&call, &scope_body(unit, pid), WAIT)?;
    Ok(())
}

/// What asks for the scope: its name, `fail` should it be there already,
/// its properties, and no other units with it.
fn scope_body(unit: &str, pid: u32) -> Body {
    let mut body = Body::default();
    body.string(unit);
    body.string("fail");
    let properties = body.begin_array(8);
    body.begin_struct();
    body.string("Description");
    body.variant_string("clackatoo's daemon, and what it starts");
    // With the desktop's other apps.
    body.begin_struct();
    body.string("Slice");
    body.variant_string("app.slice");
    body.begin_struct();
    body.string("PIDs");
    body.variant_u32s(&[pid]);
    body.begin_struct();
    body.string("Delegate");
    body.variant_bool(true);
    // Protection counts only as far as the cgroups above give it: none,
    // as systemd comes, but the daemon's share is there once they do.
    body.begin_struct();
    body.string("MemoryLow");
    body.variant_u64(MEMORY_LOW);
    body.end_array(properties);
    let others = body.begin_array(8);
    body.end_array(others);
    body
}

/// Waits for the daemon to be in the scope `unit`, which systemd puts it
/// in a moment after it answers. Returns the scope's directory.
fn wait_to_be_in(unit: &str) -> Result<PathBuf, String> {
    let until = Instant::now() + WAIT;
    loop {
        let own = own_cgroup().map_err(|err| format!("its cgroup can't be read: {err}"))?;
        if own.file_name().is_some_and(|name| name == unit) {
            return Ok(own);
        }
        if Instant::now() > until {
            return Err(format!("systemd didn't move it into {unit}"));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Divides `scope`, the daemon's, which it's in alone but for what it
/// started meanwhile: makes the leaves, moves the daemon into its own and
/// the rest into `terminals`, hands the scope's controllers down to them,
/// and weighs them. Each step is the same done again, so a scope divided
/// already, or halfway, as an upgrade can leave one, is finished.
fn divide(scope: &Path, unit: &str, pid: u32) -> io::Result<Leaves> {
    for leaf in [DAEMON, Leaf::Terminals.name(), Leaf::Agents.name()] {
        if let Err(err) = fs::create_dir(scope.join(leaf))
            && err.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(err);
        }
    }
    write(&scope.join(DAEMON).join("cgroup.procs"), &pid.to_string())?;
    // In its leaf now: the leaves are finished as far as they can be,
    // never left half made with it in one and its programs beside it.
    if let Err(err) = hand_down_all(scope) {
        log::warn!(
            "{}'s leaves have no weights of their own: {err}",
            scope.display()
        );
    }
    let daemon = scope.join(DAEMON);
    weigh(&daemon.join("cpu.weight"), DAEMON_WEIGHT);
    weigh(&daemon.join("io.weight"), DAEMON_WEIGHT);
    weigh(&daemon.join("memory.low"), &MEMORY_LOW.to_string());
    let agents = scope.join(Leaf::Agents.name());
    weigh(&agents.join("cpu.weight"), AGENTS_WEIGHT);
    weigh(&agents.join("io.weight"), AGENTS_WEIGHT);
    open_leaves(scope, unit).inspect_err(|_| leave_leaf(scope, pid))
}

/// Moves what's in `scope` itself into `terminals`, then hands the
/// scope's controllers down to its leaves: it can only with no processes
/// of its own, and a program the daemon started as it moved may be there.
fn hand_down_all(scope: &Path) -> io::Result<()> {
    let offered = fs::read_to_string(scope.join("cgroup.controllers"))?;
    for _ in 0..8 {
        move_strays(scope)?;
        match hand_down(scope, &offered) {
            Err(err) if err.raw_os_error() == Some(libc::EBUSY) => {}
            handed => return handed,
        }
    }
    Err(io::Error::other(
        "programs kept starting in the scope itself",
    ))
}

/// Takes the daemon, `pid`, out of its leaf of `scope` and back into the
/// scope itself, as it has no leaves after all: its controllers taken back
/// first, as the scope can't have processes of its own while it hands them
/// down. As far as that can be done.
fn leave_leaf(scope: &Path, pid: u32) {
    for controller in ["pids", "memory", "io", "cpu"] {
        let _ = write(
            &scope.join("cgroup.subtree_control"),
            &format!("-{controller}"),
        );
    }
    if let Err(err) = write(&scope.join("cgroup.procs"), &pid.to_string()) {
        log::warn!(
            "the daemon couldn't leave its leaf of {}: {err}",
            scope.display()
        );
    }
}

/// Moves whatever is in `scope` itself into its `terminals` leaf.
fn move_strays(scope: &Path) -> io::Result<()> {
    let strays = fs::read_to_string(scope.join("cgroup.procs"))?;
    for stray in strays.lines().filter(|line| !line.is_empty()) {
        let moved = write(
            &scope.join(Leaf::Terminals.name()).join("cgroup.procs"),
            stray,
        );
        // One that has ended meanwhile is gone anyway.
        if let Err(err) = moved
            && err.raw_os_error() != Some(libc::ESRCH)
        {
            return Err(err);
        }
    }
    Ok(())
}

/// Hands the controllers `offered` (a scope's `cgroup.controllers`) that
/// weigh the leaves down to them.
fn hand_down(scope: &Path, offered: &str) -> io::Result<()> {
    let offered: Vec<&str> = offered.split_whitespace().collect();
    for controller in ["cpu", "io", "memory", "pids"] {
        if offered.contains(&controller) {
            write(
                &scope.join("cgroup.subtree_control"),
                &format!("+{controller}"),
            )?;
        }
    }
    Ok(())
}

/// Writes `value` to the cgroup file `file`, if its controller is there:
/// a weight that can't be set leaves its leaf at the usual.
fn weigh(file: &Path, value: &str) {
    if let Err(err) = write(file, value)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::debug!("{} couldn't be set: {err}", file.display());
    }
}

/// Writes `value` to the cgroup file `file`, in one write, as the kernel
/// takes it.
fn write(file: &Path, value: &str) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .open(file)?
        .write_all(value.as_bytes())
}

/// The daemon's scope, its directory and unit, if it's in one already:
/// in its `daemon` leaf, or in the scope itself, as when an upgrade came
/// while it was being divided.
fn own_scope() -> Option<(PathBuf, String)> {
    let own = own_cgroup().ok()?;
    let scope = if own.file_name()? == DAEMON {
        own.parent()?.to_owned()
    } else {
        own
    };
    let unit = scope.file_name()?.to_str()?.to_owned();
    is_daemons_scope(&unit).then_some((scope, unit))
}

/// Whether `unit` is a scope a daemon made itself: see [`SCOPE_PREFIX`].
fn is_daemons_scope(unit: &str) -> bool {
    (unit.strip_prefix(SCOPE_PREFIX))
        .and_then(|rest| rest.strip_suffix(".scope"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The lowest fd a leaf's `cgroup.procs` is kept at: above those a
/// child's own hook puts in place, as the browser's puts its pipes at 3
/// and 4, which a daemon just upgraded, with only 0 to 2 open, would
/// otherwise open the leaves at.
const LEAF_FDS: RawFd = 64;

/// Opens the leaves of `scope`, the scope `unit`, for the daemon's
/// children to move themselves into, at [`LEAF_FDS`] or above.
fn open_leaves(scope: &Path, unit: &str) -> io::Result<Leaves> {
    // Closed on exec, as std opens every file: only a child between fork
    // and exec writes to it.
    let open = |leaf: Leaf| -> io::Result<OwnedFd> {
        let file = OpenOptions::new()
            .write(true)
            .open(scope.join(leaf.name()).join("cgroup.procs"))?;
        Ok(rustix::io::fcntl_dupfd_cloexec(&file, LEAF_FDS)?)
    };
    Ok(Leaves {
        unit: unit.to_owned(),
        terminals: open(Leaf::Terminals)?,
        agents: open(Leaf::Agents)?,
    })
}

/// The directory of this process's cgroup.
fn own_cgroup() -> io::Result<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup")?;
    let path = cgroup_in(&text)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "it isn't in a cgroup v2 tree"))?;
    Ok(Path::new(CGROUPS).join(path.trim_start_matches('/')))
}

/// The cgroup v2 path in `text`, as `/proc/PID/cgroup` says it: the line
/// `0::/user.slice/…`.
fn cgroup_in(text: &str) -> Option<&str> {
    (text.lines())
        .find_map(|line| line.strip_prefix("0::"))
        .filter(|path| path.starts_with('/'))
}

/// Moves `pid` into `leaf` of the scope `unit` through the user manager,
/// for a program outside the scope.
fn attach(user: &mut Bus, unit: &str, pid: Pid, leaf: Leaf) -> io::Result<()> {
    let call = Call {
        destination: SYSTEMD,
        path: SYSTEMD_PATH,
        interface: MANAGER,
        member: "AttachProcessesToUnit",
        signature: "ssau",
    };
    let mut body = Body::default();
    body.string(unit);
    body.string(&format!("/{}", leaf.name()));
    let pids = body.begin_array(4);
    body.u32(pid.as_raw_pid().cast_unsigned());
    body.end_array(pids);
    user.ask(&call, &body, WAIT)?;
    Ok(())
}

// --- D-Bus -----------------------------------------------------------------

/// Connects to the bus at `address`, and says hello, as the first call
/// on a connection must.
fn connect(address: &str) -> io::Result<Bus> {
    let mut bus = Bus::connect(address)?;
    let hello = bus.hello()?;
    bus.answer(hello, WAIT)?;
    Ok(bus)
}

/// The property `name` of `interface` of the object `path` of
/// `destination`: the reply, a variant.
fn property(
    bus: &mut Bus,
    destination: &str,
    path: &str,
    interface: &str,
    name: &str,
) -> io::Result<Message> {
    let call = Call {
        destination,
        path,
        interface: "org.freedesktop.DBus.Properties",
        member: "Get",
        signature: "ss",
    };
    let mut body = Body::default();
    body.string(interface);
    body.string(name);
    bus.ask(&call, &body, WAIT)
}

/// A reply that isn't what `what` should be.
fn unexpected(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} isn't what it should be"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Read as _;

    #[test]
    fn the_cgroup_is_the_unified_trees() {
        assert_eq!(
            cgroup_in("0::/user.slice/user-1000.slice/session-2.scope\n"),
            Some("/user.slice/user-1000.slice/session-2.scope")
        );
        // A hybrid machine's other trees are passed over.
        assert_eq!(
            cgroup_in("12:cpu,cpuacct:/user.slice\n0::/user.slice/a.scope\n"),
            Some("/user.slice/a.scope")
        );
        assert_eq!(cgroup_in("12:cpu,cpuacct:/user.slice\n"), None);
        assert_eq!(cgroup_in(""), None);
    }

    #[test]
    fn only_a_daemons_own_scope_is_taken_for_one() {
        assert!(is_daemons_scope("app-clackatoo-1234.scope"));
        assert!(!is_daemons_scope("app-clackatoo-.scope"));
        assert!(!is_daemons_scope("app-clackatoo-12a.scope"));
        assert!(!is_daemons_scope("app-clackatoo-1234.service"));
        assert!(!is_daemons_scope("session-2.scope"));
    }

    #[test]
    fn a_daemon_under_the_user_manager_already_is_told_apart() {
        let cgroup = |path: &str| Path::new(CGROUPS).join(path);
        assert!(under_user_manager(
            &cgroup("user.slice/user-1000.slice/user@1000.service/app.slice/a.scope"),
            1000
        ));
        assert!(!under_user_manager(
            &cgroup("user.slice/user-1000.slice/session-2.scope"),
            1000
        ));
        assert!(!under_user_manager(
            &cgroup("user.slice/user-1001.slice/user@1001.service/a.scope"),
            1000
        ));
    }

    /// The properties, laid out as the specification says: an array of
    /// (name, variant) structs, each on a multiple of 8.
    #[test]
    fn the_scope_is_asked_for_with_the_daemon_in_it_delegated() {
        let body = scope_body("app-clackatoo-7.scope", 7);
        let bytes = body.as_bytes();
        let find = |needle: &[u8]| {
            bytes
                .windows(needle.len())
                .position(|window| window == needle)
                .unwrap_or_else(|| panic!("no {needle:?}"))
        };
        // The PIDs: the signature `au`, then the array's length, 4, and 7.
        let pids = find(b"PIDs\0");
        let au = find(b"\x02au\0");
        assert!(au > pids);
        let after = au + 4;
        let length_at = after.next_multiple_of(4);
        assert_eq!(&bytes[length_at..length_at + 8], &[4, 0, 0, 0, 7, 0, 0, 0]);
        // Delegate: `b`, then true as a 32-bit 1.
        let delegate = find(b"Delegate\0") + "Delegate\0".len();
        assert_eq!(&bytes[delegate..delegate + 3], b"\x01b\0");
        let value = (delegate + 3).next_multiple_of(4);
        assert_eq!(&bytes[value..value + 4], &[1, 0, 0, 0]);
        // MemoryLow: `t`, on a multiple of 8.
        let low = find(b"MemoryLow\0") + "MemoryLow\0".len();
        assert_eq!(&bytes[low..low + 3], b"\x01t\0");
        let value = (low + 3).next_multiple_of(8);
        assert_eq!(&bytes[value..value + 8], &MEMORY_LOW.to_le_bytes());
        // Each struct starts on a multiple of 8.
        for name in ["Description\0", "Slice\0", "PIDs\0", "Delegate\0"].map(str::as_bytes) {
            assert_eq!((find(name) - 4) % 8, 0, "{name:?}");
        }
        // No other units: an empty array, padded to 8 for its elements.
        assert_eq!(&bytes[bytes.len() - 8..], &[0; 8]);
    }

    /// What the daemon says when only lingering stood in its way: windows
    /// tell the user once, and again only once what it says changes.
    #[test]
    fn a_window_says_lingering_would_help_once_until_the_daemon_says_otherwise() {
        let runtime = tempfile::tempdir().unwrap();
        let runtime = runtime.path();
        let needing = Status {
            priority: raised_to(-15),
            place: "where it started: you don't linger".to_owned(),
            needs_linger: true,
        };
        assert!(!needs_linger(runtime, Duration::ZERO), "nothing said yet");
        tell(needing.clone(), Some(runtime));
        let text = fs::read_to_string(runtime.join(STATUS_FILE)).unwrap();
        assert_eq!(without_daemon(&text), needing.text());
        assert!(text.ends_with(&format!("{NEEDS_LINGER}\n")), "{text}");
        assert!(needs_linger(runtime, Duration::ZERO));
        assert!(linger_note(runtime).is_some());
        assert!(linger_note(runtime).is_none(), "said once");
        // Said again the same, as after an upgrade: not again.
        tell(needing.clone(), Some(runtime));
        assert!(linger_note(runtime).is_none());
        // Said otherwise, then the same again: once more.
        tell(Status::usual("no rtkit"), Some(runtime));
        assert!(!needs_linger(runtime, Duration::ZERO));
        tell(needing, Some(runtime));
        assert!(linger_note(runtime).is_some());
        // What a daemon that has stopped said is no one's.
        fs::write(
            runtime.join(STATUS_FILE),
            format!("daemon: 999999999\n{NEEDS_LINGER}\n"),
        )
        .unwrap();
        assert!(!needs_linger(runtime, Duration::ZERO));
    }

    #[test]
    fn nothing_is_added_to_a_program_while_the_daemon_is_as_it_always_was() {
        // Tests never begin: their daemons are as they always were.
        assert!(!ready_first(&mut Command::new("true"), Leaf::Terminals).0);
    }

    /// The leaves are kept out of the way of the fds a child's own hook
    /// puts in place, as the browser's does at 3 and 4, even in a daemon
    /// just upgraded, with only 0 to 2 open below them.
    #[test]
    fn the_leaves_are_kept_at_64_or_above() {
        let scope = tempfile::tempdir().unwrap();
        for leaf in [Leaf::Terminals, Leaf::Agents] {
            let dir = scope.path().join(leaf.name());
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("cgroup.procs"), "").unwrap();
        }
        let leaves = open_leaves(scope.path(), "app-clackatoo-7.scope").unwrap();
        for leaf in [Leaf::Terminals, Leaf::Agents] {
            assert!(leaves.procs(leaf).as_raw_fd() >= LEAF_FDS, "{leaves:?}");
        }
        assert_eq!(leaves.unit, "app-clackatoo-7.scope");
    }

    /// A pid is moved only while it's still the program given it: its
    /// start time is field 22, past a name that may hold spaces and `)`.
    #[test]
    fn a_start_time_is_read_past_a_name_with_brackets() {
        let stat = "42 (a b) c)) S 1 42 42 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 98765 1000 10";
        assert_eq!(start_time_in(stat), Some(98765));
        assert_eq!(start_time_in("42 (short) S 1"), None);
        let ours = Pid::from_raw(i32::try_from(std::process::id()).unwrap()).unwrap();
        assert!(start_time(ours).is_some());
    }

    /// What a user chose with `nice` or `chrt` is theirs: rtkit isn't asked.
    #[test]
    fn a_users_own_scheduling_is_left_alone() {
        assert_eq!(chosen_scheduling(libc::SCHED_OTHER, 0), None);
        assert_eq!(
            chosen_scheduling(libc::SCHED_OTHER | libc::SCHED_RESET_ON_FORK, -15),
            None,
            "rtkit's, before an upgrade"
        );
        assert_eq!(
            chosen_scheduling(libc::SCHED_OTHER, 15).as_deref(),
            Some("at nice 15")
        );
        assert_eq!(
            chosen_scheduling(libc::SCHED_IDLE, 0).as_deref(),
            Some("with SCHED_IDLE")
        );
        assert_eq!(
            chosen_scheduling(libc::SCHED_BATCH, 5).as_deref(),
            Some("with SCHED_BATCH")
        );
    }

    /// The hook a child runs between fork and exec, with a plain file in
    /// place of a leaf's `cgroup.procs`: it writes `0` to it, and starts
    /// at the usual scheduling, at no higher a priority than its parent.
    #[test]
    fn a_child_moves_itself_into_its_leaf_and_never_starts_raised() {
        let dir = tempfile::tempdir().unwrap();
        let procs = dir.path().join("cgroup.procs");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&procs)
            .unwrap();
        let mut command = Command::new("cat");
        command.arg("/proc/self/stat");
        ready(&mut command, Some(file.as_raw_fd()));
        let output = command.output().unwrap();
        assert!(output.status.success());
        let mut written = String::new();
        File::open(&procs)
            .unwrap()
            .read_to_string(&mut written)
            .unwrap();
        assert_eq!(written, "0");
        let stat = String::from_utf8(output.stdout).unwrap();
        let fields: Vec<&str> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        // Fields 19 and 41 of proc(5), counted from the state, the 3rd.
        let (nice, policy): (i32, u32) = (fields[16].parse().unwrap(), fields[38].parse().unwrap());
        let ours = rustix::process::getpriority_process(None).unwrap();
        assert_eq!(nice, ours.max(0));
        assert_eq!(policy, 0, "SCHED_OTHER");
    }
}
