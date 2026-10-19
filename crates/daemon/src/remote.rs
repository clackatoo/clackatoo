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
//! the client skips everything before the announcement. Just before it,
//! the bridge says which address sshd was reached at, which is where the
//! client can try reaching the daemon over UDP.
//!
//! ssh asks for passwords on the terminal it was started from, which a
//! window may not have. A window serves an [`Askpass`] instead: ssh runs
//! clackatoo as its askpass program (`SSH_ASKPASS`), which passes each
//! question to the window over a socket, and the window's answer back.
//! A daemon connecting to another daemon does the same, passing each
//! question on to a window of its own.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::client::{Client, ClientError, UdpTarget};
use crate::launch::{self, LaunchError};
use crate::quic::{Redial, Redialed};
use crate::runtime::RuntimeDir;

/// What the bridge sends before relaying, unlikely to begin anything a
/// shell prints.
const ANNOUNCEMENT: &[u8] = b"\x1b[clackatoo-bridge]\n";

/// What begins the line before the announcement that gives the address sshd
/// was reached at, followed by the address, `]` and a newline. Clients from
/// before it skip it, as they would a greeting.
const ADDRESS: &[u8] = b"\x1b[clackatoo-address ";

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
#[must_use]
pub fn ssh_program() -> OsString {
    std::env::var_os("CLACKATOO_SSH")
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// How ssh asks its questions: by running `program` as its askpass
/// program, which asks the [`Askpass`] listening on `socket`.
#[derive(Clone, Debug)]
pub(crate) struct Asking {
    pub(crate) program: PathBuf,
    pub(crate) socket: PathBuf,
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
    let asking = match askpass {
        Some(socket) => Some(Asking {
            program: std::env::current_exe()?,
            socket: socket.to_owned(),
        }),
        None => None,
    };
    connect_with(remote, &ssh_program(), replace, asking, None)
}

/// As [`connect`], running `ssh` as ssh, which asks its questions as
/// `asking` says, if it says. `cancel`, if given, can stop it from another
/// thread while it logs in and greets the daemon: see [`Cancel`].
pub(crate) fn connect_with(
    remote: &Remote,
    ssh: &OsStr,
    replace: bool,
    asking: Option<Asking>,
    cancel: Option<&Arc<Cancel>>,
) -> Result<Client, RemoteError> {
    let command = ssh_command(remote, ssh, replace, asking.as_ref(), false);
    let (stream, ssh_process, address) = dial(command, &remote.host, cancel)?;
    // Whenever ssh closes, as when it gives up on a network that went
    // away, the connection carries on through a new one, which never
    // replaces the daemon.
    // A new ssh can be stopped as the first can.
    let redial: Redial = {
        let remote = remote.clone();
        let ssh = ssh.to_owned();
        let cancel = cancel.cloned();
        Box::new(move || {
            let command = ssh_command(&remote, &ssh, false, asking.as_ref(), false);
            let (stream, ssh, _) =
                dial(command, &remote.host, cancel.as_ref()).map_err(io::Error::other)?;
            Ok(Redialed::keeping(stream, ssh))
        })
    };
    let client = Client::over_carrier(
        stream,
        address.map(UdpTarget::Host),
        Some(ssh_process),
        Some(redial),
        clackatoo_protocol::PROTOCOL_VERSION,
    );
    if cancel.is_some_and(|cancel| cancel.cancelled()) {
        return Err(RemoteError::Unreachable {
            host: remote.host.clone(),
            reason: CANCELLED.to_owned(),
        });
    }
    Ok(client?)
}

/// Why a connection [`Cancel`]led failed.
const CANCELLED: &str = "it was stopped";

/// Runs ssh, as `ssh`, to the bridge on `remote`'s machine, and waits for
/// it to announce itself, as [`connect_with`] does before greeting the
/// daemon there: the carrier, what keeps it running, and the address the
/// machine was reached at. ssh asks its questions as `asking` says; with
/// `batch`, it asks none, failing instead, as when it's only tried to see
/// whether it gets there now. `cancel`, if given, can stop it from
/// another thread.
///
/// # Errors
///
/// If ssh can't run, or can't reach clackatoo.
pub(crate) fn dial_ssh(
    remote: &Remote,
    ssh: &OsStr,
    asking: Option<&Asking>,
    cancel: Option<&Arc<Cancel>>,
    batch: bool,
) -> Result<(UnixStream, Ssh, Option<IpAddr>), RemoteError> {
    let command = ssh_command(remote, ssh, false, asking.filter(|_| !batch), batch);
    dial(command, &remote.host, cancel)
}

/// The ssh command that runs the bridge on `remote`'s machine, running
/// `program` as ssh, replacing the daemon there first with `replace`, and
/// asking for passwords as `asking` says, if given, or, with `batch`,
/// never asking at all.
fn ssh_command(
    remote: &Remote,
    program: &OsStr,
    replace: bool,
    asking: Option<&Asking>,
    batch: bool,
) -> Command {
    let mut ssh = Command::new(program);
    if let Some(asking) = asking {
        ssh.env("SSH_ASKPASS", &asking.program)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env(ASKPASS_VAR, &asking.socket);
    }
    if batch {
        ssh.args(["-o", "BatchMode=yes"]);
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
        &destination(&remote.host),
    ])
    .arg(format!(
        "{} bridge{}",
        remote.command,
        if replace { " --replace" } else { "" }
    ));
    ssh
}

