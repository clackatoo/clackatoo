//! Starting, stopping and upgrading the daemon from a client.
//!
//! Clients start the daemon when none is running, in the background, so
//! it outlives them. A daemon from an older build speaks an older protocol.
//! From protocol 11 on, it can be asked to upgrade in place, keeping its
//! terminals (see [`upgrade`](crate::upgrade)). An older one can't be asked
//! anything, so stopping finds its process through the socket (the kernel
//! says who is on the other end) and signals it, as closing every terminal
//! would.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage, UPGRADABLE_SINCE};
use rustix::process::{Pid, Signal};

use crate::client::{self, Client, ClientError};
use crate::runtime::{LockError, RuntimeDir};

/// How long a daemon may take to start, or to stop.
const DEADLINE: Duration = Duration::from_secs(5);

/// Starting or stopping the daemon failed.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The runtime directory is unusable.
    #[error(transparent)]
    RuntimeDir(#[from] LockError),
    /// Connecting failed for a reason other than no daemon running: an
    /// older daemon, for one.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// The daemon was started but never answered.
    #[error("the daemon didn't start; see {}", .0.display())]
    DidNotStart(PathBuf),
    /// The process behind the socket belongs to someone else, which the
    /// runtime directory's permissions should make impossible.
    #[error("the daemon's socket is served by another user's process")]
    NotOurs,
    /// The daemon didn't exit when told to.
    #[error("the daemon didn't stop")]
    DidNotStop,
    /// No daemon is running.
    #[error("no daemon is running")]
    NotRunning,
    /// The daemon is from a build before daemons could upgrade in place.
    #[error(
        "the daemon speaks protocol {0}, from before daemons could upgrade in place; \
         `clackatoo stop` stops it, closing its terminals"
    )]
    CantUpgrade(u32),
    /// The daemon was asked to upgrade, and didn't.
    #[error("the daemon didn't upgrade: {0}")]
    NotUpgraded(String),
    /// A system call failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Connects to the daemon, starting one in the background if none is
/// running. It logs to `daemon.log` in the runtime directory.
///
/// # Errors
///
/// [`LaunchError::Client`] with [`ClientError::VersionMismatch`] if an
/// older daemon is running, which [`stop`] can end. Otherwise, if the
/// daemon can't be started or reached.
pub fn connect_or_start(runtime: &RuntimeDir) -> Result<Client, LaunchError> {
    Ok(Client::over(socket_or_start(runtime)?)?)
}

/// Connects to the daemon's socket, starting the daemon in the background
/// if none is running, without greeting it: whatever protocol it speaks.
///
/// # Errors
///
/// If the daemon can't be started or reached.
pub fn socket_or_start(runtime: &RuntimeDir) -> Result<UnixStream, LaunchError> {
    let socket = runtime.socket_path();
    match UnixStream::connect(&socket) {
        Ok(stream) => return Ok(stream),
        Err(err) if not_running(&err) => {}
        Err(err) => return Err(ClientError::Connect(err).into()),
    }

    runtime.create_private()?;
    let log_path = runtime.path().join("daemon.log");
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut daemon = std::process::Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        // A process group of its own, so it isn't hung up along with the
        // terminal it was started from.
        .process_group(0)
        .spawn()?;
    // Reaped if it exits while we run, as when another client's daemon
    // won the race to start.
    std::thread::spawn(move || daemon.wait());

    let deadline = Instant::now() + DEADLINE;
    loop {
        match UnixStream::connect(&socket) {
            Ok(stream) => return Ok(stream),
            Err(err) if not_running(&err) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(err) if not_running(&err) => return Err(LaunchError::DidNotStart(log_path)),
            Err(err) => return Err(ClientError::Connect(err).into()),
        }
    }
}

/// What [`replace`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Replaced {
    /// The daemon became this build, and its terminals carried on.
    Upgraded,
    /// The daemon was too old to upgrade, so it was stopped, closing its
    /// terminals. The next connection starts a new one.
    Stopped,
    /// No daemon was running.
    NotRunning,
}

/// Whether a daemon speaking `protocol` can upgrade in place to this build.
/// A daemon newer than this build won't go back to it.
#[must_use]
pub fn can_upgrade(protocol: u32) -> bool {
    (UPGRADABLE_SINCE..=PROTOCOL_VERSION).contains(&protocol)
}

/// The protocol the running daemon speaks, or `None` if none is running.
///
/// # Errors
///
/// If the daemon can't be reached, or doesn't answer.
pub fn daemon_protocol(runtime: &RuntimeDir) -> Result<Option<u32>, LaunchError> {
    let mut stream = match UnixStream::connect(runtime.socket_path()) {
        Ok(stream) => stream,
        Err(err) if not_running(&err) => return Ok(None),
        Err(err) => return Err(ClientError::Connect(err).into()),
    };
    stream.set_read_timeout(Some(DEADLINE))?;
    // A version past any daemon's: one that talks to older clients would
    // otherwise answer with this build's version, if it's older, not its
    // own. Every daemon answers this with its own.
    let hello = ClientMessage::Hello {
        protocol_version: u32::MAX,
    };
    stream.write_all(&frame::encode(PROTOCOL_VERSION, &hello).map_err(ClientError::from)?)?;
    match client::receive(&mut stream, &mut FrameDecoder::new(), PROTOCOL_VERSION)? {
        ServerMessage::Hello { protocol_version } => Ok(Some(protocol_version)),
        other => Err(ClientError::UnexpectedMessage(Box::new(other)).into()),
    }
}

