//! Stopping when asked by a signal: SIGTERM, as `clackatoo stop`, a
//! logout or a shutdown sends; SIGINT, as Ctrl+C does to a daemon run in
//! a terminal; and SIGHUP, as a session ending or that terminal closing
//! does. Each asks the daemon to stop as it always does (see
//! [`Daemon::run`](crate::Daemon::run)), rather than die on the spot.
//!
//! The handler does next to nothing, as a handler may only do what's safe
//! at any moment: it notes the signal and writes a byte to a pipe the
//! event loop watches. The loop takes it from there. A second signal while
//! the daemon stops does the same, which changes nothing.
//!
//! Why a handler, and not a `signalfd` with the signals blocked: a blocked
//! signal stays blocked in every program started from then on, as Rust's
//! `Command` leaves the mask as it is, so a shell in a terminal couldn't be
//! interrupted with Ctrl+C. A signal caught by a handler, by contrast, is
//! back to doing what it normally does in a program the daemon starts, as
//! `exec` resets it. That holds even for a signal the daemon itself was
//! started ignoring, as from `nohup` or a script's `&`: ignored, it would
//! be ignored in every terminal too. So the daemon catches all three
//! whatever it was started with, and its handler takes no notice of those
//! it was started ignoring, so `nohup` still means it outlives a hang-up.
//!
//! An upgrade `exec`s the next build, which resets the handler too, and
//! the next build installs its own only once it's running. So the daemon
//! holds the signals back for it (see [`hold`]): they're blocked on the
//! thread that `exec`s, which the next build starts as, and any that came
//! meanwhile are sent to that thread, to wait there. Blocked signals and
//! those waiting both carry across `exec`. The next build's handler, once
//! installed, takes them, and it stops at once. Only a build that says it
//! takes them (see [`crate::upgrade::TAKES_SIGNALS`]) has them held back:
//! an older one would leave them blocked for good. Signals the daemon was
//! started ignoring are ignored again across the `exec`, for the next
//! build to see them as this one did.

#![allow(
    unsafe_code,
    reason = "signal handling has no safe API in std or rustix"
)]

use std::io;
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

/// The signals that ask the daemon to stop.
const STOPPING: [libc::c_int; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// The signals that have come since they were last taken, a bit each.
static ARRIVED: AtomicU32 = AtomicU32::new(0);

/// The signals the daemon was started ignoring, a bit each: caught, so the
/// programs it starts have them as usual, but taken no notice of.
static IGNORED: AtomicU32 = AtomicU32::new(0);

/// The thread holding signals back for the next build, while there is
/// one: see [`hold`].
static HOLDER: AtomicI32 = AtomicI32::new(0);

/// The pipe the handler writes to, once it's installed: its write end's
/// fd, kept open for the life of the process.
static WRITE: AtomicI32 = AtomicI32::new(-1);

/// The pipe's read end, which the event loop watches.
static READ: OnceLock<OwnedFd> = OnceLock::new();

/// A signal that asks the daemon to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Signal(libc::c_int);

impl Signal {
    /// What it means, for the log.
    pub(crate) fn meaning(self) -> &'static str {
        match self.0 {
            libc::SIGTERM => "SIGTERM: asked to stop",
            libc::SIGINT => "SIGINT: interrupted",
            _ => "SIGHUP: the session it was in has ended",
        }
    }
}

/// Catches the signals that ask the daemon to stop, and lets them in on
/// this thread, where the build before may have held them back: any held
/// back are taken at once. Called once, before the daemon starts any
/// thread, as a thread starts with the signals its starter blocks, and
/// passes them on to every program it starts.
///
/// # Errors
///
/// If the pipe can't be made, or a handler can't be installed.
pub(crate) fn install() -> io::Result<()> {
    if READ.get().is_some() {
        return mask(libc::SIG_UNBLOCK);
    }
    let (read, write) = rustix::pipe::pipe_with(
        rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
    )?;
    let write = write.into_raw_fd();
    WRITE.store(write, Ordering::SeqCst);
    let _ = READ.set(read);
    for signal in STOPPING {
        if ignored(signal)? {
            IGNORED.fetch_or(bit(signal), Ordering::SeqCst);
        }
        catch(signal)?;
    }
    mask(libc::SIG_UNBLOCK)
}

