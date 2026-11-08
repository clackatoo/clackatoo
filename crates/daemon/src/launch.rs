//! Starting, stopping and upgrading the daemon from a client.
//!
//! Clients start the daemon when none is running, in the background, so
//! it outlives them. A daemon from an older build speaks an older protocol.
//! From protocol 11 on, it can be asked to upgrade in place, keeping its
//! terminals (see [`upgrade`](crate::upgrade)). An older one can't be asked
//! anything, so stopping finds its process through the socket (the kernel
//! says who is on the other end) and signals it, as closing every terminal
//! would.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{ClientMessage, PROTOCOL_VERSION, ServerMessage, UPGRADABLE_SINCE};
use rustix::process::{Pid, Signal};

use crate::client::{self, Client, ClientError};
use crate::runtime::{LockError, RuntimeDir};

/// How long a daemon may take to start, or to stop.
const DEADLINE: Duration = Duration::from_secs(5);

/// Where a client came from, for the programs started on its behalf: a
/// window opened by a process of windows starts them as the `clackatoo`
/// that asked for it would have, with its environment and in its
/// directory, not the process's.
#[derive(Clone, Debug, Default)]
pub struct Origin {
    /// Its whole environment, in place of this process's.
    pub env: Option<Arc<[(OsString, OsString)]>>,
    /// Its working directory.
    pub dir: Option<PathBuf>,
}

impl Origin {
    /// Starts `command` as the origin would: with its environment, and in
    /// its directory, if that's still there.
    pub fn apply<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        if let Some(env) = &self.env {
            command
                .env_clear()
                .envs(env.iter().map(|(name, value)| (name, value)));
        }
        if let Some(dir) = self.dir.as_ref().filter(|dir| dir.is_dir()) {
            command.current_dir(dir);
        }
        command
    }

    /// The variable `name`, as the origin has it.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<OsString> {
        match &self.env {
            Some(env) => (env.iter())
                .find(|(each, _)| each == name)
                .map(|(_, value)| value.clone()),
            None => std::env::var_os(name),
        }
    }
}

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

/// Which side of [`fork_apart`] this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Forked {
    /// The process that forked, and its new child.
    Parent(rustix::process::Pid),
    /// The child, apart from the parent.
    Child,
}

/// Forks this process into a child apart from it: in a session of its own,
/// so with no terminal, and neither the parent's terminal, its signals nor
/// its end reach it; with no input or output (`/dev/null` for each); and in
/// `/`, so it keeps no directory in use. The child carries on as this
/// process would, with no second program to start, which is what makes it
/// quick.
///
/// Refused while this process has more than one thread: only the thread
/// that forks goes on in the child, and anything another held, a lock
/// say, would stay held there for ever.
///
/// # Errors
///
/// If there's more than one thread, or the system won't fork.
#[allow(unsafe_code)]
pub fn fork_apart() -> std::io::Result<Forked> {
    let threads = std::fs::read_dir("/proc/self/task")?.count();
    if threads != 1 {
        return Err(std::io::Error::other(format!(
            "not forking with {threads} threads running"
        )));
    }
    // SAFETY: with only this thread running, as just checked (nothing in
    // this process starts threads behind its back), the child is a whole
    // copy of the process: no lock or allocation is half done by another
    // thread. Only this thread can start one between the check and here.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => Err(std::io::Error::last_os_error()),
        0 => {
            rustix::process::setsid()?;
            std::env::set_current_dir("/")?;
            let null = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/null")?;
            rustix::stdio::dup2_stdin(&null)?;
            rustix::stdio::dup2_stdout(&null)?;
            rustix::stdio::dup2_stderr(&null)?;
            Ok(Forked::Child)
        }
        pid => Ok(Forked::Parent(
            rustix::process::Pid::from_raw(pid).ok_or_else(std::io::Error::last_os_error)?,
        )),
    }
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
    connect_or_start_from(runtime, &Origin::default())
}

/// As [`connect_or_start`], starting the daemon as `origin` would.
///
/// # Errors
///
/// As [`connect_or_start`].
pub fn connect_or_start_from(runtime: &RuntimeDir, origin: &Origin) -> Result<Client, LaunchError> {
    Ok(Client::over(socket_or_start_from(runtime, origin)?)?)
}

/// Connects to the daemon's socket, starting the daemon in the background
/// if none is running, without greeting it: whatever protocol it speaks.
///
/// # Errors
///
/// If the daemon can't be started or reached.
pub fn socket_or_start(runtime: &RuntimeDir) -> Result<UnixStream, LaunchError> {
    socket_or_start_from(runtime, &Origin::default())
}

/// As [`socket_or_start`], starting the daemon as `origin` would.
///
/// # Errors
///
/// As [`socket_or_start`].
fn socket_or_start_from(runtime: &RuntimeDir, origin: &Origin) -> Result<UnixStream, LaunchError> {
    let socket = runtime.socket_path();
    match client::connect_socket(&socket) {
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
    let mut daemon = Command::new(std::env::current_exe()?);
    let mut daemon = origin
        .apply(&mut daemon)
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
        match client::connect_socket(&socket) {
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
    let mut stream = match client::connect_socket(&runtime.socket_path()) {
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
    let mut stream = client::connect_socket(&socket).map_err(ClientError::Connect)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_with_other_threads_is_never_forked() {
        // As a test, with the harness's threads besides.
        let held = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(500)));
        let refused = fork_apart().unwrap_err();
        assert!(refused.to_string().contains("threads running"), "{refused}");
        held.join().unwrap();
    }

    /// Runs `sh` as `origin` would, saying what it sees.
    fn seen_by_sh(origin: &Origin) -> String {
        let mut command = Command::new("/bin/sh");
        let output = origin
            .apply(&mut command)
            .args(["-c", "printf '%s %s' \"${MARKER-unset}\" \"$(pwd -P)\""])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn what_a_client_starts_has_its_origins_environment_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        let origin = Origin {
            env: Some(vec![("MARKER".into(), "from-the-asker".into())].into()),
            dir: Some(dir.path().to_owned()),
        };
        let real = dir.path().canonicalize().unwrap();
        assert_eq!(
            seen_by_sh(&origin),
            format!("from-the-asker {}", real.display())
        );
        assert_eq!(origin.var("MARKER").unwrap(), "from-the-asker");
        assert_eq!(origin.var("HOME"), None);
        // A directory gone since is left alone, rather than fail the start.
        let gone = Origin {
            dir: Some(dir.path().join("gone")),
            ..origin
        };
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(
            seen_by_sh(&gone),
            format!("from-the-asker {}", here.display())
        );
        // No origin: this process's own.
        assert_eq!(Origin::default().var("PATH"), std::env::var_os("PATH"));
    }
}
