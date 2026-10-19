//! Connections forwarded from windows on other machines (see
//! [`Forward`]): each is a QUIC stream a window opened, and the TCP
//! connection on this machine it asked for, with bytes passed between
//! them both ways.
//!
//! Nothing here holds up the loop:
//!
//! - **Connecting doesn't wait.** mio's connect returns at once, and the
//!   socket says when it's connected, or why not. Each address gets
//!   [`CONNECT_WAIT`], the one timer here, which exists only while a
//!   connection is being made.
//! - **Only `localhost` and addresses are known without looking them
//!   up.** `localhost` is 127.0.0.1, then `::1`, as dev servers listen on
//!   either. Any other name is looked up off the loop (see [`lookups`]),
//!   which wakes it with the answer, so a slow resolver never stops it. A
//!   name gets [`CONNECT_WAIT`] to be found, as an address does to answer.
//! - **Bytes move at the end of the turn**, as floods are read: after
//!   every update has gone, [`TURN_BUDGET`] at a time, and no more than
//!   [`PIPE_BUDGET`] for one connection, so many forwarded downloads share
//!   the turn and a key's echo never waits behind them. What's left waits
//!   for the next turn, which doesn't wait for an event while any is.
//! - **Bytes go to the window only as fast as the link takes them**
//!   without a queue growing (see [`Pace`]): a download never fills a
//!   slow link's queue ahead of keys. A connection held back waits for a
//!   timer, which exists only while one is.
//!
//! Each side is read only as fast as the other takes it: at most
//! [`CHUNK`] waits here each way. A browser that reads slowly leaves the
//! window not reading the stream, so QUIC tells the daemon to send no
//! more, so the daemon stops reading the socket, and the server is held
//! back by TCP. A server that reads slowly does the same the other way.
//! The streams send below everything else (see
//! [`FORWARD_PRIORITY`](crate::quic::FORWARD_PRIORITY)).
//!
//! An end finishing its side is passed on as the other's finishing: a
//! socket's end of file finishes the stream, and the stream finished
//! shuts the socket for writing. A connection reset, or a stream
//! abandoned, abandons the other, resetting a socket rather than closing
//! it, so a browser knows a page was cut short rather than ended.
//!
//! Nothing costs anything while nothing is forwarded: no timer, no thread,
//! and nothing done each turn but seeing there's nothing to do.
//!
//! An upgrade doesn't carry them over: the QUIC connections close as the
//! new build starts, and the sockets with them. The window connects again
//! and forwards new connections to the new build.
//!
//! A stream may instead ask to reach the daemon on another machine
//! ([`Request::Reach`]): a daemon connected to this one connecting
//! through it to a machine it passes on (see [`chain`](super::chain)).
//! Then the socket is a carrier on this daemon's own connection to that
//! machine, the request is passed on, and the stream is answered with
//! what comes back; or, for this daemon itself, the socket is a new
//! client of its own, as `clackatoo bridge` would make. Either way, the
//! bytes then go as a forwarded connection's do, but unpaced and beside
//! everything else rather than below it, as they're keys and views.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::FrameDecoder;
use clackatoo_protocol::{
    ClientMessage, Error, Forward, ForwardFailure, MachineId, Request, Response, ServerMessage,
    frame, wire,
};
use mio::net::{TcpStream, UnixStream};
use mio::{Interest, Token};
use quinn_proto::{ConnectionHandle, StreamId};

use super::{Daemon, Source};
use crate::quic::{End, Forwarded, Pace};
use lookups::Lookups;

mod lookups;

/// The most of a connection's bytes that wait here each way, for the
/// other side to take them.
const CHUNK: usize = 64 * 1024;

/// The most bytes moved for one connection in a turn.
const PIPE_BUDGET: usize = 32 * 1024;

/// The most bytes moved for all of them in a turn.
const TURN_BUDGET: usize = 64 * 1024;

/// The longest a request to connect may be: a name of at most 253 bytes,
/// and a port, with room to spare.
const MOST_ASKED: usize = 1024;

/// The longest answer to a stream passed on to reach another machine that
/// the daemon passing it on reads: an error's words, with room to spare.
const MOST_ANSWER: usize = 4096;

/// How long each address may take to answer before the next is tried, or
/// the connection is given up on.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// The forwarded connections.
#[derive(Debug, Default)]
pub(super) struct Forwards {
    /// Each one's token, by the QUIC connection and stream it came on.
    streams: HashMap<(ConnectionHandle, StreamId), Token>,
    /// Those with something to do, each once, in the order they came.
    ready: VecDeque<Token>,
    /// When each being looked up, or connected, gives up on the name or
    /// the address it's waiting on.
    deadlines: Vec<(Instant, Token)>,
    /// Looks names up, once one has been asked for.
    lookups: Option<Lookups>,
    /// The connections waiting for each name to be looked up.
    waiting: HashMap<String, Vec<Token>>,
    /// The streams of connections that are over but whose last bytes
    /// QUIC hasn't had acknowledged: they still count toward their
    /// connection's send window (see [`crate::quic::send_window`]).
    lingering: HashSet<(ConnectionHandle, StreamId)>,
    /// How fast each QUIC connection with forwards may be written to.
    paces: HashMap<ConnectionHandle, Pace>,
    /// The connections waiting for their pace to allow more.
    parked: Vec<Token>,
}

