//! Hearing from logind, systemd's login manager, that the machine is
//! about to shut down or sleep, in time to keep the workspaces as they
//! are (CLACKA-236).
//!
//! As the machine shuts down, systemd signals every program in the
//! daemon's scope (see [`crate::priority`]) at much the same moment, the
//! daemon's own leaf last: its shells end a few microseconds before it
//! hears it's to stop, and their panes would close, and their workspaces
//! with them, before it writes what it keeps. So the daemon asks logind
//! for a delay lock on shutting down and sleeping (`Inhibit`, mode
//! `delay`). Before either, logind says it's coming
//! (`PrepareForShutdown(true)`, `PrepareForSleep(true)`), then waits for
//! the lock to be let go, for up to `InhibitDelayMaxSec` (5 s as systemd
//! comes). The daemon writes the workspaces as they are, and lets the lock
//! go once they're on disk; before a shutdown, it then writes them no more
//! (see `server/shutdown.rs`), as what follows is everything ending. A
//! shutdown called off (`PrepareForShutdown(false)`), or a wake from
//! sleep, takes the lock again.
//!
//! It's all on a thread of its own: the loop hears only what came, with
//! the lock to let go, and is woken for it. Without a system bus or
//! logind, or a lock, it says why in one line of the log, and the daemon
//! goes on as before. The lock is closed as a program is `exec`'d, as
//! every file descriptor passed over D-Bus is here, so an upgrade takes it
//! afresh.

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use crate::dbus::{BUS, Body, Bus, Call, Kind, Message};

/// logind's name on the system bus, its object, and its interface.
const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// How long the bus, or logind, may take to answer.
const WAIT: Duration = Duration::from_secs(10);

/// What logind said is coming.
#[derive(Debug)]
pub(crate) enum News {
    /// The machine is about to shut down, with the lock, if there's one,
    /// to let go once the workspaces are written.
    ShuttingDown(Option<OwnedFd>),
    /// It isn't, after all.
    NotShuttingDown,
    /// The machine is about to sleep, with the lock, if there's one.
    Sleeping(Option<OwnedFd>),
}

/// The thread listening to logind, while there is one.
#[derive(Debug)]
pub(crate) struct Inhibitor {
    news: mpsc::Receiver<News>,
    shared: Arc<Shared>,
}

/// What the thread and the daemon share.
#[derive(Debug, Default)]
struct Shared {
    /// Set as the daemon lets the thread go: it ends without a word.
    stopped: AtomicBool,
    /// A copy of the thread's connection, to end it with, so the thread
    /// waiting on it ends too.
    stream: Mutex<Option<UnixStream>>,
}

impl Inhibitor {
    /// Starts the thread, asking logind on the system bus at `address`,
    /// and calling `wake` once there's news.
    pub(crate) fn start(address: String, wake: impl Fn() + Send + 'static) -> Self {
        let (send, news) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let theirs = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("inhibit".into())
            .spawn(move || {
                let tell = |news: News| {
                    if send.send(news).is_ok() {
                        wake();
                    }
                };
                if let Err(err) = listen(&address, &theirs, tell)
                    && !theirs.stopped.load(Ordering::SeqCst)
                {
                    log::info!("a shutdown doesn't wait for the workspaces to be kept: {err}");
                }
            });
        if let Err(err) = started {
            log::info!("a shutdown doesn't wait for the workspaces to be kept: no thread: {err}");
        }
        Self { news, shared }
    }

    /// What has come since this was last asked.
    pub(crate) fn news(&self) -> Vec<News> {
        self.news.try_iter().collect()
    }
}

