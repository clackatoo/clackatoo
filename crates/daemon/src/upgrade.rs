//! Upgrading the daemon in place: a newer build takes over its terminals,
//! and the programs in them carry on as if nothing happened.
//!
//! The daemon `exec`s the new build, replacing its own program but keeping
//! its process. That matters: the programs in its terminals are its
//! children, and only a parent can reap a child and learn its exit status.
//! A fresh process handed the PTYs could watch them, but never report that
//! a program exited with 3.
//!
//! Everything else goes across like this. The daemon writes its
//! [`State`](crate::handover::State) into a memfd, an anonymous file, then
//! sends that and every fd the state refers to (the listening socket, the
//! lock, each PTY and pidfd) over a socket pair to itself, as `SCM_RIGHTS`
//! messages. It execs the new build with the receiving end as its stdin
//! and `--resume`. Exec closes the daemon's own copies, which are all
//! close-on-exec, and the new build receives the copies in flight.
//! Receiving fds is safe Rust; inheriting them by number wouldn't be.
//!
//! The listening socket never closes, so clients that connect meanwhile
//! wait in its queue. Every connection does close, and clients reconnect.
//! If exec fails, the daemon still holds everything it sent, and carries
//! on from its own handover. A signal asking the daemon to stop, as it
//! upgrades, is held back for the new build, which stops once it's running
//! (see [`crate::signals`]).
//!
//! Once exec succeeds there's no way back, so first the daemon checks that
//! the new program is a build that can take over: it runs it with the
//! arguments the exec will use plus [`VERSIONS_FLAG`], which lists the
//! handover versions it reads. A build that wouldn't accept those
//! arguments fails that the same way.
//! Without that, an older build, or any other program, would take the
//! daemon's place and lose every terminal. (Even a file that isn't a
//! program would: exec runs those with `/bin/sh`.)

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clackatoo_protocol::frame;
use clackatoo_protocol::{PROTOCOL_VERSION, ServerMessage};
use rustix::fs::MemfdFlags;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
};

use crate::handover::{Fds, Handover, HandoverError, State};
use crate::runtime::{DaemonLock, LockError, RuntimeDir};
use crate::{Daemon, DaemonConfig};

/// The most fds one message can carry: the kernel's `SCM_MAX_FD`.
const FDS_PER_MESSAGE: usize = 253;

/// The flag that tells a new build it is taking over: see the module docs.
pub const RESUME_FLAG: &str = "--resume";

/// The flag that asks a build which handovers it can take over from,
/// instead of running the daemon. It prints [`versions_answer`]: the
/// [`readable_versions`], then [`TAKES_SIGNALS`], separated by spaces.
pub const VERSIONS_FLAG: &str = "--handover-versions";

/// The word a build adds to its answer to [`VERSIONS_FLAG`] when it takes
/// the signals the build before held back for it as it upgraded (see
/// `crate::signals`). A build from before then would leave them blocked
/// for good, and so would every program it started, so they're held back
/// only for a build that says this. Builds reading the answer skip words
/// that aren't numbers.
pub const TAKES_SIGNALS: &str = "signals";

/// The word a build adds to its answer to [`VERSIONS_FLAG`] when it takes
/// agents' processes over as they are, rather than have them stopped
/// (CLACKA-188): their pipes, what they were doing, and all they run. The
/// build upgrading decides by it before it stops anything; a build that
/// doesn't say it, as one from before, or one told not to (see
/// `AgentConfig::kept_through_upgrades`), has every agent stopped first,
/// as before. It's followed by a word for each adapter the build pins,
/// [`ADAPTER_PIN`]`NAME=VERSION`: an agent run from an adapter pinned at
/// another version than this build's is stopped too, as the new build
/// would start another.
pub const TAKES_ADAPTERS: &str = "adapters";

/// How a word naming an adapter a build pins starts (see
/// [`TAKES_ADAPTERS`]).
pub const ADAPTER_PIN: &str = "adapter:";

