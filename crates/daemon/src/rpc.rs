//! A program the daemon runs as its child and exchanges JSON-RPC messages
//! with over its stdin and stdout: the agent, which writes a message per
//! line, and language servers, which put a `Content-Length` header before
//! each.
//!
//! This only moves messages. It frames them, unframes what comes back,
//! and keeps what the program writes to stderr for the log. What the
//! messages mean is for the agent's and language servers' own modules.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::path::Path;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

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
}

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
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Queues `message`, framed.
    pub(crate) fn send(&mut self, message: &Value) {
        let body = message.to_string();
        match self.framing {
            Framing::Lines => {
                self.outgoing.extend_from_slice(body.as_bytes());
                self.outgoing.push(b'\n');
            }
            Framing::Headers => {
                let header = format!("Content-Length: {}\r\n\r\n", body.len());
                self.outgoing.extend_from_slice(header.as_bytes());
                self.outgoing.extend_from_slice(body.as_bytes());
            }
        }
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
                    self.unframe(messages)?;
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

    /// Takes the whole messages read so far, leaving the rest.
    /// An error if the stream is broken past finding where the next
    /// message starts, as a header with no length leaves it.
    fn unframe(&mut self, messages: &mut Vec<Message>) -> io::Result<()> {
        let mut start = 0;
        let broken = loop {
            match next_body(self.framing, &self.incoming[start..]) {
                Ok(Some((body, end))) => {
                    let body = &self.incoming[start + body.start..start + body.end];
                    messages.extend(parse(&self.name, body));
                    start += end;
                }
                Ok(None) => break None,
                Err(broken) => break Some(broken),
            }
        };
        self.incoming.drain(..start);
        if let Some(broken) = broken {
            log::warn!("{}: {broken}, so no more can be read", self.name);
            return Err(io::ErrorKind::InvalidData.into());
        }
        if self.incoming.len() > MAX_MESSAGE {
            if self.framing == Framing::Headers {
                log::warn!("{}: a header never ended", self.name);
                return Err(io::ErrorKind::InvalidData.into());
            }
            // The next line break starts the next message.
            log::warn!(
                "{}: dropping a message longer than {MAX_MESSAGE} bytes",
                self.name
            );
            self.incoming.clear();
        }
        Ok(())
    }

    /// Reads what the program wrote to stderr, and logs it at debug level.
    pub(crate) fn read_errors(&mut self, buffer: &mut [u8]) {
        loop {
            match self.stderr.read(buffer) {
                Ok(0) => return,
                Ok(n) => {
                    self.errors.extend_from_slice(&buffer[..n]);
                    while let Some(end) = self.errors.iter().position(|&byte| byte == b'\n') {
                        let line: Vec<u8> = self.errors.drain(..=end).collect();
                        let line = String::from_utf8_lossy(&line);
                        log::debug!("{}: {}", self.name, line.trim_end());
                    }
                    if self.errors.len() > MAX_MESSAGE {
                        self.errors.clear();
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
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

    /// Stops the program: closing its pipes ends it. Returns its process,
    /// to be reaped once it has exited.
    pub(crate) fn stop(self) -> Process {
        log::info!("{} stopped", self.name);
        self.process
    }
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
}
