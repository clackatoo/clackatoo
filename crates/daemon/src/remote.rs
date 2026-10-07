//! Reaching a daemon on another machine, over SSH.
//!
//! A client runs `ssh HOST clackatoo bridge`. On the far end, the bridge
//! starts the daemon if it isn't running, then relays bytes between its own
//! input and output and the daemon's socket. The protocol runs end to end,
//! with SSH logging in and encrypting, so clackatoo has no network code or
//! cryptography of its own. The bridge doesn't speak the protocol, so the
//! version check is between the client and the remote daemon, whichever
//! build of clackatoo runs the bridge.
//!
//! A shell can print something before the bridge starts, such as a
//! greeting from a startup file, so the bridge announces itself first, and
//! the client skips everything before the announcement.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::client::{Client, ClientError};
use crate::launch::{self, LaunchError};
use crate::runtime::RuntimeDir;

/// What the bridge sends before relaying, unlikely to begin anything a
/// shell prints.
const ANNOUNCEMENT: &[u8] = b"\x1b[clackatoo-bridge]\n";

/// The most a shell may print before the bridge announces itself.
const MAX_GREETING: usize = 64 * 1024;

/// How long ssh may take to log in and start the bridge.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);

/// The most of ssh's error output kept, to explain a failure.
const MAX_SSH_ERRORS: usize = 4 * 1024;

/// A daemon on another machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    /// The machine, as ssh names it: `host`, `user@host`, or a name from
    /// `~/.ssh/config`.
    pub host: String,
    /// The shell command that runs clackatoo there.
    pub command: String,
}

/// Reaching a remote daemon failed.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// ssh itself couldn't run.
    #[error("couldn't run {program}")]
    Spawn {
        /// The program run as ssh.
        program: String,
        /// Why it couldn't.
        #[source]
        source: io::Error,
    },
    /// ssh ran, but clackatoo never answered through it: the host is
    /// unreachable, logging in failed, or clackatoo isn't installed there.
    #[error("couldn't reach clackatoo on {host}: {reason}")]
    Unreachable {
        /// The machine.
        host: String,
        /// What ssh, or the remote shell, said.
        reason: String,
    },
    /// The daemon answered, but the connection failed: it speaks another
    /// protocol version, for one.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// A system call failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The program run as ssh: `$CLACKATOO_SSH`, or `ssh`.
fn ssh_program() -> String {
    std::env::var("CLACKATOO_SSH")
        .ok()
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// Connects to the daemon on `remote`'s machine, starting it there if it
/// isn't running. With `replace`, a daemon already running there is
/// stopped first, closing its terminals.
///
/// # Errors
///
/// [`RemoteError::Client`] with [`ClientError::VersionMismatch`] if the
/// daemon there speaks another protocol version. Otherwise, if ssh can't
/// run, or can't reach clackatoo.
pub fn connect(remote: &Remote, replace: bool) -> Result<Client, RemoteError> {
    let mut ssh = Command::new(ssh_program());
    ssh.args([
        "-T",
        "-o",
        "ConnectTimeout=10",
        // A dead connection is noticed within 15 seconds.
        "-o",
        "ServerAliveInterval=5",
        "-o",
        "ServerAliveCountMax=3",
        // Ends the options, so a host can't be taken for one.
        "--",
        &remote.host,
    ])
    .arg(format!(
        "{} bridge{}",
        remote.command,
        if replace { " --replace" } else { "" }
    ));
    through(ssh, &remote.host)
}

/// Connects to a daemon through `command`, which runs `clackatoo bridge`
/// somewhere and relays its input and output, as ssh does. `host` names
/// where, in errors.
///
/// # Errors
///
/// As [`connect`].
pub fn through(mut command: Command, host: &str) -> Result<Client, RemoteError> {
    // A socket rather than pipes, so reads can time out.
    let (stream, theirs) = UnixStream::pair()?;
    command
        .stdin(Stdio::from(OwnedFd::from(theirs.try_clone()?)))
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|source| RemoteError::Spawn {
        program: command.get_program().to_string_lossy().into_owned(),
        source,
    })?;
    // The command holds ssh's end of the socket, which must close here for
    // ssh exiting to be noticed.
    drop(command);
    let mut ssh = Ssh::new(child);

    let unreachable = |ssh: Ssh, greeting: &[u8], reason: &str| {
        let said = ssh.finish();
        let greeting = String::from_utf8_lossy(greeting);
        let reason = [said.trim(), greeting.trim(), reason]
            .into_iter()
            .find(|text| !text.is_empty())
            .unwrap_or_default()
            .to_owned();
        RemoteError::Unreachable {
            host: host.to_owned(),
            reason,
        }
    };
    stream.set_read_timeout(Some(LOGIN_TIMEOUT))?;
    let mut greeting = Vec::new();
    let mut byte = [0];
    while !greeting.ends_with(ANNOUNCEMENT) {
        match (&stream).read(&mut byte) {
            Ok(0) => return Err(unreachable(ssh, &greeting, "ssh closed the connection")),
            Ok(_) if greeting.len() == MAX_GREETING => {
                return Err(unreachable(
                    ssh,
                    &[],
                    "something other than clackatoo answered",
                ));
            }
            Ok(_) => greeting.push(byte[0]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                ssh.kill();
                return Err(unreachable(ssh, &greeting, "ssh didn't log in in time"));
            }
            Err(err) => return Err(err.into()),
        }
    }
    stream.set_read_timeout(None)?;
    Ok(Client::over(stream)?.carried_by(ssh))
}