/// How a word naming what a build declares to agents starts: one for each
/// kind of declaration, `client:KIND=HASH` (see
/// `agent::declaration_kind` and `agent::declaration_hash`). An agent was
/// told what it can rely on as it started; one that goes on is carried
/// only into a build that would tell it the same, or that build would
/// count on what it was never told.
pub const CLIENT_DECLARES: &str = "client:";

/// How long a build may take to answer [`VERSIONS_FLAG`].
const VERSIONS_TIMEOUT: Duration = Duration::from_secs(2);

/// The handover versions this build can take over from.
#[must_use]
pub fn readable_versions() -> &'static [u32] {
    crate::handover::READABLE
}

/// What this build answers [`VERSIONS_FLAG`] with: its
/// [`readable_versions`], [`TAKES_SIGNALS`], and, unless agents are to be
/// stopped as it upgrades, [`TAKES_ADAPTERS`], the adapters it pins and
/// what it declares to agents.
#[must_use]
pub fn versions_answer() -> String {
    let agents = crate::agents::AgentConfig {
        file: crate::config_dir().map(|dir| dir.join("daemon/agents.toml")),
        restart_on_upgrade: crate::restart_agents_on_upgrade(),
        ..crate::agents::AgentConfig::default()
    };
    let adapters = agents.kept_through_upgrades();
    let pins = (crate::agents::KNOWN.iter())
        .map(|known| format!("{ADAPTER_PIN}{}={}", known.name, known.version));
    let declares = [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .map(|(files, tasks)| {
            let declared = crate::agent::client_capabilities(files, tasks);
            format!(
                "{CLIENT_DECLARES}{}={}",
                crate::agent::declaration_kind(files, tasks),
                crate::agent::declaration_hash(&declared)
            )
        });
    (readable_versions().iter())
        .map(u32::to_string)
        .chain([TAKES_SIGNALS.to_owned()])
        .chain(adapters.then(|| TAKES_ADAPTERS.to_owned()))
        .chain(pins.chain(declares).filter(|_| adapters))
        .collect::<Vec<String>>()
        .join(" ")
}

/// What a build that can take over from this one takes beside the
/// handover, as it answered [`VERSIONS_FLAG`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Takes {
    /// It takes the signals held back for it (see [`TAKES_SIGNALS`]).
    pub(crate) signals: bool,
    /// It takes agents' processes as they are (see [`TAKES_ADAPTERS`]):
    /// if so, the adapters it pins, by name, at their versions.
    pub(crate) adapters: Option<Vec<(String, String)>>,
    /// What it declares to agents, by the kind of declaration (see
    /// [`CLIENT_DECLARES`]).
    pub(crate) declares: Vec<(String, String)>,
}

impl Takes {
    /// What `answer`, a build's answer to [`VERSIONS_FLAG`], says it takes.
    fn from_answer(answer: &str) -> Self {
        let words = || answer.split_whitespace();
        let pairs = |prefix: &'static str| {
            words()
                .filter_map(move |word| word.strip_prefix(prefix)?.split_once('='))
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect::<Vec<_>>()
        };
        Self {
            signals: words().any(|word| word == TAKES_SIGNALS),
            adapters: words()
                .any(|word| word == TAKES_ADAPTERS)
                .then(|| pairs(ADAPTER_PIN)),
            declares: pairs(CLIENT_DECLARES),
        }
    }

    /// Whether it takes over the agent called `name`, run from the adapter
    /// this build pins at `version` if it's one this build knows, `None`
    /// if it's the user's own, and told `declared`, a declaration's kind
    /// and hash, as it started: it takes agents' processes, pins that
    /// adapter at the same version, and declares the same.
    pub(crate) fn takes_agent(
        &self,
        name: &str,
        version: Option<&str>,
        (kind, hash): (&str, &str),
    ) -> bool {
        let Some(pins) = &self.adapters else {
            return false;
        };
        let pinned = version.is_none_or(|version| {
            (pins.iter()).any(|(pinned, at)| pinned == name && at == version)
        });
        let declared = (self.declares.iter()).any(|(each, at)| each == kind && at == hash);
        pinned && declared
    }
}