/// One forwarded connection.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "how far each way has got, two steps each"
)]
pub(super) struct Pipe {
    handle: ConnectionHandle,
    stream: StreamId,
    /// The protocol version its client talks at, which its answer is
    /// written at.
    wire: u32,
    stage: Stage,
    /// The TCP connection, once one is being made; or the socket the
    /// daemon it reaches is behind.
    socket: Option<PipeSocket>,
    /// The window asked for what comes from the socket to go as fast as
    /// it can, rather than as background traffic.
    unpaced: bool,
    /// The machine it reaches, for one asked to reach a daemon.
    pub(super) reach: Option<MachineId>,
    /// The connection it's carried on through, by its host, for one
    /// reaching another machine than this.
    via: Option<String>,
    /// What the window sent, waiting for the socket to take it: the
    /// request first, until it's whole.
    to_socket: Vec<u8>,
    /// What the socket sent, waiting for the stream to take it: the
    /// answer first.
    to_stream: Vec<u8>,
    /// The window finished its side of the stream.
    window_done: bool,
    /// The socket is shut for writing, after the window finished.
    socket_shut: bool,
    /// The socket's other end finished its side.
    socket_done: bool,
    /// The stream is finished, after the socket's other end did.
    stream_done: bool,
    /// QUIC has had everything sent on the stream acknowledged.
    acked: bool,
    /// Whether it's waiting in [`Forwards::ready`].
    queued: bool,
}

/// How far a forwarded connection has got.
#[derive(Debug)]
enum Stage {
    /// Reading the request, which is the stream's first message.
    Asking,
    /// Waiting for the host's name to be looked up.
    Resolving { host: String, port: u16 },
    /// Connecting to each address in turn: the socket is the one being
    /// tried.
    Connecting {
        addresses: VecDeque<SocketAddr>,
        /// Why the last address failed, if one did.
        last: Option<ForwardFailure>,
    },
    /// Waiting for the daemon on another machine to answer the request
    /// passed on to it, in protocol `wire`: what has come of its answer.
    Reaching { answer: Vec<u8>, wire: u32 },
    /// Connected: bytes go both ways.
    Open,
}

/// A forwarded connection's socket: a TCP connection made where it asked,
/// or one the daemon it reaches is behind.
#[derive(Debug)]
enum PipeSocket {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl PipeSocket {
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.shutdown(how),
            Self::Unix(socket) => socket.shutdown(how),
        }
    }

    fn take_error(&self) -> io::Result<Option<io::Error>> {
        match self {
            Self::Tcp(socket) => socket.take_error(),
            Self::Unix(socket) => socket.take_error(),
        }
    }
}

impl Read for PipeSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(buf),
            Self::Unix(socket) => socket.read(buf),
        }
    }
}

impl Write for PipeSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.write(buf),
            Self::Unix(socket) => socket.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Where a stream asking to reach a daemon goes (see
/// [`Request::Reach`]).
#[derive(Debug)]
pub(super) enum Reaching {
    /// To this daemon itself: the other end of `socket` is a new client.
    Here(std::os::unix::net::UnixStream),
    /// On, through `socket`, a carrier on this daemon's connection to the
    /// machine, `via`, which is sent `request`, in protocol `wire`, first.
    On {
        socket: std::os::unix::net::UnixStream,
        request: Vec<u8>,
        wire: u32,
        via: String,
    },
}

/// What the loop does with a forwarded connection next.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Nothing until it hears of something.
    Wait,
    /// It used this much of its budget, with more to do: it's done again
    /// next turn.
    Again(usize),
    /// It used this much, and waits.
    Used(usize),
    /// It used this much, and waits for its pace to allow more.
    Paced(usize),
    /// Connect where it asked.
    Connect(Forward),
    /// Reach the daemon on this machine.
    Reach(MachineId),
    /// It's connected: its address's time runs no more, and it's done
    /// again next turn, to start passing bytes.
    Connected,
    /// Try the next address, after the last failed this way, if it did.
    Retry(Option<ForwardFailure>),
    /// Tell the window it can't be made, and why, and forget it.
    Fail(ForwardFailure),
    /// The same, in words of the daemon's own.
    Refuse(Error),
    /// Both sides have finished: forget it.
    Done,
    /// Abandon it both ways, and forget it.
    Abandon,
}

impl Pipe {
    fn new(handle: ConnectionHandle, stream: StreamId, wire: u32) -> Self {
        Self {
            handle,
            stream,
            wire,
            stage: Stage::Asking,
            socket: None,
            unpaced: false,
            reach: None,
            via: None,
            to_socket: Vec::new(),
            to_stream: Vec::new(),
            window_done: false,
            socket_shut: false,
            socket_done: false,
            stream_done: false,
            acked: false,
            queued: false,
        }
    }

    /// Whether the window has abandoned the stream, as when its browser's
    /// connection was reset, while the daemon wasn't reading it.
    fn gone(&self, quic: &mut crate::quic::QuicServer) -> bool {
        quic.forward_gone(self.handle, self.stream, self.window_done)
    }