/// Whether `signal` is ignored.
fn ignored(signal: libc::c_int) -> io::Result<bool> {
    // SAFETY: only the current action is read, into a valid `sigaction`.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(signal, std::ptr::null(), &raw mut action) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(action.sa_sigaction == libc::SIG_IGN)
    }
}

/// Has the handler catch `signal`.
fn catch(signal: libc::c_int) -> io::Result<()> {
    // SAFETY: the action is a valid `sigaction`, zeroed but for what's set,
    // and the handler only does what's safe in one.
    let installed = unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
        // Interrupted system calls on other threads carry on.
        action.sa_flags = libc::SA_RESTART;
        action.sa_mask = stopping_set();
        libc::sigaction(signal, &raw const action, std::ptr::null_mut())
    };
    if installed == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The signals the daemon was started ignoring.
fn started_ignoring() -> Vec<libc::c_int> {
    let ignored = IGNORED.load(Ordering::SeqCst);
    (STOPPING.into_iter())
        .filter(|&signal| ignored & bit(signal) != 0)
        .collect()
}

/// The pipe the event loop watches, if the handler is installed.
pub(crate) fn pipe() -> Option<BorrowedFd<'static>> {
    READ.get().map(AsFd::as_fd)
}

/// The signals that have come since this was last called, emptying the
/// pipe, which the event loop watches edge-triggered.
pub(crate) fn take() -> Vec<Signal> {
    if let Some(read) = READ.get() {
        let mut bytes = [0; 64];
        while matches!(rustix::io::read(read, &mut bytes), Ok(read) if read > 0) {}
    }
    arrived(ARRIVED.swap(0, Ordering::SeqCst))
}

/// Whether a signal asking the daemon to stop has come since the event
/// loop last looked, as when one comes as the daemon hands over.
pub(crate) fn asked_to_stop() -> bool {
    ARRIVED.load(Ordering::SeqCst) != 0
}

/// Gets the signals ready for the next build, about to be `exec`'d from
/// this thread. Those the daemon was started ignoring are ignored again,
/// for it to start ignoring them too.
///
/// If it `takes_signals`, the rest are held back for it: blocked here, and
/// any that come from now on, on whichever thread, are sent here to wait.
/// Those that came since the event loop last looked, as the daemon handed
/// over, are sent here too. The next build takes them once it installs its
/// handler (see [`install`]). One can still go unheard if it reaches
/// another thread just as the `exec` ends it, a window of microseconds,
/// which is accepted. A build that doesn't take them would leave them
/// blocked for good, so for one of those nothing is held back, and a
/// signal that comes from here on goes unheard, or ends it, as it would
/// have before.
pub(crate) fn hold(takes_signals: bool) {
    if READ.get().is_none() {
        return;
    }
    for signal in started_ignoring() {
        // SAFETY: ignoring a signal has no preconditions.
        unsafe { libc::signal(signal, libc::SIG_IGN) };
    }
    if !takes_signals {
        return;
    }
    // Blocked first: from here on the handler doesn't run on this thread.
    if let Err(err) = mask(libc::SIG_BLOCK) {
        log::warn!("a signal that comes as the daemon upgrades may go unheard: {err}");
        return;
    }
    // SAFETY: gettid has no preconditions.
    let thread = unsafe { libc::syscall(libc::SYS_gettid) };
    HOLDER.store(i32::try_from(thread).unwrap_or(0), Ordering::SeqCst);
    // Set after the holder, as the handler notes a signal before it looks
    // for one: a signal is either seen here or sent here, or both.
    for Signal(signal) in arrived(ARRIVED.swap(0, Ordering::SeqCst)) {
        send_here(signal);
    }
}