/// Checks that `executable`, run with `args`, is a build that can take
/// over from this one, by asking it which handover versions it reads.
/// Returns what else it takes (see [`Takes`]).
pub(crate) fn check_takes_over(executable: &Path, args: &[OsString]) -> Result<Takes, String> {
    let run = |err: io::Error| format!("{} can't be run: {err}", executable.display());
    let mut child = Command::new(executable)
        .args(args)
        .arg(VERSIONS_FLAG)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(run)?;
    let deadline = std::time::Instant::now() + VERSIONS_TIMEOUT;
    while child.try_wait().map_err(run)?.is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{} didn't answer", executable.display()));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut answer = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = io::Read::read_to_string(&mut stdout, &mut answer);
    }
    let takes_over = answer
        .split_whitespace()
        .any(|version| version.parse() == Ok(crate::handover::VERSION));
    if takes_over {
        Ok(Takes::from_answer(&answer))
    } else {
        Err(format!(
            "{} can't take over from this build: it isn't a build of clackatoo that reads \
             its handover (version {})",
            executable.display(),
            crate::handover::VERSION
        ))
    }
}

/// A request to upgrade, which [`Daemon::run`] stopped for. The daemon has
/// handed over everything; [`serve`] does the rest.
#[derive(Debug)]
pub struct Upgrade {
    pub(crate) executable: PathBuf,
    /// Whether the build takes the signals held back for it (see
    /// [`TAKES_SIGNALS`]).
    pub(crate) takes_signals: bool,
    pub(crate) handover: Handover,
    /// The connection that asked, which is told if the upgrade fails. The
    /// exec closes it if it doesn't.
    pub(crate) requester: Option<mio::net::UnixStream>,
}

/// The daemon couldn't run.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The runtime directory is unusable, or another daemon has it.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Listening or waiting for events failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Runs the daemon for `runtime` until it is shut down, upgrading in place
/// when a newer client asks. `args` are the arguments the program was run
/// with, minus the program itself and [`RESUME_FLAG`]: an upgrade runs the
/// new build with the same ones.
///
/// With `resume`, this process was exec'd by an older build, and its
/// handover is on stdin. If it can't be read, the terminals are lost (their
/// programs are hung up), and the daemon starts afresh. A daemon starting
/// afresh makes the workspaces kept from its last run again, with
/// `restore` (see [`Daemon::restore_workspaces`]).
///
/// # Errors
///
/// If another daemon holds the runtime directory, or the daemon can't
/// listen or wait for events.
pub fn serve(
    runtime: &RuntimeDir,
    config: &DaemonConfig,
    resume: bool,
    restore: bool,
    args: &[OsString],
) -> Result<(), ServeError> {
    // First of all, before any thread starts: a signal held back by the
    // build before, as it upgraded, is taken now, and the daemon stops as
    // soon as it's running.
    crate::signals::install()?;
    serve_signalled(runtime, config, resume, restore, args)
}

/// For tests: runs the daemon as [`serve`] does, but as a build from
/// before daemons caught signals did, catching none, to try upgrading to
/// one.
///
/// # Errors
///
/// As [`serve`].
#[doc(hidden)]
pub fn serve_catching_no_signals(
    runtime: &RuntimeDir,
    config: &DaemonConfig,
    resume: bool,
    restore: bool,
    args: &[OsString],
) -> Result<(), ServeError> {
    serve_signalled(runtime, config, resume, restore, args)
}

