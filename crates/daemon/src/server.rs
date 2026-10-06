//! The daemon's event loop.
//!
//! One thread waits on every file descriptor the daemon cares about: the
//! listening socket, each client connection, each PTY, and each program's
//! pidfd. The terminals' grids stay on that thread, so nothing is locked or
//! shared, and output goes from PTY to grid with no handoff in between.
//!
//! mio delivers edge-triggered events: an fd is reported once when it
//! becomes ready, not again until it has been drained. Every handler
//! therefore reads or writes until `WouldBlock`, or records that it stopped
//! early. Handlers must also tolerate spurious events, because a token freed
//! and reused within one batch of events can receive the old one.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::RawFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{Error, PROTOCOL_VERSION, Request, Response, TerminalId};
use mio::event::Event;
use mio::net::{UnixListener, UnixStream};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};
use slab::Slab;

use crate::DaemonConfig;
use crate::hosted::{HostedTerminal, Orphan, ReadOutcome};

const LISTENER: Token = Token(usize::MAX);
const WAKER: Token = Token(usize::MAX - 1);

/// Bytes read per `read` call, from PTYs and clients alike.
const READ_BUFFER_LEN: usize = 64 * 1024;

/// The most output read from one PTY before giving the other terminals and
/// clients a turn. Keeps a flood of output (`cat` of a huge file) from
/// freezing everything else.
const OUTPUT_BUDGET: usize = 1024 * 1024;

/// What a token refers to.
#[derive(Debug)]
enum Source {
    /// A terminal's PTY.
    Output(TerminalId),
    /// A terminal's pidfd.
    Exit(TerminalId),
    /// The pidfd of a closed terminal's program that hasn't exited yet.
    Orphan(Orphan),
    Client(Client),
}

/// A terminal plus its event-loop registrations.
#[derive(Debug)]
struct Slot {
    terminal: HostedTerminal,
    /// Unset once the PTY closes.
    output: Option<Token>,
    /// Unset once the program has been reaped.
    exit: Option<Token>,
    /// Whether the PTY is registered for writability, which it only is while
    /// input is queued.
    wants_writable: bool,
}

/// A connected client.
#[derive(Debug)]
struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    /// Encoded responses not yet written.
    outgoing: Vec<u8>,
    greeted: bool,
    /// The connection ends once `outgoing` is flushed: the client hung up, or
    /// broke the protocol.
    finished: bool,
    wants_writable: bool,
}

/// The clackatoo daemon: owns every terminal and serves clients.
#[derive(Debug)]
pub struct Daemon {
    poll: Poll,
    listener: UnixListener,
    waker: Arc<Waker>,
    shutdown: Arc<AtomicBool>,
    sources: Slab<Source>,
    terminals: BTreeMap<TerminalId, Slot>,
    next_id: u32,
    /// Terminals whose output budget ran out with more output waiting.
    unread: VecDeque<TerminalId>,
    config: DaemonConfig,
    buffer: Box<[u8]>,
}

/// Stops a running [`Daemon`] from another thread.
#[derive(Clone, Debug)]
pub struct ShutdownHandle {
    waker: Arc<Waker>,
    shutdown: Arc<AtomicBool>,
}

impl ShutdownHandle {
    /// Asks the daemon to stop. [`Daemon::run`] returns soon after.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        if let Err(err) = self.waker.wake() {
            log::error!("waking the daemon to shut down failed: {err}");
        }
    }
}