/// `host` as ssh's destination. ssh takes `user@host`, but a port only in
/// a URI, so `[user@]host:port`, and an IPv6 address in brackets, become
/// `ssh://[user@]host[:port]`. A bare IPv6 address, with colons and no
/// brackets, has no port to find, and stays as it is.
fn destination(host: &str) -> std::borrow::Cow<'_, str> {
    let machine = host.rsplit_once('@').map_or(host, |(_, machine)| machine);
    let port = |port: &str| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit());
    let uri = machine.starts_with('[')
        || machine
            .split_once(':')
            .is_some_and(|(name, after)| !name.is_empty() && port(after));
    if uri && !host.starts_with("ssh://") {
        format!("ssh://{host}").into()
    } else {
        host.into()
    }
}

/// Connects to a daemon through `command`, which runs `clackatoo bridge`
/// somewhere and relays its input and output, as ssh does. `host` names
/// where, in errors.
///
/// # Errors
///
/// As [`connect`].
pub fn through(command: Command, host: &str) -> Result<Client, RemoteError> {
    let (stream, ssh, address) = dial(command, host, None)?;
    Ok(Client::over_carrier(
        stream,
        address.map(UdpTarget::Host),
        Some(ssh),
        None,
        clackatoo_protocol::PROTOCOL_VERSION,
    )?)
}

/// Runs `command`, which runs `clackatoo bridge` somewhere, and waits for
/// the bridge to announce itself. Returns the stream to it, what keeps it
/// running, and the address the bridge says its machine was reached at.
/// `cancel`, if given, can stop it from another thread.
fn dial(
    mut command: Command,
    host: &str,
    cancel: Option<&Arc<Cancel>>,
) -> Result<(UnixStream, Ssh, Option<IpAddr>), RemoteError> {
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
    let mut ssh = Ssh::new(child, cancel);

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
    let greeting = &greeting[..greeting.len() - ANNOUNCEMENT.len()];
    Ok((stream, ssh, announced_address(greeting)))
}

/// The address the bridge announced, at the end of `greeting`, if it did.
fn announced_address(greeting: &[u8]) -> Option<IpAddr> {
    let line = greeting.strip_suffix(b"]\n")?;
    let at = line
        .windows(ADDRESS.len())
        .rposition(|window| window == ADDRESS)?;
    std::str::from_utf8(&line[at + ADDRESS.len()..])
        .ok()?
        .parse()
        .ok()
}

