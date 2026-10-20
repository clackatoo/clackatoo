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
//!
//! A big message is parsed on a thread of the daemon's own, the
//! [`Parser`]'s, not on the loop: parsing a language server's answer
//! costs about 4 ms a megabyte, and every terminal would wait meanwhile.
//! What it parsed comes back in the order it came, behind nothing and
//! ahead of nothing else from the same program (see [`Inbox`]). The
//! biggest parts of what it parsed are freed there too (see [`Heavy`]).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::{Deref, Range};
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mio::Waker;
use rustix::fs::OFlags;
use rustix::process::WaitIdStatus;
use serde::Deserialize;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, Visitor};
use serde_json::{Value, json};

use crate::group::Group;
use crate::pty::Process;

/// The most one message may hold before it's dropped as broken: far more
/// than any message. A language server's message longer than this, by
/// what its header says, ends the server, as nothing it sends after can
/// be trusted to be where the header says; an agent's line longer than
/// this is dropped, and refused if its start says what it was (see
/// [`Inbox::refuse`]), and the next line read.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// How long a message must be to be parsed on the parser's thread: about
/// a millisecond's parsing in a release build, for a language server's
/// list of small objects.
pub(crate) const BIG: usize = 256 * 1024;

/// The most bytes of one program's messages that may wait for the
/// parser's thread: past this, no more is read from the program until the
/// thread catches up, so one that sends faster than it can be parsed
/// waits, rather than filling memory.
const MAX_PARSING: usize = MAX_MESSAGE;

/// How long a program that has gone is kept for what it sent last, unless
/// the parser's thread is busy while it has messages there: from when it
/// went, or from when the thread last finished something, if that was
/// later, as what it sent after that may still be in its pipe. The
/// daemon's config may say otherwise (`DaemonConfig::gone_wait`).
/// It's also how long one that has gone is kept while it, or something it
/// started, still writes. Long enough for the thread to get to its
/// messages, and for the rest to be read, short enough that a dead
/// program isn't kept for long.
pub(crate) const PARSE_WAIT: Duration = Duration::from_secs(2);

/// The longest a program that has gone is kept for what it sent last,
/// however the parser's thread is getting on with it. While the thread is
/// busy and some of its messages are there, it's kept up to this, as a big
/// message on a busy machine can take seconds to parse, and its own may
/// wait behind another program's; but no longer, so a thread stuck
/// somehow doesn't keep a dead program forever.
const LONGEST_PARSE_WAIT: Duration = Duration::from_secs(10);

