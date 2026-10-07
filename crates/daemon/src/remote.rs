//! Reaching a daemon on another machine, over SSH.
//!
//! A client runs `ssh HOST clackatoo bridge`. On the far end, the bridge
//! starts the daemon if it isn't running, then relays bytes between its own
//! input and output and the daemon's socket. The client greets the daemon
//! through it, then turns the connection into a carrier of QUIC packets
//! and talks over QUIC inside it (see [`Client::over_quic`]). SSH logs in, and is how
//! each end knows the other. The bridge doesn't speak the protocol, so the
//! version check is between the client and the remote daemon, whichever
//! build of clackatoo runs the bridge.
//!
//! A shell can print something before the bridge starts, such as a
//! greeting from a startup file, so the bridge announces itself first, and
//! the client skips everything before the announcement.
//!
//! ssh asks for passwords on the terminal it was started from, which a
//! window may not have. A window serves an [`Askpass`] instead: ssh runs
//! clackatoo as its askpass program (`SSH_ASKPASS`), which passes each
//! question to the window over a socket, and the window's answer back.

use std::fs::File;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How long ssh may take to log in and start the bridge, which includes
/// someone typing a password.
const LOGIN_TIMEOUT: Duration = Duration::from_mins(2);

/// The environment variable that tells clackatoo it was run by ssh as its
/// askpass program, naming the socket to ask through.
pub const ASKPASS_VAR: &str = "CLACKATOO_ASKPASS";

/// The longest question passed on from ssh.
const MAX_QUESTION: u64 = 4 * 1024;

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
/// stopped first, closing its terminals. With `askpass`, the socket of an
/// [`Askpass`], ssh asks for passwords through it rather than on the
/// terminal.
///
/// # Errors
///
/// [`RemoteError::Client`] with [`ClientError::VersionMismatch`] if the
/// daemon there speaks another protocol version. Otherwise, if ssh can't
/// run, or can't reach clackatoo.
pub fn connect(
    remote: &Remote,
    replace: bool,
    askpass: Option<&Path>,
) -> Result<Client, RemoteError> {
    let mut ssh = Command::new(ssh_program());
    if let Some(socket) = askpass {
        ssh.env("SSH_ASKPASS", std::env::current_exe()?)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_VAR, socket);
    }
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
    Ok(Client::over_carrier(stream, Some(ssh))?)
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

/// Somewhere for ssh to ask questions, such as for a password, and get the
/// answers from `answer`. Each question arrives on a connection to its
/// socket, from clackatoo run by ssh as its askpass program (see [`ask`]).
/// The socket is removed when this is dropped.
///
/// On the socket, the asker sends the question and shuts its side down;
/// the answer comes back as `y` and the answer's text, or nothing at all
/// when the question was turned down.
#[derive(Debug)]
pub struct Askpass {
    path: PathBuf,
    stopped: Arc<AtomicBool>,
}

impl Askpass {
    /// Starts answering questions on a socket in `runtime`, on a thread of
    /// its own, one question at a time. `answer` gets the question and
    /// returns the answer, or `None` to turn it down.
    ///
    /// # Errors
    ///
    /// If the runtime directory or the socket can't be made.
    pub fn serve(
        runtime: &RuntimeDir,
        answer: impl Fn(String) -> Option<String> + Send + 'static,
    ) -> io::Result<Self> {
        runtime.create_private().map_err(io::Error::other)?;
        let path = runtime
            .path()
            .join(format!("askpass-{}.sock", std::process::id()));
        // Left by an earlier process with the same ID that didn't exit
        // cleanly.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(&path)?;
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        std::thread::Builder::new()
            .name("askpass".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    if let Ok(stream) = stream {
                        // A broken asker is ssh's problem, not ours.
                        let _ = answer_one(stream, &answer);
                    }
                }
            })?;
        Ok(Self { path, stopped })
    }

    /// The socket, for [`connect`].
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Wakes the thread, which sees it should stop.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Reads one question from `stream` and writes back what `answer` makes of
/// it.
fn answer_one(
    mut stream: UnixStream,
    answer: &impl Fn(String) -> Option<String>,
) -> io::Result<()> {
    let mut question = String::new();
    (&mut stream)
        .take(MAX_QUESTION)
        .read_to_string(&mut question)?;
    if let Some(text) = answer(question) {
        stream.write_all(b"y")?;
        stream.write_all(text.as_bytes())?;
    }
    Ok(())
}

/// Asks the [`Askpass`] at `socket` the `question` ssh asked, and prints
/// the answer for ssh, as its askpass program. Fails if the question is
/// turned down or can't be asked, so ssh gives up.
#[must_use]
pub fn ask(socket: &Path, question: &str) -> ExitCode {
    match asked(socket, question) {
        Ok(Some(answer)) => {
            let mut out = io::stdout().lock();
            if writeln!(out, "{answer}").and_then(|()| out.flush()).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Ok(None) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("clackatoo: couldn't ask the window: {err}");
            ExitCode::FAILURE
        }
    }
}

/// The answer to `question` from the [`Askpass`] at `socket`, or `None` if
/// it was turned down.
fn asked(socket: &Path, question: &str) -> io::Result<Option<String>> {
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(question.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    Ok(reply.strip_prefix('y').map(str::to_owned))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_reach_the_askpass_and_answers_come_back() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeDir::at(temp.path().join("clackatoo"));
        let askpass = Askpass::serve(&runtime, |question| {
            (question == "Password: ").then(|| "hunter2".to_owned())
        })
        .unwrap();
        assert_eq!(
            asked(askpass.path(), "Password: ").unwrap().as_deref(),
            Some("hunter2")
        );
        // Turned down.
        assert_eq!(asked(askpass.path(), "Passphrase: ").unwrap(), None);

        let path = askpass.path().to_owned();
        drop(askpass);
        assert!(!path.exists(), "the socket is removed");
        assert!(asked(&path, "Password: ").is_err());
    }

    #[test]
    fn an_empty_answer_is_still_an_answer() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeDir::at(temp.path().join("clackatoo"));
        let askpass = Askpass::serve(&runtime, |_| Some(String::new())).unwrap();
        assert_eq!(asked(askpass.path(), "?").unwrap().as_deref(), Some(""));
    }
}
