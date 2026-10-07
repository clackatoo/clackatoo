//! Starting and stopping the daemon from a client.
//!
//! Clients start the daemon when none is running, in the background, so
//! it outlives them. A daemon from an older build speaks an older protocol
//! and can't be asked to stop, so stopping finds its process through the
//! socket (the kernel says who is on the other end) and signals it, as
//! closing every terminal would.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};

use crate::client::{Client, ClientError};
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
    let not_running = |err: &std::io::Error| {
        matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    };
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

/// Stops the running daemon, whatever protocol it speaks, and waits for it
/// to exit. Its terminals close, and their programs are hung up. Returns
/// false if no daemon was running.
///
/// # Errors
///
/// If the daemon's process can't be found, isn't ours, or won't exit.
pub fn stop(runtime: &RuntimeDir) -> Result<bool, LaunchError> {
    let stream = match UnixStream::connect(runtime.socket_path()) {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(false);
        }
        Err(err) => return Err(err.into()),
    };
    let peer = rustix::net::sockopt::socket_peercred(&stream).map_err(std::io::Error::from)?;
    drop(stream);
    if peer.uid != rustix::process::getuid() {
        return Err(LaunchError::NotOurs);
    }
    signal_and_wait(runtime, peer.pid, Signal::TERM)
        .or_else(|_| signal_and_wait(runtime, peer.pid, Signal::KILL))?;
    Ok(true)
}

/// Sends `signal` to `pid`, then waits until the daemon's lock is free,
/// which happens when its process exits.
fn signal_and_wait(runtime: &RuntimeDir, pid: Pid, signal: Signal) -> Result<(), LaunchError> {
    match rustix::process::kill_process(pid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
        Err(err) => return Err(std::io::Error::from(err).into()),
    }
    let deadline = Instant::now() + DEADLINE / 2;
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