/// How much of the start of a line too long to read is looked through
/// for its ID and method (see [`Head`]).
const HEAD: usize = 64 * 1024;

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
    /// Where its parts are freed, if it was big enough to be parsed on
    /// the parser's thread (see [`Heavy`]).
    #[serde(skip)]
    pub(crate) trash: Option<Trash>,
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
    /// What's been read and not yet taken.
    incoming: Inbox,
    /// Whether its process has been seen to exit (see
    /// [`exited`](Self::exited)).
    exited: bool,
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
    /// Starts `command`, in `cwd` if given, called `name` in the log,
    /// on the desktop's `display` if it's followed (see
    /// [`crate::display`]). Its big messages are parsed by `parser`.
    pub(crate) fn start(
        name: &str,
        command: &[OsString],
        cwd: Option<&Path>,
        display: Option<&crate::display::Env>,
        framing: Framing,
        parser: &Parser,
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
        if let Some(display) = display {
            display.apply(&mut command);
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
            incoming: Inbox::new(framing, parser),
            exited: false,
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

    /// Queues `message`, framed, unless the program has gone: then nothing
    /// can be written to it.
    pub(crate) fn send(&mut self, message: &Value) {
        if self.has_gone() {
            return;
        }
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

    /// Whether the parser's thread has parsed messages it sent, for
    /// [`read`](Self::read) to take.
    pub(crate) fn parsed(&self) -> bool {
        self.incoming.parsed()
    }

    /// Notes that the program has gone. What's queued for it is thrown
    /// away, as nothing more is written to it. Returns whether to let it go
    /// now, or wait for what it sent last (see [`Inbox::gone`]).
    pub(crate) fn gone(&mut self) -> bool {
        self.outgoing = Vec::new();
        self.incoming.gone()
    }

    /// Whether the program has been found to have gone. One whose input
    /// broke may still be running, and writing.
    pub(crate) fn has_gone(&self) -> bool {
        self.incoming.has_gone()
    }

    /// Notes that the program has exited, so it has gone. A process's own
    /// pipes are closed as it exits, before its exit is told, so all it
    /// wrote itself is in its pipe by now. Something it started may still
    /// hold its output open and write on, which isn't waited for long
    /// (see [`gone_due`](Self::gone_due)).
    pub(crate) fn exited(&mut self) {
        self.exited = true;
        self.gone();
    }

    /// Whether the program has been seen to exit: if so, a read that comes
    /// up short has read all it wrote itself. One that has only gone, as
    /// its input broke, may write more after a pause.
    pub(crate) fn has_exited(&self) -> bool {
        self.exited
    }

    /// When the program, gone, is let go however much of what it sent is
    /// still being parsed, or still coming, if it has gone (see
    /// [`Inbox::gone_due`]).
    pub(crate) fn gone_due(&self) -> Option<Instant> {
        self.incoming.gone_due()
    }

    /// Takes what the parser's thread has parsed of what the program
    /// sent, then reads on, a `buffer` at a time, until it has taken and
    /// read `budget` bytes, into `messages`, in the order they were sent.
    /// Returns how many bytes it took and read: `budget` or more if it
    /// stopped there, so more may be waiting, and fewer if there was
    /// nothing more to read; or an error if the program has gone. Nothing
    /// is read while the parser's thread is too far behind: once it catches
    /// up, [`parsed`](Self::parsed) says so.
    ///
    /// Once the program has been found to have gone, what it asks is
    /// dropped as it's read: no answer can reach it, so nothing it asks
    /// for is done, as making an edit or writing a file it would never
    /// hear of. What it tells, and its answers, are still taken in. It's
    /// found to have gone only by its caller, after a read: what came in
    /// the read that found it, as by reaching the end of its output, or
    /// before writing to it failed, is acted on as usual, requests and
    /// all.
    pub(crate) fn read(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        messages: &mut Vec<Message>,
    ) -> io::Result<usize> {
        let from = messages.len();
        let read = self.read_on(buffer, budget, messages);
        if self.has_gone() {
            let mut taken = messages.split_off(from);
            taken.retain_mut(|message| {
                let (Some(method), Some(_)) = (&message.method, &message.id) else {
                    return true;
                };
                log::debug!(
                    "{}: {method} not done: it has gone, so can't be answered",
                    self.name
                );
                if let Some(trash) = message.trash.take() {
                    trash.throw(message.params.take());
                }
                false
            });
            messages.append(&mut taken);
        }
        read
    }

    /// Reads as [`read`](Self::read) says, what's asked included.
    fn read_on(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        messages: &mut Vec<Message>,
    ) -> io::Result<usize> {
        let mut total = self.incoming.take(messages, budget);
        loop {
            if total >= budget || self.incoming.full() {
                return Ok(total);
            }
            match self.stdout.read(buffer) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    total += n;
                    if self.trace.is_none() {
                        (self.incoming).push(&self.name, &buffer[..n], messages, |_| {})?;
                    } else {
                        let mut bodies = Vec::new();
                        let unframed =
                            (self.incoming).push(&self.name, &buffer[..n], messages, |body| {
                                bodies.push(body.to_vec());
                            });
                        for body in bodies {
                            self.trace("<-", &body);
                        }
                        unframed?;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(total),
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

/// What's been read of a program's messages and not yet taken: the start
/// of one not yet whole, and those handed to the parser's thread and not
/// yet back. Messages are taken in the order they were sent: once one
/// goes to the parser's thread, every one after it does too, however
/// small, until all have been taken back.
#[derive(Debug)]
pub(crate) struct Inbox {
    framing: Framing,
    parser: Parser,
    /// What's been read of a message not yet whole.
    bytes: Vec<u8>,
    /// How much of `bytes` has been looked through for where the message
    /// ends, in vain, so a long one isn't looked through again for each
    /// read.
    scanned: usize,
    /// What the parser's thread has parsed, and how it's getting on,
    /// shared with it.
    parsed: Arc<Mutex<Parsing>>,
    /// How many messages have been handed to the parser's thread and not
    /// taken back.
    parsing: usize,
    /// How many bytes those hold.
    parsing_bytes: usize,
    /// A line too long to read, being dropped up to its end.
    dropping: Option<Dropped>,
    /// When the program was found to have gone, if it has.
    gone: Option<Instant>,
}

/// A line too long to read: what its start said, and how long it's been
/// so far.
#[derive(Debug)]
struct Dropped {
    head: Head,
    length: usize,
}

/// A message the parser's thread parsed, if it was one, and how long it
/// was.
type Parsed = (Option<Message>, usize);

/// What the parser's thread has done of one program's messages, shared
/// between it and the loop.
#[derive(Debug, Default)]
struct Parsing {
    /// What it has parsed, in order, not yet taken.
    done: VecDeque<Parsed>,
    /// When it last finished one, if it has.
    finished: Option<Instant>,
}

/// How the parser's thread is getting on with its work, whoever's it is,
/// shared between it and the loop.
#[derive(Clone, Copy, Debug, Default)]
struct Progress {
    /// Whether it's in the middle of something now.
    busy: bool,
    /// When it last finished something, if it has.
    finished: Option<Instant>,
}

impl Inbox {
    /// Nothing read yet of messages framed as `framing` says, the big
    /// ones to be parsed by `parser`.
    pub(crate) fn new(framing: Framing, parser: &Parser) -> Self {
        Self {
            framing,
            parser: parser.clone(),
            bytes: Vec::new(),
            scanned: 0,
            parsed: Arc::default(),
            parsing: 0,
            parsing_bytes: 0,
            dropping: None,
            gone: None,
        }
    }

    /// Takes in `read`, what was just read from `name`: the whole messages
    /// it ends go into `messages`, or to the parser's thread, each handed
    /// to `seen` first, as it came. An error if the stream is broken past
    /// finding where the next message starts, as a header with no length,
    /// or one too long, leaves it.
    pub(crate) fn push(
        &mut self,
        name: &str,
        mut read: &[u8],
        messages: &mut Vec<Message>,
        mut seen: impl FnMut(&[u8]),
    ) -> io::Result<()> {
        if let Some(dropping) = &mut self.dropping {
            let Some(end) = memchr::memchr(b'\n', read) else {
                dropping.length += read.len();
                return Ok(());
            };
            dropping.length += end;
            read = &read[end + 1..];
            if let Some(dropped) = self.dropping.take() {
                self.refuse(name, &dropped, messages);
            }
        }
        self.bytes.extend_from_slice(read);
        let mut start = 0;
        let mut length = None;
        let broken = loop {
            let scanned = if start == 0 { self.scanned } else { 0 };
            match next_body(self.framing, &self.bytes[start..], scanned) {
                Ok(Next::Whole(body, end)) => {
                    let (body, end) = (start + body.start..start + body.end, start + end);
                    seen(&self.bytes[body.clone()]);
                    self.scanned = 0;
                    if body.len() >= BIG {
                        // Handed over where it lies, not copied, with the
                        // messages before it, already taken.
                        let rest = self.bytes.split_off(end);
                        let bytes = std::mem::replace(&mut self.bytes, rest);
                        self.hand_over(name, bytes, body, messages);
                        start = 0;
                    } else {
                        if self.parsing > 0 {
                            let bytes = self.bytes[body].to_vec();
                            let body = 0..bytes.len();
                            self.hand_over(name, bytes, body, messages);
                        } else {
                            messages.extend(parse(name, &self.bytes[body]));
                        }
                        start = end;
                    }
                }
                Ok(Next::Partial { scanned, whole }) => {
                    self.scanned = scanned;
                    length = whole;
                    break None;
                }
                Err(broken) => break Some(broken),
            }
        };
        self.bytes.drain(..start);
        if let Some(broken) = broken {
            log::warn!("{name}: {broken}, so no more can be read");
            return Err(io::ErrorKind::InvalidData.into());
        }
        // Room for all of a message whose length is known, at once, not
        // by doubling, which copies what's been read each time.
        if let Some(length) = length {
            self.bytes
                .reserve_exact(length.saturating_sub(self.bytes.len()));
        }
        match self.framing {
            Framing::Headers if self.scanned > MAX_MESSAGE => {
                log::warn!("{name}: a header never ended, so no more can be read");
                return Err(io::ErrorKind::InvalidData.into());
            }
            Framing::Lines if self.bytes.len() > MAX_MESSAGE => {
                // Dropped up to the next line break, which starts the
                // next message, and then refused.
                let start = &self.bytes[..HEAD.min(self.bytes.len())];
                self.dropping = Some(Dropped {
                    head: Head::of(start),
                    length: self.bytes.len(),
                });
                self.bytes = Vec::new();
                self.scanned = 0;
            }
            _ => {}
        }
        Ok(())
    }

    /// Hands `body`, a message from `name` in `bytes`, to the parser's
    /// thread; or, if there's no thread, parses it into `messages` here.
    fn hand_over(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        body: Range<usize>,
        messages: &mut Vec<Message>,
    ) {
        let length = body.len();
        match self.parser.parse(name, bytes, body, &self.parsed) {
            Ok(()) => {
                self.parsing += 1;
                self.parsing_bytes += length;
            }
            Err((bytes, body)) => {
                // The thread has gone, if it was ever there: what it had
                // parsed is taken, and the rest isn't waited for, so
                // reading doesn't stop for good.
                self.take(messages, usize::MAX);
                self.parsing = 0;
                self.parsing_bytes = 0;
                messages.extend(parse(name, &bytes[body]));
            }
        }
    }

    /// Answers `dropped`, a message from `name` too long to read, if its
    /// start said what it was: a request is refused, and an answer taken
    /// as an error, so nothing waits for it forever. It comes in its
    /// place, after what came before it.
    fn refuse(&mut self, name: &str, dropped: &Dropped, messages: &mut Vec<Message>) {
        let length = dropped.length;
        let why = format!("a message of {length} bytes came, more than the most, {MAX_MESSAGE}");
        log::warn!("{name}: {why}, so it was dropped");
        let Some(id) = (dropped.head.id.as_ref()).filter(|id| id.is_number() || id.is_string())
        else {
            return;
        };
        let error = json!({ "code": TOO_LONG, "message": why });
        let refused = match &dropped.head.method {
            Some(method) => json!({ "jsonrpc": "2.0", "id": id, "method": method, "error": error }),
            None => json!({ "jsonrpc": "2.0", "id": id, "error": error }),
        };
        let bytes = refused.to_string().into_bytes();
        if self.parsing > 0 {
            let body = 0..bytes.len();
            self.hand_over(name, bytes, body, messages);
        } else {
            messages.extend(parse(name, &bytes));
        }
    }

    /// Takes what the parser's thread has parsed so far into `messages`, in
    /// order, until the messages taken were `budget` bytes long or more, so
    /// a backlog of them is taken in a little at a time, as what's read is.
    /// Returns how many bytes they were.
    pub(crate) fn take(&mut self, messages: &mut Vec<Message>, budget: usize) -> usize {
        if self.parsing == 0 {
            return 0;
        }
        let mut taken = 0;
        let mut parsed = self.parsed.lock().unwrap_or_else(PoisonError::into_inner);
        while taken < budget
            && let Some((message, length)) = parsed.done.pop_front()
        {
            taken += length;
            self.parsing = self.parsing.saturating_sub(1);
            self.parsing_bytes = self.parsing_bytes.saturating_sub(length);
            messages.extend(message);
        }
        taken
    }

    /// Whether the parser's thread has parsed messages for
    /// [`take`](Self::take).
    pub(crate) fn parsed(&self) -> bool {
        self.parsing > 0
            && !(self.parsed.lock())
                .unwrap_or_else(PoisonError::into_inner)
                .done
                .is_empty()
    }

    /// Whether messages are with the parser's thread, to be taken back.
    pub(crate) fn parsing(&self) -> bool {
        self.parsing > 0
    }

    /// Whether so much waits for the parser's thread that no more should
    /// be read until it catches up.
    pub(crate) fn full(&self) -> bool {
        self.parsing_bytes >= MAX_PARSING
    }

    /// Notes that the program has gone, as its output ended, it exited,
    /// or its input broke. Returns whether to let it go now: once nothing
    /// it sent is still with the parser's thread, so what it said last
    /// isn't lost, or once that has been waited for long enough (see
    /// [`gone_due`](Self::gone_due)).
    pub(crate) fn gone(&mut self) -> bool {
        self.gone.get_or_insert_with(Instant::now);
        !self.parsing() || self.gone_due().is_some_and(|due| due <= Instant::now())
    }

    /// Whether the program has been found to have gone.
    pub(crate) fn has_gone(&self) -> bool {
        self.gone.is_some()
    }

    /// When a program that has gone is let go however much of what it
    /// sent is still being parsed, or still coming, if it's been found to
    /// have gone. It's waited for as long as the parser's thread is getting
    /// on towards its messages, as a big one parses slowly on a busy
    /// machine, and its own may wait behind another program's.
    /// While some of its messages are with the thread: up to
    /// [`LONGEST_PARSE_WAIT`] while the thread is busy with anything, and
    /// otherwise [`PARSE_WAIT`] from when it went or the thread last
    /// finished anything, whichever was later. Once none is: [`PARSE_WAIT`]
    /// from when it went or the thread last finished one of its own. That
    /// leaves time to read what it sent after a big message, which stayed
    /// in its pipe while the thread caught up. With nothing of it ever on
    /// the thread, that's [`PARSE_WAIT`] after it went: it may have only
    /// broken its input and still be writing, or something it started may
    /// hold its output open and write on without end. What's read from its
    /// pipe doesn't count, or one writing on would be kept the longest.
    ///
    /// MCP connections to clackatoo's tools follow the same rule, though a
    /// socket that has finished can't send more: then it only waits for
    /// the thread, and the wait for a pipe's rest is harmless.
    pub(crate) fn gone_due(&self) -> Option<Instant> {
        let gone = self.gone?;
        let wait = self.parser.0.wait;
        let longest =
            (gone.checked_add(LONGEST_PARSE_WAIT.max(wait))).unwrap_or(gone + LONGEST_PARSE_WAIT);
        let finished = if self.parsing > 0 {
            let progress = self.parser.progress();
            if progress.busy {
                return Some(longest);
            }
            progress.finished
        } else {
            (self.parsed.lock())
                .unwrap_or_else(PoisonError::into_inner)
                .finished
        };
        let since = finished.map_or(gone, |finished| finished.max(gone));
        Some(
            since
                .checked_add(wait)
                .map_or(longest, |due| due.min(longest)),
        )
    }
}

impl Drop for Inbox {
    /// Messages parsed and not taken are freed on the parser's thread, as
    /// a big one takes a while to free.
    fn drop(&mut self) {
        if self.parsing > 0
            && let Some(jobs) = self.parser.started()
        {
            let parsed = std::mem::take(&mut self.parsed);
            let _ = jobs.send(Job::Free(Box::new(parsed)));
        }
    }
}

/// The code a message too long to read is refused with: JSON-RPC's for a
/// request that isn't one.
const TOO_LONG: i64 = -32600;

/// What the start of a message says of it: its ID and method, as far as
/// they come before what's cut off.
#[derive(Debug, Default)]
struct Head {
    id: Option<Value>,
    method: Option<String>,
}

impl Head {
    /// What `start`, the start of a message, says, read until it's cut
    /// off or breaks.
    fn of(start: &[u8]) -> Self {
        let mut head = Self::default();
        let mut reading = serde_json::Deserializer::from_slice(start);
        // An error is where it's cut off: what came before is kept.
        let _ = (&mut head).deserialize(&mut reading);
        head
    }
}

impl<'de> DeserializeSeed<'de> for &mut Head {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, reading: D) -> Result<(), D::Error> {
        reading.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for &mut Head {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON-RPC message")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        // A number is taken only once what follows it is read: cut off,
        // `123` may have been `1234`.
        let mut number = None;
        while let Some(key) = map.next_key::<String>()? {
            self.id = number.take().or(self.id.take());
            match key.as_str() {
                "id" => match map.next_value()? {
                    Value::Number(id) => number = Some(Value::Number(id)),
                    id => self.id = Some(id),
                },
                "method" => self.method = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        self.id = number.or(self.id.take());
        Ok(())
    }
}

/// What [`next_body`] found.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// A whole message: its bytes, and where what follows it starts.
    Whole(Range<usize>, usize),
    /// No whole message yet. The first `scanned` bytes were looked through
    /// for where it ends, in vain; `whole`, if it's known, is how many
    /// bytes it takes, its header and all.
    Partial {
        scanned: usize,
        whole: Option<usize>,
    },
}

/// Where the first whole message in `bytes` is, looking for its end past
/// the first `scanned` bytes, which were looked through before; or what's
/// wrong if a header has no length, or one too long, to tell where the
/// message ends.
fn next_body(framing: Framing, bytes: &[u8], scanned: usize) -> Result<Next, String> {
    match framing {
        Framing::Lines => Ok(match memchr::memchr(b'\n', &bytes[scanned..]) {
            Some(at) => Next::Whole(0..scanned + at, scanned + at + 1),
            None => Next::Partial {
                scanned: bytes.len(),
                whole: None,
            },
        }),
        Framing::Headers => {
            // The end of the headers may straddle what was looked through.
            let from = scanned.saturating_sub(3);
            let Some(at) = memchr::memmem::find(&bytes[from..], b"\r\n\r\n") else {
                return Ok(Next::Partial {
                    scanned: bytes.len(),
                    whole: None,
                });
            };
            let headers_end = from + at;
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let body = headers_end + 4;
            let length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            });
            match length {
                None => Err("a message came with no length".to_owned()),
                Some(length) if length > MAX_MESSAGE => Err(format!(
                    "a message of {length} bytes came, more than the most, {MAX_MESSAGE}"
                )),
                Some(length) => {
                    let end = body + length;
                    Ok(if bytes.len() >= end {
                        Next::Whole(body..end, end)
                    } else {
                        // Its headers are found again quickly next time,
                        // at the start.
                        Next::Partial {
                            scanned: 0,
                            whole: Some(end),
                        }
                    })
                }
            }
        }
    }
}

/// The daemon's thread for parsing big messages, started the first time
/// one comes, which also frees what's thrown away of them (see
/// [`Heavy`]). It waits for work, using nothing while there's none, and
/// ends once the daemon and everything it reads messages for have gone.
///
/// There's one, not one per program: it parses messages in the order
/// they're handed to it, so each program's come back in order, and a big
/// message from one program holds up another's only off the loop.
#[derive(Clone, Debug)]
pub(crate) struct Parser(Arc<Shared>);

#[derive(Debug)]
struct Shared {
    /// Wakes the loop once a message has been parsed.
    waker: Arc<Waker>,
    /// Where work goes, once the thread has started; `None` if it
    /// couldn't be, and then messages are parsed on the loop.
    jobs: OnceLock<Option<Sender<Job>>>,
    /// How the thread is getting on: apart from the rest, as the thread
    /// mustn't hold where its work comes from, or it would never end.
    progress: Arc<Mutex<Progress>>,
    /// How long a program that has gone is kept for what it sent last,
    /// [`PARSE_WAIT`] unless the daemon is told otherwise.
    wait: Duration,
}

/// Work for the parser's thread.
enum Job {
    /// To parse `body`, a message from `name` in `bytes`, and put it in
    /// `into`, to be freed in `trash` once the loop is done with it, if
    /// anywhere but where it's dropped.
    Parse {
        name: String,
        bytes: Vec<u8>,
        body: Range<usize>,
        trash: Option<Trash>,
        into: Arc<Mutex<Parsing>>,
    },
    /// To free what the loop threw away.
    Free(Box<dyn Send>),
    /// To do other heavy work, such as decoding a big message from
    /// another daemon (see [`Parser::run`]).
    Run(Box<dyn FnOnce() + Send>),
}

impl Parser {
    /// A parser that wakes the loop with `waker`, with no thread yet, for
    /// programs kept `wait` for what they sent last once they've gone
    /// (see [`Inbox::gone_due`]).
    pub(crate) fn new(waker: &Arc<Waker>, wait: Duration) -> Self {
        Self(Arc::new(Shared {
            waker: Arc::clone(waker),
            jobs: OnceLock::new(),
            progress: Arc::default(),
            wait,
        }))
    }

    /// Whether the thread is busy now, and when it last finished
    /// something.
    fn progress(&self) -> Progress {
        *(self.0.progress.lock()).unwrap_or_else(PoisonError::into_inner)
    }

    /// Where work for the thread goes, if it has been started.
    fn started(&self) -> Option<&Sender<Job>> {
        self.0.jobs.get().and_then(Option::as_ref)
    }

    /// Where work for the thread goes, starting it if it hasn't been.
    fn jobs(&self) -> Option<&Sender<Job>> {
        let jobs = self.0.jobs.get_or_init(|| {
            let (jobs, work) = mpsc::channel();
            let waker = Arc::clone(&self.0.waker);
            let progress = Arc::clone(&self.0.progress);
            let started = std::thread::Builder::new()
                .name("parse".to_owned())
                .spawn(move || {
                    for job in work {
                        run(job, &waker, &progress);
                    }
                });
            match started {
                Ok(_) => Some(jobs),
                Err(err) => {
                    log::warn!("big messages are parsed on the loop: no thread for them: {err}");
                    None
                }
            }
        });
        jobs.as_ref()
    }

    /// Hands `body`, a message from `name` in `bytes`, to the thread, to
    /// be put in `into` once it's parsed. Gives them back if there's no
    /// thread.
    fn parse(
        &self,
        name: &str,
        bytes: Vec<u8>,
        body: Range<usize>,
        into: &Arc<Mutex<Parsing>>,
    ) -> Result<(), (Vec<u8>, Range<usize>)> {
        let Some(jobs) = self.jobs() else {
            return Err((bytes, body));
        };
        let job = Job::Parse {
            name: name.to_owned(),
            trash: (body.len() >= BIG).then(|| Trash(jobs.clone())),
            bytes,
            body,
            into: Arc::clone(into),
        };
        jobs.send(job).map_err(|unsent| {
            log::error!("the thread parsing big messages has gone: they're parsed on the loop");
            match unsent.0 {
                Job::Parse { bytes, body, .. } => (bytes, body),
                Job::Free(_) | Job::Run(_) => unreachable!("a message was sent"),
            }
        })
    }
}

impl Parser {
    /// Does `work` on the thread, after what was handed to it before,
    /// starting it if it hasn't been: work that would hold the loop up,
    /// which says when it's done as it sees fit, as by waking the loop.
    /// Gives it back if there's no thread, to be done on the loop.
    pub(crate) fn run(
        &self,
        work: Box<dyn FnOnce() + Send>,
    ) -> Result<(), Box<dyn FnOnce() + Send>> {
        let Some(jobs) = self.jobs() else {
            return Err(work);
        };
        jobs.send(Job::Run(work)).map_err(|unsent| match unsent.0 {
            Job::Run(work) => work,
            Job::Parse { .. } | Job::Free(_) => unreachable!("work was sent"),
        })
    }
}

#[cfg(test)]
impl Parser {
    /// A parser for tests, waking a loop nobody polls.
    pub(crate) fn idle() -> Self {
        let poll = Box::leak(Box::new(mio::Poll::new().unwrap()));
        Self::new(
            &Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap()),
            PARSE_WAIT,
        )
    }
}

/// Does `job`, on the parser's thread, waking the loop with `waker` once a
/// message is ready, and saying in `progress` how it's getting on.
fn run(job: Job, waker: &Waker, progress: &Mutex<Progress>) {
    let progressed = |busy: bool| {
        let mut progress = progress.lock().unwrap_or_else(PoisonError::into_inner);
        progress.busy = busy;
        if !busy {
            progress.finished = Some(Instant::now());
        }
    };
    progressed(true);
    match job {
        Job::Parse {
            name,
            bytes,
            body,
            trash,
            into,
        } => {
            let length = body.len();
            let mut message = parse(&name, &bytes[body]);
            // Here, not on the loop, as it may be megabytes.
            drop(bytes);
            if let Some(message) = &mut message {
                message.trash = trash;
            }
            // Done before the message is handed over, so once it's been
            // taken, the thread never seems busy with it still.
            progressed(false);
            let mut parsing = into.lock().unwrap_or_else(PoisonError::into_inner);
            parsing.done.push_back((message, length));
            parsing.finished = Some(Instant::now());
            drop(parsing);
            if let Err(err) = waker.wake() {
                log::warn!("a message was parsed, but the loop can't be woken: {err}");
            }
        }
        Job::Free(thing) => {
            drop(thing);
            progressed(false);
        }
        Job::Run(work) => {
            work();
            progressed(false);
        }
    }
}

/// Frees what it's given on the parser's thread, not the loop: freeing
/// the millions of parts of a big message costs from a third to two thirds
/// as long as parsing them did.
#[derive(Clone, Debug)]
pub(crate) struct Trash(Sender<Job>);

impl Trash {
    /// Frees `thing` on the parser's thread, or here if it has gone.
    pub(crate) fn throw(&self, thing: impl Send + 'static) {
        let _ = self.0.send(Job::Free(Box::new(thing)));
    }
}

/// A value from a message, freed on the parser's thread when it's dropped
/// if the message was big enough to be parsed there; otherwise freed where
/// it's dropped, as any value is.
#[derive(Debug)]
pub(crate) struct Heavy {
    value: Value,
    trash: Option<Trash>,
}

impl Heavy {
    /// `value`, from a message whose [`trash`](Message::trash) is `trash`.
    pub(crate) fn new(value: Value, trash: Option<Trash>) -> Self {
        Self { value, trash }
    }

    /// The value itself, and where to free what's left of it once it's
    /// been used, if anywhere but where it's dropped.
    pub(crate) fn into_parts(mut self) -> (Value, Option<Trash>) {
        (std::mem::take(&mut self.value), self.trash.take())
    }
}

impl Deref for Heavy {
    type Target = Value;

    fn deref(&self) -> &Value {
        &self.value
    }
}

impl Drop for Heavy {
    fn drop(&mut self) {
        if let Some(trash) = self.trash.take() {
            trash.throw(std::mem::take(&mut self.value));
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
    use std::time::{Duration, Instant};

    use super::*;

    fn bodies(framing: Framing, mut bytes: &[u8]) -> (Vec<String>, usize) {
        let mut found = Vec::new();
        let mut used = 0;
        while let Ok(Next::Whole(body, end)) = next_body(framing, bytes, 0) {
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
        assert!(next_body(Framing::Headers, b"X: 1\r\n\r\n{}", 0).is_err());
        let huge = format!("Content-Length: {}\r\n\r\n", MAX_MESSAGE + 1);
        assert!(next_body(Framing::Headers, huge.as_bytes(), 0).is_err());
    }

    /// A message numbered `id`, at least `length` bytes long.
    fn message(id: u64, length: usize) -> String {
        let pad = "x".repeat(length);
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":"{pad}"}}"#)
    }

    /// `messages`, framed as `framing` says, one after another.
    fn stream(framing: Framing, messages: &[String]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for message in messages {
            frame_body(framing, message, &mut bytes);
        }
        bytes
    }

    /// The messages in `bytes`, pushed into `inbox` `chunk` bytes at a
    /// time, as reads bring them, once `count` have come.
    fn take_all(inbox: &mut Inbox, bytes: &[u8], chunk: usize, count: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        for piece in bytes.chunks(chunk) {
            inbox.take(&mut messages, usize::MAX);
            inbox.push("test", piece, &mut messages, |_| {}).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while messages.len() < count {
            assert!(Instant::now() < deadline, "only {} came", messages.len());
            std::thread::sleep(Duration::from_millis(1));
            inbox.take(&mut messages, usize::MAX);
        }
        assert!(!inbox.parsing(), "all taken back");
        messages
    }

    #[test]
    fn messages_come_in_the_order_they_were_sent_however_big() {
        let lengths = [10, BIG, 10, 10, 2 * BIG, 10, BIG, BIG];
        let sent: Vec<String> = (0..)
            .zip(lengths)
            .map(|(id, len)| message(id, len))
            .collect();
        let parser = Parser::idle();
        for framing in [Framing::Lines, Framing::Headers] {
            let bytes = stream(framing, &sent);
            // Reads of any size: a few bytes, so a header's end is split
            // every way; a read's worth; or all at once.
            for chunk in [7, 64 * 1024, bytes.len()] {
                let mut inbox = Inbox::new(framing, &parser);
                let taken = take_all(&mut inbox, &bytes, chunk, sent.len());
                let ids: Vec<u64> = (taken.iter())
                    .map(|message| message.id.as_ref().and_then(Value::as_u64).unwrap())
                    .collect();
                assert_eq!(ids, [0, 1, 2, 3, 4, 5, 6, 7], "{framing:?} by {chunk}");
                // Only the big ones are freed off the loop.
                for (message, length) in taken.iter().zip(lengths) {
                    assert_eq!(message.trash.is_some(), length >= BIG);
                }
            }
        }
    }

    #[test]
    fn what_was_parsed_is_taken_a_budget_at_a_time() {
        // Three big messages and a small one behind them, all parsed
        // before any is taken.
        let sent = [
            message(1, BIG),
            message(2, BIG),
            message(3, BIG),
            message(4, 10),
        ];
        let bytes = stream(Framing::Lines, &sent);
        let mut inbox = Inbox::new(Framing::Lines, &Parser::idle());
        let mut messages = Vec::new();
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        assert!(messages.is_empty());
        let deadline = Instant::now() + Duration::from_secs(30);
        while inbox.parsed.lock().unwrap().done.len() < sent.len() {
            assert!(Instant::now() < deadline, "never parsed");
            std::thread::sleep(Duration::from_millis(1));
        }
        // A small budget takes one message, however big, and says how
        // big; a big one takes as many as it covers.
        let first = inbox.take(&mut messages, 1);
        assert_eq!((messages.len(), first), (1, sent[0].len()));
        let next = inbox.take(&mut messages, sent[1].len() + 1);
        assert_eq!((messages.len(), next), (3, sent[1].len() + sent[2].len()));
        assert!(inbox.parsing(), "one still to take");
        assert_eq!(inbox.take(&mut messages, 1), sent[3].len());
        let ids: Vec<u64> = (messages.iter())
            .map(|message| message.id.as_ref().and_then(Value::as_u64).unwrap())
            .collect();
        assert_eq!(ids, [1, 2, 3, 4]);
        assert!(!inbox.parsing());
        assert_eq!(inbox.take(&mut messages, 1), 0);
    }

    #[test]
    fn a_message_parsed_off_the_loop_wakes_it() {
        let mut poll = mio::Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), mio::Token(7)).unwrap());
        let mut inbox = Inbox::new(Framing::Headers, &Parser::new(&waker, PARSE_WAIT));
        let mut messages = Vec::new();
        let bytes = stream(Framing::Headers, &[message(1, BIG), message(2, 10)]);
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        assert!(messages.is_empty(), "the small one waits behind the big");
        let mut events = mio::Events::with_capacity(4);
        let deadline = Instant::now() + Duration::from_secs(30);
        while messages.len() < 2 {
            assert!(Instant::now() < deadline, "never woken");
            poll.poll(&mut events, Some(Duration::from_secs(1)))
                .unwrap();
            if events.iter().any(|event| event.token() == mio::Token(7)) {
                inbox.take(&mut messages, usize::MAX);
            }
        }
    }

    #[test]
    fn a_long_message_is_looked_through_once() {
        let parser = Parser::idle();
        let mut messages = Vec::new();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        for _ in 0..10 {
            inbox
                .push("test", &[b'x'; 1000], &mut messages, |_| {})
                .unwrap();
            assert_eq!(inbox.scanned, inbox.bytes.len());
        }
        inbox.push("test", b"\n", &mut messages, |_| {}).unwrap();
        assert_eq!((inbox.scanned, inbox.bytes.len()), (0, 0));
        // With headers, room for the whole message is made once its
        // length is known.
        let mut inbox = Inbox::new(Framing::Headers, &parser);
        let bytes = stream(Framing::Headers, &[message(1, 100_000)]);
        inbox
            .push("test", &bytes[..1000], &mut messages, |_| {})
            .unwrap();
        assert!(inbox.bytes.capacity() >= bytes.len());
        // And headers not yet ended are looked through once.
        let mut inbox = Inbox::new(Framing::Headers, &parser);
        let header = b"Content-Length: 2\r\n";
        inbox.push("test", header, &mut messages, |_| {}).unwrap();
        assert_eq!(inbox.scanned, header.len());
        inbox
            .push("test", b"\r\n{}", &mut messages, |_| {})
            .unwrap();
        assert!(inbox.bytes.is_empty(), "found, though it straddled reads");
    }

    #[test]
    fn messages_past_the_most_are_refused() {
        let parser = Parser::idle();
        let mut messages = Vec::new();
        // A language server's: what follows can't be found.
        let mut inbox = Inbox::new(Framing::Headers, &parser);
        let huge = format!("Content-Length: {}\r\n\r\n{{", MAX_MESSAGE + 1);
        let pushed = inbox.push("test", huge.as_bytes(), &mut messages, |_| {});
        assert_eq!(pushed.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut inbox = Inbox::new(Framing::Headers, &parser);
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..MAX_MESSAGE / chunk.len() {
            inbox.push("test", &chunk, &mut messages, |_| {}).unwrap();
        }
        let pushed = inbox.push("test", b"x", &mut messages, |_| {});
        assert!(pushed.is_err(), "a header never ended");
        // An agent's line: dropped, and the next one read.
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        for _ in 0..MAX_MESSAGE / chunk.len() {
            inbox.push("test", &chunk, &mut messages, |_| {}).unwrap();
        }
        inbox.push("test", b"x", &mut messages, |_| {}).unwrap();
        assert!(inbox.bytes.is_empty());
        let next = format!("xx\n{}\n", message(5, 10));
        inbox
            .push("test", next.as_bytes(), &mut messages, |_| {})
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, Some(Value::from(5)));
    }

    /// What `inbox` makes of a line starting `start`, as long as `length`
    /// all told, too long to read, and the message after it.
    fn too_long(inbox: &mut Inbox, start: &str, length: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        inbox
            .push("test", start.as_bytes(), &mut messages, |_| {})
            .unwrap();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut left = length - start.len();
        while left > 0 {
            let piece = &chunk[..left.min(chunk.len())];
            inbox.push("test", piece, &mut messages, |_| {}).unwrap();
            left -= piece.len();
        }
        let next = format!("\n{}\n", message(9, 10));
        inbox
            .push("test", next.as_bytes(), &mut messages, |_| {})
            .unwrap();
        messages
    }

    #[test]
    fn a_line_too_long_to_read_is_refused_in_its_place() {
        let parser = Parser::idle();
        let length = MAX_MESSAGE + 1000;
        let why = format!("a message of {length} bytes came, more than the most, {MAX_MESSAGE}");
        // A request: refused, so its sender hears.
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let start =
            r#"{"jsonrpc":"2.0","id":3,"method":"fs/write_text_file","params":{"content":""#;
        let messages = too_long(&mut inbox, start, length);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].id, Some(Value::from(3)));
        assert_eq!(messages[0].method.as_deref(), Some("fs/write_text_file"));
        let error = messages[0].error.as_ref().unwrap();
        assert_eq!(
            (error.code, error.message.as_str()),
            (TOO_LONG, why.as_str())
        );
        assert_eq!(messages[1].id, Some(Value::from(9)), "and the next is read");
        // An answer: taken as an error, so the asker doesn't wait.
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let start = r#"{"jsonrpc":"2.0","id":"a","result":{"text":""#;
        let messages = too_long(&mut inbox, start, length);
        assert_eq!(messages[0].id, Some(Value::from("a")));
        assert_eq!(messages[0].method, None);
        assert!(messages[0].error.is_some());
        // With no ID before what's cut off, there's no telling: dropped.
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let start = r#"{"jsonrpc":"2.0","result":{"text":""#;
        let messages = too_long(&mut inbox, start, length);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, Some(Value::from(9)));
    }

    #[test]
    fn the_start_of_a_message_says_what_it_can() {
        let head = Head::of(br#"{"method":"m","params":{"a":[1,{"b":"\"}"#);
        assert_eq!((head.id, head.method.as_deref()), (None, Some("m")));
        let head = Head::of(br#"{"jsonrpc":"2.0","params":{"a":[]},"id":"x\"y","res"#);
        assert_eq!(head.id, Some(Value::from("x\"y")));
        // A number cut off may be the start of a longer one.
        let head = Head::of(br#"{"method":"m","id":12"#);
        assert_eq!(head.id, None);
        let head = Head::of(br#"{"id":12,"params":{"text":"#);
        assert_eq!(head.id, Some(Value::from(12)));
        let head = Head::of(br#"{"id":12}"#);
        assert_eq!(head.id, Some(Value::from(12)));
        let head = Head::of(b"xxxx");
        assert_eq!((head.id, head.method), (None, None));
    }

    #[test]
    fn a_program_gone_is_kept_for_what_it_sent_last() {
        let parser = Parser::idle();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let mut messages = Vec::new();
        assert!(inbox.gone(), "nothing to wait for");
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let bytes = stream(Framing::Lines, &[message(1, BIG)]);
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        assert!(!inbox.gone());
        assert!(inbox.has_gone());
        assert!(inbox.gone_due().unwrap() > Instant::now() + PARSE_WAIT / 2);
        let deadline = Instant::now() + Duration::from_secs(30);
        while messages.is_empty() {
            assert!(Instant::now() < deadline, "never parsed");
            std::thread::sleep(Duration::from_millis(1));
            inbox.take(&mut messages, usize::MAX);
        }
        assert!(!parser.progress().busy, "the thread says it's done");
        let finished = (inbox.parsed.lock().unwrap().finished).expect("the thread says when");
        assert!(inbox.gone(), "what it sent last has come");
        // The rest is still read for a while, if it, or what it started,
        // writes on, as it may have after the big message.
        let gone = inbox.gone.unwrap();
        assert_eq!(inbox.gone_due(), Some(finished.max(gone) + PARSE_WAIT));
    }

    #[test]
    fn a_program_gone_is_kept_while_the_parser_gets_on_with_what_it_sent() {
        let ago = |wait: Duration| Instant::now().checked_sub(wait).unwrap();
        let mut inbox = Inbox::new(Framing::Lines, &Parser::idle());
        // As though a message had been handed to the thread.
        inbox.parsing = 1;
        assert!(!inbox.gone());
        let gone = inbox.gone.unwrap();
        // Behind something else on the thread: from when it went.
        assert_eq!(inbox.gone_due(), Some(gone + PARSE_WAIT));
        inbox.gone = Some(ago(PARSE_WAIT));
        assert!(inbox.gone(), "the thread isn't getting to it");
        // The thread busy, with one of its messages, or another program's
        // it waits behind, however long that takes on a busy machine: up
        // to the longest wait.
        let parser = Parser::idle();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        inbox.parsing = 1;
        parser.0.progress.lock().unwrap().busy = true;
        inbox.gone = Some(ago(PARSE_WAIT * 2));
        assert!(!inbox.gone(), "it's being parsed, or getting closer");
        let gone = inbox.gone.unwrap();
        assert_eq!(inbox.gone_due(), Some(gone + LONGEST_PARSE_WAIT));
        inbox.gone = Some(ago(LONGEST_PARSE_WAIT));
        assert!(inbox.gone(), "a thread stuck keeps it no longer");
        // With nothing of its own there, a busy thread doesn't keep it.
        let mut idle = Inbox::new(Framing::Lines, &parser);
        idle.gone = Some(ago(PARSE_WAIT * 2));
        assert!(idle.gone());
        assert_eq!(idle.gone_due(), Some(idle.gone.unwrap() + PARSE_WAIT));
        // The thread finished something a moment ago, with more of its
        // own to come: from then.
        let parser = Parser::idle();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        inbox.parsing = 2;
        inbox.gone = Some(ago(PARSE_WAIT * 2));
        let finished = ago(PARSE_WAIT / 4);
        parser.0.progress.lock().unwrap().finished = Some(finished);
        assert!(!inbox.gone(), "the thread is getting on");
        assert_eq!(inbox.gone_due(), Some(finished + PARSE_WAIT));
        parser.0.progress.lock().unwrap().finished = Some(ago(PARSE_WAIT));
        assert!(inbox.gone(), "the thread hasn't got on since");
        // Getting on however long: still no longer than the longest wait.
        inbox.gone = Some(ago(LONGEST_PARSE_WAIT));
        parser.0.progress.lock().unwrap().finished = Some(Instant::now());
        assert!(inbox.gone());
        // All taken a moment ago, long after it went: what it sent after,
        // held in its pipe while the thread caught up, is still read, so
        // it isn't let go yet, though it would be if the read came to an
        // end.
        let mut inbox = Inbox::new(Framing::Lines, &Parser::idle());
        inbox.gone = Some(ago(PARSE_WAIT * 2));
        let finished = ago(PARSE_WAIT / 4);
        inbox.parsed.lock().unwrap().finished = Some(finished);
        assert_eq!(inbox.gone_due(), Some(finished + PARSE_WAIT));
        assert!(inbox.gone(), "nothing more is with the thread");
    }

    #[test]
    fn a_parser_thread_gone_leaves_nothing_waiting_for_it() {
        let mut poll = mio::Poll::new().unwrap();
        let waker = Arc::new(Waker::new(poll.registry(), mio::Token(0)).unwrap());
        let (jobs, work) = mpsc::channel();
        drop(work);
        let parser = Parser(Arc::new(Shared {
            waker,
            jobs: OnceLock::from(Some(jobs)),
            progress: Arc::default(),
            wait: PARSE_WAIT,
        }));
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        // As though messages had been handed to it before it went.
        (inbox.parsing, inbox.parsing_bytes) = (2, MAX_PARSING);
        let mut messages = Vec::new();
        let bytes = stream(Framing::Lines, &[message(1, BIG), message(2, 10)]);
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        assert_eq!(messages.len(), 2, "parsed here");
        assert!(!inbox.full());
        assert!(!inbox.parsing());
        let _ = poll.poll(&mut mio::Events::with_capacity(1), Some(Duration::ZERO));
    }

    #[test]
    fn messages_parsed_and_never_taken_are_freed() {
        let parser = Parser::idle();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let mut messages = Vec::new();
        let bytes = stream(Framing::Lines, &[message(1, BIG)]);
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !inbox.parsed() {
            assert!(Instant::now() < deadline, "never parsed");
            std::thread::sleep(Duration::from_millis(1));
        }
        let parsed = Arc::downgrade(&inbox.parsed);
        drop(inbox);
        // Handed to the parser's thread, which lets them go.
        while parsed.upgrade().is_some() {
            assert!(Instant::now() < deadline, "never freed");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn nothing_more_is_read_while_the_parser_is_too_far_behind() {
        let parser = Parser::idle();
        let mut inbox = Inbox::new(Framing::Lines, &parser);
        let mut messages = Vec::new();
        // As long as a message may be.
        let longest = message(1, MAX_PARSING - message(1, 0).len());
        let bytes = stream(Framing::Lines, &[longest]);
        inbox.push("test", &bytes, &mut messages, |_| {}).unwrap();
        assert!(inbox.full());
        let deadline = Instant::now() + Duration::from_mins(1);
        while messages.is_empty() {
            assert!(Instant::now() < deadline, "never parsed");
            std::thread::sleep(Duration::from_millis(1));
            inbox.take(&mut messages, usize::MAX);
        }
        assert!(!inbox.full());
    }

    #[test]
    fn what_is_thrown_away_is_freed_on_the_parser_thread() {
        struct Spy(mpsc::Sender<Option<String>>);
        impl Drop for Spy {
            fn drop(&mut self) {
                let thread = std::thread::current().name().map(str::to_owned);
                self.0.send(thread).unwrap();
            }
        }
        let parser = Parser::idle();
        let trash = Trash(parser.jobs().unwrap().clone());
        let (spy, freed) = mpsc::channel();
        trash.throw(Spy(spy));
        let freed = freed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(freed.as_deref(), Some("parse"));
        // Taken out of its wrapper, a value is its taker's to free.
        let (value, trash) = Heavy::new(Value::from(1), Some(trash)).into_parts();
        assert_eq!(value, Value::from(1));
        assert!(trash.is_some());
    }

    #[test]
    fn a_flood_of_stderr_is_read_a_budget_at_a_time() {
        let flood = [
            "sh",
            "-c",
            "head -c 4000000 /dev/zero | tr '\\0' x >&2; sleep 5",
        ];
        let command: Vec<OsString> = flood.iter().map(OsString::from).collect();
        let parser = Parser::idle();
        let mut peer = Peer::start("flood", &command, None, None, Framing::Lines, &parser).unwrap();
        let mut buffer = vec![0; 64 * 1024];
        let deadline = Instant::now() + Duration::from_secs(5);
        // Once a budget's worth is waiting, a read stops there, saying
        // there's more.
        while !peer.read_errors(&mut buffer, 1024) {
            assert!(Instant::now() < deadline, "never more than a budget");
            std::thread::sleep(Duration::from_millis(5));
        }
        peer.kill();
        while peer.try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn nothing_is_written_to_a_program_gone_nor_done_for_it() {
        let said = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"fs/write_text_file"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"told"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"result":3}"#,
            "\n",
        );
        let script = format!("printf '%s' '{said}'; sleep 5");
        let command: Vec<OsString> = ["sh", "-c", &script].iter().map(OsString::from).collect();
        let parser = Parser::idle();
        let mut peer = Peer::start("gone", &command, None, None, Framing::Lines, &parser).unwrap();
        peer.send(&json!({ "jsonrpc": "2.0", "method": "hello" }));
        assert!(peer.has_outgoing());
        // As when writing to it failed: what's queued is thrown away, and
        // nothing more is queued.
        peer.gone();
        assert!(!peer.has_outgoing());
        peer.send(&json!({ "jsonrpc": "2.0", "method": "hello" }));
        assert!(!peer.has_outgoing());
        // What it tells, and its answers, are taken in; what it asks
        // isn't, as no answer could reach it.
        let mut buffer = vec![0; 64 * 1024];
        let mut messages = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while messages.len() < 2 {
            assert!(Instant::now() < deadline, "never said: {messages:?}");
            std::thread::sleep(Duration::from_millis(5));
            peer.read(&mut buffer, usize::MAX, &mut messages).unwrap();
        }
        let said: Vec<(Option<&str>, Option<&Value>)> = (messages.iter())
            .map(|message| (message.method.as_deref(), message.id.as_ref()))
            .collect();
        assert_eq!(said, [(Some("told"), None), (None, Some(&Value::from(2)))]);
        peer.kill();
        while peer.try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
