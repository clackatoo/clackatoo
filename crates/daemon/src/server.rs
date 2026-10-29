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
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::frame::{self, FrameDecoder};
use clackatoo_protocol::wire;
use clackatoo_protocol::{
    BlockMarks, BufferId, ChatAction, ClientMessage, Content, Error, GridState, HistoryLine,
    HistoryLines, Line, LinkTarget, Machine, MachineId, NodeId, PROTOCOL_VERSION, Pane, PaneId,
    PaneTerminal, Patch, Request, Response, ServerMessage, Side, Size, TabId, TerminalId,
    TerminalInfo, TerminalStatus, Udp, View, ViewUpdate, WorkspaceId,
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
mod background;
mod boards;
mod branches;
mod browser;
mod buffers;
mod chain;
mod chat_queue;
mod chats;
mod clipboard;
mod disk;
mod edit_log;
mod explorers;
mod finder;
mod fits;
mod forwards;
mod groups;
mod hover;
mod keep;
mod language;
mod lending;
mod lent_background;
mod links;
mod mentions;
mod offers;
mod pieces;
mod recovery;
mod relay;
mod rename;
mod restore;
mod review;
mod settling;
mod signs;
mod stopping;
mod symbols;
mod tools;
mod turn_undo;
mod watched;
mod web;

use buffers::{BufferSlot, WindowChange};
use chats::AgentId;
use explorers::Explorers;
use finder::Finds;
use language::{Pipe, ServerId, Servers};
use offers::Installs;
use signs::Signs;
use tools::Tools;
pub(crate) use tools::unhidden;

const LISTENER: Token = Token(usize::MAX);
const WAKER: Token = Token(usize::MAX - 1);
const UDP: Token = Token(usize::MAX - 2);
const WEB: Token = Token(usize::MAX - 3);
const SIGNALS: Token = Token(usize::MAX - 4);

/// The most events taken from epoll at once.
const EVENTS: usize = 256;

/// Bytes read per `read` call, from PTYs and clients alike.
const READ_BUFFER_LEN: usize = 64 * 1024;

/// The most output read from one PTY before giving the other terminals and
/// clients a turn. Keeps a flood of output (`cat` of a huge file) from
/// freezing everything else.
const OUTPUT_BUDGET: usize = 1024 * 1024;

/// The most output read from a PTY when epoll says it has some. A key's
/// echo is a few bytes, read whole; a flood gets a read or two now, and
/// the rest at the end of the turn, once view updates have gone out.
/// Reading a MiB of a flood takes milliseconds, mostly in the kernel, and
/// that mustn't stand between a terminal being typed in and the update
/// showing what was typed (CLACKA-13).
///
/// It's also how much an agent, a language server or an agent's tools are
/// read at a time (CLACKA-105). A flood of what they send gives way only
/// between reads, and what came is parsed and taken in after each:
/// 4 KiB of small messages, such as an agent's streamed words, costs about
/// 35 µs, and a MiB of them 9 ms. A big message's bytes are only copied
/// until it's whole, at a few microseconds a read, so reading them a
/// little at a time costs next to nothing.
const FIRST_LOOK: usize = 4 * 1024;

/// How long a flood, while it has the loop, reads before asking epoll
/// whether anything else wants it. Asking costs a system call, and a flood
/// comes in many small reads: asking after every one costs a fifth more
/// CPU per MiB.
const YIELD_EVERY: Duration = Duration::from_micros(50);

/// How long after keys are sent to one terminal a flood in another asks
/// after every read, so the echo goes out as soon as it comes.
const TYPING: Duration = Duration::from_secs(1);

/// The most a client on another machine may have waiting to be sent, past
/// what its streams have taken, before it's dropped.
const MOST_UNSENT: usize = 8 * 1024 * 1024;

/// A web connection holds the in-flight frame here rather than in QUIC's streams.
/// Allow one maximum-size protocol frame plus the usual backlog headroom:
/// a chat snapshot must not disconnect a reader before it can drain the socket.
/// View updates already wait for the previous update to drain and be acked.
const MOST_WEB_UNSENT: usize = frame::MAX_FRAME_LEN + MOST_UNSENT;

/// How long a terminal's output must pause before its scrollback is
/// compacted: long enough that a flood never stops for it, short enough
/// that the memory comes back soon after.
const COMPACT_AFTER: Duration = Duration::from_millis(100);

/// The longest an upgrade waits for the boards' last changes to be
/// written, before it's refused.
const UPGRADE_BOARDS_WAIT: Duration = Duration::from_secs(10);

/// The longest the daemon waits, as it stops or upgrades, for the packets
/// closing its QUIC connections to go: one QUIC holds back goes once the
/// client is next heard from, which is soon, as a client answers the check
/// of a new path at once.
const CLOSING_WAIT: Duration = Duration::from_millis(500);

/// The longest the daemon takes to stop, well short of the 5 s `clackatoo
/// stop` waits before killing it: language servers, and what agents
/// started, are waited for [`stopping::STOP_WAIT`] of it, and file
/// operations under way and the boards' last changes all of it.
const STOPPING_WAIT: Duration = Duration::from_secs(3);

/// The longest an upgrade waits for file operations under way to finish,
/// before it's refused: short of how long `clackatoo upgrade` waits to
/// hear back, so it hears why.
const UPGRADE_OPS_WAIT: Duration = Duration::from_secs(3);

/// How many messages a client may have sent lately that couldn't be read,
/// as from a newer client, before it's taken to be broken and dropped.
const MOST_UNREADABLE: u32 = 64;

/// How many of those are forgotten each second.
const UNREADABLE_FORGOTTEN: u32 = 8;

/// An upgrade a client asked for, waiting to go ahead.
#[derive(Debug)]
struct Upgrading {
    /// The program the daemon is to become.
    executable: PathBuf,
    /// What it takes beside the handover: the signals held back for it,
    /// and agents' processes as they are.
    takes: crate::upgrade::Takes,
    /// The client that asked.
    token: Token,
    /// The longest it waits for file operations under way to finish.
    until: Instant,
}

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
    /// A connection to the web app, while it's HTTP.
    Web(Box<crate::web::HttpConn>),
    /// A connection a window on another machine forwards: its socket, if
    /// one is being made, and its QUIC stream.
    Forward(forwards::Pipe),
    /// A connection to the daemon on another machine, which this daemon
    /// is a client of.
    Reached(links::Reached),
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

    /// Whether its shell has it to itself, to be typed into (see
    /// [`HostedTerminal::shell_has_terminal`]). Never a chat.
    fn shell_has_terminal(&self) -> Result<(), crate::hosted::Taken> {
        match self {
            Self::Pty(terminal) => terminal.shell_has_terminal(),
            Self::Chat(_) => Err(crate::hosted::Taken::Ended),
        }
    }

    /// Whether its program is still running, or how it ended: a chat runs
    /// until it's closed.
    fn status(&self) -> TerminalStatus {
        match self {
            Self::Pty(terminal) => terminal.status(),
            Self::Chat(_) => TerminalStatus::Running,
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

    fn size(&self) -> Size {
        match self {
            Self::Pty(terminal) => terminal.emulator().size(),
            Self::Chat(chat) => chat.size(),
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
    /// The client it's fitted to for now, as a phone fits one, and the
    /// size it goes back to.
    fit: Option<fits::Fit>,
}

/// A connected client.
#[derive(Debug)]
// Separate facts about the connection, not states of one machine.
#[allow(clippy::struct_excessive_bools)]
struct Client {
    link: Link,
    decoder: FrameDecoder,
    /// The protocol version the connection runs at, agreed in the
    /// `Hello`s: the client's, if it's older than this build.
    wire: u32,
    /// The messages it has sent lately that couldn't be read.
    unreadable: Unreadable,
    /// Encoded responses not yet written.
    outgoing: Vec<u8>,
    greeted: bool,
    /// Whether it has sent anything but its `Hello`, so it's known what
    /// it's for: until then, it may be a relay about to ask to serve a
    /// chat's tools (see `chats/live.rs`).
    asked: bool,
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
    /// The latest picture of each chat's page the client is attached to
    /// that it hasn't been sent, waiting for it to catch up.
    browser_frames: BTreeMap<TerminalId, Arc<Vec<u8>>>,
    /// The window the client is, as it said when attaching to a buffer:
    /// see [`Request::AttachBuffer`].
    window: Option<u64>,
    /// The client is a daemon on another machine, relaying for clients of
    /// its own (see [`Request::Relay`]): its edits are answered with the
    /// edit made, and its questions never go for later ones.
    relays: bool,
    /// The machine of the daemon relaying, if it said
    /// ([`Request::RelayAs`]): it's shown the workspaces lent it, and may
    /// ask for panes to be lent it.
    machine: Option<Machine>,
    /// Whether the client is told when the chats change.
    watching_chats: bool,
    /// Made for a stream reaching this daemon from another machine: it may
    /// only greet the daemon and carry QUIC.
    carrier_only: bool,
    /// For a test's daemon in the way: the machine it signs its QUIC
    /// certificate as, in that machine's place, with its own key.
    carrier_as: Option<MachineId>,
    /// Whether the client is told when the connections to other daemons
    /// change.
    watching_connections: bool,
    /// Until when the client's user is there, as it last said.
    present_until: Option<Instant>,
    /// Events to queue after the answer to the request being answered.
    later: Vec<u8>,
    /// Answers waiting for one before them, which was passed on to the
    /// daemon on another machine (see [`relay`]): a client's answers come
    /// in the order it asked.
    held: VecDeque<Held>,
}

/// An answer of a client's, waiting its turn.
#[derive(Debug)]
enum Held {
    /// Passed on to another daemon, numbered this, whose answer comes.
    Waiting(u64),
    /// Ready, encoded, with any events to follow it.
    Ready(Vec<u8>),
}

/// The messages a client has sent lately that couldn't be read, as from a
/// newer client: a total that's forgotten as time goes by, so a client
/// that sends a few now and then is never dropped, and one that sends a
/// stream of them soon is.
///
/// Four bytes, as every client has one: a count of 255 at most, and when
/// it was last forgotten down, in the daemon's whole seconds (see
/// [`seconds`]), which wrap round every 18 hours: a count left that long
/// is forgotten as if no time had passed, which only drops a client that
/// keeps sending them sooner.
#[derive(Debug, Default)]
struct Unreadable {
    /// How many, as of `at`.
    count: u8,
    /// Whether any has been: the first is logged as a warning, the rest
    /// for debugging.
    any: bool,
    at: u16,
}

impl Unreadable {
    /// Counts one more, at `now`. Returns whether to go on with the
    /// client, not having had too many lately.
    fn one_more(&mut self, now: Instant) -> bool {
        let now = seconds(now);
        let forgotten = u32::from(now.wrapping_sub(self.at)).saturating_mul(UNREADABLE_FORGOTTEN);
        if forgotten > 0 || self.count == 0 {
            self.at = now;
        }
        let count = u32::from(self.count).saturating_sub(forgotten) + 1;
        self.count = u8::try_from(count).unwrap_or(u8::MAX);
        count <= MOST_UNREADABLE
    }
}

/// `now`, in whole seconds of the daemon's own clock, which starts the
/// first time it's read, round and round 65,536 of them.
fn seconds(now: Instant) -> u16 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(|| now);
    let seconds = now.saturating_duration_since(start).as_secs();
    u16::try_from(seconds % 65_536).expect("less than 65,536")
}

/// How much of a terminal's output, or of what a peer sent, a read takes,
/// and when it stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// A first look, as epoll says output came: up to [`FIRST_LOOK`]. If
    /// there's more, a terminal's is shown once the rest is read too.
    Look,
    /// A flood's turn, at the end of the loop's: up to [`OUTPUT_BUDGET`],
    /// giving way as soon as anything else is ready.
    Flood,
    /// What's left as the program exits: up to [`OUTPUT_BUDGET`], so its
    /// last output is shown, or its last messages taken in, before anyone
    /// hears that it ended.
    Last,
}

/// Something with more to read than one read took, queued to be read at
/// the end of each turn, after the turn's view updates have gone out,
/// until it's drained (CLACKA-13, CLACKA-105).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flood {
    /// A terminal's output.
    Terminal(TerminalId),
    /// What a language server sent, whether it's running or stopping.
    Server(ServerId),
    /// What an agent sent.
    Agent(AgentId),
    /// What an agent sent its tools, on the connection at this token.
    Tools(Token),
    /// What the daemon on another machine sent, on the connection at this
    /// token.
    Reached(Token),
}

/// Whether anything but a flood has become ready while a flood is read: a
/// key from a client, the echo in a terminal being typed in, a thread's
/// result. Asked between reads, so the flood gives way within one.
///
/// Epoll is asked without waiting, and what it says is kept in `waiting`,
/// to be handled first next turn. Only that what's being read (`own`), or
/// a flood already queued in `unread`, has more to read is let go: both
/// are read until drained, or queued again, whatever epoll says, as
/// edge-triggered readiness allows. Their readiness to write is kept, as
/// queued input waits on it.
fn others_ready(
    poll: &mut Poll,
    waiting: &mut Events,
    sources: &Slab<Source>,
    unread: &VecDeque<Flood>,
    own: Option<Token>,
) -> bool {
    if !waiting.is_empty() {
        return true;
    }
    if let Err(err) = poll.poll(waiting, Some(Duration::ZERO)) {
        // Interrupted, most likely; the next read asks again.
        log::debug!("checking for events while reading a flood failed: {err}");
        return false;
    }
    let flood = |event: &Event| {
        let token = event.token();
        let queued = match sources.get(token.0) {
            Some(&Source::Output(id)) => Some(Flood::Terminal(id)),
            Some(&Source::Server(id, Pipe::Output)) => Some(Flood::Server(id)),
            Some(&Source::Agent(id, Pipe::Output)) => Some(Flood::Agent(id)),
            Some(Source::Tools(_)) => Some(Flood::Tools(token)),
            Some(Source::Reached(_)) => Some(Flood::Reached(token)),
            _ => None,
        };
        !event.is_writable()
            && (Some(token) == own || queued.is_some_and(|flood| unread.contains(&flood)))
    };
    if waiting.iter().all(flood) {
        waiting.clear();
        return false;
    }
    true
}

/// Queues `update`, of terminal `id`'s view, on `outgoing` for a client at
/// protocol `wire`, which `subscription` is of. Returns whether it was:
/// one dropped as newer than the client, or that can't be encoded, never
/// arrives, so the subscription forgets what it recorded as sent, and its
/// next update is a snapshot.
fn queue_update(
    subscription: &mut Subscription,
    wire: u32,
    id: TerminalId,
    update: ViewUpdate,
    outgoing: &mut Vec<u8>,
) -> bool {
    let event = ServerMessage::Event(clackatoo_protocol::Event::View {
        terminal: id,
        update,
    });
    match frame::encode(wire, &event) {
        Ok(bytes) if !bytes.is_empty() => {
            outgoing.extend_from_slice(&bytes);
            subscription.awaiting_ack = true;
            true
        }
        Ok(_) => {
            subscription.forget_sent();
            false
        }
        Err(err) => {
            log::error!("encoding a view update failed: {err}");
            subscription.forget_sent();
            false
        }
    }
}

/// A message going to many clients, encoded once for each protocol
/// version they run at, and only for those.
pub(super) struct Broadcast<'a> {
    message: &'a ServerMessage,
    /// What it is, for the log, if it can't be encoded.
    what: &'static str,
    encoded: Vec<(u32, Option<Vec<u8>>)>,
}

