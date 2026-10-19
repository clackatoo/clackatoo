//! A window's end of forwarding ports (see [`crate::forward`]), on the
//! thread that carries its QUIC connection, the only one that may open
//! streams on it.
//!
//! The thread takes the connections made to each forward's sockets, up to
//! [`MOST_EACH`] at a time for each, and opens a stream to the daemon for
//! each, asking it to connect where the forward goes. Bytes then pass
//! both ways, as on the daemon's end (see `server::forwards`): each side
//! is read only as fast as the other takes it, the streams send below
//! everything else, and the thread moves at most [`TURN_BUDGET`] each
//! time round, so the keys typed and the views that come back go first.
//! What goes to the daemon goes only as fast as the link takes it without
//! a queue growing (see [`Pace`]), so an upload never fills a slow link's
//! queue ahead of keys. Nothing here costs anything while no port is
//! forwarded.
//!
//! A carrier for another connection, which reaches a daemon through this
//! one's (see [`Command::Carry`]), goes the same way, but on a socket
//! handed over rather than taken from a forward's, unpaced, and beside
//! the window's messages rather than below them, as what it carries is
//! keys and views too. Its stream carries what its socket sends as it
//! is, the request first, and the answer back with the rest: the
//! carrier's owner asks and reads the answer itself.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, Error, Forward, ForwardFailure, Request, Response, ServerMessage,
};
use mio::{Interest, Registry, Token};
use quinn_proto::{Connection, Dir, ReadError, StreamId, VarInt, WriteError};
use slab::Slab;

use super::{End, Pace};
use crate::forward::{Command, MOST_EACH, Problem, Report, Shared, Spec};

/// The first token of the sockets here: those below are the thread's own.
pub(super) const FIRST: usize = 16;

/// The most of a connection's bytes that wait here each way.
const CHUNK: usize = 64 * 1024;

/// The most bytes moved for one connection each time round.
const PIPE_BUDGET: usize = 32 * 1024;

/// The most moved for all of them each time round.
const TURN_BUDGET: usize = 64 * 1024;

/// The longest the daemon's answer may be.
const MOST_ANSWER: usize = 4096;

/// A window's forwards, as the QUIC connection's thread serves them.
#[derive(Debug, Default)]
pub(super) struct Local {
    forwards: HashMap<u64, Port>,
    /// Their sockets, and the connections they took, by token less
    /// [`FIRST`].
    sockets: Slab<Socket>,
    /// Sockets with something to do, each once.
    ready: VecDeque<usize>,
    /// Each connection's stream, once it has one.
    streams: HashMap<StreamId, usize>,
    /// Connections waiting for the daemon to allow another stream.
    unstreamed: VecDeque<usize>,
    /// The streams of connections that are over but whose last bytes
    /// QUIC hasn't had acknowledged: they still count toward the send
    /// window (see [`send_window`](super::send_window)).
    lingering: HashSet<StreamId>,
    /// How many streams the connection's send window was last sized for.
    window_for: usize,
    /// How fast connections may write to the daemon, while any has a
    /// stream.
    pace: Option<Pace>,
    /// Connections waiting for the pace to allow more.
    parked: Vec<usize>,
    /// When the connection opened, which slowdowns are counted from.
    opened: Option<Instant>,
}

/// One forward.
#[derive(Debug)]
struct Port {
    spec: Spec,
    /// What's asked of the daemon for each connection, in its protocol:
    /// to pace it, and not to.
    ask: Vec<u8>,
    ask_unpaced: Vec<u8>,
    /// The protocol the daemon's answers are in.
    wire: u32,
    /// Its listening sockets.
    listeners: Vec<usize>,
    /// How many connections it has open.
    open: usize,
    /// Whether what went wrong has been told, since a connection last
    /// worked.
    told: bool,
    shared: Arc<Shared>,
}

