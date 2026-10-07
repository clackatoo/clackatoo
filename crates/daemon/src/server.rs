//! The daemon's event loop.
//!
//! One thread waits on every file descriptor the daemon cares about: the
//! listening socket, each client connection, each PTY, and each program's
//! pidfd. The terminals' grids stay on that thread, so nothing is locked or
//! shared, and output goes from PTY to grid with no handoff in between.
//!
//! Clients on other machines talk over QUIC, inside a connection to the
//! socket that carries its packets (see [`quic`]). The QUIC endpoint is
//! driven here too: a client over QUIC is served just like one on the
//! socket.
//!
//! mio delivers edge-triggered events: an fd is reported once when it
//! becomes ready, not again until it has been drained. Every handler
//! therefore reads or writes until `WouldBlock`, or records that it stopped
//! early. Handlers must also tolerate spurious events, because a token freed
//! and reused within one batch of events can receive the old one.

use std::collections::{BTreeMap, VecDeque, btree_map};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::os::fd::{AsFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, Error, GridState, HistoryLine, HistoryLines, Line, NodeId, PROTOCOL_VERSION,
    PaneId, PaneTerminal, Patch, Request, Response, ServerMessage, Side, Size, TerminalId,
    TerminalStatus, Udp, View, ViewUpdate, WorkspaceId,
};
use clackatoo_term::{Damage, Terminal};
use mio::event::Event;
use mio::net::{UdpSocket, UnixListener, UnixStream};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Registry, Token, Waker};
use quinn_proto::{ConnectionHandle, StreamId};
use slab::Slab;

use crate::DaemonConfig;
use crate::handover::{self, Fds, Handover, HandoverError};
use crate::hosted::{HostedTerminal, Orphan, ReadOutcome};
use crate::memory;
use crate::quic::{self, CarrierId, QuicEvent, QuicServer, Received};
use crate::upgrade::Upgrade;
use crate::workspaces::WorkspaceState;

const LISTENER: Token = Token(usize::MAX);
const WAKER: Token = Token(usize::MAX - 1);
const UDP: Token = Token(usize::MAX - 2);

/// Bytes read per `read` call, from PTYs and clients alike.
const READ_BUFFER_LEN: usize = 64 * 1024;

/// The most output read from one PTY before giving the other terminals and
/// clients a turn. Keeps a flood of output (`cat` of a huge file) from
/// freezing everything else.
const OUTPUT_BUDGET: usize = 1024 * 1024;

/// How long a terminal's output must pause before its scrollback is
/// compacted: long enough that a flood never stops for it, short enough
/// that the memory comes back soon after.
const COMPACT_AFTER: Duration = Duration::from_millis(100);

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
    Carrier(Carrier),
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
    /// The clients following this terminal's view.
    viewers: Vec<Token>,
}

/// A connected client.
#[derive(Debug)]
// Separate facts about the connection, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
struct Client {
    link: Link,
    decoder: FrameDecoder,
    /// Encoded responses not yet written.
    outgoing: Vec<u8>,
    greeted: bool,
    /// The connection ends once `outgoing` is flushed: the client hung up, or
    /// broke the protocol.
    finished: bool,
    wants_writable: bool,
    /// The terminals whose views the client follows.
    views: BTreeMap<TerminalId, Subscription>,
    /// Whether the client is told when the workspaces change.
    watching_workspaces: bool,
    /// The connection is to become a carrier, once its answer is queued.
    carrying: bool,
    /// Over QUIC, the stream each terminal's view updates go on, so one
    /// terminal's never wait behind another's, or behind answers. Kept
    /// while the terminal lives, even when the client stops following it:
    /// a new stream could deliver a fresh snapshot ahead of the old
    /// stream's last patch.
    streams: BTreeMap<TerminalId, ViewStream>,
}

/// A stream a QUIC client is sent one terminal's view updates on.
#[derive(Debug)]
struct ViewStream {
    id: StreamId,
    /// Encoded updates not yet taken by the stream.
    outgoing: Vec<u8>,
}

impl Client {
    fn new(link: Link) -> Self {
        Self {
            link,
            decoder: FrameDecoder::new(),
            outgoing: Vec::new(),
            greeted: false,
            finished: false,
            wants_writable: false,
            views: BTreeMap::new(),
            watching_workspaces: false,
            carrying: false,
            streams: BTreeMap::new(),
        }
    }
}

/// How a client is connected.
#[derive(Debug)]
enum Link {
    /// To the daemon's socket.
    Unix(UnixStream),
    /// Over QUIC, from another machine.
    Quic(ConnectionHandle),
}

/// A connection to the daemon's socket that carries QUIC packets for
/// clients on another machine, each framed like a message.
#[derive(Debug)]
struct Carrier {
    stream: UnixStream,
    /// Packets arriving.
    packets: FrameDecoder,
    /// Packets not yet written.
    outgoing: Vec<u8>,
    wants_writable: bool,
    /// How many carriers came before it, which tells it apart from any
    /// that had its token before.
    generation: u32,
}

/// A client following one terminal's view, and what it was last sent.
#[derive(Debug)]
struct Subscription {
    /// Lines changed since the last update was sent.
    damage: Damage,
    /// The size last sent. `None` until the first snapshot is sent, and
    /// after a re-attach, which asks for a fresh one.
    size: Option<Size>,
    title: String,
    state: GridState,
    /// An update was sent and not yet acknowledged with `FrameDone`.
    awaiting_ack: bool,
}

impl Subscription {
    fn new() -> Self {
        Self {
            damage: Damage::new(),
            size: None,
            title: String::new(),
            state: GridState::default(),
            awaiting_ack: false,
        }
    }