impl Daemon {
    /// Listens on `socket_path`, replacing any file already there. Callers
    /// should hold the [`DaemonLock`](crate::DaemonLock) for the directory,
    /// which guarantees that file is a stale socket and not a live daemon's.
    ///
    /// # Errors
    ///
    /// If the socket can't be bound or the event loop can't be set up.
    pub fn bind(socket_path: &Path, config: DaemonConfig) -> io::Result<Self> {
        match std::fs::remove_file(socket_path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let mut listener = UnixListener::bind(socket_path)?;
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
        log::info!("listening on {}", socket_path.display());
        Ok(Self {
            poll,
            listener,
            waker,
            shutdown: Arc::new(AtomicBool::new(false)),
            sources: Slab::new(),
            terminals: BTreeMap::new(),
            next_id: 1,
            unread: VecDeque::new(),
            config,
            buffer: vec![0; READ_BUFFER_LEN].into_boxed_slice(),
        })
    }

    /// A handle that stops [`run`](Self::run) from another thread.
    #[must_use]
    pub fn shutdown_handle(&self) -> ShutdownHandle {
        ShutdownHandle {
            waker: Arc::clone(&self.waker),
            shutdown: Arc::clone(&self.shutdown),
        }
    }

    /// Serves until shut down. Dropping the daemon afterwards closes every
    /// terminal, which hangs up their programs.
    ///
    /// # Errors
    ///
    /// If waiting for events fails, which leaves the daemon unable to work.
    pub fn run(&mut self) -> io::Result<()> {
        let mut events = Events::with_capacity(256);
        while !self.shutdown.load(Ordering::Acquire) {
            // With output still waiting, poll without blocking so it gets read
            // right after anything else that's ready.
            let timeout = (!self.unread.is_empty()).then_some(std::time::Duration::ZERO);
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
            for event in &events {
                self.handle(event);
            }
            for id in std::mem::take(&mut self.unread) {
                self.read_output(id);
            }
        }
        log::info!("shutting down");
        Ok(())
    }

    fn handle(&mut self, event: &Event) {
        match event.token() {
            LISTENER => self.accept_clients(),
            WAKER => {}
            token => match self.sources.get(token.0) {
                Some(Source::Output(id)) => {
                    let id = *id;
                    if event.is_readable() || event.is_read_closed() || event.is_error() {
                        self.read_output(id);
                    }
                    if event.is_writable() {
                        self.flush_input(id);
                    }
                }
                Some(Source::Exit(id)) => {
                    let id = *id;
                    self.reap(id);
                }
                Some(Source::Orphan(_)) => self.reap_orphan(token),
                Some(Source::Client(_)) => self.serve_client(token),
                None => {}
            },
        }
    }

    // --- Terminals --------------------------------------------------------

    fn read_output(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        match slot.terminal.read_output(&mut self.buffer, OUTPUT_BUDGET) {
            ReadOutcome::Drained => {}
            ReadOutcome::BudgetExhausted => self.unread.push_back(id),
            ReadOutcome::Closed => {
                if let Some(token) = slot.output.take() {
                    self.sources.remove(token.0);
                    // The PTY fd is already closed, which removed it from epoll.
                }
                slot.wants_writable = false;
            }
        }
        // Replies to the program's queries may have queued input.
        self.update_pty_interest(id);
    }

    fn flush_input(&mut self, id: TerminalId) {
        if let Some(slot) = self.terminals.get_mut(&id) {
            slot.terminal.flush_input();
            self.update_pty_interest(id);
        }
    }

    /// Registers the PTY for writability exactly while input is queued.
    fn update_pty_interest(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        let (Some(token), Some(fd)) = (slot.output, slot.terminal.pty_fd()) else {
            return;
        };
        let wants_writable = slot.terminal.has_pending_input();
        if wants_writable == slot.wants_writable {
            return;
        }
        let interest = if wants_writable {
            Interest::READABLE | Interest::WRITABLE
        } else {
            Interest::READABLE
        };
        match self
            .poll
            .registry()
            .reregister(&mut SourceFd(&fd), token, interest)
        {
            Ok(()) => slot.wants_writable = wants_writable,
            Err(err) => log::error!("terminal {id}: updating PTY registration failed: {err}"),
        }
    }

    fn reap(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        let Some(token) = slot.exit else {
            return;
        };
        if slot.terminal.reap() {
            // Reaping closed the pidfd, which removed it from epoll.
            slot.exit = None;
            self.sources.remove(token.0);
        }
    }

    fn reap_orphan(&mut self, token: Token) {
        if let Some(Source::Orphan(orphan)) = self.sources.get_mut(token.0)
            && orphan.reap()
        {
            self.sources.remove(token.0);
        }
    }

    fn new_terminal(
        &mut self,
        request: &clackatoo_protocol::NewTerminal,
    ) -> Result<TerminalId, Error> {
        let id = TerminalId(self.next_id);
        let terminal = HostedTerminal::start(id, request, &self.config)?;
        self.next_id += 1;

        let output = self.register_fd(terminal.pty_fd(), Source::Output(id));
        let exit = self.register_fd(terminal.exit_fd(), Source::Exit(id));
        match (output, exit) {
            (Ok(output), Ok(exit)) => {
                self.terminals.insert(
                    id,
                    Slot {
                        terminal,
                        output,
                        exit,
                        wants_writable: false,
                    },
                );
                Ok(id)
            }
            (output, exit) => {
                for token in [output, exit].into_iter().flatten().flatten() {
                    self.sources.remove(token.0);
                }
                // Dropping `terminal` hangs up the program we just started.
                Err(Error::SpawnFailed(
                    "could not watch the new terminal".to_owned(),
                ))
            }
        }
    }

    fn close_terminal(&mut self, id: TerminalId) -> Result<(), Error> {
        let slot = self
            .terminals
            .remove(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        self.unread.retain(|&unread| unread != id);
        for token in [slot.output, slot.exit].into_iter().flatten() {
            self.sources.remove(token.0);
        }
        if let Some(orphan) = slot.terminal.close() {
            // The pidfd is still registered under the exit token, which we just
            // freed. Re-register it under a new token for the orphan.
            let fd = orphan.exit_fd();
            let entry = self.sources.vacant_entry();
            let token = Token(entry.key());
            entry.insert(Source::Orphan(orphan));
            if let Err(err) =
                self.poll
                    .registry()
                    .reregister(&mut SourceFd(&fd), token, Interest::READABLE)
            {
                log::error!("terminal {id}: watching the hung-up program failed: {err}");
                self.sources.remove(token.0);
            }
        }
        log::info!("terminal {id} closed");
        Ok(())
    }

    /// Registers `fd` for readability under a new token. `None` for a missing
    /// fd is not an error.
    fn register_fd(&mut self, fd: Option<RawFd>, source: Source) -> io::Result<Option<Token>> {
        let Some(fd) = fd else {
            return Ok(None);
        };
        let entry = self.sources.vacant_entry();
        let token = Token(entry.key());
        self.poll
            .registry()
            .register(&mut SourceFd(&fd), token, Interest::READABLE)?;
        entry.insert(source);
        Ok(Some(token))
    }

    // --- Clients ----------------------------------------------------------

    fn accept_clients(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((mut stream, _)) => {
                    let entry = self.sources.vacant_entry();
                    let token = Token(entry.key());
                    if let Err(err) =
                        self.poll
                            .registry()
                            .register(&mut stream, token, Interest::READABLE)
                    {
                        log::error!("registering a client failed: {err}");
                        continue;
                    }
                    entry.insert(Source::Client(Client {
                        stream,
                        decoder: FrameDecoder::new(),
                        outgoing: Vec::new(),
                        greeted: false,
                        finished: false,
                        wants_writable: false,
                    }));
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::error!("accepting a client failed: {err}");
                    return;
                }
            }
        }
    }

