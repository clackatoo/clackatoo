//! The daemon's event loop.
//!
//! One thread waits on every file descriptor the daemon cares about: the
//! listening socket, each client connection, each PTY, each program's
//! pidfd, and the pipes to the agent that chats talk to. The terminals' grids stay on that thread, so nothing is locked or
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
    BufferId, ChatAction, ClientMessage, Content, Error, GridState, HistoryLine, HistoryLines,
    Line, LinkTarget, NodeId, PROTOCOL_VERSION, Pane, PaneId, PaneTerminal, Patch, Request,
    Response, ServerMessage, Side, Size, TabId, TerminalId, TerminalInfo, TerminalStatus, Udp,
    View, ViewUpdate, WorkspaceId,
};
use clackatoo_syntax::{Languages, Syntax};
use clackatoo_term::{Damage, Terminal};
use mio::event::Event;
use mio::net::{UdpSocket, UnixListener, UnixStream};
use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Registry, Token, Waker};
use quinn_proto::{ConnectionHandle, StreamId};
use slab::Slab;

use crate::DaemonConfig;
use crate::buffer::Buffer;
use crate::chat::{ChatSent, HostedChat};
use crate::handover::{self, Fds, Handover, HandoverError};
use crate::hosted::{HostedTerminal, Orphan, ReadOutcome};
use crate::memory;
use crate::quic::{self, CarrierId, QuicEvent, QuicServer, Received};
use crate::upgrade::Upgrade;
use crate::workspaces::WorkspaceState;

mod agent_files;
mod boards;
mod buffers;
mod chats;
mod clipboard;
mod disk;
mod edit_log;
mod explorers;
mod finder;
mod language;
mod offers;
mod pieces;
mod rename;
mod review;
mod signs;
mod tools;
mod turn_undo;

use buffers::{BufferSlot, WindowChange};
use chats::AgentId;
use explorers::Explorers;
use finder::Finds;
use language::{Pipe, ServerId, Servers};
use offers::Installs;
use signs::Signs;
use tools::Tools;

const LISTENER: Token = Token(usize::MAX);
const WAKER: Token = Token(usize::MAX - 1);
const UDP: Token = Token(usize::MAX - 2);

/// Bytes read per `read` call, from PTYs and clients alike.
const READ_BUFFER_LEN: usize = 64 * 1024;

/// The most output read from one PTY before giving the other terminals and
/// clients a turn. Keeps a flood of output (`cat` of a huge file) from
/// freezing everything else.
const OUTPUT_BUDGET: usize = 1024 * 1024;

/// The most a client on another machine may have waiting to be sent, past
/// what its streams have taken, before it's dropped.
const MOST_UNSENT: usize = 8 * 1024 * 1024;

/// How long a terminal's output must pause before its scrollback is
/// compacted: long enough that a flood never stops for it, short enough
/// that the memory comes back soon after.
const COMPACT_AFTER: Duration = Duration::from_millis(100);

/// The longest an upgrade waits for the boards' last changes to be
/// written, before it's refused.
const UPGRADE_BOARDS_WAIT: Duration = Duration::from_secs(10);

/// What a token refers to.
#[derive(Debug)]
enum Source {
    /// A terminal's PTY.
    Output(TerminalId),
    /// A terminal's pidfd.
    Exit(TerminalId),
    /// The pidfd of a closed terminal's program that hasn't exited yet.
    Orphan(Orphan),
    /// One of an agent's pipes.
    Agent(AgentId, Pipe),
    /// One of a language server's pipes.
    Server(ServerId, Pipe),
    /// The inotify instance watching repositories' indexes.
    Index,
    /// The inotify instance watching the directories explorers show, and
    /// those buffers' files are in.
    Files,
    Client(Client),
    Carrier(Carrier),
    /// A connection an agent's tools are served on.
    Tools(crate::mcp::Connection),
}

/// What a terminal is: a program on a PTY, or a chat with an agent, which
/// the daemon writes itself.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "both are over a kilobyte, a few per daemon; boxing either puts a pointer \
              between the event loop and every byte of output"
)]
enum Hosted {
    Pty(HostedTerminal),
    Chat(HostedChat),
}

impl Hosted {
    fn info(&self) -> TerminalInfo {
        match self {
            Self::Pty(terminal) => terminal.info(),
            Self::Chat(chat) => chat.info(),
        }
    }

    /// The terminal's emulator: its screen and history. A chat has none.
    fn grid(&self) -> Option<&Terminal> {
        match self {
            Self::Pty(terminal) => Some(terminal.emulator()),
            Self::Chat(_) => None,
        }
    }

    fn view(&self) -> View {
        match self {
            Self::Pty(terminal) => terminal.view(),
            Self::Chat(chat) => chat.view(),
        }
    }

    fn take_damage(&mut self, into: &mut Damage) {
        match self {
            Self::Pty(terminal) => terminal.take_damage(into),
            // A chat's viewers catch up from its journal.
            Self::Chat(_) => {}
        }
    }

    fn resize(&mut self, size: Size) {
        match self {
            Self::Pty(terminal) => terminal.resize(size),
            Self::Chat(chat) => chat.resize(size),
        }
    }

    fn scrollback_needs_compaction(&mut self) -> bool {
        match self {
            Self::Pty(terminal) => terminal.scrollback_needs_compaction(),
            Self::Chat(_) => false,
        }
    }

    fn compact_scrollback(&mut self) {
        match self {
            Self::Pty(terminal) => terminal.compact_scrollback(),
            Self::Chat(_) => {}
        }
    }

    /// The directory a new pane beside this one starts in.
    fn working_directory(&self) -> Option<String> {
        match self {
            Self::Pty(terminal) => terminal.working_directory(),
            Self::Chat(chat) => Some(chat.working_directory().to_owned()),
        }
    }

