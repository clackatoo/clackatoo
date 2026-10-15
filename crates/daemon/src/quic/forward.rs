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
//! Nothing here costs anything while no port is forwarded.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::Arc;
use std::time::Duration;

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, Error, Forward, ForwardFailure, Request, Response, ServerMessage,
};
use mio::{Interest, Registry, Token};
use quinn_proto::{Connection, Dir, ReadError, StreamId, VarInt, WriteError};
use slab::Slab;

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
}

/// One forward.
#[derive(Debug)]
struct Port {
    spec: Spec,
    /// What's asked of the daemon for each connection, in its protocol.
    ask: Vec<u8>,
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

/// A connection a forward took, and the stream it goes on.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "how far each way has got, two steps each"
)]
struct Pipe {
    forward: u64,
    socket: mio::net::TcpStream,
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

/// What became of a connection, each time round.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Used this much of its budget, and waits.
    Used(usize),
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
                let request = ClientMessage::Request(Request::Forward(Forward {
                    host: spec.host.clone(),
                    port: spec.port,
                }));
                let ask = match frame::encode(wire, &request) {
                    Ok(ask) => ask,
                    Err(err) => {
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
        }
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
            .filter(|(_, socket)| matches!(socket, Socket::Connection(pipe) if pipe.forward == id))
            .map(|(key, _)| key)
            .collect();
        for key in pipes {
            self.abandon(key, connection.as_deref_mut());
        }
        port.shared.set_connections(id, 0);
    }

    /// Resets every connection, as the connection to the daemon has been
    /// lost, and lets go of every socket.
    pub(super) fn end(&mut self) {
        let ids: Vec<u64> = self.forwards.keys().copied().collect();
        for id in ids {
            self.stop(id, None);
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
        if !connected || self.ready.is_empty() {
            return;
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
            let ask = port.ask.clone();
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
                forward: id,
                socket,
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
        let Some(Socket::Connection(pipe)) = self.sockets.get_mut(key) else {
            return 0;
        };
        if pipe.stream.is_none() {
            let Some(stream) = connection.streams().open(Dir::Bi) else {
                // Tried again when the daemon allows another.
                return 0;
            };
            let _ = connection
                .send_stream(stream)
                .set_priority(super::FORWARD_PRIORITY);
            pipe.stream = Some(stream);
            self.streams.insert(stream, key);
            self.unstreamed.retain(|&waiting| waiting != key);
        }
        let Some(port) = self.forwards.get(&pipe.forward) else {
            self.abandon(key, Some(connection));
            return 0;
        };
        match pipe.relay(connection, port.wire, budget, scratch) {
            Next::Used(used) => {
                if pipe.answer.is_none() {
                    self.worked(key);
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
                    && let Some(port) = self.forwards.get_mut(&pipe.forward)
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
            && let Some(port) = self.forwards.get_mut(&pipe.forward)
        {
            port.told = false;
        }
    }

    /// Abandons the connection at `key` both ways, on `connection` if
    /// it's still open, resetting its socket, and forgets it.
    fn abandon(&mut self, key: usize, connection: Option<&mut Connection>) {
        if let Some(Socket::Connection(pipe)) = self.sockets.get(key) {
            let _ = rustix::net::sockopt::set_socket_linger(&pipe.socket, Some(Duration::ZERO));
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
        if let Some(port) = self.forwards.get_mut(&pipe.forward) {
            let was_full = port.open >= MOST_EACH;
            port.open = port.open.saturating_sub(1);
            port.shared.set_connections(pipe.forward, port.open);
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
    /// Passes bytes both ways, up to `budget` of them, the daemon's
    /// answer being in protocol `wire`, using `scratch` to read the
    /// socket into.
    fn relay(
        &mut self,
        connection: &mut Connection,
        wire: u32,
        budget: usize,
        scratch: &mut [u8],
    ) -> Next {
        let Some(stream) = self.stream else {
            return Next::Used(0);
        };
        // Stopped by the daemon, it never reads what's sent, however quiet
        // the browser is.
        if !self.stream_done && matches!(connection.send_stream(stream).stopped(), Ok(Some(_))) {
            return Next::Abandon(self.why_stopped(connection, stream, wire));
        }
        // Reset after it finished its side, which nothing reads any more
        // to find out.
        if !matches!(self.socket.take_error(), Ok(None)) {
            return Next::Abandon(None);
        }
        let mut used = 0;
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
                let most = self.to_stream.len().min(budget - used);
                match connection
                    .send_stream(stream)
                    .write(&self.to_stream[..most])
                {
                    Ok(written) => {
                        self.to_stream.drain(..written);
                        used += written;
                    }
                    Err(WriteError::Blocked) => {}
                    Err(_) => return Next::Abandon(self.why_stopped(connection, stream, wire)),
                }
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
                return Next::Used(used);
            }
        }
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

/// Whether `err` only says to try again later.
fn is_wait(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