    /// Reads the request, once it's all come.
    fn ask(&mut self, quic: &mut crate::quic::QuicServer) -> Next {
        match quic.read_forwarded(self.handle, self.stream, &mut self.to_socket, MOST_ASKED) {
            Forwarded::Gone => return Next::Abandon,
            Forwarded::Finished => self.window_done = true,
            Forwarded::Open { .. } => {}
        }
        let Some(header) = self.to_socket.first_chunk::<4>() else {
            return if self.window_done {
                Next::Abandon
            } else {
                Next::Wait
            };
        };
        let len = usize::try_from(u32::from_le_bytes(*header)).unwrap_or(usize::MAX);
        let Some(end) = len.checked_add(4).filter(|&end| end <= MOST_ASKED) else {
            return Next::Fail(ForwardFailure::Other("the request was too long".into()));
        };
        if self.to_socket.len() < end {
            return if self.window_done {
                Next::Abandon
            } else {
                Next::Wait
            };
        }
        // What came after it is the connection's first bytes.
        let rest = self.to_socket.split_off(end);
        let mut decoder = FrameDecoder::new();
        decoder.push(&self.to_socket);
        self.to_socket = rest;
        match decoder.decode::<ClientMessage>(self.wire) {
            Ok(Some(ClientMessage::Request(Request::Forward(forward)))) => Next::Connect(forward),
            Ok(Some(ClientMessage::Request(Request::Reach { machine })))
                if wire::daemon_has(self.wire, wire::CHAIN) =>
            {
                Next::Reach(machine)
            }
            _ => Next::Fail(ForwardFailure::Other(
                "a forwarded connection's stream must start by saying where it goes".into(),
            )),
        }
    }

    /// Passes the request on to the daemon on another machine, and reads
    /// its answer, once it has all come: the stream's answer is the same,
    /// in the window's protocol.
    fn reaching(&mut self, quic: &mut crate::quic::QuicServer, scratch: &mut [u8]) -> Next {
        if self.gone(quic) {
            return Next::Abandon;
        }
        let Some(socket) = &mut self.socket else {
            return Next::Abandon;
        };
        let Stage::Reaching { answer, wire } = &mut self.stage else {
            return Next::Wait;
        };
        while !self.to_socket.is_empty() {
            match socket.write(&self.to_socket) {
                Ok(written) => {
                    self.to_socket.drain(..written);
                }
                Err(err) if is_wait(&err) => break,
                Err(_) => return Next::Abandon,
            }
        }
        loop {
            let room = MOST_ANSWER.saturating_sub(answer.len()).min(scratch.len());
            if room == 0 {
                break;
            }
            match socket.read(&mut scratch[..room]) {
                Ok(0) => {
                    return Next::Refuse(Error::Other(
                        "the connection there ended before the daemon answered".into(),
                    ));
                }
                Ok(read) => answer.extend_from_slice(&scratch[..read]),
                Err(err) if is_wait(&err) => break,
                Err(_) => return Next::Abandon,
            }
        }
        let Some(header) = answer.first_chunk::<4>() else {
            return Next::Wait;
        };
        let len = usize::try_from(u32::from_le_bytes(*header)).unwrap_or(usize::MAX);
        let Some(end) = len.checked_add(4).filter(|&end| end <= MOST_ANSWER) else {
            return Next::Refuse(Error::Other("the daemon there answered at length".into()));
        };
        if answer.len() < end {
            return Next::Wait;
        }
        let rest = answer.split_off(end);
        let mut decoder = FrameDecoder::new();
        decoder.push(answer);
        let said = decoder.decode::<ServerMessage>(*wire);
        match said {
            Ok(Some(ServerMessage::Response(Response::Done))) => {
                let done = ServerMessage::Response(Response::Done);
                match frame::encode(self.wire, &done) {
                    Ok(mut bytes) => {
                        bytes.extend_from_slice(&rest);
                        self.to_stream = bytes;
                        self.stage = Stage::Open;
                        Next::Connected
                    }
                    Err(err) => {
                        log::error!("encoding a reached daemon's answer failed: {err}");
                        Next::Abandon
                    }
                }
            }
            Ok(Some(ServerMessage::Response(Response::Error(err)))) => Next::Refuse(err),
            _ => Next::Refuse(Error::Other(
                "the daemon there answered with what isn't an answer".into(),
            )),
        }
    }

    /// Sees whether the address being tried has answered.
    fn check_connected(&mut self) -> Next {
        let Some(socket) = &self.socket else {
            return Next::Retry(None);
        };
        match socket.take_error() {
            Ok(Some(err)) | Err(err) => return Next::Retry(Some(failure(&err))),
            Ok(None) => {}
        }
        let PipeSocket::Tcp(tcp) = socket else {
            return Next::Retry(None);
        };
        match tcp.peer_addr() {
            Ok(_) => {
                self.stage = Stage::Open;
                let answer = ServerMessage::Response(Response::Done);
                match frame::encode(self.wire, &answer) {
                    Ok(bytes) => {
                        self.to_stream = bytes;
                        Next::Connected
                    }
                    Err(err) => {
                        log::error!("encoding a forwarded connection's answer failed: {err}");
                        Next::Abandon
                    }
                }
            }
            // Not yet: its event was for something else.
            Err(err) if err.kind() == io::ErrorKind::NotConnected => Next::Wait,
            Err(err) => Next::Retry(Some(failure(&err))),
        }
    }