    fn pty(&mut self) -> Option<&mut HostedTerminal> {
        match self {
            Self::Pty(terminal) => Some(terminal),
            Self::Chat(_) => None,
        }
    }

    fn chat(&mut self) -> Option<&mut HostedChat> {
        match self {
            Self::Chat(chat) => Some(chat),
            Self::Pty(_) => None,
        }
    }

    fn is_chat(&self) -> bool {
        matches!(self, Self::Chat(_))
    }

    /// The chat it is, if it's one, to read.
    fn chat_ref(&self) -> Option<&HostedChat> {
        match self {
            Self::Chat(chat) => Some(chat),
            Self::Pty(_) => None,
        }
    }
}

/// A terminal plus its event-loop registrations.
#[derive(Debug)]
struct Slot {
    terminal: Hosted,
    /// Unset once the PTY closes, and for a chat, which has none.
    output: Option<Token>,
    /// Unset once the program has been reaped, and for a chat.
    exit: Option<Token>,
    /// Whether the PTY is registered for writability, which it only is while
    /// input is queued.
    wants_writable: bool,
    /// The clients following this terminal's view.
    viewers: Vec<Token>,
    /// The client that last typed into the terminal, which acts for it on
    /// the clipboard.
    typist: Option<Token>,
    /// What the program asked about the clipboard, while the client
    /// asked hasn't answered.
    clipboard: Option<clipboard::Asked>,
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
    /// The connection is to serve this chat's agent's tools, once its
    /// answer is queued.
    serving_tools: Option<TerminalId>,
    /// Over QUIC, the stream each terminal's view updates go on, so one
    /// terminal's never wait behind another's, or behind answers. Kept
    /// while the terminal lives, even when the client stops following it:
    /// a new stream could deliver a fresh snapshot ahead of the old
    /// stream's last patch.
    streams: BTreeMap<TerminalId, ViewStream>,
    /// What programs put on the clipboard for the client, waiting for it
    /// to catch up.
    clipboard: clipboard::Unsent,
    /// The window the client is, as it said when attaching to a buffer:
    /// see [`Request::AttachBuffer`].
    window: Option<u64>,
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
            serving_tools: None,
            streams: BTreeMap::new(),
            clipboard: clipboard::Unsent::default(),
            window: None,
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
    /// The terminal's history generation when the last snapshot was sent.
    /// A new one renumbers history, which only a snapshot tells a client.
    history_generation: u64,
    title: String,
    /// The working directory last sent.
    cwd: Option<String>,
    state: GridState,
    /// For a chat: what was last sent of it.
    chat: ChatSent,
    /// An update was sent and not yet acknowledged with `FrameDone`.
    awaiting_ack: bool,
}

impl Subscription {
    fn new() -> Self {
        Self {
            damage: Damage::new(),
            size: None,
            history_generation: 0,
            title: String::new(),
            cwd: None,
            state: GridState::default(),
            chat: ChatSent::default(),
            awaiting_ack: false,
        }
    }

    /// The update that brings the client from what it was last sent to what
    /// `hosted` shows now, recorded as sent. `None` if nothing changed.
    fn update_from(&mut self, hosted: &Hosted) -> Option<ViewUpdate> {
        match hosted {
            Hosted::Pty(terminal) => self.update(terminal.emulator()),
            Hosted::Chat(chat) => chat.catch_up(&mut self.chat),
        }
    }