/// Moves the running daemon onto this build, keeping its terminals and
/// their programs running, then connects to it. The daemon must speak a
/// protocol [`can_upgrade`] accepts; a daemon of this build's own protocol
/// upgrades too, onto this build's program.
///
/// # Errors
///
/// [`LaunchError::NotRunning`], [`LaunchError::CantUpgrade`] if the daemon
/// is too old or newer than this build, [`LaunchError::NotUpgraded`] if it
/// refused or failed (its log says why), or if it can't be reached.
pub fn upgrade(runtime: &RuntimeDir) -> Result<Client, LaunchError> {
    match daemon_protocol(runtime)? {
        None => return Err(LaunchError::NotRunning),
        Some(protocol) if !can_upgrade(protocol) => return Err(LaunchError::CantUpgrade(protocol)),
        Some(_) => {}
    }
    let socket = runtime.socket_path();
    let mut stream = UnixStream::connect(&socket).map_err(ClientError::Connect)?;
    stream.set_read_timeout(Some(DEADLINE))?;
    let executable = std::env::current_exe()?;
    let upgrade = ClientMessage::Upgrade {
        protocol_version: PROTOCOL_VERSION,
        executable: executable.as_os_str().as_bytes().to_vec(),
    };
    // Upgrade, and the answer, are the same in every version.
    stream.write_all(&frame::encode(PROTOCOL_VERSION, &upgrade).map_err(ClientError::from)?)?;
    // The daemon hangs up as it upgrades.
    match client::receive(&mut stream, &mut FrameDecoder::new(), PROTOCOL_VERSION) {
        Err(ClientError::Disconnected) => {}
        Ok(ServerMessage::NotUpgrading { reason }) => return Err(LaunchError::NotUpgraded(reason)),
        Ok(other) => return Err(ClientError::UnexpectedMessage(Box::new(other)).into()),
        Err(err) => return Err(err.into()),
    }
    // Connections wait in the socket's queue while the new build starts.
    // The old build may still talk to this one, at its older version.
    let still = |daemon| {
        LaunchError::NotUpgraded(format!(
            "it still speaks protocol {daemon}; {} says why",
            runtime.path().join("daemon.log").display()
        ))
    };
    match Client::connect(&socket) {
        Err(ClientError::VersionMismatch { daemon, .. }) if daemon != PROTOCOL_VERSION => {
            Err(still(daemon))
        }
        Ok(client) if client.wire() != PROTOCOL_VERSION => Err(still(client.wire())),
        result => Ok(result?),
    }
}

/// Replaces the running daemon with this build: upgrades it in place if it
/// can (see [`upgrade`]), keeping its terminals, or else stops it, closing
/// them, so the next connection starts a new one.
///
/// # Errors
///
/// If upgrading or stopping fails. A daemon that should have upgraded and
/// didn't isn't stopped instead.
pub fn replace(runtime: &RuntimeDir) -> Result<Replaced, LaunchError> {
    match daemon_protocol(runtime)? {
        None => Ok(Replaced::NotRunning),
        Some(protocol) if can_upgrade(protocol) => {
            upgrade(runtime)?;
            Ok(Replaced::Upgraded)
        }
        Some(_) => {
            stop(runtime)?;
            Ok(Replaced::Stopped)
        }
    }
}

/// Whether connecting failed because no daemon is listening.
fn not_running(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Stops the running daemon, whatever protocol it speaks, and waits for it
/// to exit. Its terminals close, and their programs are hung up. It's sent
/// SIGTERM, and stops as it always does, but is killed if it hasn't
/// exited within [`STOP_WAIT`]; a daemon from before daemons caught
/// SIGTERM simply exits. Returns false if no daemon was running.
///
/// # Errors
///
/// If the daemon's process can't be found, isn't ours, or won't exit.
pub fn stop(runtime: &RuntimeDir) -> Result<bool, LaunchError> {
    let stream = match UnixStream::connect(runtime.socket_path()) {
        Ok(stream) => stream,
        Err(err) if not_running(&err) => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    let peer = rustix::net::sockopt::socket_peercred(&stream).map_err(std::io::Error::from)?;
    drop(stream);
    if peer.uid != rustix::process::getuid() {
        return Err(LaunchError::NotOurs);
    }
    signal_and_wait(runtime, peer.pid, Signal::TERM, STOP_WAIT)
        .or_else(|_| signal_and_wait(runtime, peer.pid, Signal::KILL, DEADLINE / 2))?;
    Ok(true)
}

/// How long a daemon asked to stop has before it's killed: it stops what
/// it started, and writes what it keeps, first, in about 3 s at most.
const STOP_WAIT: Duration = Duration::from_secs(5);

/// Sends `signal` to `pid`, then waits up to `wait` until the daemon's
/// lock is free, which happens when its process exits.
fn signal_and_wait(
    runtime: &RuntimeDir,
    pid: Pid,
    signal: Signal,
    wait: Duration,
) -> Result<(), LaunchError> {
    match rustix::process::kill_process(pid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(err) => return Err(std::io::Error::from(err).into()),
    }
    let deadline = Instant::now() + wait;
    loop {
        match runtime.lock() {
            Ok(_released) => return Ok(()),
            Err(LockError::AlreadyRunning) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(LockError::AlreadyRunning) => return Err(LaunchError::DidNotStop),
            Err(err) => return Err(err.into()),
        }
    }
}