/// A socket here.
#[derive(Debug)]
enum Socket {
    Listener {
        forward: u64,
        socket: mio::net::TcpListener,
        /// Whether it's in [`Local::ready`].
        queued: bool,
    },
    Connection(Box<Pipe>),
}

/// A connection a forward took, or a carrier handed over, and the
/// stream it goes on.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "how far each way has got, two steps each"
)]
struct Pipe {
    /// The forward that took it: `None` for a carrier.
    forward: Option<u64>,
    socket: PipeSocket,
    stream: Option<StreamId>,
    /// The daemon's answer, until it has all come.
    answer: Option<Vec<u8>>,
    /// What the browser sent, waiting for the stream: the request first.
    to_stream: Vec<u8>,
    /// What the daemon sent, waiting for the socket.
    to_socket: Vec<u8>,
    /// The daemon finished its side of the stream.
    daemon_done: bool,
    /// The socket is shut for writing, after the daemon finished.
    socket_shut: bool,
    /// The socket's other end finished its side.
    socket_done: bool,
    /// The stream is finished, after the socket's other end did.
    stream_done: bool,
    /// QUIC has had everything sent on the stream acknowledged.
    acked: bool,
    queued: bool,
}

/// A pipe's socket: a connection a forward took, or a carrier's.
#[derive(Debug)]
enum PipeSocket {
    Tcp(mio::net::TcpStream),
    Unix(mio::net::UnixStream),
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