/// Runs the daemon as [`serve`] does, once the signals asking it to stop
/// are caught, if they're to be.
fn serve_signalled(
    runtime: &RuntimeDir,
    config: &DaemonConfig,
    resume: bool,
    restore: bool,
    args: &[OsString],
) -> Result<(), ServeError> {
    let resumed = if resume {
        receive()
            .and_then(|handover| resume_from(handover, config))
            .map_err(|err| log::error!("taking over from the last build failed: {err}"))
            .ok()
    } else {
        None
    };
    let afresh = resumed.is_none();
    let (lock, mut daemon) = if let Some(resumed) = resumed {
        resumed
    } else {
        let lock = runtime.lock()?;
        (lock, Daemon::bind(&runtime.socket_path(), config.clone())?)
    };
    daemon.set_upgrade_args(args);
    // Once the lock is held: the directory is ours.
    daemon.install_shell_integration(runtime.path().join("shell"), afresh);
    // Once the shells it starts are marked: an upgrade carried everything
    // over itself.
    if afresh && restore {
        daemon.restore_workspaces();
    }

    while let Some(Upgrade {
        executable,
        takes_signals,
        mut handover,
        requester,
    }) = daemon.run()?
    {
        handover.state.lock = Some(handover.fds.add(lock.as_fd().try_clone_to_owned()?));
        // Closes every connection, including the one that asked.
        drop(daemon);
        log::info!("upgrading to {}", executable.display());
        let err = exec(&executable, args, &handover, takes_signals);
        log::error!("upgrading to {} failed: {err}", executable.display());
        if let Some(requester) = requester {
            tell_failure(requester, &err);
        }
        handover.state.lock = None;
        daemon = Daemon::resume(handover, config.clone()).map_err(io::Error::other)?;
        daemon.set_upgrade_args(args);
        daemon.install_shell_integration(runtime.path().join("shell"), false);
    }
    Ok(())
}

/// Tells the client that asked for an upgrade why it failed.
fn tell_failure(requester: mio::net::UnixStream, err: &io::Error) {
    let mut stream = std::os::unix::net::UnixStream::from(OwnedFd::from(requester));
    let refusal = ServerMessage::NotUpgrading {
        reason: format!("starting the new build failed: {err}"),
    };
    let sent = stream
        .set_nonblocking(false)
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_secs(1))))
        // The refusal is the same in every version.
        .and_then(|()| frame::encode(PROTOCOL_VERSION, &refusal).map_err(io::Error::other))
        .and_then(|bytes| stream.write_all(&bytes));
    if let Err(err) = sent {
        log::debug!("telling the client failed: {err}");
    }
}

/// The lock and the daemon from `handover`.
fn resume_from(
    mut handover: Handover,
    config: &DaemonConfig,
) -> Result<(DaemonLock, Daemon), HandoverError> {
    let lock = handover
        .state
        .lock
        .take()
        .ok_or(HandoverError::Damaged("the lock is missing"))?;
    let lock = DaemonLock::from_fd(handover.fds.take(lock)?);
    Ok((lock, Daemon::resume(handover, config.clone())?))
}

/// Execs `executable` with `args` and [`RESUME_FLAG`], handing it
/// `handover`, and holding back signals for it if it `takes_signals`.
/// Returns only if that fails.
fn exec(
    executable: &Path,
    args: &[OsString],
    handover: &Handover,
    takes_signals: bool,
) -> io::Error {
    // Returns both ends of the socket pair. Ours must stay open until the
    // exec closes it, which ends the stream for the new build; if the exec
    // fails, closing it drops the fds in flight.
    let send = || -> io::Result<(OwnedFd, OwnedFd)> {
        let mut memfd = File::from(rustix::fs::memfd_create(
            "clackatoo-handover",
            MemfdFlags::CLOEXEC,
        )?);
        handover.state.write(&mut memfd)?;
        let (ours, theirs) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        // Nothing reads until the exec, so a full buffer must fail rather
        // than wait forever.
        rustix::io::ioctl_fionbio(&ours, true)?;
        let fds: Vec<BorrowedFd<'_>> = std::iter::once(memfd.as_fd())
            .chain(handover.fds.iter().map(AsFd::as_fd))
            .collect();
        for chunk in fds.chunks(FDS_PER_MESSAGE) {
            let mut space =
                vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(chunk.len()))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            control.push(SendAncillaryMessage::ScmRights(chunk));
            rustix::net::sendmsg(
                &ours,
                &[IoSlice::new(&[0])],
                &mut control,
                SendFlags::empty(),
            )?;
        }
        Ok((ours, theirs))
    };
    match send() {
        Ok((_ours, theirs)) => {
            // A build that wouldn't take a signal asking the daemon to stop,
            // which came as it handed over, isn't started: the daemon
            // carries on from its own handover, and stops.
            if !takes_signals && crate::signals::asked_to_stop() {
                return io::Error::other(
                    "the daemon was asked to stop as it upgraded, and the new build can't be \
                     told",
                );
            }
            // A signal asking the daemon to stop, from now until the next
            // build can take it, waits for it, if the next build takes it.
            crate::signals::hold(takes_signals);
            let err = Command::new(executable)
                .args(args)
                .arg(RESUME_FLAG)
                .stdin(Stdio::from(theirs))
                .exec();
            crate::signals::release(takes_signals);
            err
        }
        Err(err) => err,
    }
}