/// Undoes [`hold`], as the next build didn't start: the signals the
/// daemon was started ignoring are caught again, and those held back, if
/// the next build would have taken them, are let in, and any that came
/// are taken now, for the daemon to stop.
pub(crate) fn release(takes_signals: bool) {
    if READ.get().is_none() {
        return;
    }
    for signal in started_ignoring() {
        if let Err(err) = catch(signal) {
            log::error!("the programs the daemon starts may ignore signal {signal}: {err}");
        }
    }
    if takes_signals {
        HOLDER.store(0, Ordering::SeqCst);
        if let Err(err) = mask(libc::SIG_UNBLOCK) {
            log::error!("signals asking the daemon to stop can't reach it: {err}");
        }
    }
    // Starting a program puts SIGPIPE back as programs expect it, killing
    // the process for writing to a closed pipe. The daemon writes to
    // pipes whose readers may be gone, so it ignores it again, as Rust
    // programs start.
    // SAFETY: ignoring a signal has no preconditions.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
}

/// The handler: notes `signal`, then wakes the event loop, or, while the
/// daemon upgrades, sends it on to the thread holding signals back, unless
/// the daemon was started ignoring it. Only atomics and system calls,
/// which are safe here, and errno is left as it was.
extern "C" fn handle(signal: libc::c_int) {
    if IGNORED.load(Ordering::SeqCst) & bit(signal) != 0 {
        return;
    }
    // SAFETY: errno is this thread's.
    let errno = unsafe { *libc::__errno_location() };
    ARRIVED.fetch_or(bit(signal), Ordering::SeqCst);
    let holder = HOLDER.load(Ordering::SeqCst);
    if holder == 0 {
        // A full pipe has woken the loop already.
        // SAFETY: the fd is the pipe's write end, open for the life of the
        // process, and the byte outlives the call.
        unsafe { libc::write(WRITE.load(Ordering::SeqCst), [0u8].as_ptr().cast(), 1) };
    } else {
        // SAFETY: tgkill has no preconditions.
        unsafe { libc::syscall(libc::SYS_tgkill, libc::getpid(), holder, signal) };
    }
    // SAFETY: as above.
    unsafe { *libc::__errno_location() = errno };
}

/// Sends `signal` to this thread, which has it blocked, so it waits here.
fn send_here(signal: libc::c_int) {
    // SAFETY: tgkill and gettid have no preconditions.
    unsafe {
        let thread = libc::syscall(libc::SYS_gettid);
        libc::syscall(libc::SYS_tgkill, libc::getpid(), thread, signal);
    }
}

/// Blocks or unblocks, as `how` says, the signals that ask the daemon to
/// stop, on this thread.
fn mask(how: libc::c_int) -> io::Result<()> {
    let set = stopping_set();
    // SAFETY: `set` is a valid signal set, and the old one isn't wanted.
    match unsafe { libc::pthread_sigmask(how, &raw const set, std::ptr::null_mut()) } {
        0 => Ok(()),
        err => Err(io::Error::from_raw_os_error(err)),
    }
}

/// The set of the signals that ask the daemon to stop.
fn stopping_set() -> libc::sigset_t {
    // SAFETY: sigemptyset makes the zeroed set a valid empty one, and
    // each signal added is a valid signal.
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut set);
        for signal in STOPPING {
            libc::sigaddset(&raw mut set, signal);
        }
        set
    }
}

/// `signal`'s bit in [`ARRIVED`].
fn bit(signal: libc::c_int) -> u32 {
    u32::try_from(signal).map_or(0, |signal| 1 << signal)
}

/// The signals whose bits are set in `bits`.
fn arrived(bits: u32) -> Vec<Signal> {
    (STOPPING.into_iter())
        .filter(|&signal| bits & bit(signal) != 0)
        .map(Signal)
        .collect()
}