    /// Passes bytes both ways, up to `budget` of them, those to the
    /// window as fast as `pace` allows, unless the window asked for them
    /// not to be paced, using `scratch` to read the socket into.
    fn relay(
        &mut self,
        quic: &mut crate::quic::QuicServer,
        budget: usize,
        pace: &mut Pace,
        scratch: &mut [u8],
    ) -> Next {
        let mut pace = (!self.unpaced).then_some(pace);
        // Stopped by the window, it never reads what's sent, however
        // quiet the socket is.
        if !self.stream_done && quic.forward_stopped(self.handle, self.stream) {
            return Next::Abandon;
        }
        let Some(socket) = &mut self.socket else {
            return Next::Abandon;
        };
        // Reset after it finished its side, which nothing reads any more
        // to find out.
        if !matches!(socket.take_error(), Ok(None)) {
            return Next::Abandon;
        }
        let mut used = 0;
        // Whether the pace held back what's waiting for the window.
        let mut paced = false;
        loop {
            let before = used;
            // From the window to the socket, each step taking no more
            // than what's left of the budget.
            if !self.to_socket.is_empty() {
                let most = self.to_socket.len().min(budget - used);
                match socket.write(&self.to_socket[..most]) {
                    Ok(written) => {
                        self.to_socket.drain(..written);
                        used += written;
                    }
                    Err(err) if is_wait(&err) => {}
                    Err(err) => {
                        log::debug!("writing to a forwarded connection failed: {err}");
                        return Next::Abandon;
                    }
                }
            }
            if !self.window_done && self.to_socket.len() < CHUNK && used < budget {
                let had = self.to_socket.len();
                let most = CHUNK.min(had + budget - used);
                match quic.read_forwarded(self.handle, self.stream, &mut self.to_socket, most) {
                    Forwarded::Gone => return Next::Abandon,
                    Forwarded::Finished => self.window_done = true,
                    Forwarded::Open { .. } => {}
                }
                used += self.to_socket.len() - had;
            }
            if self.window_done && self.to_socket.is_empty() && !self.socket_shut {
                // Already gone if its other end has closed it.
                let _ = socket.shutdown(Shutdown::Write);
                self.socket_shut = true;
            }
            // From the socket to the window.
            if !self.to_stream.is_empty() && used < budget {
                let wanted = self.to_stream.len().min(budget - used);
                let most = pace.as_deref().map_or(wanted, |pace| pace.most(wanted));
                let written = if most == 0 {
                    0
                } else {
                    let to = &self.to_stream[..most];
                    let Some(written) = quic.write_forwarded(self.handle, self.stream, to) else {
                        return Next::Abandon;
                    };
                    written
                };
                if let Some(pace) = pace.as_deref_mut() {
                    pace.wrote(written, wanted);
                }
                paced = most < wanted && written == most;
                self.to_stream.drain(..written);
                used += written;
            }
            if !self.socket_done && self.to_stream.len() < CHUNK && used < budget {
                let room = (CHUNK - self.to_stream.len())
                    .min(scratch.len())
                    .min(budget - used);
                match socket.read(&mut scratch[..room]) {
                    Ok(0) => self.socket_done = true,
                    Ok(read) => {
                        self.to_stream.extend_from_slice(&scratch[..read]);
                        used += read;
                    }
                    Err(err) if is_wait(&err) => {}
                    Err(err) => {
                        log::debug!("reading a forwarded connection failed: {err}");
                        return Next::Abandon;
                    }
                }
            }
            if self.socket_done && self.to_stream.is_empty() && !self.stream_done {
                quic.finish(self.handle, self.stream);
                self.stream_done = true;
            }
            if self.socket_shut && self.stream_done {
                return Next::Done;
            }
            if used >= budget {
                return Next::Again(used);
            }
            if used == before {
                return if paced {
                    Next::Paced(used)
                } else {
                    Next::Used(used)
                };
            }
        }
    }

    /// Resets the socket rather than closing it, so its other end knows
    /// the connection was cut short. A Unix socket has no reset: its other
    /// end sees it close.
    fn reset_socket(&mut self) {
        if let Some(PipeSocket::Tcp(socket)) = self.socket.take() {
            let _ = rustix::net::sockopt::set_socket_linger(&socket, Some(Duration::ZERO));
        }
    }
}

/// Whether `err` only says to try again later.
fn is_wait(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

/// What to tell the window of a connection that failed `last`, and then,
/// at another address, `new`: a refusal, or a time out, or a name not
/// found, says more than an address that couldn't be tried at all, as `::1`
/// can't where there's no IPv6, so it's never replaced by one.
fn stronger(last: Option<ForwardFailure>, new: ForwardFailure) -> ForwardFailure {
    match (last, new) {
        (
            Some(
                last @ (ForwardFailure::Refused
                | ForwardFailure::TimedOut
                | ForwardFailure::NoSuchHost),
            ),
            ForwardFailure::Other(_),
        ) => last,
        (_, new) => new,
    }
}

/// What connecting failing with `err` tells the window.
fn failure(err: &io::Error) -> ForwardFailure {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => ForwardFailure::Refused,
        io::ErrorKind::TimedOut => ForwardFailure::TimedOut,
        _ => ForwardFailure::Other(err.to_string()),
    }
}

/// The addresses `host` names, if it needs nothing looked up: `localhost`,
/// or an address.
fn known(host: &str) -> Option<Vec<IpAddr>> {
    if host.eq_ignore_ascii_case("localhost") {
        return Some(vec![Ipv4Addr::LOCALHOST.into(), Ipv6Addr::LOCALHOST.into()]);
    }
    host.parse().ok().map(|ip| vec![ip])
}