    /// Has closing the socket reset it, so its other end knows it was cut
    /// short. A Unix socket has no reset: its other end sees it close.
    fn reset_on_close(&self) {
        if let Self::Tcp(socket) = self {
            let _ = rustix::net::sockopt::set_socket_linger(socket, Some(Duration::ZERO));
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

/// What became of a connection, each time round.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Used this much of its budget, and waits.
    Used(usize),
    /// Used this much, and waits for the pace to allow more.
    Paced(usize),
    /// Used this much, with more to do.
    Again(usize),
    /// Both sides have finished.
    Done,
    /// Abandoned both ways, and if anything's to be told, this.
    Abandon(Option<Problem>),
}

impl Local {
    /// Whether any socket has something to do now.
    pub(super) fn busy(&self) -> bool {
        !self.ready.is_empty()
    }

    /// When a connection held back by the pace may go on, if one is.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.pace.as_ref().and_then(Pace::due)
    }

    /// Notes that the connection opened, `now`.
    pub(super) fn connected(&mut self, now: Instant) {
        self.opened = Some(now);
    }

    /// Notes that the connection's path changed, as it moved between UDP
    /// and ssh, its round trip being `rtt` now.
    pub(super) fn path_changed(&mut self, now: Instant, rtt: Duration) {
        if let Some(pace) = &mut self.pace {
            pace.path_changed(now, rtt);
        }
    }

    /// Notes that the socket with `token`, one of these, had an event.
    pub(super) fn event(&mut self, token: Token) {
        if let Some(key) = token.0.checked_sub(FIRST) {
            self.queue(key);
        }
    }

    /// Notes that `stream` may have something to read, or room to write,
    /// or was stopped, or, if `acked`, had everything sent on it
    /// acknowledged. Returns whether it's a forwarded connection's.
    pub(super) fn stream_event(&mut self, stream: StreamId, acked: bool) -> bool {
        let Some(&key) = self.streams.get(&stream) else {
            return if acked {
                self.lingering.remove(&stream)
            } else {
                self.lingering.contains(&stream)
            };
        };
        if let Some(Socket::Connection(pipe)) = self.sockets.get_mut(key) {
            pipe.acked |= acked;
        }
        self.queue(key);
        true
    }

    /// Ended, or stopped by the daemon, a lingering stream counts no more.
    pub(super) fn stream_ended(&mut self, stream: StreamId) -> bool {
        self.lingering.remove(&stream) || self.stream_event(stream, false)
    }

    /// The daemon allows more streams: the connections waiting for one
    /// try again.
    pub(super) fn streams_available(&mut self) {
        let waiting: Vec<usize> = self.unstreamed.iter().copied().collect();
        for key in waiting {
            self.queue(key);
        }
    }

    fn queue(&mut self, key: usize) {
        let queued = match self.sockets.get_mut(key) {
            Some(Socket::Listener { queued, .. }) => queued,
            Some(Socket::Connection(pipe)) => &mut pipe.queued,
            None => return,
        };
        if !*queued {
            *queued = true;
            self.ready.push_back(key);
        }
    }

    /// Does what `command` says, on `connection`.
    pub(super) fn command(
        &mut self,
        command: Command,
        connection: &mut Connection,
        registry: &Registry,
    ) {
        match command {
            Command::Listen {
                id,
                spec,
                listeners,
                wire,
                shared,
            } => {
                let (ask, ask_unpaced) = match (ask(&spec, wire, false), ask(&spec, wire, true)) {
                    (Ok(ask), Ok(ask_unpaced)) => (ask, ask_unpaced),
                    (Err(err), _) | (_, Err(err)) => {
                        log::error!("encoding a request to forward failed: {err}");
                        return;
                    }
                };
                let mut keys = Vec::new();
                for listener in listeners {
                    let entry = self.sockets.vacant_entry();
                    let key = entry.key();
                    let mut socket = mio::net::TcpListener::from_std(listener);
                    if let Err(err) =
                        registry.register(&mut socket, Token(FIRST + key), Interest::READABLE)
                    {
                        log::warn!("couldn't watch a forward's socket: {err}");
                        continue;
                    }
                    entry.insert(Socket::Listener {
                        forward: id,
                        socket,
                        queued: false,
                    });
                    keys.push(key);
                }
                shared.set_connections(id, 0);
                self.forwards.insert(
                    id,
                    Port {
                        spec,
                        ask,
                        ask_unpaced,
                        wire,
                        listeners: keys.clone(),
                        open: 0,
                        told: false,
                        shared,
                    },
                );
                // Connections may be waiting already, made while the
                // window reconnected.
                for key in keys {
                    self.queue(key);
                }
            }
            Command::Stop { id } => self.stop(id, Some(connection)),
            Command::Carry { socket } => self.carry(socket, registry),
        }
    }

    /// Takes `socket`, a carrier for another connection, to pass on as it
    /// is: see [`Command::Carry`].
    fn carry(&mut self, socket: std::os::unix::net::UnixStream, registry: &Registry) {
        if let Err(err) = socket.set_nonblocking(true) {
            log::warn!("couldn't take a carrier: {err}");
            return;
        }
        let mut socket = mio::net::UnixStream::from_std(socket);
        let entry = self.sockets.vacant_entry();
        let key = entry.key();
        let both = Interest::READABLE | Interest::WRITABLE;
        if let Err(err) = registry.register(&mut socket, Token(FIRST + key), both) {
            log::warn!("couldn't watch a carrier: {err}");
            return;
        }
        entry.insert(Socket::Connection(Box::new(Pipe {
            forward: None,
            socket: PipeSocket::Unix(socket),
            stream: None,
            answer: None,
            to_stream: Vec::new(),
            to_socket: Vec::new(),
            daemon_done: false,
            socket_shut: false,
            socket_done: false,
            stream_done: false,
            acked: false,
            queued: false,
        })));
        self.unstreamed.push_back(key);
        self.queue(key);
    }

    /// Stops forward `id`, closing its sockets and resetting its
    /// connections, on `connection` if it's still open.
    fn stop(&mut self, id: u64, mut connection: Option<&mut Connection>) {
        let Some(port) = self.forwards.remove(&id) else {
            return;
        };
        for key in port.listeners {
            self.sockets.try_remove(key);
        }
        let pipes: Vec<usize> = (self.sockets.iter())
            .filter(|(_, socket)| {
                matches!(socket, Socket::Connection(pipe) if pipe.forward == Some(id))
            })
            .map(|(key, _)| key)
            .collect();
        for key in pipes {
            self.abandon(key, connection.as_deref_mut());
        }
        port.shared.set_connections(id, 0);
    }

    /// Resets every connection, as the connection to the daemon has been
    /// lost, and lets go of every socket: carriers' too, whose other ends
    /// see them close.
    pub(super) fn end(&mut self) {
        let ids: Vec<u64> = self.forwards.keys().copied().collect();
        for id in ids {
            self.stop(id, None);
        }
        let carriers: Vec<usize> = (self.sockets.iter())
            .filter(|(_, socket)| matches!(socket, Socket::Connection(_)))
            .map(|(key, _)| key)
            .collect();
        for key in carriers {
            self.abandon(key, None);
        }
        self.lingering.clear();
    }

    /// Does what the sockets queued have to do, on `connection`, within
    /// the budget: takes connections, opens their streams, and moves
    /// their bytes. `connected` says whether the daemon can be asked
    /// anything yet.
    pub(super) fn run(
        &mut self,
        connection: &mut Connection,
        registry: &Registry,
        connected: bool,
        scratch: &mut [u8],
    ) {
        self.size_window(connection);
        // With nothing forwarded, nothing is paced.
        if self.streams.is_empty() {
            self.pace = None;
            self.parked.clear();
        }
        let now = Instant::now();
        if let Some(pace) = &mut self.pace
            && pace.due().is_some_and(|due| due <= now)
        {
            pace.woken();
            for key in std::mem::take(&mut self.parked) {
                self.queue(key);
            }
        }
        if !connected || self.ready.is_empty() {
            return;
        }
        if let Some(pace) = &mut self.pace {
            pace.refresh(now, connection.rtt());
        }
        let mut budget = TURN_BUDGET;
        for _ in 0..self.ready.len() {
            if budget == 0 {
                break;
            }
            let Some(key) = self.ready.pop_front() else {
                break;
            };
            match self.sockets.get_mut(key) {
                Some(Socket::Listener {
                    queued, forward, ..
                }) => {
                    *queued = false;
                    let forward = *forward;
                    self.accept(key, forward, registry);
                }
                Some(Socket::Connection(pipe)) => {
                    pipe.queued = false;
                    budget = budget.saturating_sub(self.move_one(
                        key,
                        connection,
                        budget.min(PIPE_BUDGET),
                        scratch,
                    ));
                }
                None => {}
            }
        }
        self.unstreamed.retain(|key| self.sockets.contains(*key));
        self.size_window(connection);
    }

    /// Sizes `connection`'s send window for the streams that may have
    /// bytes not yet acknowledged, if they've changed.
    fn size_window(&mut self, connection: &mut Connection) {
        let streams = self.streams.len() + self.lingering.len();
        if self.window_for != streams {
            self.window_for = streams;
            connection.set_send_window(super::send_window(streams));
        }
    }

    /// Takes the connections waiting on the listener at `key`, forward
    /// `id`'s, while it has room for more.
    fn accept(&mut self, key: usize, id: u64, registry: &Registry) {
        loop {
            let Some(port) = self.forwards.get(&id) else {
                return;
            };
            // Taken once one of its connections ends.
            if port.open >= MOST_EACH {
                return;
            }
            let Some(Socket::Listener { socket, .. }) = self.sockets.get(key) else {
                return;
            };
            let (socket, _) = match socket.accept() {
                Ok(accepted) => accepted,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    if err.kind() != io::ErrorKind::WouldBlock {
                        log::debug!("taking a forwarded connection failed: {err}");
                    }
                    return;
                }
            };
            let ask = if port.shared.paced() {
                port.ask.clone()
            } else {
                port.ask_unpaced.clone()
            };
            let entry = self.sockets.vacant_entry();
            let pipe_key = entry.key();
            let mut socket = socket;
            let _ = socket.set_nodelay(true);
            let both = Interest::READABLE | Interest::WRITABLE;
            if let Err(err) = registry.register(&mut socket, Token(FIRST + pipe_key), both) {
                log::warn!("couldn't watch a forwarded connection: {err}");
                continue;
            }
            entry.insert(Socket::Connection(Box::new(Pipe {
                forward: Some(id),
                socket: PipeSocket::Tcp(socket),
                stream: None,
                answer: Some(Vec::new()),
                to_stream: ask,
                to_socket: Vec::new(),
                daemon_done: false,
                socket_shut: false,
                socket_done: false,
                stream_done: false,
                acked: false,
                queued: false,
            })));
            if let Some(port) = self.forwards.get_mut(&id) {
                port.open += 1;
                port.shared.set_connections(id, port.open);
            }
            self.unstreamed.push_back(pipe_key);
            self.queue(pipe_key);
        }
    }