    /// The update that brings the client from what it was last sent to what
    /// `terminal` shows now, recorded as sent. `None` if nothing changed.
    fn update(&mut self, terminal: &Terminal) -> Option<ViewUpdate> {
        let size = terminal.size();
        let state = GridState::of(terminal);
        if self.size != Some(size) {
            self.size = Some(size);
            terminal.title().clone_into(&mut self.title);
            self.state = state;
            self.damage.clear();
            return Some(ViewUpdate::Snapshot(View::capture(terminal)));
        }

        let mut patches = Vec::new();
        if terminal.title() != self.title {
            terminal.title().clone_into(&mut self.title);
            patches.push(Patch::Title(self.title.clone()));
        }
        let grid = terminal.grid();
        let lines: Vec<(u16, Line)> = self
            .damage
            .lines(grid.height())
            .map(|line| {
                let index = u16::try_from(line).expect("screen lines are bounded by Size::MAX");
                (index, Line::from_row(grid.row(line)))
            })
            .collect();
        self.damage.clear();
        if !lines.is_empty() || state != self.state {
            self.state = state;
            patches.push(Patch::Grid {
                node: NodeId::ROOT,
                state,
                lines,
            });
        }
        (!patches.is_empty()).then_some(ViewUpdate::Patch(patches))
    }
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
    workspaces: WorkspaceState,
    /// The QUIC endpoint, set up when a connection first carries QUIC.
    quic: Option<QuicServer>,
    /// What the QUIC endpoint last asked to be acted on.
    quic_events: Vec<QuicEvent>,
    /// How many connections have become carriers.
    carriers_made: u32,
    /// Where QUIC connections that have moved off their carriers send and
    /// receive, set up along with the QUIC endpoint.
    udp: Option<UdpSocket>,
    /// Terminals whose output budget ran out with more output waiting.
    unread: VecDeque<TerminalId>,
    /// When each terminal with uncompacted scrollback should compact it.
    compactions: BTreeMap<TerminalId, Instant>,
    /// Some view may have an update to send.
    views_pending: bool,
    /// Where a terminal's damage is collected before it goes to each viewer.
    damage: Damage,
    config: DaemonConfig,
    buffer: Box<[u8]>,
    /// A client asked the daemon to become this program: see
    /// [`ClientMessage::Upgrade`]. The client's token.
    upgrade_to: Option<(PathBuf, Token)>,
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
        let listener = UnixListener::bind(socket_path)?;
        log::info!("listening on {}", socket_path.display());
        Self::listening(listener, config)
    }

    /// A daemon with no terminals, listening on `listener`.
    fn listening(mut listener: UnixListener, config: DaemonConfig) -> io::Result<Self> {
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
        Ok(Self {
            poll,
            listener,
            waker,
            shutdown: Arc::new(AtomicBool::new(false)),
            sources: Slab::new(),
            terminals: BTreeMap::new(),
            next_id: 1,
            workspaces: WorkspaceState::new(),
            quic: None,
            quic_events: Vec::new(),
            carriers_made: 0,
            udp: None,
            unread: VecDeque::new(),
            compactions: BTreeMap::new(),
            views_pending: false,
            damage: Damage::new(),
            config,
            buffer: vec![0; READ_BUFFER_LEN].into_boxed_slice(),
            upgrade_to: None,
        })
    }

    /// Carries on from the daemon that handed over `handover`: an older
    /// build, or this process if exec'ing the new build failed. A terminal
    /// that can't be restored is lost, and its program hung up; workspaces
    /// that can't be are dropped, keeping their terminals.
    ///
    /// # Errors
    ///
    /// If the listening socket is missing, or can't be watched.
    pub(crate) fn resume(
        mut handover: Handover,
        config: DaemonConfig,
    ) -> Result<Self, HandoverError> {
        let state = &handover.state;
        let fds = &mut handover.fds;
        let listener = std::os::unix::net::UnixListener::from(fds.take(state.listener)?);
        let mut daemon = Self::listening(UnixListener::from_std(listener), config)?;
        daemon.next_id = state.next_terminal;
        daemon.workspaces = WorkspaceState::resume(&state.workspaces).unwrap_or_else(|err| {
            log::error!("the workspaces couldn't be taken over: {err}");
            WorkspaceState::new()
        });

        for terminal in &state.terminals {
            let id = TerminalId(terminal.id);
            let resumed = HostedTerminal::resume(terminal, fds, &daemon.config)
                .map_err(|err| err.to_string())
                .and_then(|terminal| daemon.add_terminal(terminal).map_err(|err| err.to_string()));
            if let Err(err) = resumed {
                log::error!("terminal {id} couldn't be taken over: {err}");
                daemon.workspaces.remove_terminal(id);
            }
        }
        for orphan in &state.orphans {
            match Orphan::resume(orphan, fds) {
                Ok(orphan) => daemon.watch_orphan(orphan),
                Err(err) => log::error!("a hung-up program couldn't be taken over: {err}"),
            }
        }
        let terminals = daemon.terminals.len();
        log::info!(
            "carrying on with {terminals} terminal{}",
            if terminals == 1 { "" } else { "s" }
        );
        Ok(daemon)
    }

    /// Everything the next build needs, taken out of this daemon, which is
    /// left with nothing to hang up when it's dropped. The listening socket
    /// is shared: closing this daemon's copy leaves it open.
    fn hand_over(&mut self) -> io::Result<Handover> {
        let mut fds = Fds::default();
        let listener = fds.add(self.listener.as_fd().try_clone_to_owned()?);
        let terminals = std::mem::take(&mut self.terminals)
            .into_values()
            .map(|slot| slot.terminal.hand_over(&mut fds))
            .collect();
        let orphan_tokens: Vec<usize> = (self.sources.iter())
            .filter(|(_, source)| matches!(source, Source::Orphan(_)))
            .map(|(key, _)| key)
            .collect();
        let orphans = (orphan_tokens.into_iter())
            .filter_map(|key| match self.sources.remove(key) {
                Source::Orphan(orphan) => Some(orphan.hand_over(&mut fds)),
                _ => None,
            })
            .collect();
        Ok(Handover {
            state: handover::State {
                listener,
                lock: None,
                next_terminal: self.next_id,
                terminals,
                orphans,
                workspaces: self.workspaces.hand_over(),
            },
            fds,
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

    /// Serves until shut down, or until a client asks the daemon to
    /// upgrade: then it returns the [`Upgrade`], holding everything the
    /// daemon had, for [`serve`](crate::upgrade::serve). Dropping the daemon
    /// once it is shut down closes every terminal, which hangs up their
    /// programs.
    ///
    /// # Errors
    ///
    /// If waiting for events fails, which leaves the daemon unable to work.
    pub fn run(&mut self) -> io::Result<Option<Upgrade>> {
        let mut events = Events::with_capacity(256);
        while !self.shutdown.load(Ordering::Acquire) {
            if let Some((executable, token)) = self.upgrade_to.take() {
                // The client that asked hears how it went: its connection
                // closes as the new build starts, or says why it didn't.
                let requester = match self.sources.try_remove(token.0) {
                    Some(Source::Client(Client {
                        link: Link::Unix(mut stream),
                        ..
                    })) => {
                        let _ = self.poll.registry().deregister(&mut stream);
                        Some(stream)
                    }
                    _ => None,
                };
                return Ok(Some(Upgrade {
                    executable,
                    handover: self.hand_over()?,
                    requester,
                }));
            }
            // With output still waiting, poll without blocking so it gets read
            // right after anything else that's ready. Otherwise wait for the
            // next compaction or QUIC timer, if there is one.
            let timeout = if self.unread.is_empty() {
                let compaction = self.compactions.values().min().copied();
                let quic = self.quic.as_mut().and_then(QuicServer::next_timeout);
                let next = compaction.into_iter().chain(quic).min();
                next.map(|due| due.saturating_duration_since(Instant::now()))
            } else {
                Some(Duration::ZERO)
            };
            match self.poll.poll(&mut events, timeout) {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
            for event in &events {
                self.handle(event);
            }
            self.drive_quic();
            for id in std::mem::take(&mut self.unread) {
                self.read_output(id);
            }
            self.compact_scrollbacks();
            self.send_view_updates();
            self.transmit_quic();
        }
        log::info!("shutting down");
        Ok(None)
    }

    fn handle(&mut self, event: &Event) {
        match event.token() {
            LISTENER => self.accept_clients(),
            WAKER => {}
            UDP => self.receive_udp(),
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
                Some(Source::Carrier(_)) => self.serve_carrier(token, event),
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
        self.schedule_compaction(id);
        self.collect_damage(id);
    }

    /// Hands the lines the terminal changed to each client following it.
    fn collect_damage(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        self.damage.clear();
        slot.terminal.take_damage(&mut self.damage);
        // The cursor or title may have changed without damage, so viewers
        // are checked even then.
        for &token in &slot.viewers {
            if let Some(Source::Client(client)) = self.sources.get_mut(token.0)
                && let Some(subscription) = client.views.get_mut(&id)
            {
                subscription.damage.merge(&self.damage);
                self.views_pending = true;
            }
        }
    }

    /// Sends each view update that's due: one per subscription that has
    /// changes, has had its last update acknowledged, and whose client has
    /// written everything queued before it: on the socket, everything; over
    /// QUIC, everything on that terminal's stream. Holding back for slow
    /// clients means changes pile up into one update instead of a backlog.
    fn send_view_updates(&mut self) {
        if !std::mem::take(&mut self.views_pending) {
            return;
        }
        let mut ready = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            let quic = match client.link {
                Link::Unix(_) => None,
                Link::Quic(handle) => Some(handle),
            };
            if client.finished || (quic.is_none() && !client.outgoing.is_empty()) {
                continue;
            }
            for (&id, subscription) in &mut client.views {
                if subscription.awaiting_ack {
                    continue;
                }
                let Some(slot) = self.terminals.get(&id) else {
                    continue;
                };
                // Its own stream, if the connection allows another.
                let stream = quic.and_then(|handle| {
                    let quic = self.quic.as_mut()?;
                    let stream = match client.streams.entry(id) {
                        btree_map::Entry::Occupied(entry) => entry.into_mut(),
                        btree_map::Entry::Vacant(entry) => entry.insert(ViewStream {
                            id: quic.open_stream(handle)?,
                            outgoing: Vec::new(),
                        }),
                    };
                    Some(&mut stream.outgoing)
                });
                let outgoing = match stream {
                    Some(outgoing) if !outgoing.is_empty() => continue,
                    Some(outgoing) => outgoing,
                    None if quic.is_some() && !client.outgoing.is_empty() => continue,
                    None => &mut client.outgoing,
                };
                if let Some(update) = subscription.update(slot.terminal.emulator()) {
                    subscription.awaiting_ack = true;
                    let event = ServerMessage::Event(clackatoo_protocol::Event::View {
                        terminal: id,
                        update,
                    });
                    match frame::encode(&event) {
                        Ok(bytes) => {
                            outgoing.extend_from_slice(&bytes);
                            ready.push(Token(key));
                        }
                        Err(err) => log::error!("encoding a view update failed: {err}"),
                    }
                }
            }
        }
        ready.dedup();
        for token in ready {
            self.flush_client(token);
        }
    }

    /// Puts off compacting the terminal's scrollback until its output has
    /// paused, if it has rows to compact.
    fn schedule_compaction(&mut self, id: TerminalId) {
        if self
            .terminals
            .get(&id)
            .is_some_and(|slot| slot.terminal.scrollback_needs_compaction())
        {
            self.compactions.insert(id, Instant::now() + COMPACT_AFTER);
        }
    }

    /// Compacts the scrollback of each terminal whose output has paused.
    fn compact_scrollbacks(&mut self) {
        if self.compactions.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut compacted = false;
        self.compactions.retain(|id, due| {
            if *due > now {
                return true;
            }
            if let Some(slot) = self.terminals.get_mut(id) {
                slot.terminal.compact_scrollback();
                compacted = true;
            }
            false
        });
        if compacted {
            memory::release_free_memory();
        }
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
            // A pane closes when its program exits, as in other terminals.
            if self.workspaces.get().pane_showing(id).is_some()
                && let Err(err) = self.close_terminal(id)
            {
                log::error!("terminal {id}: closing its pane failed: {err}");
            }
        }
    }

    /// Watches a hung-up program until it exits, to reap it.
    fn watch_orphan(&mut self, orphan: Orphan) {
        let fd = orphan.exit_fd();
        if let Err(err) = self.register_fd(Some(fd), Source::Orphan(orphan)) {
            // Dropped unreaped: it stays a zombie until the daemon exits.
            log::error!("watching a hung-up program failed: {err}");
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
        // Dropping `terminal` on failure hangs up the program just started.
        self.add_terminal(terminal)
            .map_err(|_| Error::SpawnFailed("could not watch the new terminal".to_owned()))?;
        Ok(id)
    }

    /// Watches `terminal`'s PTY and program, and adds it.
    fn add_terminal(&mut self, terminal: HostedTerminal) -> io::Result<()> {
        let id = terminal.info().id;
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
                        viewers: Vec::new(),
                    },
                );
                // Input left over from before an upgrade.
                self.update_pty_interest(id);
                Ok(())
            }
            (output, exit) => {
                for token in [&output, &exit].into_iter().flatten().flatten() {
                    self.sources.remove(token.0);
                }
                Err(output.and(exit).expect_err("one failed"))
            }
        }
    }

    fn close_terminal(&mut self, id: TerminalId) -> Result<(), Error> {
        let slot = self
            .terminals
            .remove(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        self.unread.retain(|&unread| unread != id);
        self.compactions.remove(&id);
        for token in &slot.viewers {
            if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                client.views.remove(&id);
            }
        }
        // Clients over QUIC keep a stream for each terminal they've
        // followed, until it closes.
        for (_, source) in &mut self.sources {
            if let Source::Client(client) = source
                && let Link::Quic(handle) = client.link
                && let Some(stream) = client.streams.remove(&id)
                && let Some(quic) = self.quic.as_mut()
            {
                quic.finish(handle, stream.id);
            }
        }
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
        if self.workspaces.remove_terminal(id) {
            self.workspaces_changed();
        }
        // Its history may have been thousands of rows.
        memory::release_free_memory();
        log::info!("terminal {id} closed");
        Ok(())
    }

    /// Closes `terminals`, as closing their tab or workspace does.
    fn close_terminals(&mut self, terminals: &[TerminalId]) -> Result<Response, Error> {
        for &terminal in terminals {
            self.close_terminal(terminal)?;
        }
        Ok(Response::Done)
    }

    /// The terminal for a new pane: a new one, starting by default in the
    /// directory `beside`'s terminal is in, or an existing one in no pane.
    fn pane_terminal(
        &mut self,
        terminal: PaneTerminal,
        beside: Option<PaneId>,
    ) -> Result<TerminalId, Error> {
        match terminal {
            PaneTerminal::Existing(id) => {
                let slot = self.terminals.get(&id).ok_or(Error::NoSuchTerminal(id))?;
                if slot.terminal.info().status != TerminalStatus::Running {
                    return Err(Error::TerminalExited(id));
                }
                if self.workspaces.get().pane_showing(id).is_some() {
                    return Err(Error::TerminalInPane(id));
                }
                Ok(id)
            }
            PaneTerminal::New(mut request) => {
                if request.cwd.is_none() {
                    request.cwd = beside.and_then(|pane| self.working_directory(pane));
                }
                self.new_terminal(&request)
            }
        }
    }

    /// The directory the program in `pane`'s terminal is in: for a shell,
    /// where it last changed to.
    fn working_directory(&self, pane: PaneId) -> Option<String> {
        let terminal = self.workspaces.check_pane(pane).ok()?;
        let pid = self.terminals.get(&terminal)?.terminal.info().pid;
        let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        cwd.into_os_string().into_string().ok()
    }

    fn new_workspace(
        &mut self,
        name: Option<&str>,
        terminal: PaneTerminal,
    ) -> Result<Response, Error> {
        let name = self.workspaces.name_for_new(name)?;
        let terminal = self.pane_terminal(terminal, None)?;
        let pane = self.workspaces.new_workspace(name, terminal);
        self.workspaces_changed();
        Ok(Response::PaneCreated(pane))
    }

    fn new_tab(
        &mut self,
        workspace: WorkspaceId,
        terminal: PaneTerminal,
    ) -> Result<Response, Error> {
        self.workspaces.check_workspace(workspace)?;
        let beside = self.workspaces.focused_pane(workspace);
        let terminal = self.pane_terminal(terminal, beside)?;
        let pane = self.workspaces.new_tab(workspace, terminal);
        self.workspaces_changed();
        Ok(Response::PaneCreated(pane))
    }

    fn split(
        &mut self,
        pane: PaneId,
        side: Side,
        terminal: PaneTerminal,
    ) -> Result<Response, Error> {
        self.workspaces.check_pane(pane)?;
        let terminal = self.pane_terminal(terminal, Some(pane))?;
        let new = self.workspaces.split(pane, side, terminal);
        self.workspaces_changed();
        Ok(Response::PaneCreated(new))
    }

    /// Tells every client watching the workspaces what they are now. The
    /// client whose request changed them hears before the request's answer,
    /// which is queued after this.
    fn workspaces_changed(&mut self) {
        let event = ServerMessage::Event(clackatoo_protocol::Event::Workspaces(
            self.workspaces.get().clone(),
        ));
        let bytes = match frame::encode(&event) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::error!("encoding the workspaces failed: {err}");
                return;
            }
        };
        let mut watchers = Vec::new();
        for (key, source) in &mut self.sources {
            if let Source::Client(client) = source
                && client.watching_workspaces
                && !client.finished
            {
                client.outgoing.extend_from_slice(&bytes);
                watchers.push(Token(key));
            }
        }
        for token in watchers {
            self.flush_client(token);
        }
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
                    entry.insert(Source::Client(Client::new(Link::Unix(stream))));
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
        let Some(messages) = self.receive_messages(token) else {
            return;
        };
        for message in messages {
            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            let greeted = client.greeted;
            let reply = match message {
                ClientMessage::Hello { protocol_version } => {
                    let compatible = protocol_version == PROTOCOL_VERSION;
                    if !compatible {
                        log::warn!(
                            "client speaks protocol {protocol_version}, we speak {PROTOCOL_VERSION}"
                        );
                    }
                    client.greeted = compatible;
                    client.finished |= !compatible;
                    ServerMessage::Hello {
                        protocol_version: PROTOCOL_VERSION,
                    }
                }
                ClientMessage::Upgrade {
                    protocol_version,
                    executable,
                } if !greeted => {
                    client.finished = true;
                    let local = matches!(client.link, Link::Unix(_));
                    let executable = PathBuf::from(std::ffi::OsString::from_vec(executable));
                    match self.check_upgrade(local, protocol_version, &executable) {
                        Ok(()) => {
                            // The connection closes as the daemon upgrades.
                            self.upgrade_to = Some((executable, token));
                            return;
                        }
                        Err(reason) => {
                            log::warn!("not upgrading: {reason}");
                            ServerMessage::NotUpgrading { reason }
                        }
                    }
                }
                ClientMessage::Upgrade { .. } => {
                    client.finished = true;
                    ServerMessage::NotUpgrading {
                        reason: "an upgrade must be asked for instead of Hello".to_owned(),
                    }
                }
                _ if !greeted => {
                    client.finished = true;
                    ServerMessage::Response(Response::Error(Error::HelloRequired))
                }
                ClientMessage::FrameDone { terminal } => {
                    if let Some(subscription) = client.views.get_mut(&terminal) {
                        subscription.awaiting_ack = false;
                        self.views_pending = true;
                    }
                    continue;
                }
                ClientMessage::Request(request) => {
                    ServerMessage::Response(self.respond(request, token))
                }
            };

            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            match frame::encode(&reply) {
                Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                Err(err) => {
                    log::error!("encoding a response failed: {err}");
                    client.finished = true;
                }
            }
            if client.finished || client.carrying {
                break;
            }
        }
        if matches!(self.sources.get(token.0), Some(Source::Client(client)) if client.carrying) {
            self.become_carrier(token);
        } else {
            self.flush_client(token);
        }
    }

    /// Why the daemon won't become `executable`, if it won't, as a client
    /// speaking `protocol_version` asked, on this machine if `local`.
    fn check_upgrade(
        &self,
        local: bool,
        protocol_version: u32,
        executable: &Path,
    ) -> Result<(), String> {
        if !local {
            return Err("only a client on the daemon's machine can upgrade it".to_owned());
        }
        if protocol_version < PROTOCOL_VERSION {
            return Err(format!(
                "the daemon is newer (protocol {PROTOCOL_VERSION}) than the build it was asked to \
                 become (protocol {protocol_version})"
            ));
        }
        if self.upgrade_to.is_some() {
            return Err("the daemon is already upgrading".to_owned());
        }
        crate::upgrade::check_takes_over(executable)
    }

    /// Reads and decodes everything a client has sent. Returns `None` if the
    /// client was dropped.
    fn receive_messages(&mut self, token: Token) -> Option<Vec<ClientMessage>> {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return None;
        };
        match &mut client.link {
            Link::Unix(stream) => loop {
                match stream.read(&mut self.buffer) {
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
            },
            Link::Quic(handle) => {
                let quic = self.quic.as_mut()?;
                if quic.read(*handle, &mut client.decoder) == Received::Ended {
                    client.finished = true;
                }
            }
        }

        let mut messages = Vec::new();
        loop {
            match client.decoder.decode::<ClientMessage>() {
                Ok(Some(message)) => messages.push(message),
                Ok(None) => break,
                Err(err) => {
                    log::warn!("dropping a client that sent a bad frame: {err}");
                    self.drop_client(token);
                    return None;
                }
            }
        }
        Some(messages)
    }

    /// Answers one request from the client at `token`.
    fn respond(&mut self, request: Request, token: Token) -> Response {
        let response = match request {
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
                .map(|slot| Response::View(slot.terminal.view()))
                .ok_or(Error::NoSuchTerminal(terminal)),
            Request::CloseTerminal { terminal } => {
                self.close_terminal(terminal).map(|()| Response::Done)
            }
            Request::Resize { terminal, size } => match self.terminals.get_mut(&terminal) {
                Some(slot) => {
                    slot.terminal.resize(size);
                    // Rewrapping may have pushed rows into scrollback.
                    self.schedule_compaction(terminal);
                    self.collect_damage(terminal);
                    Ok(Response::Done)
                }
                None => Err(Error::NoSuchTerminal(terminal)),
            },
            Request::Attach { terminal } => self.attach(token, terminal).map(|()| Response::Done),
            Request::Detach { terminal } => {
                self.detach(token, terminal);
                Ok(Response::Done)
            }
            Request::History {
                terminal,
                end,
                count,
            } => self
                .terminals
                .get(&terminal)
                .map(|slot| Response::History(history(slot.terminal.emulator(), end, count)))
                .ok_or(Error::NoSuchTerminal(terminal)),
            Request::Text {
                terminal,
                from,
                to,
                whole_lines,
            } => self
                .terminals
                .get(&terminal)
                .map(|slot| Response::Text(slot.terminal.emulator().text(from, to, whole_lines)))
                .ok_or(Error::NoSuchTerminal(terminal)),
            Request::Search {
                terminal,
                query,
                from,
                backward,
            } => self
                .terminals
                .get(&terminal)
                .map(|slot| {
                    Response::Found(slot.terminal.emulator().search(&query, from, backward))
                })
                .ok_or(Error::NoSuchTerminal(terminal)),
            request @ (Request::WatchWorkspaces
            | Request::NewWorkspace { .. }
            | Request::NewTab { .. }
            | Request::Split { .. }
            | Request::ClosePane { .. }
            | Request::Focus { .. }
            | Request::RenameWorkspace { .. }
            | Request::RenameTab { .. }
            | Request::CloseTab { .. }
            | Request::CloseWorkspace { .. }) => self.respond_about_workspaces(request, token),
            Request::CarryQuic => self.carry_quic(token),
        };
        response.unwrap_or_else(Response::Error)
    }

    /// Answers a request about workspaces from the client at `token`.
    fn respond_about_workspaces(
        &mut self,
        request: Request,
        token: Token,
    ) -> Result<Response, Error> {
        match request {
            Request::WatchWorkspaces => {
                if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                    client.watching_workspaces = true;
                }
                Ok(Response::Workspaces(self.workspaces.get().clone()))
            }
            Request::NewWorkspace { name, terminal } => {
                self.new_workspace(name.as_deref(), terminal)
            }
            Request::NewTab {
                workspace,
                terminal,
            } => self.new_tab(workspace, terminal),
            Request::Split {
                pane,
                side,
                terminal,
            } => self.split(pane, side, terminal),
            Request::ClosePane { pane } => self
                .workspaces
                .check_pane(pane)
                .and_then(|terminal| self.close_terminal(terminal))
                .map(|()| Response::Done),
            Request::Focus { pane } => self.workspaces.focus(pane).map(|()| {
                self.workspaces_changed();
                Response::Done
            }),
            Request::RenameWorkspace { workspace, name } => {
                self.workspaces.rename(workspace, &name).map(|()| {
                    self.workspaces_changed();
                    Response::Done
                })
            }
            Request::RenameTab { tab, name } => self.workspaces.rename_tab(tab, &name).map(|()| {
                self.workspaces_changed();
                Response::Done
            }),
            Request::CloseTab { tab } => {
                let terminals = self.workspaces.tab_terminals(tab)?;
                self.close_terminals(&terminals)
            }
            Request::CloseWorkspace { workspace } => {
                let terminals = self.workspaces.workspace_terminals(workspace)?;
                self.close_terminals(&terminals)
            }
            other => unreachable!("{other:?} isn't about workspaces"),
        }
    }

    /// Starts sending `id`'s view to the client at `token`, beginning with a
    /// snapshot.
    fn attach(&mut self, token: Token, id: TerminalId) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        if !slot.viewers.contains(&token) {
            slot.viewers.push(token);
        }
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.views.insert(id, Subscription::new());
        }
        self.views_pending = true;
        Ok(())
    }

    /// Stops sending `id`'s view to the client at `token`.
    fn detach(&mut self, token: Token, id: TerminalId) {
        if let Some(slot) = self.terminals.get_mut(&id) {
            slot.viewers.retain(|&viewer| viewer != token);
        }
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.views.remove(&id);
        }
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

    /// Writes a client's queued responses until the socket or stream is
    /// full, and drops the client once it is finished and flushed.
    fn flush_client(&mut self, token: Token) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        let written = match &mut client.link {
            Link::Unix(stream) => write_out(
                self.poll.registry(),
                stream,
                token,
                &mut client.outgoing,
                &mut client.wants_writable,
            ),
            Link::Quic(handle) => self
                .quic
                .as_mut()
                .and_then(|quic| {
                    let mut written = quic.write(*handle, None, &mut client.outgoing)?;
                    for stream in client.streams.values_mut() {
                        written += quic.write(*handle, Some(stream.id), &mut stream.outgoing)?;
                    }
                    Some(written)
                })
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe)),
        };
        let written = match written {
            Ok(written) => written,
            Err(err) => {
                log::debug!("client write failed: {err}");
                self.drop_client(token);
                return;
            }
        };
        let flushed = client.outgoing.is_empty();
        // Over QUIC, closing the connection discards what's still to send,
        // so a client dropped for breaking the protocol may not hear why.
        if flushed && client.finished {
            self.drop_client(token);
            return;
        }
        // View updates wait for a client to catch up: on the socket, with
        // everything; over QUIC, with each terminal's stream, which may
        // have drained even if answers are still waiting.
        if written > 0 && !client.views.is_empty() {
            self.views_pending = true;
        }
    }

    /// Forgets a client. Dropping its stream closes the socket, which also
    /// removes it from epoll.
    fn drop_client(&mut self, token: Token) {
        if let Some(Source::Client(client)) = self.sources.try_remove(token.0) {
            self.forget_viewer(token, &client);
            if let Link::Quic(handle) = client.link
                && let Some(quic) = self.quic.as_mut()
            {
                quic.close(handle, Instant::now(), "");
            }
        }
    }

    /// Takes the client at `token` off the viewers of the terminals it
    /// follows.
    fn forget_viewer(&mut self, token: Token, client: &Client) {
        for id in client.views.keys() {
            if let Some(slot) = self.terminals.get_mut(id) {
                slot.viewers.retain(|&viewer| viewer != token);
            }
        }
    }

    // --- QUIC -------------------------------------------------------------

    /// Answers [`Request::CarryQuic`] from the client at `token`, setting up
    /// the QUIC endpoint if this is the first carrier.
    fn carry_quic(&mut self, token: Token) -> Result<Response, Error> {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return Err(Error::CantCarry("the connection closed".into()));
        };
        if !matches!(client.link, Link::Unix(_)) {
            return Err(Error::CantCarry(
                "only a connection to the daemon's socket can carry QUIC".into(),
            ));
        }
        if self.quic.is_none() {
            self.quic = Some(QuicServer::new().map_err(|err| Error::CantCarry(err.to_string()))?);
            self.udp = self.config.udp_ports.clone().and_then(|ports| {
                bind_udp(self.poll.registry(), ports)
                    .inspect_err(|err| log::warn!("no UDP port for QUIC: {err}"))
                    .ok()
            });
        }
        client.carrying = true;
        let quic = self.quic.as_ref();
        let udp = self.udp.as_ref().and_then(|udp| udp.local_addr().ok());
        Ok(Response::Carrying {
            certificate: quic
                .map(|quic| quic.certificate().to_vec())
                .unwrap_or_default(),
            udp: udp.zip(quic).map(|(address, quic)| Udp {
                port: address.port(),
                probe_key: quic.probe_key(),
            }),
        })
    }

    /// Hands the QUIC endpoint every packet that has arrived over UDP, and
    /// sends probes straight back.
    fn receive_udp(&mut self) {
        let (Some(udp), Some(quic)) = (&self.udp, self.quic.as_mut()) else {
            return;
        };
        let now = Instant::now();
        let key = quic.probe_key();
        loop {
            match udp.recv_from(&mut self.buffer) {
                Ok((n, from)) if quic::is_probe(&self.buffer[..n], &key) => {
                    if let Err(err) = udp.send_to(&self.buffer[..n], from) {
                        log::debug!("answering a probe from {from} failed: {err}");
                    }
                }
                Ok((n, from)) => quic.receive(now, from, &self.buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    // Such as an ICMP error for an earlier packet. Whatever
                    // else is waiting brings another event.
                    if err.kind() != io::ErrorKind::WouldBlock {
                        log::debug!("UDP read failed: {err}");
                    }
                    return;
                }
            }
        }
    }

    /// Turns the client at `token`, which asked to carry QUIC and has its
    /// answer queued, into a carrier.
    fn become_carrier(&mut self, token: Token) {
        let Some(Source::Client(client)) = self.sources.try_remove(token.0) else {
            return;
        };
        self.forget_viewer(token, &client);
        let Link::Unix(mut stream) = client.link else {
            return;
        };
        let entry = self.sources.vacant_entry();
        let carrier = Token(entry.key());
        if let Err(err) = self
            .poll
            .registry()
            .reregister(&mut stream, carrier, Interest::READABLE)
        {
            log::error!("registering a carrier failed: {err}");
            return;
        }
        self.carriers_made = self.carriers_made.wrapping_add(1);
        entry.insert(Source::Carrier(Carrier {
            stream,
            packets: client.decoder,
            outgoing: client.outgoing,
            wants_writable: false,
            generation: self.carriers_made,
        }));
        log::info!("a connection now carries QUIC");
        self.flush_carrier(carrier);
        // The client waits for the answer before sending packets, but
        // anything already here is a packet.
        self.receive_packets(carrier, false);
    }

    /// Reads the packets a carrier has brought, and writes what it can of
    /// those waiting to go.
    fn serve_carrier(&mut self, token: Token, event: &Event) {
        if event.is_writable() {
            self.flush_carrier(token);
        }
        let Some(Source::Carrier(carrier)) = self.sources.get_mut(token.0) else {
            return;
        };
        let mut closed = false;
        loop {
            match carrier.stream.read(&mut self.buffer) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(n) => carrier.packets.push(&self.buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::debug!("carrier read failed: {err}");
                    closed = true;
                    break;
                }
            }
        }
        self.receive_packets(token, closed);
    }

    /// Hands the QUIC endpoint the packets the carrier at `token` has
    /// brought, then closes the carrier if it has `closed`, or its packets
    /// are malformed.
    fn receive_packets(&mut self, token: Token, mut closed: bool) {
        let (Some(Source::Carrier(carrier)), Some(quic)) =
            (self.sources.get_mut(token.0), self.quic.as_mut())
        else {
            return;
        };
        let now = Instant::now();
        let from = quic::carrier_address(CarrierId {
            key: token.0,
            generation: carrier.generation,
        });
        loop {
            match carrier.packets.next_payload() {
                Ok(Some(packet)) => quic.receive(now, from, packet),
                Ok(None) => break,
                Err(err) => {
                    log::warn!("closing a carrier that sent a bad frame: {err}");
                    closed = true;
                    break;
                }
            }
        }
        if closed {
            self.sources.remove(token.0);
            quic.carrier_closed(from, now);
            log::info!("a carrier closed");
        }
    }

    /// Writes what it can of a carrier's waiting packets, and closes it if
    /// that fails.
    fn flush_carrier(&mut self, token: Token) {
        let Some(Source::Carrier(carrier)) = self.sources.get_mut(token.0) else {
            return;
        };
        if let Err(err) = write_out(
            self.poll.registry(),
            &mut carrier.stream,
            token,
            &mut carrier.outgoing,
            &mut carrier.wants_writable,
        ) {
            log::debug!("carrier write failed: {err}");
            self.receive_packets(token, true);
        }
    }

    /// Lets the QUIC endpoint act on its due timers and the packets it was
    /// handed, and acts on what it asks.
    fn drive_quic(&mut self) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        quic.handle_timeouts(Instant::now());
        let mut events = std::mem::take(&mut self.quic_events);
        loop {
            if let Some(quic) = self.quic.as_mut() {
                quic.events(&mut events);
            }
            if events.is_empty() {
                break;
            }
            for event in events.drain(..) {
                match event {
                    QuicEvent::Opened(handle) => self.new_quic_client(handle),
                    QuicEvent::Readable(token) => self.serve_client(token),
                    QuicEvent::Writable(token) => self.flush_client(token),
                    QuicEvent::Closed(token) => self.drop_client(token),
                }
            }
        }
        self.quic_events = events;
    }

    /// Serves the connection with `handle`, which opened its stream, as a
    /// client.
    fn new_quic_client(&mut self, handle: ConnectionHandle) {
        let entry = self.sources.vacant_entry();
        let token = Token(entry.key());
        entry.insert(Source::Client(Client::new(Link::Quic(handle))));
        if let Some(quic) = self.quic.as_mut() {
            quic.set_client(handle, token);
        }
        // Its greeting may have come with the stream.
        self.serve_client(token);
    }

    /// Sends the packets the QUIC endpoint has ready, each on its carrier.
    fn transmit_quic(&mut self) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        let mut carriers = Vec::new();
        quic.transmit(Instant::now(), |address, packet| {
            match quic::carrier_id(address) {
                Some(CarrierId { key, generation }) => {
                    if let Some(Source::Carrier(carrier)) = self.sources.get_mut(key)
                        && carrier.generation == generation
                    {
                        quic::frame_packet(packet, &mut carrier.outgoing);
                        if !carriers.contains(&key) {
                            carriers.push(key);
                        }
                    }
                }
                // A packet the socket has no room for is as good as lost, and
                // QUIC sends it again.
                None => {
                    if let Some(udp) = &self.udp
                        && let Err(err) = udp.send_to(packet, address)
                        && err.kind() != io::ErrorKind::WouldBlock
                    {
                        log::debug!("UDP send to {address} failed: {err}");
                    }
                }
            }
        });
        for key in carriers {
            self.flush_carrier(Token(key));
        }
    }
}