impl Daemon {
    /// The client on the connection with `handle` opened `stream` for a
    /// connection it forwards.
    pub(super) fn forwarded(&mut self, handle: ConnectionHandle, stream: StreamId) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        let wire = (quic.client_of(handle))
            .and_then(|client| match self.sources.get(client.0) {
                Some(Source::Client(client)) if client.greeted => Some(client.wire),
                _ => None,
            })
            .filter(|&wire| wire::daemon_has(wire, wire::FORWARDS));
        // A client too old to forward never asks, and one that hasn't
        // greeted can't: refused as an older daemon refuses.
        let Some(wire) = wire else {
            quic.abandon(handle, stream);
            return;
        };
        let entry = self.sources.vacant_entry();
        let token = Token(entry.key());
        entry.insert(Source::Forward(Pipe::new(handle, stream, wire)));
        self.forwards.streams.insert((handle, stream), token);
        self.count_forwarded(handle);
        // QUIC says nothing of what came with the stream as it opened.
        self.queue_forward(token);
    }

    /// A forwarded connection's stream on the connection with `handle`
    /// may have something to read, or room to write.
    pub(super) fn forward_ready(&mut self, handle: ConnectionHandle, stream: StreamId) {
        if let Some(&token) = self.forwards.streams.get(&(handle, stream)) {
            self.queue_forward(token);
        }
    }

    /// Queues the forwarded connection at `token` to be done at the end of
    /// the turn, as its socket had an event, or its stream did.
    pub(super) fn queue_forward(&mut self, token: Token) {
        if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0)
            && !pipe.queued
        {
            pipe.queued = true;
            self.forwards.ready.push_back(token);
        }
    }

    /// Whether a forwarded connection has something to do this turn.
    pub(super) fn forwards_ready(&self) -> bool {
        !self.forwards.ready.is_empty()
    }

    /// When a connection being made next gives up on the address it's
    /// trying, or one held back by its pace may go on.
    pub(super) fn next_forward_due(&self) -> Option<Instant> {
        let paced = self.forwards.paces.values().filter_map(Pace::due);
        (self.forwards.deadlines.iter().map(|&(due, _)| due))
            .chain(paced)
            .min()
    }

    /// Does what each forwarded connection queued has to do, within the
    /// turn's budget. Returns whether any was, so what they wrote to QUIC
    /// can be sent.
    pub(super) fn move_forwarded(&mut self) -> bool {
        let mut budget = TURN_BUDGET;
        let mut any = false;
        for _ in 0..self.forwards.ready.len() {
            // Anything that came while a flood was read goes first.
            if budget == 0 || !self.waiting.is_empty() {
                break;
            }
            let Some(token) = self.forwards.ready.pop_front() else {
                break;
            };
            let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) else {
                continue;
            };
            pipe.queued = false;
            any = true;
            budget = budget.saturating_sub(self.move_one(token, budget.min(PIPE_BUDGET)));
        }
        any
    }

    /// Does what the forwarded connection at `token` has to do, moving at
    /// most `budget` bytes. Returns how many it moved.
    fn move_one(&mut self, token: Token, budget: usize) -> usize {
        let (Some(Source::Forward(pipe)), Some(quic)) =
            (self.sources.get_mut(token.0), self.quic.as_mut())
        else {
            return 0;
        };
        let next = match pipe.stage {
            Stage::Asking => pipe.ask(quic),
            Stage::Resolving { .. } | Stage::Connecting { .. } if pipe.gone(quic) => Next::Abandon,
            Stage::Resolving { .. } => Next::Wait,
            Stage::Connecting { .. } => pipe.check_connected(),
            Stage::Reaching { .. } => pipe.reaching(quic, &mut self.buffer),
            Stage::Open => match quic.pace_inputs(pipe.handle) {
                Some((rtt, opened, moved)) => {
                    let now = Instant::now();
                    let pace = match self.forwards.paces.entry(pipe.handle) {
                        Entry::Occupied(entry) => {
                            let pace = entry.into_mut();
                            if moved {
                                pace.path_changed(now, rtt);
                            }
                            pace
                        }
                        Entry::Vacant(entry) => {
                            entry.insert(Pace::new(now, rtt, opened, End::Daemon))
                        }
                    };
                    pace.refresh(now, rtt);
                    pipe.relay(quic, budget, pace, &mut self.buffer)
                }
                // Its connection is gone.
                None => Next::Abandon,
            },
        };
        match next {
            Next::Wait => 0,
            Next::Used(used) => used,
            Next::Paced(used) => {
                if !self.forwards.parked.contains(&token) {
                    self.forwards.parked.push(token);
                }
                used
            }
            Next::Again(used) => {
                self.queue_forward(token);
                used
            }
            Next::Connect(forward) => {
                self.connect_forward(token, forward);
                0
            }
            Next::Reach(machine) => {
                self.reach_forward(token, machine);
                0
            }
            Next::Connected => {
                self.forwards.deadlines.retain(|&(_, of)| of != token);
                self.queue_forward(token);
                0
            }
            Next::Retry(why) => {
                self.try_next_address(token, why);
                0
            }
            Next::Fail(why) => {
                self.fail_forward(token, why);
                0
            }
            Next::Refuse(why) => {
                self.refuse_forward(token, why);
                0
            }
            Next::Done => {
                self.forget_forward(token, true);
                0
            }
            Next::Abandon => {
                self.abandon_forward(token);
                0
            }
        }
    }

    /// Starts connecting the forwarded connection at `token` where
    /// `forward` says: at once if its host needs nothing looked up, and
    /// otherwise once it's been.
    fn connect_forward(&mut self, token: Token, forward: Forward) {
        let Forward {
            host,
            port,
            unpaced,
        } = forward;
        if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) {
            pipe.unpaced = unpaced;
        }
        if let Some(ips) = known(&host) {
            self.connect_to(token, ips, port);
            return;
        }
        let waker = &self.waker;
        let lookups =
            (self.forwards.lookups).get_or_insert_with(|| Lookups::new(Arc::clone(waker)));
        // Asked once for everyone waiting on the same name, and not again
        // while it's being looked up, even if nobody waits for it now.
        lookups.ask(host.clone());
        self.forwards
            .waiting
            .entry(host.clone())
            .or_default()
            .push(token);
        let due = Instant::now() + CONNECT_WAIT;
        self.forwards.deadlines.push((due, token));
        if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) {
            pipe.stage = Stage::Resolving { host, port };
        }
    }

    /// Has the stream at `token`, which asked to reach the daemon on
    /// `machine`, reach it: through this daemon's connection to it, which
    /// is asked the same and answers for it, or as a new client of this
    /// daemon, if it's this one; or tells the asker why it can't.
    fn reach_forward(&mut self, token: Token, machine: MachineId) {
        let Some(Source::Forward(pipe)) = self.sources.get(token.0) else {
            return;
        };
        let handle = pipe.handle;
        let reaching = match self.reaching(handle, machine) {
            Ok(reaching) => reaching,
            Err(why) => {
                self.refuse_forward(token, why);
                return;
            }
        };
        let (socket, then, via) = match reaching {
            Reaching::Here(socket) => (socket, None, None),
            Reaching::On {
                socket,
                request,
                wire,
                via,
            } => (socket, Some((request, wire)), Some(via)),
        };
        let registered = socket.set_nonblocking(true).and_then(|()| {
            let mut socket = UnixStream::from_std(socket);
            let both = Interest::READABLE | Interest::WRITABLE;
            self.poll.registry().register(&mut socket, token, both)?;
            Ok(socket)
        });
        let socket = match registered {
            Ok(socket) => socket,
            Err(err) => {
                let why = Error::Other(format!("the daemon couldn't watch a socket: {err}"));
                self.refuse_forward(token, why);
                return;
            }
        };
        let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) else {
            return;
        };
        pipe.socket = Some(PipeSocket::Unix(socket));
        pipe.unpaced = true;
        pipe.reach = Some(machine);
        pipe.via = via;
        if let Some(quic) = self.quic.as_mut() {
            quic.raise_forwarded(handle, pipe.stream);
        }
        match then {
            None => {
                let done = ServerMessage::Response(Response::Done);
                match frame::encode(pipe.wire, &done) {
                    Ok(bytes) => {
                        pipe.to_stream = bytes;
                        pipe.stage = Stage::Open;
                    }
                    Err(err) => {
                        log::error!("encoding a forwarded connection's answer failed: {err}");
                        self.abandon_forward(token);
                        return;
                    }
                }
            }
            Some((mut request, wire)) => {
                // What came after the request goes on after it.
                request.append(&mut pipe.to_socket);
                pipe.to_socket = request;
                pipe.stage = Stage::Reaching {
                    answer: Vec::new(),
                    wire,
                };
                let due = Instant::now() + CONNECT_WAIT;
                self.forwards.deadlines.push((due, token));
            }
        }
        self.queue_forward(token);
    }

    /// Takes the names the lookups have found, and connects the
    /// connections waiting for them. Answers nobody waits for any more
    /// are dropped.
    pub(super) fn take_lookups(&mut self) {
        let Some(lookups) = &mut self.forwards.lookups else {
            return;
        };
        if !lookups.busy() {
            return;
        }
        let mut found = Vec::new();
        for (host, ips) in lookups.answers() {
            if let Some(tokens) = self.forwards.waiting.remove(&host) {
                found.push((host, ips, tokens));
            }
        }
        for (host, ips, tokens) in found {
            for token in tokens {
                let port = match self.sources.get(token.0) {
                    Some(Source::Forward(Pipe {
                        stage: Stage::Resolving { host: waited, port },
                        ..
                    })) if *waited == host => *port,
                    _ => continue,
                };
                if ips.is_empty() {
                    self.fail_forward(token, ForwardFailure::NoSuchHost);
                } else {
                    self.connect_to(token, ips.clone(), port);
                }
            }
        }
    }

    /// Starts connecting the forwarded connection at `token` to `port` on
    /// each of `ips`, in turn.
    fn connect_to(&mut self, token: Token, ips: Vec<IpAddr>, port: u16) {
        if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) {
            pipe.stage = Stage::Connecting {
                addresses: (ips.into_iter())
                    .map(|ip| SocketAddr::new(ip, port))
                    .collect(),
                last: None,
            };
        }
        self.try_next_address(token, None);
    }

    /// Tries the next address for the forwarded connection at `token`,
    /// the last having failed `why`, if it did; or, with none left, tells
    /// the window why the last failed.
    fn try_next_address(&mut self, token: Token, why: Option<ForwardFailure>) {
        self.forwards.deadlines.retain(|&(_, of)| of != token);
        let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) else {
            return;
        };
        let Stage::Connecting { addresses, last } = &mut pipe.stage else {
            return;
        };
        if let Some(why) = why {
            *last = Some(stronger(last.take(), why));
        }
        // The last try's socket goes, and leaves epoll with it.
        pipe.socket = None;
        while let Some(address) = addresses.pop_front() {
            let connected = TcpStream::connect(address).and_then(|mut socket| {
                let both = Interest::READABLE | Interest::WRITABLE;
                self.poll.registry().register(&mut socket, token, both)?;
                Ok(socket)
            });
            match connected {
                Ok(socket) => {
                    pipe.socket = Some(PipeSocket::Tcp(socket));
                    let due = Instant::now() + CONNECT_WAIT;
                    self.forwards.deadlines.push((due, token));
                    return;
                }
                Err(err) => {
                    log::debug!("connecting a forwarded connection to {address} failed: {err}");
                    *last = Some(stronger(last.take(), failure(&err)));
                }
            }
        }
        let why = last.take().unwrap_or(ForwardFailure::Refused);
        self.fail_forward(token, why);
    }

    /// Gives up on each connection whose name has taken too long to be
    /// found, and moves on from each whose address has taken too long to
    /// answer.
    pub(super) fn time_out_forwards(&mut self) {
        self.unpark_forwards();
        if self.forwards.deadlines.is_empty() {
            return;
        }
        let now = Instant::now();
        let late: Vec<Token> = (self.forwards.deadlines.iter())
            .filter(|&&(due, _)| due <= now)
            .map(|&(_, token)| token)
            .collect();
        for token in late {
            let waiting = matches!(
                self.sources.get(token.0),
                Some(Source::Forward(Pipe {
                    stage: Stage::Resolving { .. } | Stage::Reaching { .. },
                    ..
                }))
            );
            if waiting {
                self.fail_forward(token, ForwardFailure::TimedOut);
            } else {
                self.try_next_address(token, Some(ForwardFailure::TimedOut));
            }
        }
    }

    /// Queues again each connection held back by its pace, once the pace
    /// allows more.
    fn unpark_forwards(&mut self) {
        if self
            .forwards
            .paces
            .values()
            .all(|pace| pace.due().is_none())
        {
            return;
        }
        let now = Instant::now();
        let mut woken = Vec::new();
        for (&handle, pace) in &mut self.forwards.paces {
            if pace.due().is_some_and(|due| due <= now) {
                pace.woken();
                woken.push(handle);
            }
        }
        let parked = std::mem::take(&mut self.forwards.parked);
        for token in parked {
            match self.sources.get(token.0) {
                Some(Source::Forward(pipe)) if woken.contains(&pipe.handle) => {
                    self.queue_forward(token);
                }
                Some(Source::Forward(_)) => self.forwards.parked.push(token),
                _ => {}
            }
        }
    }

    /// Tells the window the forwarded connection at `token` couldn't be
    /// made, and why, and forgets it.
    fn fail_forward(&mut self, token: Token, why: ForwardFailure) {
        log::info!("a forwarded connection couldn't be made: {why}");
        self.refuse_forward(token, Error::Forward(why));
    }

    /// Answers the forwarded connection at `token` with `why` it couldn't
    /// be made, and forgets it.
    fn refuse_forward(&mut self, token: Token, why: Error) {
        if let (Some(Source::Forward(pipe)), Some(quic)) =
            (self.sources.get_mut(token.0), self.quic.as_mut())
        {
            if !matches!(why, Error::Forward(_)) {
                log::info!("a stream asking to reach a daemon was refused: {why}");
            }
            pipe.reset_socket();
            let answer = ServerMessage::Response(Response::Error(why));
            match frame::encode(pipe.wire, &answer) {
                // A new stream always has room for it.
                Ok(bytes) => {
                    let _ = quic.write_forwarded(pipe.handle, pipe.stream, &bytes);
                    quic.finish(pipe.handle, pipe.stream);
                    quic.stop_reading(pipe.handle, pipe.stream);
                }
                Err(err) => {
                    log::error!("encoding why a forwarded connection failed failed: {err}");
                    quic.abandon(pipe.handle, pipe.stream);
                }
            }
        }
        self.forget_forward(token, true);
    }

    /// Abandons the forwarded connection at `token` both ways, resetting
    /// its socket, and forgets it.
    fn abandon_forward(&mut self, token: Token) {
        if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) {
            pipe.reset_socket();
            if let Some(quic) = self.quic.as_mut() {
                quic.abandon(pipe.handle, pipe.stream);
            }
        }
        self.forget_forward(token, false);
    }

    /// Forgets the forwarded connection at `token`. Its socket closes,
    /// leaving epoll as it does. If its stream was `finished`, rather than
    /// abandoned, it counts toward the send window until QUIC has had
    /// all that was sent on it acknowledged.
    fn forget_forward(&mut self, token: Token, finished: bool) {
        let Some(Source::Forward(pipe)) = self.sources.get(token.0) else {
            return;
        };
        let (handle, stream) = (pipe.handle, pipe.stream);
        if finished && !pipe.acked {
            self.forwards.lingering.insert((handle, stream));
        }
        self.forwards.streams.remove(&(handle, stream));
        self.sources.remove(token.0);
        self.forwards.ready.retain(|&queued| queued != token);
        self.forwards.parked.retain(|&parked| parked != token);
        self.forwards.deadlines.retain(|&(_, of)| of != token);
        self.forwards.waiting.retain(|_, waiting| {
            waiting.retain(|&waiter| waiter != token);
            !waiting.is_empty()
        });
        self.count_forwarded(handle);
    }

    /// QUIC has had everything sent on `stream`, a forwarded connection's
    /// on the connection with `handle`, acknowledged (`acked`), or the
    /// window stopped it: it no longer counts toward the send window once
    /// its connection is over.
    pub(super) fn forward_ended(
        &mut self,
        handle: ConnectionHandle,
        stream: StreamId,
        acked: bool,
    ) {
        if let Some(&token) = self.forwards.streams.get(&(handle, stream)) {
            if let Some(Source::Forward(pipe)) = self.sources.get_mut(token.0) {
                pipe.acked |= acked;
            }
            self.queue_forward(token);
        } else if self.forwards.lingering.remove(&(handle, stream)) {
            self.count_forwarded(handle);
        }
    }

    /// Sizes the send window of the connection with `handle` for the
    /// connections forwarded through it.
    fn count_forwarded(&mut self, handle: ConnectionHandle) {
        let open = (self.forwards.streams.keys())
            .filter(|&&(of, _)| of == handle)
            .count();
        // With nothing forwarded, nothing is paced.
        if open == 0 {
            self.forwards.paces.remove(&handle);
        }
        let count = open
            + (self.forwards.lingering.iter())
                .filter(|&&(of, _)| of == handle)
                .count();
        if let Some(quic) = self.quic.as_mut() {
            quic.set_forwarded(handle, count);
        }
    }

    /// Abandons every stream reaching the daemon on `machine`, as this
    /// daemon was asked not to pass it on, or no longer connects to it.
    pub(super) fn abandon_reaching(&mut self, machine: MachineId) {
        let tokens: Vec<Token> = (self.forwards.streams.values())
            .copied()
            .filter(|token| {
                matches!(
                    self.sources.get(token.0),
                    Some(Source::Forward(pipe)) if pipe.reach == Some(machine)
                )
            })
            .collect();
        for token in tokens {
            self.abandon_forward(token);
        }
    }

    /// Abandons every stream carried on through the connection to `host`,
    /// which has ended.
    pub(super) fn abandon_carried_by(&mut self, host: &str) {
        let tokens: Vec<Token> = (self.forwards.streams.values())
            .copied()
            .filter(|token| {
                matches!(
                    self.sources.get(token.0),
                    Some(Source::Forward(pipe)) if pipe.via.as_deref() == Some(host)
                )
            })
            .collect();
        for token in tokens {
            self.abandon_forward(token);
        }
    }

    /// Abandons every connection forwarded through the connection with
    /// `handle`, as its client has gone; or, with none, every one there
    /// is, as the daemon stops or upgrades.
    pub(super) fn abandon_forwards(&mut self, handle: Option<ConnectionHandle>) {
        let tokens: Vec<Token> = (self.forwards.streams.iter())
            .filter(|&(&(of, _), _)| handle.is_none_or(|handle| handle == of))
            .map(|(_, &token)| token)
            .collect();
        for token in tokens {
            self.abandon_forward(token);
        }
        // Their connection is over, or is about to be.
        (self.forwards.lingering).retain(|&(of, _)| handle.is_some_and(|handle| handle != of));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_and_addresses_need_nothing_looked_up() {
        assert_eq!(
            known("localhost"),
            Some(vec![
                IpAddr::from(Ipv4Addr::LOCALHOST),
                Ipv6Addr::LOCALHOST.into()
            ])
        );
        assert_eq!(known("LocalHost").map(|ips| ips.len()), Some(2));
        assert_eq!(known("10.0.0.2"), Some(vec!["10.0.0.2".parse().unwrap()]));
        assert_eq!(known("::1"), Some(vec![Ipv6Addr::LOCALHOST.into()]));
        assert_eq!(known("desk"), None);
        assert_eq!(known("localhost.example"), None);
    }

    #[test]
    fn an_address_that_cant_be_tried_never_hides_a_refusal() {
        let unavailable = || ForwardFailure::Other("Cannot assign requested address".into());
        assert_eq!(
            stronger(Some(ForwardFailure::Refused), unavailable()),
            ForwardFailure::Refused
        );
        assert_eq!(
            stronger(Some(ForwardFailure::TimedOut), unavailable()),
            ForwardFailure::TimedOut
        );
        assert_eq!(stronger(None, unavailable()), unavailable());
        assert_eq!(
            stronger(Some(unavailable()), ForwardFailure::Refused),
            ForwardFailure::Refused
        );
    }

    #[test]
    fn a_refused_connection_and_one_that_timed_out_are_told_apart() {
        let refused = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert_eq!(failure(&refused), ForwardFailure::Refused);
        let late = io::Error::from(io::ErrorKind::TimedOut);
        assert_eq!(failure(&late), ForwardFailure::TimedOut);
        let other = io::Error::from_raw_os_error(libc::EHOSTUNREACH);
        assert!(matches!(failure(&other), ForwardFailure::Other(_)));
    }
}