    /// Does what the connection at `key` has to do, moving at most
    /// `budget` bytes. Returns how many it moved.
    fn move_one(
        &mut self,
        key: usize,
        connection: &mut Connection,
        budget: usize,
        scratch: &mut [u8],
    ) -> usize {
        // Counted only for a connection still waiting for its stream.
        let unstreamed = matches!(
            self.sockets.get(key),
            Some(Socket::Connection(pipe)) if pipe.stream.is_none()
        );
        let forwarding = if unstreamed {
            (self.streams.values())
                .filter(|&&other| {
                    matches!(
                        self.sockets.get(other),
                        Some(Socket::Connection(other)) if other.forward.is_some()
                    )
                })
                .count()
                + self.lingering.len()
        } else {
            0
        };
        let Some(Socket::Connection(pipe)) = self.sockets.get_mut(key) else {
            return 0;
        };
        if pipe.stream.is_none() {
            // Forwards leave the streams kept for carriers to them.
            if pipe.forward.is_some()
                && forwarding >= usize::try_from(super::MOST_FORWARDED).unwrap_or(usize::MAX)
            {
                return 0;
            }
            let Some(stream) = connection.streams().open(Dir::Bi) else {
                // Tried again when the daemon allows another.
                return 0;
            };
            // A carrier's goes beside the window's messages, at the
            // priority streams start at.
            if pipe.forward.is_some() {
                let _ = connection
                    .send_stream(stream)
                    .set_priority(super::FORWARD_PRIORITY);
            }
            pipe.stream = Some(stream);
            self.streams.insert(stream, key);
            self.unstreamed.retain(|&waiting| waiting != key);
            if self.pace.is_none() && pipe.forward.is_some() {
                let now = Instant::now();
                let opened = self.opened.unwrap_or(now);
                self.pace = Some(Pace::new(now, connection.rtt(), opened, End::Window));
            }
        }
        let (wire, pace) = match pipe.forward {
            // A carrier's answer is its owner's to read, and it's unpaced.
            None => (0, None),
            Some(forward) => {
                let (Some(port), Some(pace)) = (self.forwards.get(&forward), self.pace.as_mut())
                else {
                    self.abandon(key, Some(connection));
                    return 0;
                };
                (port.wire, port.shared.paced().then_some(pace))
            }
        };
        match pipe.relay(connection, wire, budget, pace, scratch) {
            Next::Used(used) => {
                if pipe.answer.is_none() {
                    self.worked(key);
                }
                used
            }
            Next::Paced(used) => {
                if pipe.answer.is_none() {
                    self.worked(key);
                }
                if !self.parked.contains(&key) {
                    self.parked.push(key);
                }
                used
            }
            Next::Again(used) => {
                if pipe.answer.is_none() {
                    self.worked(key);
                }
                self.queue(key);
                used
            }
            Next::Done => {
                self.forget(key, true);
                0
            }
            Next::Abandon(problem) => {
                if let Some(problem) = problem
                    && let Some(Socket::Connection(pipe)) = self.sockets.get(key)
                    && let Some(port) = (pipe.forward).and_then(|id| self.forwards.get_mut(&id))
                    && !std::mem::replace(&mut port.told, true)
                {
                    port.shared.report(Report {
                        spec: port.spec.clone(),
                        problem,
                    });
                }
                self.abandon(key, Some(connection));
                0
            }
        }
    }