/// Binds a UDP socket on the first free port in `ports`, taking IPv6 and
/// IPv4 alike where the system allows, and registers it.
fn bind_udp(registry: &Registry, ports: RangeInclusive<u16>) -> io::Result<UdpSocket> {
    let mut last = io::Error::other("the range of ports is empty");
    for port in ports {
        let any = [
            IpAddr::from(Ipv6Addr::UNSPECIFIED),
            Ipv4Addr::UNSPECIFIED.into(),
        ];
        for ip in any {
            match UdpSocket::bind(SocketAddr::new(ip, port)) {
                Ok(mut udp) => {
                    registry.register(&mut udp, UDP, Interest::READABLE)?;
                    log::info!("QUIC takes UDP on port {}", udp.local_addr()?.port());
                    return Ok(udp);
                }
                Err(err) => last = err,
            }
        }
    }
    Err(last)
}

/// Writes what it can of `outgoing` to `stream`, registered with `token`,
/// and asks to hear of writability exactly while some is left. Returns how
/// much was written.
fn write_out(
    registry: &Registry,
    stream: &mut UnixStream,
    token: Token,
    outgoing: &mut Vec<u8>,
    wants_writable: &mut bool,
) -> io::Result<usize> {
    let mut written = 0;
    while written < outgoing.len() {
        match stream.write(&outgoing[written..]) {
            Ok(n) => written += n,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    outgoing.drain(..written);
    let wanted = !outgoing.is_empty();
    if wanted != *wants_writable {
        let interest = if wanted {
            Interest::READABLE | Interest::WRITABLE
        } else {
            Interest::READABLE
        };
        registry.reregister(stream, token, interest)?;
        *wants_writable = wanted;
    }
    Ok(written)
}

/// Up to `count` of `terminal`'s history lines, ending before line `end`.
fn history(terminal: &Terminal, end: u64, count: u16) -> HistoryLines {
    let history = terminal.primary();
    let start = end.saturating_sub(u64::from(count));
    let mut first = None;
    let lines = history
        .scrollback_lines(start..end)
        .map(|(number, row)| {
            first.get_or_insert(number);
            HistoryLine {
                width: u16::try_from(row.width()).expect("rows are at most Size::MAX wide"),
                line: Line::from_row(&row),
            }
        })
        .collect();
    HistoryLines {
        first: first.unwrap_or(end),
        lines,
    }
}