/// Relays between this process's input and output and the daemon, which
/// is started if it isn't running, until either end hangs up. With
/// `replace`, a daemon already running is stopped first.
///
/// # Errors
///
/// If the daemon can't be stopped, started or reached.
pub fn bridge(runtime: &RuntimeDir, replace: bool) -> Result<(), LaunchError> {
    if replace {
        launch::stop(runtime)?;
    }
    let daemon = launch::socket_or_start(runtime)?;
    // The raw descriptors: the standard library's stdout holds bytes back
    // until a newline.
    let mut input = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut output = File::from(io::stdout().as_fd().try_clone_to_owned()?);
    output.write_all(ANNOUNCEMENT)?;
    let mut to_daemon = daemon.try_clone()?;
    std::thread::Builder::new()
        .name("bridge-input".into())
        .spawn(move || {
            // The client hung up: so does the daemon's side, which ends
            // the relay the other way.
            let _ = relay(&mut input, &mut to_daemon);
            let _ = to_daemon.shutdown(Shutdown::Write);
        })?;
    relay(&mut &daemon, &mut output)?;
    Ok(())
}

/// Copies everything from `from` to `to`, each piece as soon as it
/// arrives. Not `io::copy`, which between a pipe and a socket moves data
/// with `splice`, and that holds the pipe locked while it waits for the
/// socket: the bridge, with pipes from sshd, would stall.
fn relay(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => to.write_all(&buffer[..n])?,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// The ssh process carrying a connection to a remote daemon. Ended when
/// dropped.
#[derive(Debug)]
pub(crate) struct Ssh {
    child: Child,
    /// Collects what ssh prints to stderr.
    errors: Option<JoinHandle<String>>,
}

impl Ssh {
    fn new(mut child: Child) -> Self {
        let errors = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut said = Vec::new();
                let mut buffer = [0; 1024];
                while let Ok(n @ 1..) = stderr.read(&mut buffer) {
                    said.extend_from_slice(&buffer[..n]);
                    // Keep the end, where the reason is.
                    let excess = said.len().saturating_sub(MAX_SSH_ERRORS);
                    said.drain(..excess);
                }
                String::from_utf8_lossy(&said).into_owned()
            })
        });
        Self { child, errors }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Waits for ssh to exit, and returns what it printed to stderr.
    fn finish(mut self) -> String {
        let _ = self.child.wait();
        self.errors
            .take()
            .and_then(|errors| errors.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Ssh {
    fn drop(&mut self) {
        self.kill();
        let _ = self.child.wait();
    }
}