    /// Reads whatever a client sent, answers its requests, and writes what
    /// it can of the answers.
    fn serve_client(&mut self, token: Token) {
        let Some(requests) = self.receive_requests(token) else {
            return;
        };
        for request in requests {
            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            let greeted = client.greeted;
            let (response, finish) = self.respond(request, greeted);

            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            client.greeted |= matches!(response, Response::Hello { .. });
            client.finished |= finish;
            match frame::encode(&response) {
                Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                Err(err) => {
                    log::error!("encoding a response failed: {err}");
                    client.finished = true;
                }
            }
            if client.finished {
                break;
            }
        }
        self.flush_client(token);
    }

    /// Reads and decodes everything a client has sent. Returns `None` if the
    /// client was dropped.
    fn receive_requests(&mut self, token: Token) -> Option<Vec<Request>> {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return None;
        };
        loop {
            match client.stream.read(&mut self.buffer) {
                Ok(0) => {
                    client.finished = true;
                    break;
                }
                Ok(n) => client.decoder.push(&self.buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::debug!("client read failed: {err}");
                    self.drop_client(token);
                    return None;
                }
            }
        }

        let mut requests = Vec::new();
        loop {
            match client.decoder.decode::<Request>() {
                Ok(Some(request)) => requests.push(request),
                Ok(None) => break,
                Err(err) => {
                    log::warn!("dropping a client that sent a bad frame: {err}");
                    self.drop_client(token);
                    return None;
                }
            }
        }
        Some(requests)
    }

    /// Answers one request. Also returns whether to end the connection after
    /// sending the answer.
    fn respond(&mut self, request: Request, greeted: bool) -> (Response, bool) {
        let response = match request {
            Request::Hello { protocol_version } => {
                let compatible = protocol_version == PROTOCOL_VERSION;
                if !compatible {
                    log::warn!(
                        "client speaks protocol {protocol_version}, we speak {PROTOCOL_VERSION}"
                    );
                }
                return (
                    Response::Hello {
                        protocol_version: PROTOCOL_VERSION,
                    },
                    !compatible,
                );
            }
            _ if !greeted => return (Response::Error(Error::HelloRequired), true),
            Request::ListTerminals => Ok(Response::Terminals(
                self.terminals
                    .values()
                    .map(|slot| slot.terminal.info())
                    .collect(),
            )),
            Request::NewTerminal(request) => {
                self.new_terminal(&request).map(Response::TerminalCreated)
            }
            Request::SendInput { terminal, bytes } => {
                self.send_input(terminal, &bytes).map(|()| Response::Done)
            }
            Request::Capture { terminal } => self
                .terminals
                .get(&terminal)
                .map(|slot| Response::Screen(slot.terminal.screen()))
                .ok_or(Error::NoSuchTerminal(terminal)),
            Request::CloseTerminal { terminal } => {
                self.close_terminal(terminal).map(|()| Response::Done)
            }
        };
        (response.unwrap_or_else(Response::Error), false)
    }

    fn send_input(&mut self, id: TerminalId, bytes: &[u8]) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        slot.terminal.send_input(bytes)?;
        self.update_pty_interest(id);
        Ok(())
    }

    /// Writes a client's queued responses until the socket is full, and drops
    /// the client once it is finished and flushed.
    fn flush_client(&mut self, token: Token) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        let mut written = 0;
        while written < client.outgoing.len() {
            match client.stream.write(&client.outgoing[written..]) {
                Ok(n) => written += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::debug!("client write failed: {err}");
                    self.drop_client(token);
                    return;
                }
            }
        }
        client.outgoing.drain(..written);

        let flushed = client.outgoing.is_empty();
        if flushed && client.finished {
            self.drop_client(token);
            return;
        }
        // Ask to hear about writability only while responses are queued.
        let wants_writable = !flushed;
        if wants_writable != client.wants_writable {
            let interest = if wants_writable {
                Interest::READABLE | Interest::WRITABLE
            } else {
                Interest::READABLE
            };
            if let Err(err) = self
                .poll
                .registry()
                .reregister(&mut client.stream, token, interest)
            {
                log::error!("updating a client registration failed: {err}");
                self.drop_client(token);
                return;
            }
            client.wants_writable = wants_writable;
        }
    }

    /// Forgets a client. Dropping its stream closes the socket, which also
    /// removes it from epoll.
    fn drop_client(&mut self, token: Token) {
        self.sources.remove(token.0);
    }
}
