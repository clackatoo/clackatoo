//! A program the daemon runs as its child and exchanges JSON-RPC messages
//! with over its stdin and stdout: the agent, which writes a message per
//! line, and language servers, which put a `Content-Length` header before
//! each. The framing is shared with the connections agents' tools are
//! served on (see [`crate::mcp`]).
//!
//! This only moves messages. It frames them, unframes what comes back,
//! and keeps what the program writes to stderr for the log. What the
//! messages mean is for the agent's and language servers' own modules.
//! Every message either way can also be written to a trace file, a line
//! each, to see what was said (see [`Peer::trace_to`]).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::OFlags;
use rustix::process::WaitIdStatus;
use serde::Deserialize;
use serde_json::Value;

use crate::group::Group;
use crate::pty::Process;

/// The most one message may hold before it's dropped as broken: far more
/// than any message.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// How messages are told apart in the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Framing {
    /// One message per line, as ACP's.
    Lines,
    /// Each message after a `Content-Length` header, as LSP's.
    Headers,
}

/// One JSON-RPC message, of any kind: a request has a method and an ID, a
/// notification a method alone, and an answer an ID alone.
#[derive(Debug, Deserialize)]
pub(crate) struct Message {
    pub(crate) id: Option<Value>,
    pub(crate) method: Option<String>,
    #[serde(default)]
    pub(crate) params: Value,
    pub(crate) result: Option<Value>,
    pub(crate) error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RpcError {
    #[serde(default)]
    pub(crate) code: i64,
    pub(crate) message: String,
}

/// A running program, and the messages on their way to and from it.
#[derive(Debug)]
pub(crate) struct Peer {
    /// What it's called in the log.
    name: String,
    framing: Framing,
    process: Process,
    stdin: ChildStdin,
    stdout: ChildStdout,
    stderr: ChildStderr,
    /// Encoded messages not yet written.
    outgoing: Vec<u8>,
    /// What's been read of a message not yet whole.
    incoming: Vec<u8>,
    /// What's been read of a line of stderr not yet ended.
    errors: Vec<u8>,
    /// The last lines of stderr, to say why it stopped if it stops soon.
    last_errors: VecDeque<String>,
    /// Where every message to and from it is written, if anywhere.
    trace: Option<File>,
}

/// How many of a program's last lines of stderr are kept.
const LAST_ERRORS: usize = 4;

/// The most of each of those lines kept, in bytes.
const LAST_ERROR_LEN: usize = 300;

impl Peer {
    /// Starts `command`, in `cwd` if given, called `name` in the log.
    pub(crate) fn start(
        name: &str,
        command: &[OsString],
        cwd: Option<&Path>,
        framing: Framing,
    ) -> io::Result<Self> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no command"))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        // In a process group of its own, so stopping it reaches whatever
        // it starts too, as `npx` starts node (see [`crate::group`]). The
        // daemon has no terminal whose signals it would miss.
        command.process_group(0);
        let mut child = command.spawn()?;
        let pid = rustix::process::Pid::from_child(&child);
        // Taken first, so failing below drops the pipes, which ends it.
        let (stdin, stdout, stderr) = (
            child.stdin.take().expect("piped"),
            child.stdout.take().expect("piped"),
            child.stderr.take().expect("piped"),
        );
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())?;
        for fd in [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()] {
            rustix::io::ioctl_fionbio(fd, true)?;
        }
        log::info!("{name} started: pid {}", pid.as_raw_nonzero());
        Ok(Self {
            name: name.to_owned(),
            framing,
            process: Process::from_parts(pid, pidfd),
            stdin,
            stdout,
            stderr,
            outgoing: Vec::new(),
            incoming: Vec::new(),
            errors: Vec::new(),
            last_errors: VecDeque::new(),
            trace: None,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Writes every message to and from the program from now on, and what
    /// it writes to stderr, to the end of the file at `path`, a line each,
    /// stamped with the time in UTC and the program's name and pid: `->`
    /// before what it's sent, `<-` before what it sends, `!!` before a
    /// line of its stderr. The file is made readable by its owner only, as
    /// messages hold files' text. Only a regular file is written: never
    /// one through a link, nor a pipe, which could hold up the loop. If
    /// it can't be, the log says why.
    pub(crate) fn trace_to(&mut self, path: &Path) {
        let opened = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(
                i32::try_from((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits()).unwrap_or_default(),
            )
            .open(path)
            .and_then(|file| {
                if file.metadata()?.is_file() {
                    Ok(file)
                } else {
                    Err(io::Error::other("it isn't a regular file"))
                }
            });
        match opened {
            Ok(file) => self.trace = Some(file),
            Err(err) => log::warn!("{}: can't trace to {}: {err}", self.name, path.display()),
        }
    }

    /// Writes `body`, which went the way `way` says, to the trace, if
    /// there's one: on one line, as JSON breaks lines only between its
    /// parts, where a space does as well.
    fn trace(&mut self, way: &str, body: &[u8]) {
        let Some(file) = &mut self.trace else { return };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let mut line = format!(
            "{}.{:03}Z {}[{}] {way} ",
            crate::explorer::date(now.as_secs()),
            now.subsec_millis(),
            self.name,
            self.process.pid().as_raw_nonzero(),
        )
        .into_bytes();
        line.extend(body.iter().map(|&byte| match byte {
            b'\n' | b'\r' => b' ',
            byte => byte,
        }));
        line.push(b'\n');
        // In one write, so the lines of servers tracing to one file never
        // run into each other.
        if let Err(err) = file.write_all(&line) {
            log::warn!("{}: tracing stopped: {err}", self.name);
            self.trace = None;
        }
    }

    /// Queues `message`, framed.
    pub(crate) fn send(&mut self, message: &Value) {
        let body = message.to_string();
        self.trace("->", body.as_bytes());
        frame_body(self.framing, &body, &mut self.outgoing);
    }

    /// Writes queued messages until the pipe is full. An error means the
    /// program has gone.
    pub(crate) fn flush(&mut self) -> io::Result<()> {
        let mut written = 0;
        let result = loop {
            if written == self.outgoing.len() {
                break Ok(());
            }
            match self.stdin.write(&self.outgoing[written..]) {
                Ok(n) => written += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => break Err(err),
            }
        };
        self.outgoing.drain(..written);
        result
    }

    /// Whether messages are waiting for the pipe to take them.
    pub(crate) fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }

    /// Reads what the program sent, up to `budget` bytes, into `messages`.
    /// Returns whether more may be waiting, as when the budget ran out, or
    /// an error if the program has gone.
    pub(crate) fn read(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        messages: &mut Vec<Message>,
    ) -> io::Result<bool> {
        let mut total = 0;
        loop {
            match self.stdout.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    total += n;
                    self.incoming.extend_from_slice(&buffer[..n]);
                    if self.trace.is_none() {
                        unframe(&self.name, self.framing, &mut self.incoming, messages)?;
                    } else {
                        let mut bodies = Vec::new();
                        let unframed =
                            unframe_bodies(&self.name, self.framing, &mut self.incoming, |body| {
                                bodies.push(body.to_vec());
                            });
                        for body in bodies {
                            self.trace("<-", &body);
                            messages.extend(parse(&self.name, &body));
                        }
                        unframed?;
                    }
                    if total >= budget {
                        return Ok(true);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
    }

    /// Reads what the program wrote to stderr, up to `budget` bytes, and
    /// logs it at debug level. Returns whether more may be waiting, as when
    /// the budget ran out.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8], budget: usize) -> bool {
        let mut total = 0;
        loop {
            if total >= budget {
                return true;
            }
            match self.stderr.read(buffer) {
                Ok(0) => return false,
                Ok(n) => {
                    total += n;
                    self.errors.extend_from_slice(&buffer[..n]);
                    while let Some(end) = self.errors.iter().position(|&byte| byte == b'\n') {
                        let line: Vec<u8> = self.errors.drain(..=end).collect();
                        let line = String::from_utf8_lossy(&line);
                        let line = line.trim();
                        log::debug!("{}: {line}", self.name);
                        self.trace("!!", line.as_bytes());
                        if !line.is_empty() {
                            if self.last_errors.len() == LAST_ERRORS {
                                self.last_errors.pop_front();
                            }
                            let mut end = line.len().min(LAST_ERROR_LEN);
                            while !line.is_char_boundary(end) {
                                end -= 1;
                            }
                            self.last_errors.push_back(line[..end].to_owned());
                        }
                    }
                    if self.errors.len() > MAX_MESSAGE {
                        self.errors.clear();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }

    /// The last few lines it wrote to stderr, on one line, if it wrote
    /// any.
    pub(crate) fn last_errors(&self) -> Option<String> {
        let lines: Vec<&str> = self.last_errors.iter().map(String::as_str).collect();
        (!lines.is_empty()).then(|| lines.join(" / "))
    }

    pub(crate) fn stdout_fd(&self) -> RawFd {
        self.stdout.as_raw_fd()
    }

    pub(crate) fn stderr_fd(&self) -> RawFd {
        self.stderr.as_raw_fd()
    }

    pub(crate) fn stdin_fd(&self) -> RawFd {
        self.stdin.as_raw_fd()
    }

    pub(crate) fn exit_fd(&self) -> RawFd {
        self.process.as_raw_fd()
    }

    /// Collects the program's exit status, if it has exited. After that
    /// it's gone, and this mustn't be called again.
    pub(crate) fn try_wait(&self) -> io::Result<Option<WaitIdStatus>> {
        self.process.try_wait()
    }

    /// Kills the program, if it's still there, and whatever it started
    /// that's still in its process group.
    pub(crate) fn kill(&self) {
        self.process.kill_group();
    }

    /// Its process group, to signal by later (see [`crate::group`]).
    pub(crate) fn group(&self) -> io::Result<Group> {
        self.process.group()
    }

    /// Stops the program: closing its pipes ends it. Returns its process,
    /// to be reaped once it has exited.
    pub(crate) fn stop(self) -> Process {
        log::info!("{} stopped", self.name);
        self.process
    }
}

/// Queues `message` onto `outgoing`, framed as `framing` says.
pub(crate) fn frame(framing: Framing, message: &Value, outgoing: &mut Vec<u8>) {
    frame_body(framing, &message.to_string(), outgoing);
}

/// Queues `body`, a message's JSON, onto `outgoing`, framed as `framing`
/// says.
fn frame_body(framing: Framing, body: &str, outgoing: &mut Vec<u8>) {
    match framing {
        Framing::Lines => {
            outgoing.extend_from_slice(body.as_bytes());
            outgoing.push(b'\n');
        }
        Framing::Headers => {
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            outgoing.extend_from_slice(header.as_bytes());
            outgoing.extend_from_slice(body.as_bytes());
        }
    }
}

/// Takes the whole messages read so far into `incoming`, from `name`,
/// leaving the rest. An error if the stream is broken past finding where
/// the next message starts, as a header with no length leaves it.
pub(crate) fn unframe(
    name: &str,
    framing: Framing,
    incoming: &mut Vec<u8>,
    messages: &mut Vec<Message>,
) -> io::Result<()> {
    unframe_bodies(name, framing, incoming, |body| {
        messages.extend(parse(name, body));
    })
}

/// As [`unframe`] does, but hands each whole message's bytes to `take`,
/// as they came.
fn unframe_bodies(
    name: &str,
    framing: Framing,
    incoming: &mut Vec<u8>,
    mut take: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut start = 0;
    let broken = loop {
        match next_body(framing, &incoming[start..]) {
            Ok(Some((body, end))) => {
                take(&incoming[start + body.start..start + body.end]);
                start += end;
            }
            Ok(None) => break None,
            Err(broken) => break Some(broken),
        }
    };
    incoming.drain(..start);
    if let Some(broken) = broken {
        log::warn!("{name}: {broken}, so no more can be read");
        return Err(io::ErrorKind::InvalidData.into());
    }
    if incoming.len() > MAX_MESSAGE {
        if framing == Framing::Headers {
            log::warn!("{name}: a header never ended");
            return Err(io::ErrorKind::InvalidData.into());
        }
        // The next line break starts the next message.
        log::warn!("{name}: dropping a message longer than {MAX_MESSAGE} bytes");
        incoming.clear();
    }
    Ok(())
}

/// The bytes of a message, and where what follows it starts.
type Framed = (std::ops::Range<usize>, usize);

/// Where the first whole message in `bytes` is, or `None` if there's no
/// whole message yet, or what's wrong if a header has no length, or one
/// too long, to tell where the message ends.
fn next_body(framing: Framing, bytes: &[u8]) -> Result<Option<Framed>, &'static str> {
    match framing {
        Framing::Lines => Ok(bytes
            .iter()
            .position(|&byte| byte == b'\n')
            .map(|end| (0..end, end + 1))),
        Framing::Headers => {
            let Some(headers_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                return Ok(None);
            };
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let body = headers_end + 4;
            let length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            });
            match length {
                None => Err("a message came with no length"),
                Some(length) if length > MAX_MESSAGE => Err("a message was too long"),
                Some(length) => {
                    let end = body + length;
                    Ok((bytes.len() >= end).then_some((body..end, end)))
                }
            }
        }
    }
}

/// A message's bytes as a message, or `None` if they aren't one.
fn parse(name: &str, body: &[u8]) -> Option<Message> {
    let body = body.trim_ascii();
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice(body)
        .map_err(|err| log::debug!("{name}: skipping a message that isn't JSON-RPC: {err}"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bodies(framing: Framing, mut bytes: &[u8]) -> (Vec<String>, usize) {
        let mut found = Vec::new();
        let mut used = 0;
        while let Ok(Some((body, end))) = next_body(framing, bytes) {
            found.push(String::from_utf8_lossy(&bytes[body]).into_owned());
            bytes = &bytes[end..];
            used += end;
        }
        (found, used)
    }

    #[test]
    fn lines_are_messages() {
        let (found, used) = bodies(Framing::Lines, b"{\"a\":1}\n{\"b\":2}\n{\"c\"");
        assert_eq!(found, ["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(used, 16, "the part after waits for the rest");
    }

    #[test]
    fn headers_say_how_long_each_message_is() {
        let stream = b"Content-Length: 7\r\n\r\n{\"a\":1}content-length:7\r\nContent-Type: x\r\n\r\n{\"b\":2}Content-Length: 99\r\n\r\n{";
        let (found, used) = bodies(Framing::Headers, stream);
        assert_eq!(found, ["{\"a\":1}", "{\"b\":2}"]);
        assert_eq!(
            &stream[used..],
            b"Content-Length: 99\r\n\r\n{",
            "not whole yet"
        );
        // Headers not ended yet.
        assert_eq!(
            bodies(Framing::Headers, b"Content-Length: 7\r\n").0.len(),
            0
        );
        // A header with no length, or one too long, leaves no telling
        // where the next message starts.
        assert!(next_body(Framing::Headers, b"X: 1\r\n\r\n{}").is_err());
        let huge = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1);
        assert!(next_body(Framing::Headers, huge.as_bytes()).is_err());
    }

    #[test]
    fn a_flood_of_stderr_is_read_a_budget_at_a_time() {
        let flood = [
            "sh",
            "-c",
            "head -c 4000000 /dev/zero | tr '\\0' x >&2; sleep 5",
        ];
        let command: Vec<OsString> = flood.iter().map(OsString::from).collect();
        let mut peer = Peer::start("flood", &command, None, Framing::Lines).unwrap();
        let mut buffer = vec![0; 64 * 1024];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        // Once a budget's worth is waiting, a read stops there, saying
        // there's more.
        while !peer.read_errors(&mut buffer, 1024) {
            assert!(
                std::time::Instant::now() < deadline,
                "never more than a budget"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        peer.kill();
        while peer.try_wait().unwrap().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