    /// The update that brings the client from what it was last sent to what
    /// `terminal` shows now, recorded as sent. `None` if nothing changed.
    fn update(&mut self, terminal: &Terminal) -> Option<ViewUpdate> {
        let size = terminal.size();
        let state = GridState::of(terminal);
        let history_generation = terminal.primary().history_generation();
        if self.size != Some(size) || self.history_generation != history_generation {
            self.size = Some(size);
            self.history_generation = history_generation;
            terminal.title().clone_into(&mut self.title);
            self.cwd = terminal.cwd().map(str::to_owned);
            self.state = state;
            self.damage.clear();
            return Some(ViewUpdate::Snapshot(View::capture(terminal)));
        }

        let mut patches = Vec::new();
        if terminal.title() != self.title {
            terminal.title().clone_into(&mut self.title);
            patches.push(Patch::Title(self.title.clone()));
        }
        if terminal.cwd() != self.cwd.as_deref() {
            self.cwd = terminal.cwd().map(str::to_owned);
            patches.push(Patch::Cwd(self.cwd.clone()));
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
            let shown = LinkTarget::of(lines.iter().map(|(_, line)| line), terminal);
            patches.push(Patch::Grid {
                node: NodeId::ROOT,
                state,
                lines,
                links: shown,
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
    /// Files being edited.
    buffers: BTreeMap<BufferId, BufferSlot>,
    next_buffer: u32,
    /// The daemon's own number, chosen at random when it first started and
    /// kept through upgrades: see [`BufferSnapshot::daemon`](clackatoo_protocol::BufferSnapshot).
    id: u64,
    /// The language packs buffers are parsed with, if they're looked for.
    languages: Option<Languages>,
    /// The thread parsing buffers, once one is in a language there's a
    /// pack for.
    syntax: Option<Syntax>,
    /// The language servers buffers are open in.
    servers: Servers,
    /// Languages offered to clients to install, and being installed.
    installs: Installs,
    /// Renames whose files are being written.
    renames: rename::Renames,
    /// The finder's searches going on.
    finds: Finds,
    /// What agents' tools are waiting for, and the panes and terminals
    /// they opened.
    tools: Tools,
    /// File explorers, and the directories expanded in them.
    explorers: Explorers,
    /// The boards agents and the user coordinate on.
    boards: boards::BoardState,
    /// Buffers being compared with the files' text in their repositories,
    /// and the indexes watched.
    signs: Signs,
    /// Questions about buffers' pieces of code, on their way to the
    /// thread parsing buffers.
    piece_questions: pieces::PieceQuestions,
    /// How many questions about the clipboard programs have asked, which
    /// numbers them.
    clipboard_asks: u64,
    /// When each question about the clipboard runs out of time: the
    /// terminal, and the question's number.
    clipboard_due: Vec<(Instant, TerminalId, u64)>,
    /// A client may have writes to the clipboard waiting that it can be
    /// sent.
    clipboard_unsent: bool,
    /// The agents chats talk to, while any chat needs them, and those
    /// being installed.
    agents: chats::Agents,
    /// Files as agents read them, and what agents changed in them.
    agent_files: agent_files::AgentFiles,
    /// Buffers' files changed on disk, waiting to be looked at.
    disk: disk::Disk,
    /// What agents changed and the user hasn't reviewed, and reviews.
    reviews: review::Reviews,
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
    /// The arguments an upgrade runs the next build with. Without them,
    /// the daemon can't upgrade.
    upgrade_args: Vec<std::ffi::OsString>,
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
        let finds = {
            let waker = Arc::clone(&waker);
            Finds::new(&config.home, move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for what was found failed: {err}");
                }
            })
        };
        let boards = boards::BoardState::start(config.boards.clone(), &waker);
        Ok(Self {
            poll,
            listener,
            waker,
            shutdown: Arc::new(AtomicBool::new(false)),
            sources: Slab::new(),
            terminals: BTreeMap::new(),
            next_id: 1,
            workspaces: WorkspaceState::new(),
            buffers: BTreeMap::new(),
            next_buffer: 1,
            id: crate::client::random_window(),
            languages: config.languages.clone().map(Languages::new),
            syntax: None,
            servers: Servers::default(),
            installs: Installs::new(config.declined.as_deref()),
            renames: rename::Renames::default(),
            finds,
            tools: Tools::default(),
            explorers: Explorers::default(),
            boards,
            signs: Signs::default(),
            piece_questions: pieces::PieceQuestions::default(),
            clipboard_asks: 0,
            clipboard_due: Vec::new(),
            clipboard_unsent: false,
            agents: chats::Agents::default(),
            agent_files: agent_files::AgentFiles::default(),
            disk: disk::Disk::default(),
            reviews: review::Reviews::default(),
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
            upgrade_args: Vec::new(),
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
        let chats = std::mem::take(&mut handover.state.chats);
        let buffers = std::mem::take(&mut handover.state.buffers);
        let explorers = std::mem::take(&mut handover.state.explorers);
        let edits = std::mem::take(&mut handover.state.edits);
        let reviews = std::mem::take(&mut handover.state.reviews);
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
        for chat in chats {
            let agent = daemon.agent_spec(&chat.agent);
            daemon.add_chat(HostedChat::resume(chat, &agent));
        }
        daemon.next_buffer = state.next_buffer;
        // Older builds had none, and keep the one just chosen.
        if state.daemon != 0 {
            daemon.id = state.daemon;
        }
        for buffer in buffers {
            let id = BufferId(buffer.id);
            let mut slot = BufferSlot::new(Buffer::resume(buffer));
            slot.applied = (state.applied.iter())
                .filter(|applied| applied.buffer == id.0)
                .map(|applied| {
                    let change = WindowChange {
                        seq: applied.seq,
                        landed: applied.landed,
                    };
                    (applied.window, change)
                })
                .collect();
            daemon.buffers.insert(id, slot);
            daemon.parse_buffer(id);
            // Its file may have changed while no one was watching.
            daemon.watch_buffer(id);
            daemon.disk_maybe_changed(id);
        }
        daemon.resume_files(state.next_files, explorers);
        // Messages waiting in chats' inboxes go as their turns end.
        daemon.resume_talk();
        daemon.agent_files.log = edit_log::EditLog::resume(edits);
        daemon.resume_reviews(reviews);
        daemon.learn_from_chats();
        daemon.refresh_all_files();
        for orphan in &state.orphans {
            match Orphan::resume(orphan, fds) {
                Ok(orphan) => daemon.watch_orphan(orphan),
                Err(err) => log::error!("a hung-up program couldn't be taken over: {err}"),
            }
        }
        // The handover's copy of every terminal, and the rows restoring
        // their history built, are gone.
        drop(handover);
        memory::release_free_memory();
        let terminals = daemon.terminals.len();
        log::info!(
            "carrying on with {terminals} terminal{}",
            if terminals == 1 { "" } else { "s" }
        );
        Ok(daemon)
    }

    /// Lets the daemon upgrade, running the next build with `args`.
    pub(crate) fn set_upgrade_args(&mut self, args: &[std::ffi::OsString]) {
        self.upgrade_args = args.to_vec();
    }

    /// Writes the shell integration scripts into `dir`, for the shells
    /// this daemon starts from now on. If that fails, they start as they
    /// are.
    pub(crate) fn install_shell_integration(&mut self, dir: PathBuf) {
        self.config.shell_integration = crate::shell::install(dir)
            .map_err(|err| log::warn!("shells won't mark their commands: {err}"))
            .ok();
    }

    /// Everything the next build needs, taken out of this daemon, which is
    /// left with nothing to hang up when it's dropped. The listening socket
    /// is shared: closing this daemon's copy leaves it open.
    fn hand_over(&mut self) -> io::Result<Handover> {
        let mut fds = Fds::default();
        let listener = fds.add(self.listener.as_fd().try_clone_to_owned()?);
        let mut terminals = Vec::new();
        let mut chats = Vec::new();
        self.abandon_clipboard_asks();
        for slot in std::mem::take(&mut self.terminals).into_values() {
            match slot.terminal {
                Hosted::Pty(terminal) => terminals.push(terminal.hand_over(&mut fds)),
                Hosted::Chat(chat) => chats.push(chat.hand_over()),
            }
        }
        // Renames under way are dropped, rather than leave their new files
        // beside the old.
        self.abandon_renames();
        // The agents and language servers stop, and the next build reaps
        // them. The boards' last changes are written, for it to read.
        self.stop_agents();
        self.stop_servers();
        self.stop_boards();
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
        let (next_files, explorers) = self.hand_over_files();
        Ok(Handover {
            state: handover::State {
                listener,
                lock: None,
                next_terminal: self.next_id,
                terminals,
                chats,
                orphans,
                workspaces: self.workspaces.hand_over(),
                next_buffer: self.next_buffer,
                buffers: (self.buffers.iter())
                    .map(|(id, slot)| slot.buffer.hand_over(id.0))
                    .collect(),
                next_files,
                explorers,
                edits: self.agent_files.log.hand_over(),
                applied: (self.buffers.iter())
                    .flat_map(|(id, slot)| {
                        (slot.applied.iter()).map(|(&window, change)| handover::Applied {
                            buffer: id.0,
                            window,
                            seq: change.seq,
                            landed: change.landed,
                        })
                    })
                    .collect(),
                daemon: self.id,
                reviews: self.hand_over_reviews(),
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
                // Every change to the boards is on disk, and their lock let
                // go of, before there's no way back.
                if let Err(reason) = self.prepare_boards_for_upgrade(UPGRADE_BOARDS_WAIT) {
                    log::warn!("not upgrading: {reason}");
                    self.refuse_upgrade(token, reason);
                    continue;
                }
                // The client that asked hears how it went: its connection
                // closes as the new build starts, or says why it didn't.
                self.close_remotes("the daemon is upgrading to a new build");
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
            let agent_unread = self.agents.unread();
            let servers_unread = self.servers_unread();
            let tools_unread = self.tools_unread();
            let timeout =
                if self.unread.is_empty() && !agent_unread && !servers_unread && !tools_unread {
                    let compaction = self.compactions.values().min().copied();
                    let quic = self.quic.as_mut().and_then(QuicServer::next_timeout);
                    let idle = self.next_idle_stop();
                    let retry = self.next_retry();
                    let clipboard = self.next_clipboard_timeout();
                    let explore = self.next_explore();
                    let tools = self.next_tool_due();
                    let rewatch = self.next_rewatch();
                    let reviews = self.next_review_due();
                    let next = (compaction.into_iter().chain(quic).chain(idle))
                        .chain(retry)
                        .chain(clipboard)
                        .chain(explore)
                        .chain(tools)
                        .chain(rewatch)
                        .chain(reviews)
                        .chain(self.next_agent_due())
                        .min();
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
            if agent_unread {
                self.read_agents_unread();
            }
            if servers_unread {
                self.read_servers_unread();
            }
            if tools_unread {
                self.read_tools_unread();
            }
            self.take_highlights();
            self.take_signs();
            self.take_installs();
            self.agent_chores();
            self.take_renames();
            if self.take_boards() {
                self.boards_read();
            }
            self.ask_again();
            self.take_found();
            self.explore();
            // After what the daemon's own threads wrote has been taken, so
            // it isn't taken for a change from outside.
            self.check_disk();
            self.stop_idle_servers();
            self.compact_scrollbacks();
            self.time_out_clipboard_asks();
            self.time_out_tools();
            self.relay_chats();
            self.send_clipboard_writes();
            self.send_reviews();
            self.send_view_updates();
            self.transmit_quic();
        }
        log::info!("shutting down");
        self.close_remotes("the daemon stopped");
        self.abandon_renames();
        self.stop_boards();
        Ok(None)
    }

    /// Tells the client at `token`, which asked the daemon to upgrade,
    /// that it won't, and why.
    fn refuse_upgrade(&mut self, token: Token, reason: String) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        match frame::encode(&ServerMessage::NotUpgrading { reason }) {
            Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
            Err(err) => log::error!("encoding a refusal to upgrade failed: {err}"),
        }
        self.flush_client(token);
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
                Some(&Source::Agent(id, pipe)) => self.agent_event(id, pipe),
                Some(&Source::Server(id, pipe)) => match pipe {
                    Pipe::Output => self.read_server(id),
                    Pipe::Errors => self.read_server_errors(id),
                    Pipe::Input => self.flush_server(id),
                    Pipe::Exit => {
                        self.read_server(id);
                        self.server_gone(id);
                    }
                },
                Some(Source::Index) => self.read_index_changes(),
                Some(Source::Files) => self.read_files_changes(),
                Some(Source::Client(_)) => self.serve_client(token),
                Some(Source::Carrier(_)) => self.serve_carrier(token, event),
                Some(Source::Tools(_)) => self.serve_tools(token, event),
                None => {}
            },
        }
    }

    // --- Terminals --------------------------------------------------------

    fn read_output(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        let Some(terminal) = slot.terminal.pty() else {
            return;
        };
        match terminal.read_output(&mut self.buffer, OUTPUT_BUDGET) {
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
        self.serve_clipboard(id);
        self.schedule_compaction(id);
        self.collect_damage(id);
        self.terminal_moved_on(id);
    }

    /// Hands the lines the terminal changed to each client following it.
    fn collect_damage(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        // A chat an agent started shows in that chat's card.
        let started =
            (slot.terminal.chat_ref()).is_some_and(|chat| chat.relations().parent.is_some());
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
        if started {
            self.note_subagent(id);
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
                if let Some(update) = subscription.update_from(&slot.terminal) {
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
    /// paused, if it has rows to compact. Memory freed by history being
    /// cleared is handed back then too.
    fn schedule_compaction(&mut self, id: TerminalId) {
        if self
            .terminals
            .get_mut(&id)
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
        if let Some(terminal) = (self.terminals.get_mut(&id)).and_then(|slot| slot.terminal.pty()) {
            terminal.flush_input();
            self.update_pty_interest(id);
        }
    }

    /// Registers the PTY for writability exactly while input is queued.
    fn update_pty_interest(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        let Some(terminal) = slot.terminal.pty() else {
            return;
        };
        let (Some(token), Some(fd)) = (slot.output, terminal.pty_fd()) else {
            return;
        };
        let wants_writable = terminal.has_pending_input();
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
        let Some(terminal) = slot.terminal.pty() else {
            return;
        };
        if terminal.reap() {
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
                        terminal: Hosted::Pty(terminal),
                        output,
                        exit,
                        wants_writable: false,
                        viewers: Vec::new(),
                        typist: None,
                        clipboard: None,
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
        self.tools_closed(id);
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
        let terminal = match slot.terminal {
            Hosted::Pty(terminal) => terminal,
            Hosted::Chat(chat) => {
                self.close_chat(id, &chat);
                self.agent_files.forget(id);
                self.chat_closed_in_review(id);
                if self.workspaces.remove_terminal(id) {
                    self.workspaces_changed();
                }
                log::info!("chat {id} closed");
                self.subagent_closed(id, chat.relations().parent);
                return Ok(());
            }
        };
        if let Some(orphan) = terminal.close() {
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

    /// What a new pane shows: a new terminal, starting by default in the
    /// directory `beside`'s is in, an existing one in no pane, a chat, or
    /// a file.
    fn pane_content(
        &mut self,
        content: PaneTerminal,
        beside: Option<PaneId>,
    ) -> Result<Content, Error> {
        let terminal = match content {
            PaneTerminal::Existing(id) => {
                let slot = self.terminals.get(&id).ok_or(Error::NoSuchTerminal(id))?;
                if slot.terminal.info().status != TerminalStatus::Running {
                    return Err(Error::TerminalExited(id));
                }
                if self.workspaces.get().pane_showing(id).is_some() {
                    return Err(Error::TerminalInPane(id));
                }
                id
            }
            PaneTerminal::New(mut request) => {
                if request.cwd.is_none() {
                    request.cwd = beside.and_then(|pane| self.working_directory(pane));
                }
                self.new_terminal(&request)?
            }
            PaneTerminal::Agent(request) => self.new_chat(request, beside),
            PaneTerminal::File(file) => {
                return self.open_buffer(file, beside).map(Content::Buffer);
            }
            PaneTerminal::Files(open) => {
                return self.new_files(open, beside).map(Content::Files);
            }
            PaneTerminal::Board(open) => {
                return self.open_board(open, beside).map(Content::Board);
            }
            PaneTerminal::Review(new) => {
                return self.new_review(new, beside).map(Content::Review);
            }
        };
        Ok(Content::Terminal(terminal))
    }

    /// The directory `pane` works in: for a terminal, the one the program
    /// in front is in; for a file, the one it was opened from.
    fn working_directory(&self, pane: PaneId) -> Option<String> {
        match self.workspaces.check_pane(pane).ok()? {
            Content::Terminal(terminal) => {
                self.terminals.get(&terminal)?.terminal.working_directory()
            }
            Content::Buffer(buffer) => {
                let dir = self.buffers.get(&buffer)?.buffer.dir();
                Some(dir.display().to_string())
            }
            Content::Files(files) => Some(self.files_root(files)?.display().to_string()),
            Content::Board(_) => None,
            Content::Review(review) => Some(self.review_root_of(review)?.display().to_string()),
        }
    }

    /// Closes `panes`, as closing their tab or workspace does: their
    /// terminals, and their buffers that have no changes to save.
    fn close_panes(&mut self, panes: &[Pane]) -> Result<Response, Error> {
        for pane in panes {
            match pane.content {
                Content::Terminal(terminal) => self.close_terminal(terminal)?,
                Content::Buffer(buffer) => self.close_buffer_pane(pane.id, buffer),
                Content::Files(files) => self.close_files_pane(pane.id, files),
                Content::Board(_) => self.close_board_pane(pane.id),
                Content::Review(review) => self.close_review_pane(pane.id, review),
            }
        }
        Ok(Response::Done)
    }

    fn new_workspace(
        &mut self,
        name: Option<&str>,
        terminal: PaneTerminal,
    ) -> Result<Response, Error> {
        let name = self.workspaces.name_for_new(name)?;
        let content = self.pane_content(terminal, None)?;
        let pane = self.workspaces.new_workspace(name, content);
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
        let content = self.pane_content(terminal, beside)?;
        let pane = self.workspaces.new_tab(workspace, content);
        self.workspaces_changed();
        Ok(Response::PaneCreated(pane))
    }

    fn split_tab(
        &mut self,
        tab: TabId,
        side: Side,
        terminal: PaneTerminal,
        share: Option<u16>,
    ) -> Result<Response, Error> {
        self.workspaces.check_tab(tab)?;
        let beside = (self.workspaces.get().find_tab(tab)).map(|(_, tab)| tab.focused);
        let content = self.pane_content(terminal, beside)?;
        let new = self.workspaces.split_tab(tab, side, content, share);
        self.workspaces_changed();
        Ok(Response::PaneCreated(new))
    }

    fn split(
        &mut self,
        pane: PaneId,
        side: Side,
        terminal: PaneTerminal,
    ) -> Result<Response, Error> {
        self.workspaces.check_pane(pane)?;
        let content = self.pane_content(terminal, Some(pane))?;
        let new = self.workspaces.split(pane, side, content);
        self.workspaces_changed();
        Ok(Response::PaneCreated(new))
    }

    /// Tells every client watching the workspaces what they are now. The
    /// client whose request changed them hears before the request's answer,
    /// which is queued after this.
    fn workspaces_changed(&mut self) {
        // A chat agents started may have come out of the background, or
        // gone back.
        self.note_subagents();
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
                message @ (ClientMessage::Edit { .. }
                | ClientMessage::Query { .. }
                | ClientMessage::Rename { .. }
                | ClientMessage::Find { .. }
                | ClientMessage::StopFinding { .. }
                | ClientMessage::Stage { .. }
                | ClientMessage::Clipboard { .. }
                | ClientMessage::Files { .. }
                | ClientMessage::FileOp { .. }
                | ClientMessage::Board { .. }) => {
                    self.serve_unanswered(token, message);
                    continue;
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
            if client.finished || client.carrying || client.serving_tools.is_some() {
                break;
            }
        }
        match self.sources.get(token.0) {
            Some(Source::Client(client)) if client.carrying => self.become_carrier(token),
            Some(Source::Client(client)) if client.serving_tools.is_some() => {
                self.become_tools(token);
            }
            _ => self.flush_client(token),
        }
    }

    /// Acts on a message from the client at `token` that isn't answered as
    /// a request is: a change to a buffer, a question or a rename for its
    /// language server, or a search, all answered in events of their own,
    /// or what's on its clipboard, for a program that asked.
    fn serve_unanswered(&mut self, token: Token, message: ClientMessage) {
        match message {
            ClientMessage::Edit {
                buffer,
                seq,
                change,
            } => self.serve_edit(token, buffer, seq, change),
            ClientMessage::Query {
                buffer,
                id,
                version,
                at,
                query,
            } if query.for_grammar() => self.serve_pieces(token, buffer, (id, version, at), query),
            message @ (ClientMessage::Query { .. } | ClientMessage::Rename { .. }) => {
                self.serve_query(token, message);
            }
            ClientMessage::Find { id, pane, search } => self.serve_find(token, id, pane, search),
            ClientMessage::StopFinding { id } => self.stop_finding(token, id),
            ClientMessage::Stage {
                buffer,
                id,
                version,
                at,
            } => self.serve_stage(token, buffer, (id, version, at)),
            ClientMessage::Clipboard { terminal, id, text } => {
                self.clipboard_answered(token, terminal, id, text.as_deref());
            }
            ClientMessage::Files { files, action } => self.serve_files(token, files, action),
            ClientMessage::FileOp { id, op } => self.serve_file_op(token, id, op),
            ClientMessage::Board { board, action } => self.serve_board(token, board, action),
            other => log::error!("{other:?} is answered"),
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
        if self.upgrade_args.is_empty() {
            return Err("this daemon wasn't started to be upgraded".to_owned());
        }
        crate::upgrade::check_takes_over(executable, &self.upgrade_args)
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

    /// What each terminal is, chats included.
    fn terminal_infos(&self) -> Vec<TerminalInfo> {
        (self.terminals.values())
            .map(|slot| slot.terminal.info())
            .collect()
    }

    /// Answers one request from the client at `token`.
    #[allow(clippy::too_many_lines, reason = "one arm per request, each short")]
    fn respond(&mut self, request: Request, token: Token) -> Response {
        let response = match request {
            Request::Version => Ok(Response::Text(env!("CARGO_PKG_VERSION").to_owned())),
            Request::ListTerminals => Ok(Response::Terminals(self.terminal_infos())),
            Request::NewTerminal(request) => {
                self.new_terminal(&request).map(Response::TerminalCreated)
            }
            Request::SendInput { terminal, bytes } => self
                .send_input(token, terminal, &bytes)
                .map(|()| Response::Done),
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
                .grid(terminal)
                .map(|grid| Response::History(history(grid, end, count))),
            Request::Text {
                terminal,
                from,
                to,
                whole_lines,
            } => self
                .grid(terminal)
                .map(|grid| Response::Text(grid.text(from, to, whole_lines))),
            Request::Search {
                terminal,
                query,
                from,
                backward,
                exact,
            } => self
                .grid(terminal)
                .map(|grid| Response::Found(grid.search(&query, from, backward, exact))),
            Request::Block {
                terminal,
                line,
                which,
            } => self
                .grid(terminal)
                .map(|grid| Response::Block(grid.block(line, which))),
            Request::Chat { terminal, action } => {
                self.chat_action(terminal, action).map(|()| Response::Done)
            }
            request @ (Request::WatchWorkspaces
            | Request::NewWorkspace { .. }
            | Request::NewTab { .. }
            | Request::Split { .. }
            | Request::ClosePane { .. }
            | Request::Focus { .. }
            | Request::RenameWorkspace { .. }
            | Request::RenameTab { .. }
            | Request::CloseTab { .. }
            | Request::CloseWorkspace { .. }
            | Request::ResizeSplit { .. }
            | Request::OpenHere { .. }
            | Request::SplitTab { .. }) => self.respond_about_workspaces(request, token),
            asked @ (Request::CarryQuic | Request::CarryMcp { .. }) => self.carry(token, &asked),
            Request::AttachBuffer {
                buffer,
                window,
                resume,
            } => {
                if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                    client.window = Some(window);
                }
                self.forget_lost_connections(token, buffer, window);
                self.attach_buffer(token, buffer, resume)
                    .map(|()| Response::Done)
            }
            Request::DetachBuffer { buffer } => {
                self.detach_buffer(token, buffer);
                Ok(Response::Done)
            }
            Request::Save { buffer, force } => self.save_buffer(buffer, force),
            Request::AttachFiles { files } => self.attach_files(token, files),
            Request::DetachFiles { files } => Ok(self.detach_files(token, files)),
            Request::AttachBoard { board } => self.attach_board(token, board),
            Request::DetachBoard { board } => Ok(self.detach_board(token, board)),
            Request::Discard { buffer } => self.discard_buffer(buffer),
            Request::Reload { buffer } => self.reload_buffer(buffer),
            Request::AttachReview { review } => self.attach_review(token, review),
            Request::DetachReview { review } => Ok(self.detach_review(token, review)),
            Request::Review(action) => self.serve_review(action),
            request @ (Request::Install { .. } | Request::Decline { .. }) => {
                self.respond_about_installs(token, request);
                Ok(Response::Done)
            }
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
            Request::SplitTab {
                tab,
                side,
                terminal,
                share,
            } => self.split_tab(tab, side, terminal, share),
            Request::ClosePane { pane } => match self.workspaces.check_pane(pane)? {
                Content::Terminal(terminal) => {
                    self.close_terminal(terminal).map(|()| Response::Done)
                }
                Content::Buffer(buffer) => {
                    self.close_buffer_pane(pane, buffer);
                    Ok(Response::Done)
                }
                Content::Files(files) => {
                    self.close_files_pane(pane, files);
                    Ok(Response::Done)
                }
                Content::Board(_) => {
                    self.close_board_pane(pane);
                    Ok(Response::Done)
                }
                Content::Review(review) => {
                    self.close_review_pane(pane, review);
                    Ok(Response::Done)
                }
            },
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
            Request::ResizeSplit { tab, split, shares } => self
                .workspaces
                .resize_split(tab, &split, &shares)
                .map(|()| {
                    self.workspaces_changed();
                    Response::Done
                }),
            Request::OpenHere { pane, file } => self.open_here(pane, file),
            Request::CloseTab { tab } => {
                let panes = self.workspaces.tab_panes(tab)?;
                self.close_panes(&panes)
            }
            Request::CloseWorkspace { workspace } => {
                let panes = self.workspaces.workspace_panes(workspace)?;
                self.close_panes(&panes)
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
        self.forget_clipboard_asks(token, Some(id));
    }

    /// Writes `bytes` to terminal `id`, as typed by the client at `token`.
    fn send_input(&mut self, token: Token, id: TerminalId, bytes: &[u8]) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        match &mut slot.terminal {
            Hosted::Pty(terminal) => {
                terminal.send_input(bytes)?;
                slot.typist = Some(token);
                self.update_pty_interest(id);
            }
            Hosted::Chat(_) => return Err(Error::IsAChat(id)),
        }
        Ok(())
    }

    /// The emulator of terminal `id`: its screen and history.
    fn grid(&self, id: TerminalId) -> Result<&Terminal, Error> {
        let slot = self.terminals.get(&id).ok_or(Error::NoSuchTerminal(id))?;
        slot.terminal.grid().ok_or(Error::IsAChat(id))
    }

    /// Does what a client asked of the chat `id`.
    fn chat_action(&mut self, id: TerminalId, action: ChatAction) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        let chat = slot.terminal.chat().ok_or(Error::NotAChat(id))?;
        // Undoing is about files, so the daemon's, not the agent's.
        if let ChatAction::Undo { turn, force } | ChatAction::Redo { turn, force } = action {
            let redo = matches!(action, ChatAction::Redo { .. });
            let turn = usize::try_from(turn).unwrap_or(usize::MAX);
            self.undo_turn(id, turn, redo, force);
            return Ok(());
        }
        // Other agents are the server's.
        match action {
            ChatAction::Open { chat } => {
                self.open_subagent(id, chat);
                return Ok(());
            }
            ChatAction::Switch {
                agent,
                model,
                budget,
            } => {
                let budget = usize::try_from(budget).unwrap_or(usize::MAX);
                self.switch_chat(id, &agent, model, budget);
                self.collect_damage(id);
                return Ok(());
            }
            ChatAction::LearnModels => {
                self.learn_models(id);
                return Ok(());
            }
            _ => {}
        }
        let actions = chat.act(action);
        self.act(id, actions);
        self.collect_damage(id);
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
        // A client on another machine that has stopped reading, as one gone
        // without a word has, would hold ever more: it starts again on a
        // new connection, from a snapshot.
        if matches!(client.link, Link::Quic(_)) && client.outgoing.len() > MOST_UNSENT {
            log::warn!(
                "dropping a client on another machine {} B behind",
                client.outgoing.len()
            );
            self.drop_client_saying(token, "the window fell too far behind");
            return;
        }
        let flushed = client.outgoing.is_empty();
        // Writes to the clipboard wait for a client to catch up.
        if flushed && client.clipboard.any() {
            self.clipboard_unsent = true;
        }
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
        self.drop_client_saying(token, "");
    }

    /// Forgets a client, as [`drop_client`](Self::drop_client) does,
    /// telling one over QUIC `why`.
    fn drop_client_saying(&mut self, token: Token, why: &str) {
        if let Some(Source::Client(client)) = self.sources.try_remove(token.0) {
            self.forget_viewer(token, &client);
            self.forget_clipboard_asks(token, None);
            for slot in self.terminals.values_mut() {
                slot.typist.take_if(|typist| *typist == token);
            }
            self.forget_queries(token, None);
            self.installs.forget(token);
            self.forget_finds(token);
            self.forget_files_viewer(token);
            self.forget_board_viewer(token);
            self.forget_review_viewer(token);
            self.forget_stages(token, None);
            self.forget_piece_questions(token, None);
            if let Link::Quic(handle) = client.link
                && let Some(quic) = self.quic.as_mut()
            {
                quic.close(handle, Instant::now(), why);
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
        for slot in self.buffers.values_mut() {
            slot.viewers.retain(|&viewer| viewer != token);
        }
    }

    // --- QUIC -------------------------------------------------------------

    /// Answers a request from the client at `token` to turn its connection
    /// into something else: a carrier of QUIC packets, or a connection an
    /// agent's tools are served on.
    fn carry(&mut self, token: Token, request: &Request) -> Result<Response, Error> {
        match request {
            &Request::CarryMcp { chat } => self.carry_mcp(token, chat),
            _ => self.carry_quic(token),
        }
    }

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
        log::info!(
            "a connection now carries QUIC, as {}",
            quic::Way(quic::carrier_address(CarrierId {
                key: carrier.0,
                generation: self.carriers_made,
            }))
        );
        self.flush_carrier(carrier);
        // The client waits for the answer before sending packets, but
        // anything already here is a packet.
        self.receive_packets(carrier, None);
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
        let mut closed = None;
        loop {
            match carrier.stream.read(&mut self.buffer) {
                Ok(0) => {
                    closed = Some("its ssh session ended".to_owned());
                    break;
                }
                Ok(n) => carrier.packets.push(&self.buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    closed = Some(format!("reading from it failed: {err}"));
                    break;
                }
            }
        }
        self.receive_packets(token, closed);
    }

    /// Hands the QUIC endpoint the packets the carrier at `token` has
    /// brought, then closes the carrier if it has `closed`, saying why, or
    /// if its packets are malformed.
    fn receive_packets(&mut self, token: Token, mut closed: Option<String>) {
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
                    closed = Some(format!("it sent a bad frame: {err}"));
                    break;
                }
            }
        }
        if let Some(why) = closed {
            self.sources.remove(token.0);
            log::info!("{} closed: {why}", quic::Way(from));
            quic.carrier_closed(from);
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
            self.receive_packets(token, Some(format!("writing to it failed: {err}")));
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
                    // Gone quiet, it may be gone: programs' questions about
                    // the clipboard go to someone who can answer.
                    QuicEvent::Silent(token) => {
                        for slot in self.terminals.values_mut() {
                            slot.typist.take_if(|typist| *typist == token);
                        }
                    }
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

    /// Closes every QUIC connection, telling its client `reason`, and sends
    /// what says so as far as the sockets take it now: the daemon is about
    /// to stop or become another build, and nobody would answer after.
    /// Without it, a client on UDP would go on sending to a connection
    /// nobody knows, out of contact, until its idle timeout.
    fn close_remotes(&mut self, reason: &str) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        quic.close_all(Instant::now(), reason);
        self.transmit_quic();
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
        .collect::<Vec<_>>();
    HistoryLines {
        first: first.unwrap_or(end),
        links: LinkTarget::of(lines.iter().map(|line| &line.line), terminal),
        lines,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_term::Config;

    use super::*;

    #[test]
    fn renumbering_history_sends_a_snapshot() {
        let size = |cols| Size::new(cols, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size(10), &Config::default());
        terminal.advance(b"abcdefghijklmnopqrstuvwxy");
        let mut sent = Subscription::new();
        assert!(matches!(
            sent.update(&terminal),
            Some(ViewUpdate::Snapshot(_))
        ));
        terminal.advance(b"z");
        assert!(matches!(sent.update(&terminal), Some(ViewUpdate::Patch(_))));

        // Back at the same size before the client heard of the first
        // resize, but the line in history was taken back and rewrapped.
        terminal.resize(size(7));
        terminal.resize(size(10));
        assert!(matches!(
            sent.update(&terminal),
            Some(ViewUpdate::Snapshot(_))
        ));
    }

    #[test]
    fn updates_and_history_carry_the_links_their_lines_show_and_the_cwd() {
        let size = Size::new(20, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        terminal.advance(b"\x1b]8;id=d;https://docs/\x07docs\x1b]8;;\x07\r\n");
        let mut sent = Subscription::new();
        let Some(ViewUpdate::Snapshot(view)) = sent.update(&terminal) else {
            panic!("a snapshot first");
        };
        let grid = view.grid().expect("a grid");
        let uris = |links: &clackatoo_protocol::Links| -> Vec<(String, bool)> {
            (links.iter())
                .map(|(_, link)| (link.uri.clone(), link.named))
                .collect()
        };
        assert_eq!(uris(&grid.links), [("https://docs/".to_owned(), true)]);
        assert_eq!(view.cwd, None);

        // A patch carries the links of the lines it sends, and no others.
        terminal.take_damage(&mut Damage::new());
        terminal.advance(b"\x1b]7;file:///w\x07\x1b]8;;https://new/\x07new\x1b]8;;\x07");
        terminal.take_damage(&mut sent.damage);
        let Some(ViewUpdate::Patch(patches)) = sent.update(&terminal) else {
            panic!("a patch");
        };
        assert!(patches.contains(&Patch::Cwd(Some("/w".to_owned()))));
        let links = patches.iter().find_map(|patch| match patch {
            Patch::Grid { links, .. } => Some(links),
            _ => None,
        });
        assert_eq!(
            uris(links.expect("the grid changed")),
            [("https://new/".to_owned(), false)]
        );

        // Lines scrolled into history bring theirs when fetched.
        terminal.advance(b"\r\n\r\n");
        let fetched = history(&terminal, terminal.primary().scrollback_end(), 10);
        assert_eq!(fetched.lines[0].line.text(), "docs");
        assert_eq!(
            uris(&fetched.links),
            [
                ("https://docs/".to_owned(), true),
                ("https://new/".to_owned(), false)
            ]
        );
    }

    #[test]
    fn a_reset_sends_a_snapshot() {
        let size = Size::new(10, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        terminal.advance(b"one\r\ntwo\r\nthree\r\n");
        let mut sent = Subscription::new();
        sent.update(&terminal);
        // A reset numbers history from 0 again, and more output can take
        // it past where it was before the client hears of either.
        terminal.advance(b"\x1bc");
        terminal.advance(&b"line\r\n".repeat(10));
        assert!(matches!(
            sent.update(&terminal),
            Some(ViewUpdate::Snapshot(_))
        ));
    }
}