/// The address sshd was reached at, from what it tells the programs it
/// runs: `client-address client-port server-address server-port`.
fn reached_at() -> Option<IpAddr> {
    let connection = std::env::var("SSH_CONNECTION").ok()?;
    connection.split_whitespace().nth(2)?.parse().ok()
}

/// Relays between this process's input and output and the daemon, which
/// is started if it isn't running, until either end hangs up. With
/// `replace`, a daemon already running is replaced first: upgraded in
/// place if it can be, or else stopped (see [`launch::replace`]).
///
/// # Errors
///
/// If the daemon can't be stopped, started or reached.
pub fn bridge(runtime: &RuntimeDir, replace: bool) -> Result<(), LaunchError> {
    if replace {
        launch::replace(runtime)?;
    }
    let daemon = launch::socket_or_start(runtime)?;
    // The raw descriptors: the standard library's stdout holds bytes back
    // until a newline.
    let mut input = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    let mut output = File::from(io::stdout().as_fd().try_clone_to_owned()?);
    if let Some(address) = reached_at() {
        let mut line = ADDRESS.to_vec();
        line.extend_from_slice(format!("{address}]\n").as_bytes());
        output.write_all(&line)?;
    }
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
        Self::serve_at(path, move |question, _| answer(question))
    }

    /// As [`serve`](Self::serve), on the socket at `path`, in a directory
    /// for the user alone. `answer` is also given the [`Asker`], to see
    /// while it waits whether the asker has gone.
    ///
    /// # Errors
    ///
    /// If the socket can't be made.
    pub fn serve_at(
        path: PathBuf,
        answer: impl Fn(String, &Asker<'_>) -> Option<String> + Send + 'static,
    ) -> io::Result<Self> {
        // Left by an earlier process with the same ID that didn't exit
        // cleanly.
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let listener = UnixListener::bind(&path)?;
        // The user's alone, whatever the directory allows.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
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
    answer: &impl Fn(String, &Asker<'_>) -> Option<String>,
) -> io::Result<()> {
    // Only the user's own programs are answered: a password is never
    // handed to anyone else's.
    let asker = rustix::net::sockopt::socket_peercred(&stream)?;
    if asker.uid != rustix::process::getuid() {
        return Err(io::Error::other("asked by another user's program"));
    }
    let mut question = String::new();
    (&mut stream)
        .take(MAX_QUESTION)
        .read_to_string(&mut question)?;
    if let Some(text) = answer(question, &Asker(&stream)) {
        stream.write_all(b"y")?;
        stream.write_all(text.as_bytes())?;
    }
    Ok(())
}

/// The program asking an [`Askpass`] a question: clackatoo, run by ssh as
/// its askpass program, which waits for the answer.
#[derive(Debug)]
pub struct Asker<'a>(&'a UnixStream);

impl Asker<'_> {
    /// Whether it has hung up, as it does when ssh gives up, or is ended:
    /// its question needs no answer then. Doesn't wait.
    #[must_use]
    pub fn gone(&self) -> bool {
        use rustix::event::{PollFd, PollFlags, Timespec};
        let mut fds = [PollFd::new(self.0, PollFlags::empty())];
        let now = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        match rustix::event::poll(&mut fds, Some(&now)) {
            Ok(_) => (fds[0].revents()).intersects(PollFlags::HUP | PollFlags::ERR),
            Err(_) => false,
        }
    }
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

/// A way to stop a connection from another thread, as it logs in with ssh
/// and greets the daemon, or as a new ssh logs in to carry it on: every
/// ssh of its is killed, and none is started again. Once it's made,
/// dropping the client ends it too.
#[derive(Debug, Default)]
pub(crate) struct Cancel(Mutex<Cancelling>);

#[derive(Debug, Default)]
struct Cancelling {
    cancelled: bool,
    /// The IDs of its ssh processes, the first and any opened to carry it
    /// on, while each runs and hasn't been reaped, so none can be another
    /// process's.
    ssh: Vec<u32>,
}

impl Cancel {
    /// Stops the connection: kills its ssh, if it's running, and fails it
    /// once it next looks.
    pub(crate) fn cancel(&self) {
        let mut cancelling = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cancelling.cancelled = true;
        for pid in &cancelling.ssh {
            if let Some(pid) = rustix::process::Pid::from_raw(pid.cast_signed()) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }

    /// Whether it was stopped.
    pub(crate) fn cancelled(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }

    /// Notes ssh running as `pid`. Returns whether to go on: not if it was
    /// stopped already.
    fn watch(&self, pid: u32) -> bool {
        let mut cancelling = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cancelling.ssh.push(pid);
        !cancelling.cancelled
    }

    /// Forgets the ssh with ID `pid`, as it's about to be reaped.
    fn forget(&self, pid: u32) {
        let mut cancelling = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        cancelling.ssh.retain(|&watched| watched != pid);
    }
}

/// The ssh process carrying a connection to a remote daemon. Ended when
/// dropped.
#[derive(Debug)]
pub(crate) struct Ssh {
    child: Child,
    /// Collects what ssh prints to stderr.
    errors: Option<JoinHandle<String>>,
    /// What can kill it from another thread, while the connection is made.
    cancel: Option<Arc<Cancel>>,
}

impl Ssh {
    fn new(mut child: Child, cancel: Option<&Arc<Cancel>>) -> Self {
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
        let cancel = cancel.cloned();
        if let Some(cancel) = &cancel
            && !cancel.watch(child.id())
        {
            let _ = child.kill();
        }
        Self {
            child,
            errors,
            cancel,
        }
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Reaps ssh, once it has exited.
    fn wait(&mut self) {
        // First, while its ID can't yet be another process's.
        if let Some(cancel) = self.cancel.take() {
            cancel.forget(self.child.id());
        }
        let _ = self.child.wait();
    }

    /// Waits for ssh to exit, and returns what it printed to stderr.
    fn finish(mut self) -> String {
        self.wait();
        self.errors
            .take()
            .and_then(|errors| errors.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for Ssh {
    fn drop(&mut self) {
        self.kill();
        self.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_comes_from_the_line_before_the_announcement() {
        assert_eq!(
            announced_address(b"Welcome!\n\x1b[clackatoo-address 192.0.2.7]\n"),
            Some("192.0.2.7".parse().unwrap())
        );
        assert_eq!(
            announced_address(b"\x1b[clackatoo-address 2001:db8::1]\n"),
            Some("2001:db8::1".parse().unwrap())
        );
        assert_eq!(announced_address(b"Welcome!\n"), None, "an older bridge");
        assert_eq!(
            announced_address(b"\x1b[clackatoo-address 192.0.2.7]\nMore greeting\n"),
            None,
            "only right before the announcement"
        );
    }

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
    fn a_port_or_an_ipv6_address_goes_to_ssh_as_a_uri() {
        for (host, destination_) in [
            ("desktop", "desktop"),
            ("jack@desktop", "jack@desktop"),
            ("desktop:2222", "ssh://desktop:2222"),
            ("jack@desktop:2222", "ssh://jack@desktop:2222"),
            ("[fd7a::1]", "ssh://[fd7a::1]"),
            ("jack@[fd7a::1]:22", "ssh://jack@[fd7a::1]:22"),
            ("fd7a::1", "fd7a::1"),
            ("desktop:ssh", "desktop:ssh"),
            ("ssh://jack@desktop:2222", "ssh://jack@desktop:2222"),
        ] {
            assert_eq!(destination(host), destination_, "{host}");
        }
    }

    #[test]
    fn an_empty_answer_is_still_an_answer() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = RuntimeDir::at(temp.path().join("clackatoo"));
        let askpass = Askpass::serve(&runtime, |_| Some(String::new())).unwrap();
        assert_eq!(asked(askpass.path(), "?").unwrap().as_deref(), Some(""));
    }
}