    /// Notes that the connection at `key` was answered, so its forward
    /// works: what goes wrong next is told again.
    fn worked(&mut self, key: usize) {
        if let Some(Socket::Connection(pipe)) = self.sockets.get(key)
            && let Some(port) = (pipe.forward).and_then(|id| self.forwards.get_mut(&id))
        {
            port.told = false;
        }
    }

    /// Abandons the connection at `key` both ways, on `connection` if
    /// it's still open, resetting its socket, and forgets it.
    fn abandon(&mut self, key: usize, connection: Option<&mut Connection>) {
        if let Some(Socket::Connection(pipe)) = self.sockets.get(key) {
            pipe.socket.reset_on_close();
            if let (Some(stream), Some(connection)) = (pipe.stream, connection) {
                let _ = connection.send_stream(stream).reset(VarInt::from_u32(0));
                let _ = connection.recv_stream(stream).stop(VarInt::from_u32(0));
            }
        }
        self.forget(key, false);
    }

    /// Forgets the connection at `key`, closing its socket, and takes
    /// another for its forward, if one was waiting for room. If its stream
    /// was `finished`, rather than abandoned, it counts toward the send
    /// window until QUIC has had all that was sent on it acknowledged.
    fn forget(&mut self, key: usize, finished: bool) {
        let Some(Socket::Connection(pipe)) = self.sockets.try_remove(key) else {
            return;
        };
        if let Some(stream) = pipe.stream {
            self.streams.remove(&stream);
            if finished && !pipe.acked {
                self.lingering.insert(stream);
            }
        }
        self.ready.retain(|&queued| queued != key);
        self.unstreamed.retain(|&waiting| waiting != key);
        self.parked.retain(|&parked| parked != key);
        if let Some(forward) = pipe.forward
            && let Some(port) = self.forwards.get_mut(&forward)
        {
            let was_full = port.open >= MOST_EACH;
            port.open = port.open.saturating_sub(1);
            port.shared.set_connections(forward, port.open);
            if was_full {
                let listeners = port.listeners.clone();
                for listener in listeners {
                    self.queue(listener);
                }
            }
        }
    }
}

