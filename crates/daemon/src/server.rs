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
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::{
    ClientMessage, Error, GridState, HistoryLine, HistoryLines, Line, NodeId, PROTOCOL_VERSION,
    PaneId, PaneTerminal, Patch, Request, Response, ServerMessage, Side, Size, TerminalId,
    TerminalStatus, View, ViewUpdate, WorkspaceId,
};
use clackatoo_term::{Damage, Terminal};
use mio::event::Event;
use mio::net::{UnixListener, UnixStream};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token, Waker};
use slab::Slab;

use crate::DaemonConfig;
use crate::hosted::{HostedTerminal, Orphan, ReadOutcome};
use crate::memory;
use crate::workspaces::WorkspaceState;

const LISTENER: Token = Token(usize::MAX);
const WAKER: Token = Token(usize::MAX - 1);

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
    stream: UnixStream,
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
            workspaces: WorkspaceState::new(),
            unread: VecDeque::new(),
            compactions: BTreeMap::new(),
            views_pending: false,
            damage: Damage::new(),
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
            // right after anything else that's ready. Otherwise wait for the
            // next compaction, if there is one.
            let timeout = if self.unread.is_empty() {
                let next = self.compactions.values().min();
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
            for id in std::mem::take(&mut self.unread) {
                self.read_output(id);
            }
            self.compact_scrollbacks();
            self.send_view_updates();
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
    /// written everything queued before. Holding back for slow clients means
    /// changes pile up into one update instead of a backlog.
    fn send_view_updates(&mut self) {
        if !std::mem::take(&mut self.views_pending) {
            return;
        }
        let mut ready = Vec::new();
        for (key, source) in &mut self.sources {
            let Source::Client(client) = source else {
                continue;
            };
            if !client.outgoing.is_empty() || client.finished {
                continue;
            }
            for (&id, subscription) in &mut client.views {
                if subscription.awaiting_ack {
                    continue;
                }
                let Some(slot) = self.terminals.get(&id) else {
                    continue;
                };
                if let Some(update) = subscription.update(slot.terminal.emulator()) {
                    subscription.awaiting_ack = true;
                    let event = ServerMessage::Event(clackatoo_protocol::Event::View {
                        terminal: id,
                        update,
                    });
                    match frame::encode(&event) {
                        Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
                        Err(err) => log::error!("encoding a view update failed: {err}"),
                    }
                }
            }
            if !client.outgoing.is_empty() {
                ready.push(Token(key));
            }
        }
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
                        viewers: Vec::new(),
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
        self.compactions.remove(&id);
        for token in &slot.viewers {
            if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                client.views.remove(&id);
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
                    entry.insert(Source::Client(Client {
                        stream,
                        decoder: FrameDecoder::new(),
                        outgoing: Vec::new(),
                        greeted: false,
                        finished: false,
                        wants_writable: false,
                        views: BTreeMap::new(),
                        watching_workspaces: false,
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
            if client.finished {
                break;
            }
        }
        self.flush_client(token);
    }

    /// Reads and decodes everything a client has sent. Returns `None` if the
    /// client was dropped.
    fn receive_messages(&mut self, token: Token) -> Option<Vec<ClientMessage>> {
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
        // View updates wait for a client to catch up.
        if flushed && written > 0 && !client.views.is_empty() {
            self.views_pending = true;
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
        if let Some(Source::Client(client)) = self.sources.try_remove(token.0) {
            for id in client.views.keys() {
                if let Some(slot) = self.terminals.get_mut(id) {
                    slot.viewers.retain(|&viewer| viewer != token);
                }
            }
        }
    }
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