impl<'a> Broadcast<'a> {
    pub(super) fn new(message: &'a ServerMessage, what: &'static str) -> Self {
        Self {
            message,
            what,
            encoded: Vec::new(),
        }
    }

    /// The message, encoded for a connection at `wire`, or `None` if it
    /// can't be, which is logged once.
    pub(super) fn to(&mut self, wire: u32) -> Option<&[u8]> {
        if let Some(at) = self.encoded.iter().position(|&(at, _)| at == wire) {
            return self.encoded[at].1.as_deref();
        }
        let bytes = frame::encode(wire, self.message)
            .inspect_err(|err| log::error!("encoding {} failed: {err}", self.what))
            .ok();
        self.encoded.push((wire, bytes));
        self.encoded.last().and_then(|(_, bytes)| bytes.as_deref())
    }
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
            wire: PROTOCOL_VERSION,
            unreadable: Unreadable::default(),
            outgoing: Vec::new(),
            greeted: false,
            asked: false,
            finished: false,
            wants_writable: false,
            views: BTreeMap::new(),
            watching_workspaces: false,
            carrying: false,
            serving_tools: None,
            streams: BTreeMap::new(),
            clipboard: clipboard::Unsent::default(),
            browser_frames: BTreeMap::new(),
            window: None,
            relays: false,
            machine: None,
            watching_chats: false,
            watching_connections: false,
            carrier_only: false,
            carrier_as: None,
            present_until: None,
            later: Vec::new(),
            held: VecDeque::new(),
        }
    }

    /// Whether what's been queued for it is still waiting to be written:
    /// view updates wait for it to catch up.
    fn backlogged(&self) -> bool {
        match &self.link {
            Link::Unix(_) => !self.outgoing.is_empty(),
            Link::Quic(_) => false,
            Link::Web(link) => !self.outgoing.is_empty() || link.backlogged(),
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
    /// Over a WebSocket, from a device paired with the web app.
    Web(Box<crate::web::WebLink>),
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
    /// The newest block of the shell's the client has been told how it
    /// ended, or needn't be: blocks finish in order, and a patch tells of
    /// those finished after it (see [`BlockMarks::finished_after`]).
    settled: Option<u32>,
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
            settled: None,
            awaiting_ack: false,
        }
    }

    /// Forgets what the client was sent, so the next update is a snapshot,
    /// as after a re-attach.
    fn forget_sent(&mut self) {
        let recent = self.chat.recent;
        let first = self.chat.first;
        *self = Self::new();
        self.chat.recent = recent;
        self.chat.first = first;
    }

    /// The update that brings the client, which speaks `wire`, from what
    /// it was last sent to what `hosted` shows now, recorded as sent.
    /// `None` if nothing changed.
    fn update_from(&mut self, hosted: &Hosted, wire: u32) -> Option<ViewUpdate> {
        match hosted {
            Hosted::Pty(terminal) => self.update(terminal.emulator(), wire),
            Hosted::Chat(chat) => chat.catch_up(&mut self.chat),
        }
    }

    /// The update that brings the client, which speaks `wire`, from what
    /// it was last sent to what `terminal` shows now, recorded as sent.
    /// `None` if nothing changed.
    fn update(&mut self, terminal: &Terminal, wire: u32) -> Option<ViewUpdate> {
        let size = terminal.size();
        let state = GridState::of(terminal);
        let history_generation = terminal.primary().history_generation();
        if self.size != Some(size) || self.history_generation != history_generation {
            self.size = Some(size);
            self.history_generation = history_generation;
            terminal.title().clone_into(&mut self.title);
            self.cwd = terminal.cwd().map(str::to_owned);
            self.state = state;
            // The snapshot says how the blocks it shows the prompts of
            // ended, and history how those it holds did.
            self.settled = BlockMarks::settled(terminal);
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
        let damaged: Vec<u16> = self
            .damage
            .lines(grid.height())
            .map(|line| u16::try_from(line).expect("screen lines are bounded by Size::MAX"))
            .collect();
        self.damage.clear();
        let row = |line: u16| grid.row(usize::from(line));
        let lines: Vec<(u16, Line)> = (damaged.iter())
            .map(|&line| (line, Line::from_row(row(line))))
            .collect();
        // Only a window that reads them is told where prompts are, or how
        // blocks ended: a look at each line sent, and at the blocks after
        // the last one it was told of, which is the newest but after a
        // burst of commands.
        let marks = if wire::daemon_has(wire, wire::BLOCK_MARKS) {
            let (exits, settled) = BlockMarks::finished_after(terminal, self.settled);
            self.settled = settled;
            BlockMarks {
                prompts: BlockMarks::prompts_of(damaged.iter().map(|&line| (line, row(line)))),
                exits,
            }
        } else {
            BlockMarks::default()
        };
        if !lines.is_empty() || state != self.state || !marks.exits.is_empty() {
            self.state = state;
            let shown = LinkTarget::of(lines.iter().map(|(_, line)| line), terminal);
            patches.push(Patch::Grid {
                node: NodeId::ROOT,
                state,
                lines,
                links: shown,
                marks,
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
    /// Parses big messages from agents and language servers off the loop,
    /// with a thread started the first time one comes.
    parser: crate::rpc::Parser,
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
    /// The hovers whose code is being coloured.
    colourings: hover::Colourings,
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
    /// The messages to chats waiting for the files they mention to be
    /// read, and what's held behind them.
    mentioning: mentions::Mentioning,
    /// The process groups of agents and language servers stopped, until
    /// nothing they started is left.
    groups: groups::Groups,
    /// Files as agents read them, and what agents changed in them.
    agent_files: agent_files::AgentFiles,
    /// Buffers' files changed on disk, waiting to be looked at.
    disk: disk::Disk,
    /// What agents changed and the user hasn't reviewed, and reviews.
    reviews: review::Reviews,
    /// Chats' pages in clackatoo's browser, and the windows watching them.
    browsing: browser::Browsing,
    /// The QUIC endpoint, set up when a connection first carries QUIC.
    quic: Option<QuicServer>,
    /// What the QUIC endpoint last asked to be acted on.
    quic_events: Vec<QuicEvent>,
    /// How many connections have become carriers.
    carriers_made: u32,
    /// Where QUIC connections that have moved off their carriers send and
    /// receive, set up along with the QUIC endpoint.
    udp: Option<UdpSocket>,
    /// Terminals and peers whose read budget ran out with more waiting,
    /// each once, read at the end of each turn.
    unread: VecDeque<Flood>,
    /// What a peer sent was read at the end of the last turn. Taking it in
    /// may have left work for the chores earlier in the turn, such as a
    /// message to relay to another chat, so the next turn doesn't wait for
    /// an event.
    peers_flooded: bool,
    /// Events that came while a terminal's flood was being read, taken so
    /// the read could stop for them. Handled first next turn.
    waiting: Events,
    /// The terminal a client last sent keys to, and when.
    typed: Option<(TerminalId, Instant)>,
    /// When each terminal with uncompacted scrollback should compact it.
    compactions: BTreeMap<TerminalId, Instant>,
    /// Some view may have an update to send.
    views_pending: bool,
    /// Where a terminal's damage is collected before it goes to each viewer.
    damage: Damage,
    config: DaemonConfig,
    buffer: Box<[u8]>,
    /// A client asked the daemon to become another program: see
    /// [`ClientMessage::Upgrade`].
    upgrade_to: Option<Upgrading>,
    /// The arguments an upgrade runs the next build with. Without them,
    /// the daemon can't upgrade.
    upgrade_args: Vec<std::ffi::OsString>,
    /// The web app, if it's on.
    web: Option<crate::web::Web>,
    /// The chats as a list, and the notifications they're to send.
    chat_watch: web::ChatWatch,
    /// Web connections with more to read or answer than one turn takes.
    web_unread: Vec<Token>,
    /// Connections windows on other machines forward.
    forwards: forwards::Forwards,
    /// Buffers' recovery files, written as their changes are made.
    recovery: recovery::Recovery,
    /// Keeping the workspaces from one run of the daemon to the next.
    keeping: restore::Keeping,
    /// What's kept of the terminals in background tabs.
    background: background::Backgrounds,
    /// The terminals kept running always, and how running each again
    /// goes.
    keeps: keep::Keeps,
    /// Which daemon this is, kept from one run to the next.
    machine: Machine,
    /// The key that proves it, which signs the QUIC certificate.
    signer: crate::machine::Signer,
    /// The directory the daemon's socket is in, where ssh's askpass
    /// sockets go.
    runtime: Option<PathBuf>,
    /// The connections to the daemons on other machines.
    links: links::Links,
    /// The desktop's display the programs it starts follow, if they do.
    display: Option<crate::display::Display>,
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
        let mut daemon = Self::listening(listener, config, None)?;
        // What the daemons before it kept of buffers' changes not saved.
        daemon.start_recovery(None);
        Ok(daemon)
    }

    /// A daemon with no terminals, listening on `listener`: it waits a
    /// moment for the lock on the workspaces it keeps if it's held (see
    /// [`saved::lock`](crate::saved::lock)). It's the machine `machine`,
    /// handed over by the build before, or else the one its config keeps.
    #[allow(clippy::too_many_lines, reason = "a field each")]
    fn listening(
        mut listener: UnixListener,
        config: DaemonConfig,
        machine: Option<crate::machine::Handed>,
    ) -> io::Result<Self> {
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut listener, LISTENER, Interest::READABLE)?;
        let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
        // Signals asking it to stop, in a daemon that catches them (see
        // `crate::signals`).
        if let Some(pipe) = crate::signals::pipe() {
            let pipe = pipe.as_raw_fd();
            (poll.registry()).register(&mut SourceFd(&pipe), SIGNALS, Interest::READABLE)?;
        }
        let finds = {
            let waker = Arc::clone(&waker);
            Finds::new(&config.home, move || {
                if let Err(err) = waker.wake() {
                    log::warn!("waking the event loop for what was found failed: {err}");
                }
            })
        };
        let boards = boards::BoardState::start(config.boards.clone(), &waker);
        let keeping = restore::Keeping::new(&config);
        // Chosen afresh, unless the build before hands its own over.
        let id = crate::client::random_window();
        let (machine, signer) = crate::machine::identity(
            config.machine.as_deref(),
            config.machine_name.as_deref(),
            machine,
            id,
        );
        log::info!("this is {} ({})", machine.name, machine.id);
        let runtime = (listener.local_addr().ok())
            .and_then(|address| Some(address.as_pathname()?.parent()?.to_owned()));
        let mut workspaces = WorkspaceState::with_home(&config.home);
        workspaces.set_machine(machine.clone());
        let links = links::Links::knowing(config.machine.as_deref());
        let display = config.display.clone().map(crate::display::Display::start);
        Ok(Self {
            poll,
            listener,
            parser: crate::rpc::Parser::new(
                &waker,
                config
                    .gone_wait
                    .clamp(crate::rpc::PARSE_WAIT, std::time::Duration::from_hours(24)),
            ),
            waker,
            shutdown: Arc::new(AtomicBool::new(false)),
            sources: Slab::new(),
            terminals: BTreeMap::new(),
            next_id: 1,
            workspaces,
            buffers: BTreeMap::new(),
            next_buffer: 1,
            id,
            languages: config.languages.clone().map(Languages::new),
            syntax: None,
            servers: Servers::default(),
            installs: Installs::new(config.declined.as_deref()),
            renames: rename::Renames::default(),
            finds,
            tools: Tools::default(),
            background: background::Backgrounds::default(),
            keeps: keep::Keeps::default(),
            explorers: Explorers::default(),
            boards,
            signs: Signs::default(),
            piece_questions: pieces::PieceQuestions::default(),
            colourings: hover::Colourings::default(),
            clipboard_asks: 0,
            clipboard_due: Vec::new(),
            clipboard_unsent: false,
            agents: chats::Agents::default(),
            mentioning: mentions::Mentioning::default(),
            groups: groups::Groups::default(),
            agent_files: agent_files::AgentFiles::default(),
            disk: disk::Disk::default(),
            reviews: review::Reviews::default(),
            browsing: browser::Browsing::default(),
            quic: None,
            quic_events: Vec::new(),
            carriers_made: 0,
            udp: None,
            unread: VecDeque::new(),
            peers_flooded: false,
            waiting: Events::with_capacity(EVENTS),
            typed: None,
            compactions: BTreeMap::new(),
            views_pending: false,
            damage: Damage::new(),
            config,
            buffer: vec![0; READ_BUFFER_LEN].into_boxed_slice(),
            upgrade_to: None,
            upgrade_args: Vec::new(),
            web: None,
            chat_watch: web::ChatWatch::default(),
            web_unread: Vec::new(),
            forwards: forwards::Forwards::default(),
            recovery: recovery::Recovery::default(),
            keeping,
            machine,
            signer,
            runtime,
            links,
            display,
        })
        .map(|mut daemon| {
            daemon.start_web();
            daemon
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
    #[allow(clippy::too_many_lines, reason = "each part of the state in turn")]
    pub(crate) fn resume(
        mut handover: Handover,
        config: DaemonConfig,
    ) -> Result<Self, HandoverError> {
        let mut previews: std::collections::HashMap<_, _> =
            std::mem::take(&mut handover.state.chat_previews)
                .into_iter()
                .map(|previews| (previews.chat, previews))
                .collect();
        let mut tool_previews = std::mem::take(&mut handover.state.tool_previews);
        let nested = std::mem::take(&mut handover.state.nested);
        let mut image_originals = std::mem::take(&mut handover.state.image_originals);
        let mut chat_queues = std::mem::take(&mut handover.state.chat_queues);
        let mut drafts = std::mem::take(&mut handover.state.drafts);
        let mut uids = std::mem::take(&mut handover.state.uids);
        let mut branches = std::mem::take(&mut handover.state.branches);
        let mut forks = std::mem::take(&mut handover.state.forks);
        let mut renumbered: Vec<TerminalId> = Vec::new();
        let mut continuations: std::collections::HashMap<_, _> =
            std::mem::take(&mut handover.state.chat_continuations)
                .into_iter()
                .map(|c| (c.chat, c))
                .collect();
        let mut continuing = Vec::new();
        let chats = std::mem::take(&mut handover.state.chats);
        let buffers = std::mem::take(&mut handover.state.buffers);
        let mut undo_states = std::mem::take(&mut handover.state.undo_states);
        let explorers = std::mem::take(&mut handover.state.explorers);
        let edits = std::mem::take(&mut handover.state.edits);
        let reviews = std::mem::take(&mut handover.state.reviews);
        let mut standing = handover.state.talk.take();
        let mut live = handover.state.live.take();
        let mut live_chats: std::collections::HashMap<u32, handover::LiveChat> = (live.as_mut())
            .map(|live| std::mem::take(&mut live.chats))
            .unwrap_or_default()
            .into_iter()
            .map(|chat| (chat.chat, chat))
            .collect();
        let state = &handover.state;
        let fds = &mut handover.fds;
        let listener = std::os::unix::net::UnixListener::from(fds.take(state.listener)?);
        let machine =
            (state.machine).map(|id| (MachineId(id), state.key.map(crate::machine::Seed)));
        let mut daemon = Self::listening(UnixListener::from_std(listener), config, machine)?;
        daemon.next_id = state.next_terminal;
        daemon.workspaces =
            WorkspaceState::resume(&state.workspaces, &state.lent).unwrap_or_else(|err| {
                log::error!("the workspaces couldn't be taken over: {err}");
                WorkspaceState::new()
            });
        daemon.workspaces.set_home(&daemon.config.home);
        daemon.workspaces.resume_background(&state.background);
        daemon
            .workspaces
            .resume_lent_background(&state.lent_background);
        daemon.workspaces.resume_kept(&state.kept);
        if let Some(restored) = &state.restored {
            daemon.workspaces.resume_restored(restored);
        }
        daemon.resume_machines(state);

        for terminal in &state.terminals {
            let id = TerminalId(terminal.id);
            let shell = (state.shells.iter()).find(|shell| shell.terminal == terminal.id);
            let resumed = HostedTerminal::resume(terminal, shell, fds, &daemon.config)
                .map_err(|err| err.to_string())
                .and_then(|terminal| daemon.add_terminal(terminal).map_err(|err| err.to_string()));
            if let Err(err) = resumed {
                log::error!("terminal {id} couldn't be taken over: {err}");
                daemon.workspaces.remove_terminal(id);
            }
        }
        // What each shell is known to have run, as its own, and the program
        // it was started as.
        for trusted in &state.trusted {
            let slot = daemon.terminals.get_mut(&TerminalId(trusted.terminal));
            if let Some(pty) = slot.and_then(|slot| slot.terminal.pty()) {
                pty.trust_blocks(&trusted.blocks);
            }
        }
        for exe in &state.exes {
            let slot = daemon.terminals.get_mut(&TerminalId(exe.terminal));
            if let Some(pty) = slot.and_then(|slot| slot.terminal.pty()) {
                pty.started_as(Some((exe.dev, exe.ino)));
            }
        }
        daemon.restate_backgrounds();
        daemon.restate_lent_backgrounds();
        // The terminals kept running always, as they were, and how running
        // each again was going.
        daemon.resume_restarts(&state.kept);
        daemon.refresh_kept();
        // The agents that went on through the upgrade, before their chats.
        let kept = match live.as_mut() {
            Some(live) => daemon.resume_agents(live, fds),
            None => std::collections::HashSet::new(),
        };
        let mut kept_chats = std::collections::HashSet::new();
        for chat in chats {
            let continuation = continuations.remove(&chat.id);
            let agent = daemon.agent_spec(&chat.agent);
            let images = previews.remove(&chat.id);
            let record = live_chats.remove(&chat.id);
            let mut chat = HostedChat::resume(chat, &agent);
            // Before anything handed over keyed by turn.
            if !chat.resume_uids(&mut uids) {
                renumbered.push(chat.info().id);
            }
            chat.resume_branches(&mut branches);
            chat.resume_forks(&mut forks);
            chat.resume_nesting(&nested);
            if let Some(images) = images {
                chat.resume_previews(images);
            }
            chat.resume_tool_previews(&mut tool_previews);
            chat.resume_originals(&mut image_originals);
            chat.resume_queue(&mut chat_queues);
            chat.resume_draft(&mut drafts);
            let id = chat.info().id;
            // Its agent went on through the upgrade: so does it, as it
            // was, with nothing to tell. If its agent couldn't be taken
            // back, it goes on as one whose agent the upgrade stopped.
            let mut continuation = continuation;
            let going_on = record.is_some() && kept.contains(chat.agent());
            let stopped = (standing.as_mut())
                .map(|standing| chat.resume_standing(standing, going_on))
                .unwrap_or_default();
            if let Some(record) = record {
                if going_on {
                    chat.resume_live(record);
                    if chat.is_connected() {
                        chat.set_native_steering(daemon.agent_steers(chat.agent()));
                    }
                    log::info!("chat {id}: it goes on with its agent through the upgrade");
                    kept_chats.insert(id);
                    daemon.add_chat(chat);
                    continue;
                }
                log::info!("chat {id}: its agent couldn't go on through the upgrade");
                continuation = chat.live_lost(record);
            }
            // What it ran in the background went with its agent: it's told
            // so, as its turn goes on, or else in a turn of its own.
            let mut told = false;
            if let Some(continuation) = continuation {
                if chat.continue_after_upgrade(continuation, &stopped) {
                    log::info!("chat {id}: its turn goes on after the upgrade");
                    continuing.push(id);
                    told = true;
                } else {
                    log::info!("chat {id}: its turn the upgrade cut off can't go on");
                }
            }
            // A turn the user had asked to stop isn't told of, nor what it
            // ran: the tasks show lost all the same.
            let asked_to_stop =
                (standing.as_ref()).is_some_and(|standing| standing.stopped.contains(&id.0));
            if !told && !asked_to_stop && chat.tell_upgrade(&stopped) {
                log::info!(
                    "chat {id}: told what the upgrade stopped ({} background tasks)",
                    stopped.len()
                );
            }
            daemon.add_chat(chat);
        }
        // What waits on the agents that went on, and the connections their
        // tools are served on.
        if let Some(live) = live.as_mut() {
            daemon.resume_agent_timers(live, &kept_chats);
            daemon.resume_tools(live, fds, &kept_chats);
        }
        daemon.next_buffer = state.next_buffer;
        // Older builds had none, and keep the one just chosen.
        if state.daemon != 0 {
            daemon.id = state.daemon;
        }
        // The same run, to any daemon connected to it.
        daemon.machine.run = daemon.id;
        daemon.workspaces.set_machine(daemon.machine.clone());
        for buffer in buffers {
            let id = BufferId(buffer.id);
            let undo = (undo_states.iter())
                .position(|undo| undo.buffer == buffer.id)
                .map(|at| undo_states.swap_remove(at));
            let mut slot = BufferSlot::new(Buffer::resume(buffer, undo));
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
        daemon.resume_talk(&state.undelivered, standing.as_ref());
        daemon.resume_activities(&state.activities);
        daemon.agent_files.log = edit_log::EditLog::resume(edits);
        // A chat whose turns were numbered again keeps nothing by the old
        // numbers.
        for chat in renumbered {
            daemon.agent_files.log.forget(chat);
        }
        daemon.resume_reviews(reviews);
        daemon.learn_from_chats();
        daemon.refresh_all_files();
        // After the buffers, whose files it has, with the lock the build
        // before held.
        let lock = (state.recovery_lock)
            .map(|lock| fds.take(lock))
            .transpose()
            .unwrap_or_else(|err| {
                log::error!("the recovery files' lock couldn't be taken over: {err}");
                None
            });
        daemon.start_recovery(lock);
        daemon.keep_places_kept();
        // A build that couldn't hand its offers over kept them: after the
        // terminals, and what their shells are known to have run. One that
        // could has, even if it had none, and nothing is taken.
        if state.restored.is_none() {
            daemon.offers_kept();
        }
        // The keys it knew other machines by, and the connections it had,
        // made again.
        for known in &state.keys {
            let pin = links::Pin {
                key: known.key,
                met: known.met,
            };
            daemon.links.pins.learn(MachineId(known.machine), pin);
        }
        daemon.connect_kept(links::Kept::taken_over(state));
        for orphan in &state.orphans {
            match Orphan::resume(orphan, fds) {
                Ok(orphan) => daemon.watch_orphan(orphan),
                Err(err) => log::error!("a hung-up program couldn't be taken over: {err}"),
            }
        }
        // The handover's copy of every terminal, and the rows restoring
        // their history built, are gone.
        drop(handover);
        // Every chat, file checkpoint, permission and orphan is restored
        // before resumed agents can act. No window or user message is needed.
        for id in continuing {
            daemon.act(id, vec![crate::chat::Action::Connect]);
            daemon.watch_chat_queue(id);
        }
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
    /// are. A daemon started `afresh` removes the shells' secrets a
    /// daemon that crashed left there; one that took over from an upgrade
    /// keeps its terminals'.
    pub fn install_shell_integration(&mut self, dir: PathBuf, afresh: bool) {
        // Not following the display, a file a daemon that did left would
        // have shells follow what it last said.
        let display = dir.join("display");
        if self.display.is_none()
            && let Err(err) = std::fs::remove_file(&display)
            && err.kind() != io::ErrorKind::NotFound
        {
            log::warn!("{}: removing it failed: {err}", display.display());
        }
        self.config.shell_integration = crate::shell::install(dir, afresh)
            .map_err(|err| log::warn!("shells won't mark their commands: {err}"))
            .ok();
    }

    /// The display the programs it starts now are given, if they follow
    /// the desktop's (see [`crate::display`]).
    pub(crate) fn display_env(&self) -> Option<Arc<crate::display::Env>> {
        self.display.as_ref().map(crate::display::Display::current)
    }

    /// Everything the next build needs, taken out of this daemon, which is
    /// left with nothing to hang up when it's dropped. The listening socket
    /// is shared: closing this daemon's copy leaves it open. Language
    /// servers are waited for until `servers_until`.
    #[cfg(test)]
    fn hand_over(&mut self, servers_until: Instant) -> io::Result<Handover> {
        self.hand_over_keeping(servers_until, &[])
    }

    /// Everything the next build needs, as [`hand_over`](Self::hand_over)
    /// takes it, but the agents `keep` aren't stopped: they go on, with
    /// their chats, as they are, and the connections their tools are
    /// served on (see `chats/live.rs`).
    #[allow(clippy::too_many_lines, reason = "each part of the state in turn")]
    fn hand_over_keeping(
        &mut self,
        servers_until: Instant,
        keep: &[AgentId],
    ) -> io::Result<Handover> {
        let kept_names = self.agent_names(keep);
        let kept_chats = self.chats_of(&kept_names);
        let mut live_chats = Vec::new();
        // Agents' changes to big files still being worked out go into the
        // edit log first, so the next build has them.
        self.settle_all_pending();
        // Written, for the next build to keep on from, before anything
        // goes over to it.
        self.keep_last();
        let activities = self.hand_over_activities().map_err(io::Error::other)?;
        let mut fds = Fds::default();
        let listener = fds.add(self.listener.as_fd().try_clone_to_owned()?);
        let mut terminals = Vec::new();
        let mut shells = Vec::new();
        let mut trusted = Vec::new();
        let mut exes = Vec::new();
        let mut chats = Vec::new();
        let mut chat_previews = Vec::new();
        let mut tool_previews = Vec::new();
        let mut nested = Vec::new();
        let mut image_originals = Vec::new();
        let mut chat_queues = Vec::new();
        let mut drafts = Vec::new();
        let mut uids = Vec::new();
        let mut branches = Vec::new();
        let mut forks = Vec::new();
        let mut chat_continuations = Vec::new();
        let mut standing = handover::Talk::default();
        self.abandon_clipboard_asks();
        self.put_back_fits();
        for slot in std::mem::take(&mut self.terminals).into_values() {
            match slot.terminal {
                Hosted::Pty(terminal) => {
                    let blocks = terminal.emulator().trusted_blocks();
                    let exe = terminal.exe();
                    let (terminal, shell) = terminal.hand_over(&mut fds);
                    if let Some((dev, ino)) = exe {
                        exes.push(handover::Exe {
                            terminal: terminal.id,
                            dev,
                            ino,
                        });
                    }
                    if !blocks.is_empty() {
                        trusted.push(handover::Trusted {
                            terminal: terminal.id,
                            blocks,
                        });
                    }
                    terminals.push(terminal);
                    shells.extend(shell);
                }
                Hosted::Chat(mut chat) => {
                    // Its agent goes on, and so does it, as it is.
                    let live = kept_chats.contains(&chat.info().id);
                    let continuation = if live { None } else { chat.prepare_upgrade() };
                    chat.hand_over_standing(&mut standing);
                    chat_previews.push(chat.hand_over_previews());
                    tool_previews.extend(chat.hand_over_tool_previews());
                    nested.extend(chat.hand_over_nesting());
                    image_originals.push(chat.hand_over_originals());
                    chat_queues.push(chat.hand_over_queue());
                    drafts.extend(chat.hand_over_draft());
                    uids.extend(chat.hand_over_uids());
                    branches.extend(chat.hand_over_branches());
                    forks.extend(chat.hand_over_forks());
                    if live {
                        let (handed, live) = chat.hand_over_live();
                        chats.push(handed);
                        live_chats.push(live);
                    } else {
                        chats.push(chat.hand_over());
                    }
                    chat_continuations.extend(continuation);
                }
            }
        }
        // Renames under way are dropped, rather than leave their new files
        // beside the old.
        self.abandon_renames();
        // The agents and language servers stop, and the next build reaps
        // them; nothing they started is left to it, but what the agents
        // kept run, which goes on with them. The boards' last changes are
        // written, for it to read.
        self.stop_agents(keep);
        self.stop_servers();
        self.begin_stopping_browser();
        self.wait_for_stopping(servers_until);
        self.stop_browser();
        self.stop_boards();
        let live = (!keep.is_empty()).then(|| {
            let mut live = self.hand_over_agents(keep, &kept_chats, &mut fds);
            live.chats = live_chats;
            self.hand_over_tools(&mut live, &mut fds, &kept_chats);
            live
        });
        let recovery_lock = self.stop_recovery().map(|lock| fds.add(lock));
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
        self.note_borrowers_seen();
        let (workspaces, lent) = self.workspaces.hand_over();
        let borrowers = (self.workspaces.borrowers().iter())
            .map(|(machine, borrower)| handover::Borrower {
                machine: machine.0,
                name: borrower.name.clone(),
                last_seen: borrower.last_seen,
            })
            .collect();
        let lenders = (self.workspaces.lenders().iter())
            .map(|(machine, name)| handover::Lender {
                machine: machine.0,
                name: name.clone(),
            })
            .collect();
        // Their ssh processes end with them, rather than outlive the exec.
        let (connections, sharing) = links::Kept::handed_over(self.end_links());
        Ok(Handover {
            state: handover::State {
                listener,
                lock: None,
                next_terminal: self.next_id,
                terminals,
                chats,
                chat_previews,
                tool_previews,
                image_originals,
                chat_queues,
                drafts,
                uids,
                branches,
                forks,
                chat_continuations,
                activities,
                lent_background: self.workspaces.hand_over_lent_background(),
                undelivered: self.hand_over_talk(),
                talk: Some(self.hand_over_counts(standing)),
                live,
                nested,
                orphans,
                workspaces,
                next_buffer: self.next_buffer,
                buffers: (self.buffers.iter())
                    .map(|(id, slot)| slot.buffer.hand_over(id.0))
                    .collect(),
                undo_states: (self.buffers.iter())
                    .filter_map(|(id, slot)| slot.buffer.hand_over_undo(id.0))
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
                recovery_lock,
                shells,
                machine: Some(self.machine.id.0),
                key: Some(self.signer.seed().0),
                keys: (self.links.pinned().into_iter())
                    .map(|(machine, pin)| handover::Known {
                        machine: machine.0,
                        key: pin.key,
                        met: pin.met,
                    })
                    .collect(),
                connections,
                lent,
                background: self.workspaces.hand_over_background(),
                kept: self.hand_over_kept(),
                restored: Some(self.workspaces.hand_over_restored()),
                borrowers,
                lenders,
                sharing,
                trusted,
                exes,
            },
            fds,
        })
    }

    /// Takes the machines the last build lent workspaces to, and what the
    /// machines its borrowed panes are on are called, from `state`, which
    /// it handed over.
    fn resume_machines(&mut self, state: &handover::State) {
        self.workspaces.set_borrowers(
            (state.borrowers.iter())
                .map(|borrower| {
                    let machine = MachineId(borrower.machine);
                    let kept = crate::workspaces::Borrower {
                        name: borrower.name.clone(),
                        last_seen: borrower.last_seen,
                    };
                    (machine, kept)
                })
                .collect(),
        );
        self.workspaces.set_lenders(
            (state.lenders.iter())
                .map(|lender| (MachineId(lender.machine), lender.name.clone()))
                .collect(),
        );
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
    #[allow(clippy::too_many_lines, reason = "the event loop: one call per chore")]
    pub fn run(&mut self) -> io::Result<Option<Upgrade>> {
        let mut events = Events::with_capacity(EVENTS);
        let mut waiting = Events::with_capacity(EVENTS);
        while !self.shutdown.load(Ordering::Acquire) {
            if let Some(upgrade) = self.upgrade_when_ready()? {
                return Ok(Some(upgrade));
            }
            // With output still waiting, view updates due (as for floods
            // read after this turn's went out), what peers sent taken in
            // after this turn's chores, or events taken while a flood was
            // read, poll without blocking so they're handled right
            // after anything else that's ready. Otherwise wait for the
            // next compaction or QUIC timer, if there is one.
            let agent_unread = self.agents.unread();
            let servers_unread = self.servers_unread();
            let tools_unread = self.tools_unread();
            // Queued as floods, which the poll doesn't wait for.
            self.relays_unpacked();
            let web_unread = !self.web_unread.is_empty();
            let peers_flooded = std::mem::take(&mut self.peers_flooded);
            let timeout = if self.unread.is_empty()
                && !peers_flooded
                && !self.forwards_ready()
                && !self.views_pending
                && self.waiting.is_empty()
                && !agent_unread
                && !servers_unread
                && !tools_unread
                && !web_unread
            {
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
                    .chain(self.next_stopping_due())
                    .chain(self.groups.next_due())
                    .chain(self.next_file_changes_due())
                    .chain(self.next_agent_due())
                    .chain(self.next_gone_server_due())
                    .chain(self.next_gone_agent_due())
                    .chain(self.next_web_due())
                    .chain(self.next_fit_due())
                    .chain(self.next_forward_due())
                    .chain(self.next_recovery_due())
                    .chain(self.upgrade_to.as_ref().map(|upgrading| upgrading.until))
                    .chain(self.keeping.due())
                    .chain(self.links.due())
                    .chain(self.relay_due())
                    .chain(self.next_lent_due())
                    .chain(self.next_restart_due())
                    .chain(self.next_restate_due())
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
            // What came while a flood was being read came before what epoll
            // has now.
            std::mem::swap(&mut waiting, &mut self.waiting);
            for event in waiting.iter().chain(&events) {
                self.handle(event);
            }
            waiting.clear();
            self.drive_quic();
            if web_unread {
                self.serve_web_unread();
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
                self.close_missing_boards();
            }
            self.ask_again();
            self.take_found();
            self.take_ports();
            self.restart_kept();
            self.tell_restated();
            self.explore();
            // After what the daemon's own threads wrote has been taken, so
            // it isn't taken for a change from outside.
            self.check_disk();
            self.keep_for_recovery();
            self.stop_idle_servers();
            self.let_gone_servers_go();
            self.let_gone_agents_go();
            self.push_stopping_servers();
            self.groups.push();
            self.tell_file_changes();
            self.compact_scrollbacks();
            self.time_out_clipboard_asks();
            self.lapse_fits();
            self.time_out_tools();
            self.take_lookups();
            self.time_out_forwards();
            self.take_link_news();
            self.time_out_relays();
            if self
                .next_lent_due()
                .is_some_and(|due| due <= Instant::now())
            {
                self.unlend_unseen();
            }
            self.tell_forward_reports();
            self.retry_links();
            self.relay_chats();
            self.take_browser();
            self.web_chores();
            self.send_clipboard_writes();
            self.send_browser_frames();
            self.send_reviews();
            self.keep_if_due();
            self.send_view_updates();
            self.transmit_quic();
            // Floods last, once this turn's updates have gone out, and
            // forwarded connections with them. Updates for what floods
            // showed go next turn, which doesn't wait for an event while
            // there are any; what else they sent over QUIC, such as a copy
            // to a remote window's clipboard, goes now, as do the bytes of
            // forwarded connections.
            let flooded = self.read_floods();
            if self.move_forwarded() || flooded {
                self.transmit_quic();
            }
        }
        self.stop();
        Ok(None)
    }

    /// Takes the signals that came asking the daemon to stop: it stops
    /// once the loop next goes round. Any that come once it's stopping
    /// change nothing.
    fn take_signals(&mut self) {
        for signal in crate::signals::take() {
            log::info!("{}", signal.meaning());
            self.shutdown.store(true, Ordering::Release);
        }
    }

    /// Stops everything, as the daemon stops, within [`STOPPING_WAIT`],
    /// waiting for it all at once. Language servers are asked to shut
    /// down, agents are stopped, and what they started with them, the
    /// boards' last changes are written, the browser is stopped, and
    /// remote windows are told why. Terminals are left to the daemon being
    /// dropped, which hangs up their programs.
    fn stop(&mut self) {
        log::info!("shutting down");
        let started = Instant::now();
        // As they are, before anything closes.
        self.keep_last();
        self.end_links();
        let servers_until = started + stopping::STOP_WAIT;
        let until = started + STOPPING_WAIT;
        // A client waiting for an upgrade hears why there won't be one.
        if let Some(upgrading) = self.upgrade_to.take() {
            self.refuse_upgrade(upgrading.token, "the daemon is stopping".to_owned());
        }
        // Asked first, to shut down while the rest stops. The boards'
        // last changes and the browser's moment to stop go by meanwhile.
        self.stop_servers();
        self.stop_agents(&[]);
        self.begin_stopping_boards();
        self.begin_stopping_recovery();
        self.begin_stopping_browser();
        self.abandon_renames();
        self.close_web("the daemon stopped");
        self.close_remotes("the daemon stopped");
        self.wait_for_stopping(servers_until);
        self.wait_for_file_ops(until);
        self.finish_stopping_boards(until);
        drop(self.finish_stopping_recovery(until));
        self.stop_browser();
        log::info!("stopped in {} ms", started.elapsed().as_millis());
    }

    /// The upgrade a client asked for, if it's to go ahead now: once file
    /// operations under way have finished, and the boards are written.
    /// Until then, or refused, the daemon carries on.
    fn upgrade_when_ready(&mut self) -> io::Result<Option<Upgrade>> {
        if self.upgrade_to.is_none() {
            return Ok(None);
        }
        // An operation that has just finished counts as finished.
        self.explore();
        let Some(upgrading) = self.upgrade_to.take() else {
            return Ok(None);
        };
        // Once nothing holds it up, which agents go on through the upgrade
        // is decided, before anything stops, and what the parser's thread
        // had of them taken in (see `chats/live.rs`): before the file
        // operations are waited for again, and the boards and the web app
        // made ready, as what's taken in may ask for any of them. Only on
        // the turn the upgrade goes ahead.
        let mut drained = None;
        let keep = loop {
            match self.file_ops_hold_upgrade(upgrading.until, Instant::now()) {
                Ok(false) => {}
                // Asked again when one finishes, or at the time.
                Ok(true) => {
                    self.upgrade_to = Some(upgrading);
                    return Ok(None);
                }
                Err(reason) => {
                    log::warn!("not upgrading: {reason}");
                    self.refuse_upgrade(upgrading.token, reason);
                    return Ok(None);
                }
            }
            match drained {
                Some(keep) => break keep,
                None => drained = Some(self.agents_to_keep(&upgrading.takes)),
            }
        };
        let Upgrading {
            executable,
            takes,
            token,
            ..
        } = upgrading;
        // Every change to the boards is on disk, and their lock let go of,
        // before there's no way back.
        if let Err(reason) = self.prepare_boards_for_upgrade(UPGRADE_BOARDS_WAIT) {
            log::warn!("not upgrading: {reason}");
            self.refuse_upgrade(token, reason);
            return Ok(None);
        }
        // So are the devices paired with the web app.
        if let Err(reason) = self.prepare_web_for_upgrade() {
            log::warn!("not upgrading: {reason}");
            // The boards were let go for it: taken back.
            self.take_boards_back();
            self.refuse_upgrade(token, reason);
            return Ok(None);
        }
        // There's no way back now. What can't wait for the next build is
        // answered for the agents that go on. Language servers are asked to
        // shut down, and the other agents stopped, to do so while remote
        // windows are told.
        self.ready_kept(&keep);
        let servers_until = Instant::now() + stopping::STOP_WAIT;
        self.stop_servers();
        self.stop_agents(&keep);
        // The client that asked hears how it went: its connection closes
        // as the new build starts, or says why it didn't.
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
        Ok(Some(Upgrade {
            executable,
            takes_signals: takes.signals,
            handover: self.hand_over_keeping(servers_until, &keep)?,
            requester,
        }))
    }

    /// Tells the client at `token`, which asked the daemon to upgrade,
    /// that it won't, and why.
    fn refuse_upgrade(&mut self, token: Token, reason: String) {
        let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
            return;
        };
        match frame::encode(client.wire, &ServerMessage::NotUpgrading { reason }) {
            Ok(bytes) => client.outgoing.extend_from_slice(&bytes),
            Err(err) => log::error!("encoding a refusal to upgrade failed: {err}"),
        }
        self.flush_client(token);
    }

    fn handle(&mut self, event: &Event) {
        match event.token() {
            LISTENER => self.accept_clients(),
            WAKER => {}
            SIGNALS => self.take_signals(),
            UDP => self.receive_udp(),
            WEB => self.accept_web(),
            token => match self.sources.get(token.0) {
                Some(Source::Output(id)) => {
                    let id = *id;
                    // A terminal already queued to be read is read at the end
                    // of the turn, until it's drained or queued again, which
                    // covers whatever this event says came.
                    if (event.is_readable() || event.is_read_closed() || event.is_error())
                        && !self.unread.contains(&Flood::Terminal(id))
                    {
                        self.read_output(id, Reading::Look);
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
                Some(&Source::Server(id, pipe)) => self.server_event(id, pipe),
                Some(Source::Index) => self.read_index_changes(),
                Some(Source::Files) => self.read_files_changes(),
                Some(Source::Client(_)) => self.serve_client(token),
                Some(Source::Carrier(_)) => self.serve_carrier(token, event),
                Some(Source::Tools(_)) => self.serve_tools(token, event),
                Some(Source::Web(_)) => self.serve_web(token),
                Some(Source::Forward(_)) => self.queue_forward(token),
                Some(Source::Reached(_)) => {
                    if event.is_writable() {
                        self.flush_reached(token);
                    }
                    // One already queued is read at the end of the turn.
                    if (event.is_readable() || event.is_read_closed() || event.is_error())
                        && !self.unread.contains(&Flood::Reached(token))
                    {
                        self.read_reached(token, Reading::Look);
                    }
                }
                None => {}
            },
        }
    }

    // --- Terminals --------------------------------------------------------

    /// Reads each terminal and peer queued with more waiting, in turn, for
    /// up to [`OUTPUT_BUDGET`] each. A read stops as soon as anything else
    /// becomes ready, and the rest wait for the next turn, so a flood only
    /// ever has the loop while nothing else wants it.
    ///
    /// Returns whether it read any.
    fn read_floods(&mut self) -> bool {
        let mut read = false;
        for _ in 0..self.unread.len() {
            if !self.waiting.is_empty() {
                break;
            }
            let Some(flood) = self.unread.pop_front() else {
                break;
            };
            match flood {
                Flood::Terminal(id) => self.read_output(id, Reading::Flood),
                Flood::Server(id) => self.read_server(id, Reading::Flood),
                Flood::Agent(id) => self.read_agent(id, Reading::Flood),
                Flood::Tools(token) => self.read_tools(token, Reading::Flood),
                Flood::Reached(token) => self.read_reached(token, Reading::Flood),
            }
            self.peers_flooded |= !matches!(flood, Flood::Terminal(_));
            read = true;
        }
        read
    }

    /// Queues `flood` to be read at the end of the turn, unless it is
    /// already.
    fn queue_flood(&mut self, flood: Flood) {
        if !self.unread.contains(&flood) {
            self.unread.push_back(flood);
        }
    }

    /// How long a flood reads, while it has the loop, before asking epoll
    /// whether anything else is ready. While keys have lately gone to a
    /// terminal other than `reading`, the one being read if it's a
    /// terminal's, it asks after every read, so their echo goes out as
    /// soon as it comes.
    fn yield_every(&self, reading: Option<TerminalId>, now: Instant) -> Duration {
        match self.typed {
            Some((typed, at)) if Some(typed) != reading && now.duration_since(at) < TYPING => {
                Duration::ZERO
            }
            _ => YIELD_EVERY,
        }
    }

    /// Reads what a peer sent as `reading` says, `read` at a time, until
    /// there's nothing more for now or the reading's budget is spent.
    ///
    /// `read` takes and reads [`FIRST_LOOK`] bytes of what the peer sent,
    /// or a little more, and takes in the messages they finish. It returns
    /// how many bytes that was, fewer than [`FIRST_LOOK`] once there was
    /// nothing more for now, or `None` once the peer is no longer read.
    /// Each call holds the loop for one such step, messages taken in
    /// included. A flood asks epoll between steps whether anything else
    /// is ready, as a terminal's does, and gives way if so. `own` is the
    /// token of what's read, whose readiness a flood lets go.
    ///
    /// Returns whether more may be waiting, for the caller to queue it.
    fn read_peer(
        &mut self,
        reading: Reading,
        own: Option<Token>,
        mut read: impl FnMut(&mut Self) -> Option<usize>,
    ) -> bool {
        let start = Instant::now();
        let every = self.yield_every(None, start);
        let mut next = start + every;
        let mut total = 0;
        loop {
            let Some(bytes) = read(self) else {
                return false;
            };
            if bytes < FIRST_LOOK {
                return false;
            }
            total += bytes;
            if reading == Reading::Look || total >= OUTPUT_BUDGET {
                return true;
            }
            if reading == Reading::Flood {
                let now = Instant::now();
                if now >= next {
                    next = now + every;
                    let (poll, waiting) = (&mut self.poll, &mut self.waiting);
                    if others_ready(poll, waiting, &self.sources, &self.unread, own) {
                        return true;
                    }
                }
            }
        }
    }

    /// Reads terminal `id`'s output as `reading` says, queueing it to be
    /// read again if there may be more.
    fn read_output(&mut self, id: TerminalId, reading: Reading) {
        let (budget, yielding) = match reading {
            Reading::Look => (FIRST_LOOK, false),
            Reading::Flood => (OUTPUT_BUDGET, true),
            Reading::Last => (OUTPUT_BUDGET, false),
        };
        let now = Instant::now();
        let every = self.yield_every(Some(id), now);
        let mut next = now + every;
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        let own = slot.output;
        let Some(terminal) = slot.terminal.pty() else {
            return;
        };
        let (poll, waiting, sources, unread) = (
            &mut self.poll,
            &mut self.waiting,
            &self.sources,
            &self.unread,
        );
        let stop = || {
            if !yielding {
                return false;
            }
            let now = Instant::now();
            if now < next {
                return false;
            }
            next = now + every;
            others_ready(poll, waiting, sources, unread, own)
        };
        let outcome = terminal.read_output(&mut self.buffer, budget, stop);
        match outcome {
            ReadOutcome::Drained => {}
            ReadOutcome::BudgetExhausted => self.queue_flood(Flood::Terminal(id)),
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
        // Output bigger than a first look, such as a program redrawing its
        // screen, is read on at the end of the turn and shown whole then,
        // not half now and half a frame later.
        if outcome == ReadOutcome::BudgetExhausted && reading == Reading::Look {
            return;
        }
        self.serve_clipboard(id);
        self.schedule_compaction(id);
        self.collect_damage(id);
        self.terminal_moved_on(id);
        self.background_moved_on(id);
        self.kept_moved_on(id);
        self.note_shell(id);
    }

    /// Hands the lines the terminal changed to each client following it.
    fn collect_damage(&mut self, id: TerminalId) {
        let Some(slot) = self.terminals.get_mut(&id) else {
            return;
        };
        // A chat an agent started shows in that chat's card.
        let started =
            (slot.terminal.chat_ref()).is_some_and(|chat| chat.relations().parent.is_some());
        let chat = slot.terminal.is_chat();
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
        if chat {
            self.glance(id);
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
                Link::Unix(_) | Link::Web(_) => None,
                Link::Quic(handle) => Some(handle),
            };
            if client.finished || client.backlogged() {
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
                if let Some(update) = subscription.update_from(&slot.terminal, client.wire)
                    && queue_update(subscription, client.wire, id, update, outgoing)
                {
                    ready.push(Token(key));
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
            // What the program printed last, maybe still queued behind a
            // first look, reaches the recording, and viewers ready for an
            // update, before they hear it ended.
            self.read_output(id, Reading::Last);
            self.send_view_updates();
            // A pane closes when its program exits, as in other terminals.
            // Those watching it hear first that its program ended, so a
            // window can tell that from the pane closed by someone else.
            let viewers =
                (self.terminals.get(&id)).map_or_else(Vec::new, |slot| slot.viewers.clone());
            for token in viewers {
                self.send_event(token, clackatoo_protocol::Event::Exited { terminal: id });
            }
            // A terminal kept running always gets a new shell in its place,
            // a moment later; a background tab's stays, its screen readable,
            // until the tab is dismissed.
            let again = self.kept_ended(id);
            self.background_ended(id);
            // So does a shell lent another daemon as a background tab.
            let lent_background = self.lent_background_of(id).is_some();
            if !again
                && !lent_background
                && self.workspaces.get().pane_showing(id).is_some()
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
        let display = self.display_env();
        let terminal = HostedTerminal::start(id, request, &self.config, display.as_deref())?;
        if let Some(display) = &self.display {
            display.terminal_made();
        }
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
                        fit: None,
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
        self.unread.retain(|&unread| unread != Flood::Terminal(id));
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
                self.forget_glance(id);
                self.agent_files.forget(id);
                self.chat_closed_in_review(id);
                if self.workspaces.remove_terminal(id) {
                    self.workspaces_changed();
                }
                log::info!("chat {id} closed");
                self.subagent_closed(id, chat.relations().parent, &chat.node().subagents);
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
                // In a pane, or a background tab's.
                if self.pane_of(id).is_some() {
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
            PaneTerminal::Agent(request) => self.new_chat(request, beside)?,
            PaneTerminal::File(file) => return self.open_content(file, beside),
            PaneTerminal::Files(open) => {
                return self.new_files(open, beside).map(Content::Files);
            }
            PaneTerminal::Board(open) => {
                return self.open_board(open, beside).map(Content::Board);
            }
            PaneTerminal::Review(new) => {
                return self.new_review(new, beside).map(Content::Review);
            }
            PaneTerminal::Browser(chat) => {
                return match self.chat_ref(chat) {
                    Some(_) => Ok(Content::Browser(chat)),
                    None => Err(Error::NotAChat(chat)),
                };
            }
            PaneTerminal::Blank(blank) => {
                return Ok(Content::Buffer(self.open_blank(&blank, beside)));
            }
            // Only another machine's daemon puts one of these, as a new tab
            // or beside a pane (see `place_from`).
            PaneTerminal::Lent { .. } => {
                return Err(Error::Other(
                    "a pane from another machine goes only beside a pane".into(),
                ));
            }
        };
        Ok(Content::Terminal(terminal))
    }

    /// The directory `pane` works in: for a terminal, the one the program
    /// in front is in; for a file, the one it was opened from; for a
    /// folder, the folder.
    fn working_directory(&self, pane: PaneId) -> Option<String> {
        match self.workspaces.check_pane(pane).ok()? {
            Content::Terminal(terminal) => {
                self.terminals.get(&terminal)?.terminal.working_directory()
            }
            Content::Buffer(buffer) => {
                let dir = self.buffers.get(&buffer)?.buffer.dir();
                Some(dir.display().to_string())
            }
            Content::Files(files) | Content::Folder(files) => {
                Some(self.files_root(files)?.display().to_string())
            }
            Content::Board(_) | Content::Other { .. } => None,
            Content::Review(review) => Some(self.review_root_of(review)?.display().to_string()),
            Content::Browser(chat) => Some(self.chat_ref(chat)?.working_directory().to_owned()),
        }
    }

    /// Closes `pane`, which shows the terminal `terminal`: ends its
    /// program, or its chat. But if `back`, a chat an agent started, whose
    /// chat is still open, only loses its pane: it goes on in the
    /// background, as a card in that chat again, as if it had never been
    /// shown.
    fn close_terminal_pane(
        &mut self,
        pane: PaneId,
        terminal: TerminalId,
        back: bool,
    ) -> Result<(), Error> {
        if back && self.send_subagent_back(pane, terminal) {
            return Ok(());
        }
        self.close_terminal(terminal)
    }

    /// Whether closing a pane for the client at `token` sends a
    /// subagent's chat back to the background rather than closing it:
    /// for a client of protocol 109 on, as to an older one it means
    /// closing it, and for the daemon itself.
    pub(super) fn sends_subagents_back(&self, token: Token) -> bool {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.wire >= wire::SUBAGENT_PROFILE,
            _ => true,
        }
    }

    /// Closes `panes`, as closing their tab or workspace does: their
    /// terminals, and their buffers that have no changes to save. A
    /// subagent's chat goes back to the background if `back`, as
    /// [`close_terminal_pane`](Self::close_terminal_pane) says.
    fn close_panes(&mut self, panes: &[Pane], back: bool) -> Result<Response, Error> {
        for pane in panes {
            match pane.content {
                Content::Terminal(terminal) => self.close_terminal_pane(pane.id, terminal, back)?,
                Content::Buffer(buffer) => self.close_buffer_pane(pane.id, buffer),
                Content::Files(files) | Content::Folder(files) => {
                    self.close_files_pane(pane.id, files);
                }
                Content::Board(_) | Content::Browser(_) | Content::Other { .. } => {
                    self.close_board_pane(pane.id);
                }
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
        self.workspaces.check_new_tab(workspace)?;
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
        self.workspaces.check_split_tab(tab, side)?;
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
        self.workspaces.check_split(pane, side)?;
        let content = self.pane_content(terminal, Some(pane))?;
        let new = self.workspaces.split(pane, side, content);
        self.workspaces_changed();
        Ok(Response::PaneCreated(new))
    }

    /// Tells every client watching the workspaces what they are now. The
    /// client whose request changed them hears before the request's answer,
    /// which is queued after this.
    fn workspaces_changed(&mut self) {
        self.workspaces.refresh_lent();
        self.workspaces.refresh_background();
        self.forget_backgrounds_gone();
        // A background tab brought back in place of a workspace's last tab
        // is as one brought back by hand.
        for tab in self.workspaces.take_promoted() {
            self.renew_ended(tab);
            self.workspaces.refresh_background();
        }
        // The ended background tabs of a workspace gone with its last tab.
        for terminal in self.workspaces.take_dropped() {
            if let Err(err) = self.close_terminal(terminal) {
                log::warn!("terminal {terminal}: closing it failed: {err}");
            }
        }
        self.workspaces.sweep_restored();
        self.workspaces.sweep_kept();
        self.refresh_kept();
        // A pane borrowed from another machine that has closed here is
        // closed there.
        let gone = self.workspaces.sweep_borrowed();
        if !gone.is_empty() {
            self.borrowed_closed(&gone);
        }
        // A pane of this machine's in another machine's layout that's gone
        // here goes there.
        self.held_changed();
        self.keep_soon();
        // A chat agents started may have come out of the background, or
        // gone back.
        self.note_subagents();
        self.chats_moved();
        self.tell_workspaces();
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
    #[allow(clippy::too_many_lines, reason = "one arm per message, each short")]
    fn serve_client(&mut self, token: Token) {
        let Some(messages) = self.receive_messages(token) else {
            return;
        };
        for message in messages {
            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            // A device being cut off, as when it's unpaired, has nothing
            // more done for it, even what it sent before it heard. (A
            // socket's client that hung up is still answered what it asked
            // before: it may be waiting for the answers.)
            if client.finished && matches!(client.link, Link::Web(_)) {
                break;
            }
            let greeted = client.greeted;
            client.asked |= !matches!(message, Some(ClientMessage::Hello { .. }));
            let reply = match message {
                // A request it couldn't read, from a newer client: answered
                // all the same, so the answers to the rest stay in order.
                None => ServerMessage::Response(Response::Error(Error::Unsupported)),
                Some(ClientMessage::Hello { protocol_version }) => {
                    let wire = wire::for_client(protocol_version);
                    match wire {
                        None => log::warn!(
                            "client speaks protocol {protocol_version}, older than any we talk \
                             to ({}); we speak {PROTOCOL_VERSION}",
                            wire::MIN_CLIENT
                        ),
                        Some(wire) if wire < PROTOCOL_VERSION => {
                            log::info!("client speaks protocol {protocol_version}: talking at it");
                        }
                        Some(_) => {}
                    }
                    client.greeted = wire.is_some();
                    client.finished |= wire.is_none();
                    // Refused, it's told this build's own, as daemons older
                    // than the window of versions always say.
                    client.wire = wire.unwrap_or(PROTOCOL_VERSION);
                    ServerMessage::Hello {
                        protocol_version: client.wire,
                    }
                }
                // A client made for a stream reaching this daemon from
                // another machine (see `Request::Reach`) may only greet it
                // and carry QUIC: it's not one of this machine's own,
                // whatever its socket.
                Some(ref message)
                    if client.carrier_only
                        && !matches!(
                            message,
                            ClientMessage::Hello { .. }
                                | ClientMessage::Request(Request::CarryQuic)
                        ) =>
                {
                    client.finished = true;
                    ServerMessage::Response(Response::Error(Error::Other(
                        "a stream reaching this daemon from another machine only carries QUIC"
                            .to_owned(),
                    )))
                }
                Some(ClientMessage::Upgrade {
                    protocol_version,
                    executable,
                }) if !greeted => {
                    client.finished = true;
                    let local = matches!(client.link, Link::Unix(_));
                    let executable = PathBuf::from(std::ffi::OsString::from_vec(executable));
                    match self.check_upgrade(local, protocol_version, &executable) {
                        Ok(takes) => {
                            // The connection closes as the daemon upgrades.
                            self.upgrade_to = Some(Upgrading {
                                executable,
                                takes,
                                token,
                                until: Instant::now() + UPGRADE_OPS_WAIT,
                            });
                            return;
                        }
                        Err(reason) => {
                            log::warn!("not upgrading: {reason}");
                            ServerMessage::NotUpgrading { reason }
                        }
                    }
                }
                Some(ClientMessage::Upgrade { .. }) => {
                    client.finished = true;
                    ServerMessage::NotUpgrading {
                        reason: "an upgrade must be asked for instead of Hello".to_owned(),
                    }
                }
                _ if !greeted => {
                    client.finished = true;
                    ServerMessage::Response(Response::Error(Error::HelloRequired))
                }
                Some(ClientMessage::FrameDone { terminal }) => {
                    if let Some(subscription) = client.views.get_mut(&terminal) {
                        subscription.awaiting_ack = false;
                        self.views_pending = true;
                    } else {
                        self.relayed_frame_done(token, terminal);
                    }
                    continue;
                }
                Some(ClientMessage::Present { here }) => {
                    self.present(token, here);
                    continue;
                }
                Some(ClientMessage::Request(request)) => match self.route_request(token, request) {
                    relay::Routed::Here(request) => {
                        ServerMessage::Response(self.respond(request, token))
                    }
                    relay::Routed::Answered(response) => ServerMessage::Response(response),
                    relay::Routed::Waiting => continue,
                },
                Some(
                    message @ (ClientMessage::Edit { .. }
                    | ClientMessage::Query { .. }
                    | ClientMessage::Rename { .. }
                    | ClientMessage::Find { .. }
                    | ClientMessage::StopFinding { .. }
                    | ClientMessage::Stage { .. }
                    | ClientMessage::Clipboard { .. }
                    | ClientMessage::Files { .. }
                    | ClientMessage::FileOp { .. }
                    | ClientMessage::Board { .. }
                    | ClientMessage::ListSessions { .. }
                    | ClientMessage::Colour { .. }
                    | ClientMessage::Place { .. }
                    | ClientMessage::Dismiss { .. }
                    | ClientMessage::Login { .. }
                    | ClientMessage::NativeGrant { .. }
                    | ClientMessage::NativeReply { .. }),
                ) => {
                    if let Some(message) = self.route_message(token, message) {
                        self.serve_unanswered(token, message);
                    }
                    continue;
                }
            };

            let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                return;
            };
            let mut answer = match frame::encode(client.wire, &reply) {
                Ok(bytes) => bytes,
                Err(err) => {
                    log::error!("encoding a response failed: {err}");
                    client.finished = true;
                    Vec::new()
                }
            };
            answer.append(&mut client.later);
            // Behind an answer still to come from another machine.
            if client.held.is_empty() {
                client.outgoing.extend_from_slice(&answer);
            } else {
                client.held.push_back(Held::Ready(answer));
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
            ClientMessage::NativeGrant { chat, access } => self.native_grant(token, chat, access),
            ClientMessage::NativeReply { id, result } => self.native_reply(token, id, result),
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
                if self.relayed_terminal(terminal).is_some() {
                    self.relayed_clipboard(token, terminal, id, text);
                } else {
                    self.clipboard_answered(token, terminal, id, text.as_deref());
                }
            }
            ClientMessage::Files { files, action } => self.serve_files(token, files, action),
            ClientMessage::FileOp { id, op, pane: _ } => self.serve_file_op(token, id, op),
            ClientMessage::Board { board, action } => self.serve_board(token, board, action),
            ClientMessage::ListSessions {
                id,
                agent,
                cwd,
                on: _,
            } => {
                self.list_sessions(token, id, &agent, &cwd);
            }
            ClientMessage::Colour { id, code } => self.colour_code(token, id, code),
            ClientMessage::Place { pane, place } => self.serve_place(pane, place),
            ClientMessage::Dismiss { pane } => self.serve_dismiss(pane),
            ClientMessage::Login { ask, answer } => {
                self.serve_login(token, ask, answer.map(|answer| answer.0));
            }
            other => log::error!("{other:?} is answered"),
        }
    }

    /// Why the daemon won't become `executable`, if it won't, as a client
    /// speaking `protocol_version` asked, on this machine if `local`. If it
    /// will, what `executable` takes beside the handover (see
    /// [`crate::upgrade::Takes`]).
    fn check_upgrade(
        &self,
        local: bool,
        protocol_version: u32,
        executable: &Path,
    ) -> Result<crate::upgrade::Takes, String> {
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

    /// Reads and decodes everything a client has sent: `None` in place of
    /// each request it couldn't read, as from a newer client. Returns
    /// `None` if the client was dropped.
    fn receive_messages(&mut self, token: Token) -> Option<Vec<Option<ClientMessage>>> {
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
            Link::Web(link) => {
                let received = link.receive(&mut self.buffer, &mut client.decoder, Instant::now());
                if received.drop {
                    self.drop_client(token);
                    return None;
                }
                client.finished |= received.finished;
                // The rest is read next turn, so no device has the loop to
                // itself.
                if received.more && !self.web_unread.contains(&token) {
                    self.web_unread.push(token);
                }
            }
        }

        let mut messages = Vec::new();
        let mut greeted = client.greeted;
        loop {
            match client.decoder.decode::<ClientMessage>(client.wire) {
                Ok(Some(message)) => {
                    // What comes after a hello is read at the version it
                    // agrees, as it will be answered.
                    if let ClientMessage::Hello { protocol_version } = message
                        && let Some(wire) = wire::for_client(protocol_version)
                    {
                        client.wire = wire;
                        greeted = true;
                    }
                    messages.push(Some(message));
                }
                Ok(None) => break,
                // A message from a newer client, which knows what this build
                // doesn't: one that's greeted it, and so knows it may be
                // older, and that skips what it can't read too. It's a
                // request, which is answered, or a kind this build hasn't;
                // one of a kind it has is broken, as are a stream of them.
                Err(err)
                    if err.skipped().is_some_and(|head| {
                        head.is_request() || !head.is_known_client_message()
                    }) && greeted
                        && client.wire >= wire::FALLBACKS
                        && client.unreadable.one_more(Instant::now()) =>
                {
                    if std::mem::replace(&mut client.unreadable.any, true) {
                        log::debug!("a client sent a message this build can't read: {err}");
                    } else {
                        log::warn!("a client sent a message this build can't read: {err}");
                    }
                    if err.skipped().is_some_and(|head| head.is_request()) {
                        messages.push(None);
                    }
                }
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
    fn terminal_infos(&self, token: Token) -> Vec<TerminalInfo> {
        (self.terminals.iter())
            .filter(|&(&id, _)| self.may_see_terminal(token, id))
            .map(|(_, slot)| slot.terminal.info())
            .collect()
    }

    /// Answers one request from the client at `token`.
    #[allow(clippy::too_many_lines, reason = "one arm per request, each short")]
    fn respond(&mut self, request: Request, token: Token) -> Response {
        // What's lent to another machine's daemon is that daemon's alone.
        if let Err(err) = self.refuse_lent(token, &request) {
            return Response::Error(err);
        }
        // What's parked here while the machine it's lent to is away.
        if let Some(answer) = self.parked_request(token, &request) {
            return answer.unwrap_or_else(Response::Error);
        }
        // A window from before background tabs never saw them, so closing
        // the workspace would end what it didn't know was there.
        if let Request::CloseWorkspace { workspace } = request
            && self
                .client_wire(token)
                .is_some_and(|at| at < wire::BACKGROUND)
            && let Some(found) = self.workspaces.get().get(workspace)
            && !found.background.is_empty()
        {
            return Response::Error(Error::Other(format!(
                "‘{}’ has background tabs: close it from a newer window",
                found.name
            )));
        }
        let response = match request {
            Request::HarnessCapabilities { terminal } => {
                if self.may_see_terminal(token, terminal) && self.chat_ref(terminal).is_some() {
                    Ok(Response::Text(self.harness_capabilities(terminal)))
                } else {
                    Err(Error::NotAChat(terminal))
                }
            }
            Request::CancelActivity { terminal, id } => {
                if self.may_see_terminal(token, terminal) {
                    self.cancel_activity(terminal, &id)
                        .map(|_| Response::Done)
                        .map_err(Error::Other)
                } else {
                    Err(Error::NotAChat(terminal))
                }
            }
            Request::Version => Ok(Response::Text(env!("CARGO_PKG_VERSION").to_owned())),
            Request::ListTerminals => Ok(Response::Terminals(self.terminal_infos(token))),
            Request::NewTerminal(request) => {
                self.new_terminal(&request).map(Response::TerminalCreated)
            }
            Request::SendInput { terminal, bytes } => {
                self.note_typed(terminal);
                self.send_input(token, terminal, &bytes)
                    .map(|()| Response::Done)
            }
            Request::Capture { terminal } => self
                .terminals
                .get(&terminal)
                .map(|slot| Response::View(slot.terminal.view()))
                .ok_or(Error::NoSuchTerminal(terminal)),
            Request::CloseTerminal { terminal } => {
                self.close_terminal(terminal).map(|()| Response::Done)
            }
            Request::Resize { terminal, size } => {
                self.resize_asked(terminal, size).map(|()| Response::Done)
            }
            Request::Attach { terminal } => self.attach(token, terminal).map(|()| Response::Done),
            Request::AttachRecent { terminal, first } => (|| {
                self.attach(token, terminal)?;
                if let Some(Source::Client(client)) = self.sources.get_mut(token.0)
                    && let Some(sub) = client.views.get_mut(&terminal)
                {
                    sub.chat.recent = true;
                    sub.chat.first = first;
                }
                Ok(Response::Done)
            })(),
            Request::ChatHistory { terminal, before } => (|| {
                let slot = self
                    .terminals
                    .get(&terminal)
                    .ok_or(Error::NoSuchTerminal(terminal))?;
                let Hosted::Chat(chat) = &slot.terminal else {
                    return Err(Error::Other("Not a chat".into()));
                };
                if before == 0 || before as usize > chat.node().turns.len() {
                    return Err(Error::Other("History boundary is outside this chat".into()));
                }
                let Some(Source::Client(client)) = self.sources.get_mut(token.0) else {
                    return Err(Error::NoSuchTerminal(terminal));
                };
                let sub = client
                    .views
                    .get_mut(&terminal)
                    .ok_or(Error::NoSuchTerminal(terminal))?;
                if !sub.chat.older.contains(&before) {
                    if sub.chat.older.len() >= 64 {
                        return Err(Error::Other("Too many history pages pending".into()));
                    }
                    sub.chat.older.push_back(before);
                    self.views_pending = true;
                }
                Ok(Response::Done)
            })(),
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
            Request::BlockText { terminal, from, to } => self
                .grid(terminal)
                .map(|grid| Response::Text(grid.block_text(from, to))),
            Request::Search {
                terminal,
                query,
                from,
                backward,
                exact,
                count,
            } => self.grid(terminal).map(|grid| match count {
                None => Response::Found(grid.search(&query, from, backward, exact)),
                Some(count) => {
                    let count = usize::try_from(count).unwrap_or(usize::MAX);
                    let found = grid.search_nth(&query, from, backward, exact, count);
                    Response::Searched {
                        found: found.map(|(found, _)| found),
                        wrapped: found.is_some_and(|(_, wrapped)| wrapped),
                    }
                }
            }),
            Request::Block {
                terminal,
                line,
                which,
            } => self
                .grid(terminal)
                .map(|grid| Response::Block(grid.block(line, which))),
            Request::ChatOriginal { terminal, original } => self
                .chat(terminal)
                .and_then(|chat| chat.originals.get(original))
                .map(Response::Text)
                .ok_or_else(|| Error::Other("Original image is no longer available".into())),
            Request::Chat { terminal, action } => {
                self.note_device_action(token, terminal, &action);
                // Answered with the message's text, for the box asking.
                if let ChatAction::TakeBack { queued } = action {
                    self.take_back_queued(terminal, queued).map(Response::Text)
                } else {
                    self.chat_action(terminal, action).map(|()| Response::Done)
                }
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
            | Request::BlankHere { .. }
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
            Request::SaveAs {
                buffer,
                path,
                force,
            } => self.save_as(token, buffer, &path, force),
            Request::AttachFiles { files } => self.attach_files(token, files),
            Request::DetachFiles { files } => Ok(self.detach_files(token, files)),
            Request::AttachBoard { board } => self.attach_board(token, board),
            Request::DetachBoard { board } => Ok(self.detach_board(token, board)),
            Request::Discard { buffer } => self.discard_buffer(buffer),
            Request::Reload { buffer } => self.reload_buffer(buffer),
            Request::Recover { pane, drop } => self.recover_here(pane, drop),
            Request::AttachReview { review } => self.attach_review(token, review),
            Request::DetachReview { review } => Ok(self.detach_review(token, review)),
            Request::AttachBrowser {
                chat,
                width,
                height,
            } => Ok(self.attach_browser(token, chat, [width, height])),
            Request::DetachBrowser { chat } => Ok(self.detach_browser(token, chat)),
            Request::Review(action) => self.serve_review(action),
            Request::WatchChats => Ok(self.watch_chats(token)),
            Request::ChatChoices { pane } => Ok(Response::ChatChoices(self.chat_choices(pane))),
            Request::Fit {
                terminal,
                size,
                renew,
            } => self
                .fit(token, terminal, size, renew)
                .map(|()| Response::Done),
            Request::Devices => Ok(self.devices()),
            Request::RevokeDevice { device } => self.revoke_device(device),
            Request::AnswerPairing { code } => self.answer_pairing(token, code.as_deref()),
            Request::UnlockPairing => self.unlock_pairing(token),
            Request::Connect {
                host,
                command,
                share,
            } => self.serve_connect(token, &host, command, share),
            Request::Disconnect { host } => self.serve_disconnect(token, &host),
            Request::WatchConnections { answer } => self.watch_connections(token, answer),
            Request::Forget { host } => self.serve_forget(token, &host),
            Request::Forward(_) => Err(Error::Forward(clackatoo_protocol::ForwardFailure::Other(
                "a forwarded connection asks on a QUIC stream of its own".into(),
            ))),
            Request::Reach { .. } => Err(Error::Other(
                "a daemon is reached on a QUIC stream of its own, beside the one it talks on"
                    .into(),
            )),
            request @ (Request::Install { .. } | Request::Decline { .. }) => {
                self.respond_about_installs(token, request);
                Ok(Response::Done)
            }
            Request::Relay => self.relaying(token),
            Request::RelayAs { machine } => self.relaying_as(token, machine),
            Request::Unlend {
                workspace,
                close_idle,
            } => self.serve_unlend(token, workspace, close_idle),
            Request::Lent => Ok(self.list_lent()),
            Request::Background(action) => self.serve_background(token, &action),
            Request::Keep { pane, how } => self.serve_keep(token, pane, how),
            Request::Lend {
                key,
                beside,
                terminal,
                command,
            } => self.lend(token, key, beside, terminal, command.as_deref()),
            Request::LentBackground { key, action } => {
                self.serve_lent_background(token, key, action)
            }
            // One for a pane on another machine is the relay's.
            Request::Forwards { .. } => Err(Error::Other(
                "this daemon's own ports are this machine's: there's nothing to forward".into(),
            )),
        };
        response.unwrap_or_else(Response::Error)
    }

    /// Answers a request about workspaces from the client at `token`.
    /// Nothing is closed, or shown in place of what a pane shows, that
    /// would lose a buffer with no file yet that has changes, whoever asks:
    /// a window that saw it, one that didn't, or one too old to know of it.
    #[allow(clippy::too_many_lines, reason = "one arm per request, each short")]
    fn respond_about_workspaces(
        &mut self,
        request: Request,
        token: Token,
    ) -> Result<Response, Error> {
        // A pane another machine's daemon puts here, of its own.
        if let Some(placed) = self.place_from(token, &request) {
            return placed;
        }
        let ids = |panes: Vec<Pane>| panes.iter().map(|pane| pane.id).collect();
        let closing: Vec<PaneId> = match &request {
            Request::ClosePane { pane }
            | Request::OpenHere { pane, .. }
            | Request::BlankHere { pane, .. } => vec![*pane],
            Request::CloseTab { tab } => ids(self.workspaces.tab_panes(*tab)?),
            Request::CloseWorkspace { workspace } => {
                ids(self.workspaces.workspace_panes(*workspace)?)
            }
            _ => Vec::new(),
        };
        if self.loses_blank(&closing) {
            // A workspace lent to the daemon asking, which no longer shows
            // it, is given back rather than kept hidden.
            if let Request::CloseWorkspace { workspace } = request
                && self.unlend_rather_than_lose(token, workspace)
            {
                return Ok(Response::Done);
            }
            let wire = match self.sources.get(token.0) {
                Some(Source::Client(client)) => client.wire,
                _ => PROTOCOL_VERSION,
            };
            return Err(buffers::blank_kept(wire));
        }
        match request {
            Request::WatchWorkspaces => {
                if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
                    client.watching_workspaces = true;
                }
                // A window opening while devices ask hears of them.
                self.tell_of_asks(token);
                let mut shown = self.shown_workspaces();
                if self.relays(token)
                    && let Some(without) = self.without_parked(&shown)
                {
                    shown = without;
                }
                Ok(Response::Workspaces(
                    self.with_lent(token, &shown).unwrap_or(shown),
                ))
            }
            // Made on another machine, it was the relay's: the machine
            // asked for is this one.
            Request::NewWorkspace { name, terminal, .. } => {
                self.new_workspace(name.as_deref(), terminal)
            }
            Request::NewTab {
                workspace,
                terminal,
                ..
            } => self.new_tab(workspace, terminal),
            Request::Split {
                pane,
                side,
                terminal,
                ..
            } => self.split(pane, side, terminal),
            Request::SplitTab {
                tab,
                side,
                terminal,
                share,
                ..
            } => self.split_tab(tab, side, terminal, share),
            Request::ClosePane { pane } => match self.workspaces.check_pane(pane)? {
                Content::Terminal(terminal) => {
                    let back = self.sends_subagents_back(token);
                    (self.close_terminal_pane(pane, terminal, back)).map(|()| Response::Done)
                }
                Content::Buffer(buffer) => {
                    self.close_buffer_pane(pane, buffer);
                    Ok(Response::Done)
                }
                Content::Files(files) | Content::Folder(files) => {
                    self.close_files_pane(pane, files);
                    Ok(Response::Done)
                }
                Content::Board(_) | Content::Browser(_) | Content::Other { .. } => {
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
            Request::BlankHere { pane, blank } => self.blank_here(pane, &blank),
            Request::CloseTab { tab } => {
                let panes = self.workspaces.tab_panes(tab)?;
                self.close_panes(&panes, self.sends_subagents_back(token))
            }
            Request::CloseWorkspace { workspace } => {
                // Its background tabs first: none takes the place of its
                // last tab as that closes.
                let mut panes = self.workspaces.background_panes(workspace)?;
                panes.extend(self.workspaces.workspace_panes(workspace)?);
                self.close_panes(&panes, self.sends_subagents_back(token))
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
        let chat = slot.terminal.is_chat();
        if let Some(Source::Client(client)) = self.sources.get_mut(token.0) {
            client.views.insert(id, Subscription::new());
        }
        self.views_pending = true;
        if chat {
            self.attached_to_chat(token, id);
            self.show_restored_chat(id);
        }
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
        self.forget_fits(token, Some(id));
    }

    /// Writes `bytes` to terminal `id`, as typed by the client at `token`.
    fn send_input(&mut self, token: Token, id: TerminalId, bytes: &[u8]) -> Result<(), Error> {
        let slot = self.terminals.get(&id).ok_or(Error::NoSuchTerminal(id))?;
        if slot.terminal.is_chat() {
            return Err(Error::IsAChat(id));
        }
        self.typing_ends_fit(token, id);
        let Some(slot) = self.terminals.get_mut(&id) else {
            return Err(Error::NoSuchTerminal(id));
        };
        if let Some(terminal) = slot.terminal.pty() {
            terminal.send_input(bytes)?;
            slot.typist = Some(token);
            self.typed_into(id, bytes);
            self.typed = Some((id, Instant::now()));
            self.update_pty_interest(id);
            self.note_device_typing(token, id);
        }
        Ok(())
    }

    /// The emulator of terminal `id`: its screen and history.
    fn grid(&self, id: TerminalId) -> Result<&Terminal, Error> {
        let slot = self.terminals.get(&id).ok_or(Error::NoSuchTerminal(id))?;
        slot.terminal.grid().ok_or(Error::IsAChat(id))
    }

    /// Takes the message `queued` out of the chat `id`'s queue and into its
    /// draft, returning the message's text (see `HostedChat::take_back`).
    fn take_back_queued(&mut self, id: TerminalId, queued: u64) -> Result<String, Error> {
        let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
        let made = chat.take_back(queued).map_err(Error::Other)?;
        self.collect_damage(id);
        Ok(made)
    }

    /// Does what a client asked of the chat `id`. A message sent from the
    /// box, once taken, empties the draft, and tells viewers what was sent,
    /// for every box holding just that to empty too.
    fn chat_action(&mut self, id: TerminalId, action: ChatAction) -> Result<(), Error> {
        let sent = match &action {
            ChatAction::Send(text)
            | ChatAction::SendImages { text, .. }
            | ChatAction::Submit { text, .. }
            | ChatAction::SendTo { text, .. } => Some(text.clone()),
            _ => None,
        };
        self.chat_act(id, action)?;
        if let Some(sent) = sent
            && let Some(chat) = self.chat(id)
        {
            chat.draft_sent(sent);
            self.collect_damage(id);
        }
        Ok(())
    }

    /// Does what a client asked of the chat `id`, as [`Self::chat_action`].
    #[allow(clippy::too_many_lines, reason = "one arm for each action")]
    fn chat_act(&mut self, id: TerminalId, action: ChatAction) -> Result<(), Error> {
        let slot = self
            .terminals
            .get_mut(&id)
            .ok_or(Error::NoSuchTerminal(id))?;
        let chat = slot.terminal.chat().ok_or(Error::NotAChat(id))?;
        // Rewinding and switching branches are about files and sessions,
        // so the daemon's.
        match action {
            ChatAction::Rewind {
                at,
                files,
                edit,
                stop_tasks,
            } => return self.rewind_chat(id, at as usize, files, edit, stop_tasks),
            ChatAction::SwitchBranch {
                at,
                to,
                files,
                stop_tasks,
            } => return self.switch_chat_branch(id, at as usize, to, files, stop_tasks),
            _ => {}
        }
        // Undoing is about files, so the daemon's, not the agent's.
        if let ChatAction::Undo { turn, force } | ChatAction::Redo { turn, force } = action {
            let redo = matches!(action, ChatAction::Redo { .. });
            let turn = usize::try_from(turn).unwrap_or(usize::MAX);
            self.undo_turn(id, turn, redo, force);
            return Ok(());
        }
        // A message sent while the agent answers waits its turn in the
        // queue, as the window's does, rather than going nowhere.
        let busy = (chat.answering() && !chat.loading()) || chat.node().delivery.sending.is_some();
        // Other agents are the server's.
        match action {
            ChatAction::Send(text) if busy => {
                return self.submit_chat(id, text, Vec::new(), None, false);
            }
            ChatAction::SendImages { text, images } if busy => {
                return self.submit_chat(id, text, images, None, false);
            }
            ChatAction::RefreshUsage { force } => {
                self.refresh_usage(force);
                return Ok(());
            }
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
            ChatAction::Submit {
                text,
                images,
                target,
                steer,
            } => {
                return self.submit_chat(id, text, images, target, steer);
            }
            // What's typed in the box: the clients', which the agent never
            // hears of, nor do viewers.
            ChatAction::Draft { text } => {
                chat.set_draft(text);
                return Ok(());
            }
            ChatAction::TakeBack { queued } => {
                return self.take_back_queued(id, queued).map(drop);
            }
            ChatAction::RemoveQueued { id: message } => {
                chat.take_queued_user(message).map_err(Error::Other)?;
                self.collect_damage(id);
                return Ok(());
            }
            ChatAction::SendQueued { id: message, steer } => {
                return self.send_queued(id, message, steer);
            }
            ChatAction::SendTo {
                agent,
                model,
                budget,
                text,
                images,
            } => {
                // Validate before touching the old session. Another client may
                // have started a turn since this window last saw the chat.
                if chat.answering() {
                    return Err(Error::Other(
                        "The agent is still answering; send the handoff once it finishes.".into(),
                    ));
                }
                if text.trim().is_empty() && images.is_empty() {
                    return Err(Error::Other("A handoff needs a message or image.".into()));
                }
                if images.len() > clackatoo_protocol::ChatImage::MAX_COUNT
                    || images.iter().any(|image| !image.valid())
                    || images.iter().map(|image| image.data.len()).sum::<usize>()
                        > clackatoo_protocol::ChatImage::MAX_DATA
                {
                    return Err(Error::Other("Images are too large or invalid.".into()));
                }
                self.config.agents.find(&agent).map_err(Error::Other)?;
                self.switch_chat(
                    id,
                    &agent,
                    model,
                    usize::try_from(budget).unwrap_or(usize::MAX),
                );
                let chat = self.chat(id).ok_or(Error::NotAChat(id))?;
                if chat.agent() != agent {
                    return Err(Error::Other("The provider handoff was refused.".into()));
                }
                let actions = chat.act(ChatAction::SendImages { text, images });
                self.act(id, actions);
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
            Link::Web(link) => link.send(self.poll.registry(), token, &mut client.outgoing),
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
        let remote = matches!(client.link, Link::Quic(_) | Link::Web(_));
        // A device's frames waiting count too, pongs among them.
        let unsent = client.outgoing.len()
            + match &client.link {
                Link::Web(link) => link.unsent(),
                _ => 0,
            };
        let limit = if matches!(client.link, Link::Web(_)) {
            MOST_WEB_UNSENT
        } else {
            MOST_UNSENT
        };
        if remote && unsent > limit {
            log::warn!("dropping a client on another machine {unsent} B behind");
            self.drop_client_saying(token, "the window fell too far behind");
            return;
        }
        let flushed = match &client.link {
            Link::Web(link) => client.outgoing.is_empty() && !link.backlogged(),
            _ => client.outgoing.is_empty(),
        };
        // Writes to the clipboard, and pictures of pages, wait for a
        // client to catch up.
        if flushed && client.clipboard.any() {
            self.clipboard_unsent = true;
        }
        if flushed && !client.browser_frames.is_empty() {
            self.browsing.frames_unsent = true;
        }
        // Over QUIC, closing the connection discards what's still to send,
        // so a client dropped for breaking the protocol may not hear why.
        if flushed && client.finished {
            self.drop_client(token);
            return;
        }
        // View updates wait for a client to catch up: on the socket, with
        // everything; over QUIC, with each terminal's stream, which may
        // have drained even if answers are still waiting; over a
        // WebSocket, with its frames, which may have drained though none
        // of the answers were just taken.
        let drained = written > 0 || (matches!(client.link, Link::Web(_)) && flushed);
        if drained && !client.views.is_empty() {
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
        // Before it's forgotten, so the log can say which device it was.
        self.forget_fits(token, None);
        if let Some(Source::Client(client)) = self.sources.try_remove(token.0) {
            // An upgrade it asked for, still waiting, is given up: no one
            // is left to hear how it went, and its token may be another
            // client's next.
            if self
                .upgrade_to
                .take_if(|upgrading| upgrading.token == token)
                .is_some()
            {
                log::info!("not upgrading: the client that asked has gone");
            }
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
            self.forget_browser_viewer(token);
            self.forget_stages(token, None);
            self.forget_piece_questions(token, None);
            self.forget_listings(token);
            self.forget_link_client(token);
            self.forget_relay_client(token);
            // What's lent to its machine may be parked now.
            if client.machine.is_some() {
                self.held_reach_changed();
            }
            if let Link::Quic(handle) = client.link {
                self.abandon_forwards(Some(handle));
                if let Some(quic) = self.quic.as_mut() {
                    quic.close(handle, Instant::now(), why);
                }
            }
            if let Some(web) = self.web.as_mut() {
                web.conns.retain(|&conn| conn != token);
            }
        }
    }

    /// Takes the client at `token` off the viewers of the terminals it
    /// follows.
    fn forget_viewer(&mut self, token: Token, client: &Client) {
        self.native_disconnected(token);
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
        let signer_as = client.carrier_as.unwrap_or(self.machine.id);
        let quic = self.quic.as_ref();
        let udp = self.udp.as_ref().and_then(|udp| udp.local_addr().ok());
        let certificate = quic
            .map(|quic| quic.certificate().to_vec())
            .unwrap_or_default();
        // Signed, so a client reaching it through another machine, which
        // could put a certificate of its own in its place, can tell.
        let signed = clackatoo_protocol::Signed {
            machine: signer_as,
            key: self.signer.key(),
            signature: self.signer.sign(signer_as, &certificate),
        };
        Ok(Response::Carrying {
            certificate,
            udp: udp.zip(quic).map(|(address, quic)| Udp {
                port: address.port(),
                probe_key: quic.probe_key(),
            }),
            signed: Some(signed),
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
                    QuicEvent::Forwarded(handle, stream) => self.forwarded(handle, stream),
                    QuicEvent::ForwardReady(handle, stream) => self.forward_ready(handle, stream),
                    QuicEvent::ForwardEnded(handle, stream, acked) => {
                        self.forward_ended(handle, stream, acked);
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
    /// what says so: the daemon is about to stop or become another build,
    /// and nobody would answer after. Without it, a client on UDP would go
    /// on sending to a connection nobody knows, out of contact, until its
    /// idle timeout.
    ///
    /// It waits, up to [`CLOSING_WAIT`], for each packet saying so to go,
    /// and for the carriers to take them. QUIC doesn't always send one at
    /// once: a connection just moved onto UDP may send only three times
    /// what has come from there until the client answers the check of its
    /// new path, and the close waits for the client to be heard from.
    /// Meanwhile, only what comes over UDP and the carriers is taken.
    /// Everything else waits for the next build, which hears of it anew, or
    /// is dropped with the daemon.
    fn close_remotes(&mut self, reason: &str) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        let started = Instant::now();
        quic.close_all(started, reason);
        // Their sockets are reset rather than left to close as the
        // daemon's do.
        self.abandon_forwards(None);
        let deadline = started + CLOSING_WAIT;
        let mut events = Events::with_capacity(64);
        loop {
            self.transmit_quic();
            let carrying = (self.sources.iter())
                .any(|(_, source)| matches!(source, Source::Carrier(c) if !c.outgoing.is_empty()));
            let Some(quic) = self.quic.as_mut() else {
                return;
            };
            if !carrying && !quic.closes_waiting() {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                log::info!("stopping without some QUIC connections' clients told why");
                return;
            }
            let due = quic
                .next_timeout()
                .map_or(deadline, |due| due.min(deadline));
            match self
                .poll
                .poll(&mut events, Some(due.saturating_duration_since(now)))
            {
                Ok(()) => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    log::warn!("waiting for QUIC connections to close failed: {err}");
                    return;
                }
            }
            for event in &events {
                match event.token() {
                    UDP => self.receive_udp(),
                    token if matches!(self.sources.get(token.0), Some(Source::Carrier(_))) => {
                        self.serve_carrier(token, event);
                    }
                    // Language servers stopping meanwhile, as the daemon
                    // stops, go on their way, and agents' processes are
                    // reaped: their events come only once.
                    token => match self.sources.get(token.0) {
                        Some(&Source::Server(id, pipe)) => self.server_event(id, pipe),
                        Some(Source::Orphan(_)) => self.reap_orphan(token),
                        _ => {}
                    },
                }
            }
            if let Some(quic) = self.quic.as_mut() {
                quic.handle_timeouts(Instant::now());
            }
        }
    }

    /// Sends the packets the QUIC endpoint has ready, each on its carrier.
    fn transmit_quic(&mut self) {
        let Some(quic) = self.quic.as_mut() else {
            return;
        };
        let mut carriers = Vec::new();
        let registry = self.poll.registry();
        // Carriers backed up already get no packets made for them.
        let full: Vec<usize> = (self.sources.iter())
            .filter(|(_, source)| {
                matches!(source, Source::Carrier(carrier)
                    if carrier.outgoing.len() >= quic::CARRIER_BACKLOG)
            })
            .map(|(key, _)| key)
            .collect();
        let backed_up =
            |address| quic::carrier_id(address).is_some_and(|id| full.contains(&id.key));
        quic.transmit(Instant::now(), backed_up, |address, packet| {
            let Some(CarrierId { key, generation }) = quic::carrier_id(address) else {
                // A packet the socket has no room for is as good as lost,
                // and QUIC sends it again.
                if let Some(udp) = &self.udp
                    && let Err(err) = udp.send_to(packet, address)
                    && err.kind() != io::ErrorKind::WouldBlock
                {
                    log::debug!("UDP send to {address} failed: {err}");
                }
                return true;
            };
            if let Some(Source::Carrier(carrier)) = self.sources.get_mut(key)
                && carrier.generation == generation
            {
                quic::frame_packet(packet, &mut carrier.outgoing);
                if !carriers.contains(&key) {
                    carriers.push(key);
                }
                // Backed up: written now, and if its socket won't take it,
                // the rest waits in QUIC, in order of priority. A failure
                // is found by the flush after.
                if carrier.outgoing.len() >= quic::CARRIER_BACKLOG {
                    let _ = write_out(
                        registry,
                        &mut carrier.stream,
                        Token(key),
                        &mut carrier.outgoing,
                        &mut carrier.wants_writable,
                    );
                    return carrier.outgoing.len() < quic::CARRIER_BACKLOG;
                }
            }
            true
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
    let first = first.unwrap_or(end);
    let count = u16::try_from(lines.len()).expect("no more lines than were asked for");
    HistoryLines {
        first,
        links: LinkTarget::of(lines.iter().map(|line| &line.line), terminal),
        lines,
        marks: BlockMarks::of_history(terminal, first, count),
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_term::Config;

    use super::*;

    /// A web reader with a real, nonblocking socket, so large frames remain
    /// queued until the test lets the reader catch up.
    fn web_reader(daemon: &mut Daemon) -> (Token, std::os::unix::net::UnixStream) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        theirs.set_nonblocking(true).unwrap();
        let mut stream = mio::net::UnixStream::from_std(ours);
        let token = Token(daemon.sources.vacant_key());
        daemon
            .poll
            .registry()
            .register(&mut stream, token, Interest::READABLE)
            .unwrap();
        let now = Instant::now();
        let link = crate::web::HttpConn::new(
            crate::web::Transport::Plain(crate::web::Stream::Unix(stream)),
            crate::web::Peer::Local,
            now,
            Duration::from_secs(10),
            true,
        )
        .into_link(1, now);
        daemon
            .sources
            .insert(Source::Client(Client::new(Link::Web(Box::new(link)))));
        (token, theirs)
    }

    #[test]
    fn a_web_reader_can_drain_a_large_chat_snapshot() {
        use std::io::Read as _;
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, id) = agent_files::tests::daemon_with_chat(dir.path());
        let (token, mut reader) = web_reader(&mut daemon);
        let mut node = daemon.chat_ref(id).unwrap().node().clone();
        node.turns[0].items.push(clackatoo_protocol::Item::Words(
            "x".repeat(frame::MAX_FRAME_LEN - 4096),
        ));
        let snapshot = ServerMessage::Event(clackatoo_protocol::Event::View {
            terminal: id,
            update: ViewUpdate::Snapshot(View {
                title: "Large conversation".into(),
                cwd: None,
                root: clackatoo_protocol::Node::Chat(Box::new(node)),
            }),
        });
        let encoded = frame::encode(PROTOCOL_VERSION, &snapshot).unwrap();
        assert!(encoded.len() > 8 * 1024 * 1024);
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("client")
        };
        client.outgoing.extend_from_slice(&encoded);
        // No reads yet: the old 8 MiB limit dropped even a healthy reader.
        daemon.flush_client(token);
        assert!(
            daemon.sources.contains(token.0),
            "a legal snapshot disconnected its reader"
        );
        let mut received = Vec::new();
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match reader.read_to_end(&mut received) {
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                result => panic!("reader closed: {result:?}"),
            }
            daemon.flush_client(token);
            let Some(Source::Client(client)) = daemon.sources.get(token.0) else {
                panic!("client dropped")
            };
            if !client.backlogged() {
                let _ = reader.read_to_end(&mut received);
                break;
            }
            assert!(Instant::now() < until, "snapshot did not drain");
        }
        // A single server binary WebSocket frame, with its 64-bit length.
        assert_eq!(&received[..2], &[0x82, 127]);
        assert_eq!(
            u64::from_be_bytes(received[2..10].try_into().unwrap()),
            encoded.len() as u64
        );
        assert_eq!(&received[10..], &encoded);
        let mut decoder = FrameDecoder::new();
        decoder.push(&received[10..]);
        assert_eq!(
            decoder.decode::<ServerMessage>(PROTOCOL_VERSION).unwrap(),
            Some(snapshot)
        );
    }

    #[test]
    fn a_web_reader_with_excessive_backlog_is_still_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = agent_files::tests::daemon_with_chat(dir.path());
        let (token, _reader) = web_reader(&mut daemon);
        let Some(Source::Client(client)) = daemon.sources.get_mut(token.0) else {
            panic!("client")
        };
        client.outgoing.resize(MOST_WEB_UNSENT + 4 * 1024 * 1024, 0);
        daemon.flush_client(token);
        assert!(!daemon.sources.contains(token.0));
    }

    #[test]
    fn messages_that_cant_be_read_are_forgotten_as_time_goes_by() {
        // Past whenever the daemon's clock started, in this test process.
        let start = Instant::now() + Duration::from_secs(100);
        let mut unreadable = Unreadable::default();
        for _ in 0..MOST_UNREADABLE {
            assert!(unreadable.one_more(start));
        }
        // A stream of them is too many.
        assert!(!unreadable_as(&unreadable).one_more(start));
        // A few seconds later, as many as were forgotten fit again.
        let later = start + Duration::from_secs(2);
        for _ in 0..2 * UNREADABLE_FORGOTTEN {
            assert!(unreadable.one_more(later));
        }
        assert!(!unreadable.one_more(later));
        // And a long while later, all of them.
        let much_later = later + Duration::from_hours(1);
        for _ in 0..MOST_UNREADABLE {
            assert!(unreadable.one_more(much_later));
        }
    }

    /// A copy of `unreadable`, to try one more on without changing it.
    fn unreadable_as(unreadable: &Unreadable) -> Unreadable {
        Unreadable {
            count: unreadable.count,
            at: unreadable.at,
            any: unreadable.any,
        }
    }

    #[test]
    fn a_view_update_dropped_for_an_older_client_is_followed_by_a_snapshot() {
        let size = Size::new(10, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        let mut sent = Subscription::new();
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Snapshot(_))
        ));
        terminal.advance(b"a");
        let Some(update @ ViewUpdate::Patch(_)) = sent.update(&terminal, PROTOCOL_VERSION) else {
            panic!("a patch");
        };
        // A patch queued for a client of this build's version goes.
        let mut outgoing = Vec::new();
        assert!(queue_update(
            &mut sent,
            PROTOCOL_VERSION,
            TerminalId(1),
            update,
            &mut outgoing
        ));
        assert!(sent.awaiting_ack && !outgoing.is_empty());
        sent.awaiting_ack = false;
        // A view newer than the oldest client is dropped for it: it never
        // has it, so the next update starts afresh.
        let newer = ViewUpdate::Snapshot(View {
            title: String::new(),
            cwd: None,
            root: clackatoo_protocol::Node::Other,
        });
        let mut outgoing = Vec::new();
        let oldest = clackatoo_protocol::wire::MIN_CLIENT;
        assert!(!queue_update(
            &mut sent,
            oldest,
            TerminalId(1),
            newer,
            &mut outgoing
        ));
        assert!(outgoing.is_empty() && !sent.awaiting_ack);
        terminal.advance(b"b");
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Snapshot(_))
        ));
    }

    #[test]
    fn renumbering_history_sends_a_snapshot() {
        let size = |cols| Size::new(cols, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size(10), &Config::default());
        terminal.advance(b"abcdefghijklmnopqrstuvwxy");
        let mut sent = Subscription::new();
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Snapshot(_))
        ));
        terminal.advance(b"z");
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Patch(_))
        ));

        // Back at the same size before the client heard of the first
        // resize, but the line in history was taken back and rewrapped.
        terminal.resize(size(7));
        terminal.resize(size(10));
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Snapshot(_))
        ));
    }

    #[test]
    fn updates_and_history_carry_the_links_their_lines_show_and_the_cwd() {
        let size = Size::new(20, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        terminal.advance(b"\x1b]8;id=d;https://docs/\x07docs\x1b]8;;\x07\r\n");
        let mut sent = Subscription::new();
        let Some(ViewUpdate::Snapshot(view)) = sent.update(&terminal, PROTOCOL_VERSION) else {
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
        let Some(ViewUpdate::Patch(patches)) = sent.update(&terminal, PROTOCOL_VERSION) else {
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

    /// A command run at a prompt, ending with `exit`, as shell integration
    /// marks it.
    fn ran(exit: i32) -> Vec<u8> {
        format!("\x1b]133;A\x07$ x\r\n\x1b]133;C\x07\x1b]133;D;{exit}\x07").into_bytes()
    }

    /// The marks of the grid patch in `update`, if it's a patch with one.
    fn patch_marks(update: Option<ViewUpdate>) -> Option<BlockMarks> {
        let Some(ViewUpdate::Patch(patches)) = update else {
            return None;
        };
        patches.into_iter().find_map(|patch| match patch {
            Patch::Grid { marks, .. } => Some(marks),
            _ => None,
        })
    }

    #[test]
    fn a_burst_of_commands_tells_how_the_last_256_ended() {
        let size = Size::new(20, 4).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        let mut sent = Subscription::new();
        sent.update(&terminal, PROTOCOL_VERSION);
        for n in 0..300 {
            terminal.advance(&ran(n % 3));
        }
        terminal.advance(b"\x1b]133;A\x07$ ");
        terminal.take_damage(&mut sent.damage);
        let marks = patch_marks(sent.update(&terminal, PROTOCOL_VERSION)).expect("a patch");
        assert_eq!(marks.exits.len(), 256);
        assert_eq!(marks.exits.first(), Some(&(44, 44 % 3)));
        assert_eq!(marks.exits.last(), Some(&(299, 299 % 3)));
        // Older ones come with their history.
        let fetched = history(&terminal, 50, 10);
        assert_eq!(fetched.marks.exits.first(), Some(&(40, 40 % 3)));
        terminal.take_damage(&mut sent.damage);
        terminal.advance(b"y");
        terminal.take_damage(&mut sent.damage);
        let marks = patch_marks(sent.update(&terminal, PROTOCOL_VERSION)).expect("a patch");
        assert_eq!(marks.exits, [], "each told once");
    }

    #[test]
    fn after_a_reset_blocks_are_told_from_its_snapshot_on() {
        let size = Size::new(20, 4).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        terminal.advance(&ran(1));
        let mut sent = Subscription::new();
        sent.update(&terminal, PROTOCOL_VERSION);
        terminal.advance(b"\x1bc");
        terminal.advance(&ran(2));
        terminal.advance(b"\x1b]133;A\x07$ ");
        let Some(ViewUpdate::Snapshot(view)) = sent.update(&terminal, PROTOCOL_VERSION) else {
            panic!("a reset renumbers history, which a snapshot says");
        };
        // Numbers carry on past the reset, so block 0's end is never
        // taken for the new block's.
        let marks = &view.grid().expect("a grid").marks;
        assert_eq!(marks.prompts, [(0, 1), (1, 2)]);
        assert_eq!(marks.exits, [(1, 2)]);
        terminal.take_damage(&mut sent.damage);
        terminal.advance(b"x\r\n\x1b]133;C\x07\x1b]133;D;3\x07\x1b]133;A\x07$ ");
        terminal.take_damage(&mut sent.damage);
        let marks = patch_marks(sent.update(&terminal, PROTOCOL_VERSION)).expect("a patch");
        assert_eq!(marks.exits, [(2, 3)]);
    }

    #[test]
    fn blocks_numbered_again_send_a_snapshot() {
        let size = Size::new(20, 4).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        // Blocks handed over at the last number, as if four billion
        // commands had run: the snapshot's fields in order, as postcard
        // writes them.
        let last: (Option<String>, Vec<()>, u32, Vec<()>) =
            (None, Vec::new(), u32::MAX, Vec::new());
        let blocks: clackatoo_term::BlocksSnapshot =
            postcard::from_bytes(&postcard::to_allocvec(&last).unwrap()).unwrap();
        terminal.restore_blocks(&blocks);
        terminal.advance(&ran(1));
        let mut sent = Subscription::new();
        sent.update(&terminal, PROTOCOL_VERSION);
        assert_eq!(
            terminal.latest_block().map(|(block, _)| block),
            Some(u32::MAX)
        );
        // The next prompt needs a number there's none left for.
        terminal.take_damage(&mut sent.damage);
        terminal.advance(b"\x1b]133;A\x07$ ");
        terminal.take_damage(&mut sent.damage);
        let Some(ViewUpdate::Snapshot(view)) = sent.update(&terminal, PROTOCOL_VERSION) else {
            panic!("numbered again, history is too, which a snapshot says");
        };
        assert_eq!(view.grid().expect("a grid").state.block, Some(0));
        assert_eq!(sent.settled, None);
    }

    #[test]
    fn a_blocks_end_is_told_once_and_only_to_windows_that_read_it() {
        const PROMPT: &[u8] = b"\x1b]133;A\x07$ ";
        let size = Size::new(20, 4).expect("test sizes are valid");
        for wire in [PROTOCOL_VERSION, wire::BLOCK_MARKS - 1] {
            let mut terminal = Terminal::new(size, &Config::default());
            terminal.advance(PROMPT);
            terminal.advance(b"false\r\n\x1b]133;C\x07");
            let mut sent = Subscription::new();
            sent.update(&terminal, wire);
            let mut patched = |terminal: &mut Terminal, output: &[u8]| {
                terminal.take_damage(&mut Damage::new());
                terminal.advance(output);
                terminal.take_damage(&mut sent.damage);
                let Some(ViewUpdate::Patch(patches)) = sent.update(terminal, wire) else {
                    return None;
                };
                patches.into_iter().find_map(|patch| match patch {
                    Patch::Grid { marks, .. } => Some(marks),
                    _ => None,
                })
            };
            // Its end, with nothing drawn: a patch says it, to a window
            // that reads it.
            let ended = patched(&mut terminal, b"\x1b]133;D;1\x07");
            let prompt = patched(&mut terminal, PROMPT);
            let again = patched(&mut terminal, b"x");
            if wire < wire::BLOCK_MARKS {
                assert_eq!(ended, None, "nothing changed it can read");
                assert_eq!(prompt.unwrap(), BlockMarks::default());
                assert_eq!(again.unwrap(), BlockMarks::default());
                continue;
            }
            assert_eq!(ended.unwrap().exits, [(0, 1)]);
            let prompt = prompt.unwrap();
            assert_eq!(prompt.prompts, [(1, 1)], "the new prompt's line");
            assert_eq!(prompt.exits, [], "said once");
            assert_eq!(again.unwrap().prompts, [(1, 1)], "typing sends its line");
        }
    }

    #[test]
    fn a_reset_sends_a_snapshot() {
        let size = Size::new(10, 2).expect("test sizes are valid");
        let mut terminal = Terminal::new(size, &Config::default());
        terminal.advance(b"one\r\ntwo\r\nthree\r\n");
        let mut sent = Subscription::new();
        sent.update(&terminal, PROTOCOL_VERSION);
        // A reset numbers history from 0 again, and more output can take
        // it past where it was before the client hears of either.
        terminal.advance(b"\x1bc");
        terminal.advance(&b"line\r\n".repeat(10));
        assert!(matches!(
            sent.update(&terminal, PROTOCOL_VERSION),
            Some(ViewUpdate::Snapshot(_))
        ));
    }
}