impl Pipe {
    /// Passes bytes both ways, up to `budget` of them, those to the
    /// daemon as fast as `pace` allows, if they're paced, the daemon's
    /// answer being in protocol `wire`, using `scratch` to read the socket
    /// into.
    fn relay(
        &mut self,
        connection: &mut Connection,
        wire: u32,
        budget: usize,
        mut pace: Option<&mut Pace>,
        scratch: &mut [u8],
    ) -> Next {
        let Some(stream) = self.stream else {
            return Next::Used(0);
        };
        // Stopped by the daemon, it never reads what's sent, however quiet
        // the browser is.
        if let Some(next) = self.stopped(connection, stream, wire) {
            return next;
        }
        // Reset after it finished its side, which nothing reads any more
        // to find out.
        if !matches!(self.socket.take_error(), Ok(None)) {
            return Next::Abandon(None);
        }
        let mut used = 0;
        // Whether the pace held back what's waiting for the daemon.
        let mut paced = false;
        loop {
            let before = used;
            // From the daemon to the browser, its answer first, each step
            // taking no more than what's left of the budget.
            if !self.daemon_done && self.to_socket.len() < CHUNK && used < budget {
                let had = self.to_socket.len();
                if let Err(problem) = self.read_stream(connection, stream, budget - used) {
                    return Next::Abandon(problem);
                }
                used += self.to_socket.len().saturating_sub(had);
                if let Err(problem) = self.take_answer(wire) {
                    return Next::Abandon(Some(problem));
                }
            }
            if !self.to_socket.is_empty() && used < budget {
                let most = self.to_socket.len().min(budget - used);
                match self.socket.write(&self.to_socket[..most]) {
                    Ok(written) => {
                        self.to_socket.drain(..written);
                        used += written;
                    }
                    Err(err) if is_wait(&err) => {}
                    Err(err) => {
                        log::debug!("writing to a forwarded connection failed: {err}");
                        return Next::Abandon(None);
                    }
                }
            }
            if self.daemon_done && self.to_socket.is_empty() && !self.socket_shut {
                let _ = self.socket.shutdown(Shutdown::Write);
                self.socket_shut = true;
            }
            // From the browser to the daemon.
            if !self.to_stream.is_empty() && used < budget {
                let wanted = self.to_stream.len().min(budget - used);
                let most = pace.as_deref().map_or(wanted, |pace| pace.most(wanted));
                let written = if most == 0 {
                    0
                } else {
                    match connection
                        .send_stream(stream)
                        .write(&self.to_stream[..most])
                    {
                        Ok(written) => written,
                        Err(WriteError::Blocked) => 0,
                        Err(_) => return Next::Abandon(self.why_stopped(connection, stream, wire)),
                    }
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
                match self.socket.read(&mut scratch[..room]) {
                    Ok(0) => self.socket_done = true,
                    Ok(read) => {
                        self.to_stream.extend_from_slice(&scratch[..read]);
                        used += read;
                    }
                    Err(err) if is_wait(&err) => {}
                    Err(err) => {
                        log::debug!("reading a forwarded connection failed: {err}");
                        return Next::Abandon(None);
                    }
                }
            }
            if self.socket_done && self.to_stream.is_empty() && !self.stream_done {
                let _ = connection.send_stream(stream).finish();
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

    /// What becomes of it, if the daemon stopped `stream`: a forwarded
    /// connection is abandoned, saying why if there's anything to say; a
    /// carrier sends nothing more, and still passes on what came, as its
    /// owner reads why from that.
    fn stopped(
        &mut self,
        connection: &mut Connection,
        stream: StreamId,
        wire: u32,
    ) -> Option<Next> {
        if self.stream_done || !matches!(connection.send_stream(stream).stopped(), Ok(Some(_))) {
            return None;
        }
        if self.forward.is_some() {
            return Some(Next::Abandon(self.why_stopped(connection, stream, wire)));
        }
        self.to_stream.clear();
        self.socket_done = true;
        self.stream_done = true;
        None
    }

    /// What to tell of the daemon having stopped `stream` before it was
    /// done with, if anything. Once it has answered, it stops a stream
    /// when its end of the connection is reset, which needs no word. Not
    /// yet answered, it may have said why with its answer, sent just
    /// before; and with none, it never took forwards, as an older one
    /// doesn't.
    fn why_stopped(
        &mut self,
        connection: &mut Connection,
        stream: StreamId,
        wire: u32,
    ) -> Option<Problem> {
        self.answer.as_ref()?;
        if let Err(problem) = self.read_stream(connection, stream, MOST_ANSWER) {
            return problem;
        }
        match self.take_answer(wire) {
            Err(problem) => Some(problem),
            Ok(()) if self.answer.is_none() => None,
            Ok(()) => Some(Problem::Refused),
        }
    }

    /// Reads what the daemon sent on `stream`: into the answer while it's
    /// coming, and then for the socket, at most `left` more of it.
    fn read_stream(
        &mut self,
        connection: &mut Connection,
        stream: StreamId,
        left: usize,
    ) -> Result<(), Option<Problem>> {
        let (into, most) = if let Some(answer) = &mut self.answer {
            (answer, MOST_ANSWER)
        } else {
            let most = CHUNK.min(self.to_socket.len() + left);
            (&mut self.to_socket, most)
        };
        let mut receiving = connection.recv_stream(stream);
        let Ok(mut chunks) = receiving.read(true) else {
            return Err(None);
        };
        let read = loop {
            let room = most.saturating_sub(into.len());
            if room == 0 {
                break Ok(());
            }
            match chunks.next(room) {
                Ok(Some(chunk)) => into.extend_from_slice(&chunk.bytes),
                Ok(None) => {
                    self.daemon_done = true;
                    break Ok(());
                }
                Err(ReadError::Blocked) => break Ok(()),
                // Abandoned before it answered: it took no forwards.
                Err(ReadError::Reset(_)) if self.answer.is_some() => {
                    break Err(Some(Problem::Refused));
                }
                Err(ReadError::Reset(_)) => break Err(None),
            }
        };
        let _ = chunks.finalize();
        read
    }

    /// Takes the daemon's answer, once it has all come, keeping what came
    /// after it for the socket. Fails with what's to be told if it
    /// couldn't connect, or finished without answering, or answered with
    /// what isn't an answer.
    fn take_answer(&mut self, wire: u32) -> Result<(), Problem> {
        let Some(answer) = self.answer.as_mut() else {
            return Ok(());
        };
        let end = (answer.first_chunk::<4>())
            .map(|header| usize::try_from(u32::from_le_bytes(*header)).unwrap_or(usize::MAX))
            .map(|len| len.saturating_add(4));
        if end.is_some_and(|end| end > MOST_ANSWER) {
            return Err(Problem::Other("the daemon's answer was too long".into()));
        }
        let Some(end) = end.filter(|&end| answer.len() >= end) else {
            // Finished without a word: it took no forwards.
            return if self.daemon_done {
                Err(Problem::Refused)
            } else {
                Ok(())
            };
        };
        let rest = answer.split_off(end);
        let mut decoder = FrameDecoder::new();
        decoder.push(answer);
        self.answer = None;
        self.to_socket = rest;
        match decoder.decode::<ServerMessage>(wire) {
            Ok(Some(ServerMessage::Response(Response::Done))) => Ok(()),
            Ok(Some(ServerMessage::Response(Response::Error(Error::Forward(why))))) => {
                Err(Problem::Failed(why))
            }
            Ok(Some(ServerMessage::Response(Response::Error(err)))) => {
                Err(Problem::Failed(ForwardFailure::Other(err.to_string())))
            }
            _ => Err(Problem::Other(
                "the daemon answered with what isn't an answer".into(),
            )),
        }
    }
}

/// What's asked of the daemon for a connection forward `spec` takes, in
/// protocol `wire`: to connect where it goes, and if `unpaced`, to send
/// what comes back as fast as it can.
fn ask(spec: &Spec, wire: u32, unpaced: bool) -> Result<Vec<u8>, frame::FrameError> {
    let request = ClientMessage::Request(Request::Forward(Forward {
        host: spec.host.clone(),
        port: spec.port,
        unpaced,
    }));
    frame::encode(wire, &request)
}

/// Whether `err` only says to try again later.
fn is_wait(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::wire;

    use super::*;

    /// The request `bytes` holds, read as the daemon reads it at `wire`.
    fn asked(bytes: &[u8], wire: u32) -> Forward {
        let mut decoder = FrameDecoder::new();
        decoder.push(bytes);
        match decoder.decode::<ClientMessage>(wire) {
            Ok(Some(ClientMessage::Request(Request::Forward(forward)))) => forward,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_connection_asks_not_to_be_paced_only_of_a_daemon_that_knows_how() {
        let spec: Spec = "3000:box:8080".parse().unwrap();
        for unpaced in [false, true] {
            let bytes = ask(&spec, wire::FORWARD_PACE, unpaced).unwrap();
            let forward = asked(&bytes, wire::FORWARD_PACE);
            assert_eq!((forward.host.as_str(), forward.port), ("box", 8080));
            assert_eq!(forward.unpaced, unpaced);
        }
        // An older daemon is asked as before: it paces nothing anyway.
        let older = wire::FORWARD_PACE - 1;
        let unpaced = ask(&spec, older, true).unwrap();
        assert_eq!(unpaced, ask(&spec, older, false).unwrap());
        assert!(!asked(&unpaced, older).unpaced);
    }
}