impl Drop for Inhibitor {
    /// Ends the thread, letting go of the lock it holds.
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        let stream = self
            .shared
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(stream) = &*stream {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Takes logind's lock on the bus at `address`, and tells what logind
/// says is coming, until the connection ends: as the daemon lets the
/// thread go, or on an error, which it returns.
fn listen(address: &str, shared: &Shared, tell: impl Fn(News)) -> io::Result<()> {
    let mut bus = Bus::connect_passing_fds(address)?;
    {
        let mut stream = shared.stream.lock().unwrap_or_else(PoisonError::into_inner);
        if shared.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        *stream = Some(bus.reader()?);
    }
    let hello = bus.hello()?;
    bus.answer(hello, WAIT)?;
    // Only logind's own signals count: anyone could send one to the
    // daemon's connection.
    let asked = bus.get_name_owner(LOGIN)?;
    let mut owner = (bus.answer(asked, WAIT)?.args("s"))
        .and_then(|mut args| args.string().map(str::to_owned))
        .ok_or_else(|| unexpected("logind's name"))?;
    for member in ["PrepareForShutdown", "PrepareForSleep"] {
        bus.add_match(&format!(
            "type='signal',sender='{LOGIN}',path='{LOGIN_PATH}',interface='{MANAGER}',\
             member='{member}'"
        ))?;
    }
    // logind gets a new name each time it starts again, as an upgrade of
    // systemd restarts it.
    bus.add_match(&format!(
        "type='signal',sender='{BUS}',interface='{BUS}',member='NameOwnerChanged',arg0='{LOGIN}'"
    ))?;
    let mut asking = Some(inhibit(&mut bus)?);
    let mut lock: Option<OwnedFd> = None;
    loop {
        let (message, mut fds) = bus.receive()?;
        let Some(message) = message else {
            continue;
        };
        if asking.is_some() && message.reply_serial == asking {
            asking = None;
            lock = Some(locked(&message, &mut fds)?);
            continue;
        }
        if let Some(new) = new_owner(&message) {
            // Its locks are its to keep, or not: asked for again if this
            // one has none.
            owner = new;
            if !owner.is_empty() && lock.is_none() && asking.is_none() {
                asking = Some(inhibit(&mut bus)?);
            }
            continue;
        }
        if message.sender.as_deref() != Some(owner.as_str()) {
            continue;
        }
        let Some(coming) = coming(&message) else {
            continue;
        };
        match coming {
            Coming::Shutdown(true) => tell(News::ShuttingDown(lock.take())),
            Coming::Sleep(true) => tell(News::Sleeping(lock.take())),
            Coming::Shutdown(false) | Coming::Sleep(false) => {
                if matches!(coming, Coming::Shutdown(false)) {
                    tell(News::NotShuttingDown);
                }
                if lock.is_none() && asking.is_none() {
                    asking = Some(inhibit(&mut bus)?);
                }
            }
        }
    }
}

/// What logind's signal says is coming, or has gone by: `true` as it
/// comes, `false` once it has, or was called off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Coming {
    Shutdown(bool),
    Sleep(bool),
}

/// What `message` says is coming, if it's one of logind's signals.
fn coming(message: &Message) -> Option<Coming> {
    let coming = message.args("b")?.boolean()?;
    if message.is_signal(MANAGER, "PrepareForShutdown") {
        Some(Coming::Shutdown(coming))
    } else if message.is_signal(MANAGER, "PrepareForSleep") {
        Some(Coming::Sleep(coming))
    } else {
        None
    }
}

/// logind's new name on the bus, empty if it has gone, if `message` is
/// the bus saying so.
fn new_owner(message: &Message) -> Option<String> {
    if !message.is_signal(BUS, "NameOwnerChanged") || message.sender.as_deref() != Some(BUS) {
        return None;
    }
    let mut args = message.args("sss")?;
    let (name, _old, new) = (args.string()?, args.string()?, args.string()?);
    (name == LOGIN).then(|| new.to_owned())
}

/// Asks logind for its lock: its answer, which comes later, is the
/// call's number.
fn inhibit(bus: &mut Bus) -> io::Result<u32> {
    let call = Call {
        destination: LOGIN,
        path: LOGIN_PATH,
        interface: MANAGER,
        member: "Inhibit",
        signature: "ssss",
    };
    bus.call(&call, &inhibit_body(), false)
}

/// What asks for the lock: what it holds up, who holds it, why, and how.
fn inhibit_body() -> Body {
    let mut body = Body::default();
    body.string("shutdown:sleep");
    body.string("clackatoo");
    body.string("saving workspaces");
    body.string("delay");
    body
}

/// The lock logind's answer `message` passed, of those it came with,
/// `fds`, or why there isn't one.
fn locked(message: &Message, fds: &mut Vec<OwnedFd>) -> io::Result<OwnedFd> {
    if !message.is(Kind::Reply) {
        let said = (message.args("s"))
            .and_then(|mut args| args.string())
            .unwrap_or_default();
        let name = message.error.as_deref().unwrap_or("an error");
        return Err(io::Error::other(format!("logind: {name}: {said}")));
    }
    let index = (message.args("h"))
        .and_then(|mut args| args.u32())
        .ok_or_else(|| unexpected("logind's lock"))?;
    let index = usize::try_from(index).map_err(|_| unexpected("logind's lock"))?;
    if index >= fds.len() {
        return Err(unexpected("logind's lock"));
    }
    Ok(fds.swap_remove(index))
}

/// An answer that isn't what `what` should be.
fn unexpected(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{what} isn't what it should be"),
    )
}

#[cfg(test)]
mod tests;