/// Receives the handover the last build sent on stdin (see the module
/// docs), then points stdin at `/dev/null` again.
fn receive() -> Result<Handover, HandoverError> {
    let fds = receive_fds();
    // Whatever happened: anything left in flight on the old stdin, such as
    // the lock, is dropped with it.
    rustix::stdio::dup2_stdin(File::open("/dev/null")?).map_err(io::Error::from)?;

    let mut fds = fds?.into_iter();
    let mut memfd = File::from(fds.next().ok_or(HandoverError::NotAHandover)?);
    let state = State::read(&mut memfd)?;
    Ok(Handover {
        state,
        fds: Fds::from_received(fds.collect()),
    })
}

/// Every fd the last build sent on stdin, in order.
fn receive_fds() -> Result<Vec<OwnedFd>, HandoverError> {
    let stdin = io::stdin();
    let mut fds = Vec::new();
    loop {
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(FDS_PER_MESSAGE))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let mut byte = [0];
        let received = rustix::net::recvmsg(
            stdin.as_fd(),
            &mut [IoSliceMut::new(&mut byte)],
            &mut control,
            RecvFlags::CMSG_CLOEXEC,
        )
        .map_err(io::Error::from)?;
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = message {
                fds.extend(received);
            }
        }
        if received.flags.contains(ReturnFlags::CTRUNC) {
            return Err(HandoverError::Damaged("fds were cut off"));
        }
        if received.bytes == 0 {
            return Ok(fds);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a build takes is read from its answer, words this build doesn't
    /// know skipped: agents only from one that says so, and each at the
    /// version it pins (CLACKA-188).
    #[test]
    fn what_a_build_takes_is_read_from_its_answer() {
        let told = ("ft", "0123");
        let before = Takes::from_answer("36 37 signals");
        assert_eq!(
            before,
            Takes {
                signals: true,
                adapters: None,
                declares: Vec::new(),
            }
        );
        assert!(!before.takes_agent("claude", Some("0.98.0"), told));
        assert!(!before.takes_agent("mine", None, told));

        let now = Takes::from_answer(
            "37 38 signals adapters adapter:claude=0.98.0 client:ft=0123 client:f-=4567 later",
        );
        assert!(now.takes_agent("claude", Some("0.98.0"), told));
        assert!(
            !now.takes_agent("claude", Some("0.99.0"), told),
            "pinned otherwise"
        );
        assert!(
            !now.takes_agent("codex", Some("2.2.2"), told),
            "pinned by no one"
        );
        assert!(
            now.takes_agent("mine", None, told),
            "the user's own command"
        );
        assert!(now.takes_agent("mine", None, ("f-", "4567")));
        // Told otherwise as it started, by kind or by what was declared.
        assert!(!now.takes_agent("mine", None, ("ft", "4567")));
        assert!(!now.takes_agent("claude", Some("0.98.0"), ("-t", "0123")));
        assert!(!Takes::from_answer("38").signals);
    }

    /// This build says it takes agents over, at the versions it pins,
    /// unless it's told to stop them as it upgrades.
    #[test]
    fn this_build_says_it_takes_agents_at_its_pins() {
        let takes = Takes::from_answer(&versions_answer());
        let claude = crate::agents::known("claude").unwrap();
        // The environment of the tests doesn't stop them.
        if !crate::restart_agents_on_upgrade() {
            let declared = crate::agent::client_capabilities(true, true);
            let told = ("ft", crate::agent::declaration_hash(&declared));
            assert!(takes.takes_agent("claude", Some(claude.version), (told.0, &told.1)));
        }
        assert!(takes.signals);
    }
}
