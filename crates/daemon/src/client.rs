//! A blocking client for the daemon, for the CLI, the GUI and tests.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::net::UnixStream;
use std::path::Path;

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError};
use clackatoo_protocol::{
    Block, BufferId, Change, ChatAction, ClientMessage, Error, Event, FileOp, FilesAction, FilesId,
    HistoryLines, NewTerminal, PROTOCOL_VERSION, PaneId, PaneTerminal, Query, Request, Response,
    Search, ServerMessage, Side, Size, TerminalId, TerminalInfo, Udp, View, Which, WorkspaceId,
    Workspaces,
};

use crate::quic::{self, Carried, Contact, Redial, Route};
use crate::remote::Ssh;

/// How long the daemon may take to answer a new connection's hello.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Where a client on another machine sends what goes to the daemon over
/// UDP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpTarget {
    /// The daemon's machine, at this address, on the port the daemon names.
    Host(IpAddr),
    /// Exactly here, whatever port the daemon names: a relay in between,
    /// as tests use (see `clackatoo-netem`).
    Exactly(SocketAddr),
}

/// A connection to the daemon that sends one request at a time and waits for
/// each answer. Events that arrive while it waits are kept for
/// [`next_event`](Self::next_event).
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    events: VecDeque<Event>,
    /// What carries the connection to a remote daemon.
    carried: Option<Carried>,
    /// The number this client attaches to buffers as: see
    /// [`Request::AttachBuffer`].
    window: u64,
}

/// A number for a window to attach to buffers as, chosen at random.
#[must_use]
pub fn random_window() -> u64 {
    use std::hash::BuildHasher;
    // Each RandomState is keyed afresh from the system's randomness.
    std::hash::RandomState::new().hash_one(std::process::id())
}

impl Client {
    /// Connects to the daemon listening at `socket_path` and checks that it
    /// speaks our protocol version.
    ///
    /// # Errors
    ///
    /// If nothing is listening, or the daemon speaks another version.
    pub fn connect(socket_path: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket_path).map_err(ClientError::Connect)?;
        Self::over(stream)
    }

    /// Greets the daemon on the other end of `stream`, and checks that it
    /// speaks our protocol version.
    ///
    /// # Errors
    ///
    /// If the daemon doesn't answer, or speaks another version.
    pub fn over(stream: UnixStream) -> Result<Self, ClientError> {
        let mut client = Self {
            stream,
            decoder: FrameDecoder::new(),
            events: VecDeque::new(),
            carried: None,
            window: random_window(),
        };
        // A socket nothing serves would otherwise hang the handshake.
        client.stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        client.send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        })?;
        match client.receive()? {
            ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                client.stream.set_read_timeout(None)?;
                Ok(client)
            }
            ServerMessage::Hello { protocol_version } => Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                daemon: protocol_version,
            }),
            other => Err(ClientError::UnexpectedMessage(Box::new(other))),
        }
    }

    /// Greets the daemon on the other end of `stream`, a connection to its
    /// socket, and talks to it over QUIC inside that, as a client on
    /// another machine does (see [`remote`](crate::remote)).
    ///
    /// # Errors
    ///
    /// If the daemon doesn't answer, speaks another version, or the QUIC
    /// connection can't be set up.
    pub fn over_quic(stream: UnixStream) -> Result<Self, ClientError> {
        Self::over_carrier(stream, None, None, None)
    }

    /// As [`over_quic`](Self::over_quic), moving the connection onto UDP
    /// when the daemon, at `daemon_ip`, can be reached there.
    ///
    /// # Errors
    ///
    /// As [`over_quic`](Self::over_quic).
    pub fn over_quic_via(stream: UnixStream, daemon_ip: IpAddr) -> Result<Self, ClientError> {
        Self::over_carrier(stream, Some(UdpTarget::Host(daemon_ip)), None, None)
    }

    /// As [`over_quic`](Self::over_quic), moving the connection onto UDP
    /// to `udp` when it can be reached there.
    ///
    /// # Errors
    ///
    /// As [`over_quic`](Self::over_quic).
    pub fn over_quic_to(stream: UnixStream, udp: UdpTarget) -> Result<Self, ClientError> {
        Self::over_carrier(stream, Some(udp), None, None)
    }

    /// As [`over_quic_to`](Self::over_quic_to), opening a new carrier with
    /// `redial` whenever the one in use closes, as a window does with ssh.
    ///
    /// # Errors
    ///
    /// As [`over_quic`](Self::over_quic).
    pub fn over_quic_redialing(
        stream: UnixStream,
        udp: Option<UdpTarget>,
        redial: Redial,
    ) -> Result<Self, ClientError> {
        Self::over_carrier(stream, udp, None, Some(redial))
    }

    /// As [`over_quic_redialing`](Self::over_quic_redialing), with `ssh`
    /// carrying `stream`, kept running as long as it's in use.
    pub(crate) fn over_carrier(
        mut stream: UnixStream,
        udp_target: Option<UdpTarget>,
        ssh: Option<Ssh>,
        redial: Option<Redial>,
    ) -> Result<Self, ClientError> {
        let (certificate, udp) = open_carrier(&mut stream)?;
        let udp = udp_target.zip(udp).map(|(target, udp)| {
            let address = match target {
                UdpTarget::Host(ip) => SocketAddr::new(ip, udp.port),
                UdpTarget::Exactly(address) => address,
            };
            (address, udp.probe_key)
        });
        let (local, carried) = quic::carry(stream, certificate, udp, ssh, redial)?;
        let mut client = Self::over(local)?;
        client.carried = Some(carried);
        Ok(client)
    }

    /// Whether a daemon on another machine is in contact, for showing when
    /// it isn't. `None` for a daemon on this one. The handle outlives a
    /// [`split`](Self::split).
    #[must_use]
    pub fn contact(&self) -> Option<Contact> {
        self.carried.as_ref().map(Carried::contact)
    }

    /// How the connection reaches a daemon on another machine: through
    /// SSH, or straight over UDP. `None` for a daemon on this one.
    #[must_use]
    pub fn route(&self) -> Option<Route> {
        self.carried.as_ref().map(Carried::route)
    }

    /// Sends `request` and waits for the daemon's answer.
    ///
    /// # Errors
    ///
    /// If the connection fails. An error *answer* is returned as
    /// `Ok(Response::Error(…))`. The typed helpers turn it into
    /// [`ClientError::Daemon`].
    pub fn request(&mut self, request: &Request) -> Result<Response, ClientError> {
        self.send(&ClientMessage::Request(request.clone()))?;
        loop {
            match self.receive()? {
                ServerMessage::Response(response) => return Ok(response),
                ServerMessage::Event(event) => self.events.push_back(event),
                other @ (ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. }) => {
                    return Err(ClientError::UnexpectedMessage(Box::new(other)));
                }
            }
        }
    }

    /// Waits for the next event: changes to the views attached to, or to
    /// the workspaces, if watching them.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn next_event(&mut self) -> Result<Event, ClientError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        match self.receive()? {
            ServerMessage::Event(event) => Ok(event),
            other => Err(ClientError::UnexpectedMessage(Box::new(other))),
        }
    }

    /// The events that came before the last answer, not yet taken, without
    /// waiting for more.
    pub fn take_events(&mut self) -> Vec<Event> {
        self.events.drain(..).collect()
    }

    /// Splits the connection in two, so one thread can wait for messages
    /// while another sends. Requests sent through the [`Sender`] get their
    /// answers through the [`Receiver`], in order, among the events.
    ///
    /// # Errors
    ///
    /// If the socket can't be duplicated.
    pub fn split(self) -> Result<(Sender, Receiver), ClientError> {
        let sender = Sender {
            stream: self.stream.try_clone()?,
        };
        let receiver = Receiver {
            stream: self.stream,
            decoder: self.decoder,
            events: self.events,
            carried: self.carried,
        };
        Ok((sender, receiver))
    }

    /// Follows a terminal's view: an [`Event::View`] snapshot arrives next,
    /// then a patch for each change, each sent once the last was
    /// acknowledged with [`frame_done`](Self::frame_done).
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn attach(&mut self, terminal: TerminalId) -> Result<(), ClientError> {
        match self.request(&Request::Attach { terminal })? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Stops following a terminal's view.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn detach(&mut self, terminal: TerminalId) -> Result<(), ClientError> {
        match self.request(&Request::Detach { terminal })? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Tells the daemon the last view update for `terminal` has been drawn,
    /// so it can send the next.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn frame_done(&mut self, terminal: TerminalId) -> Result<(), ClientError> {
        self.send(&ClientMessage::FrameDone { terminal })
    }

    /// Follows a buffer: an [`Event::Buffer`] snapshot arrives next, then
    /// each change to it.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn attach_buffer(&mut self, buffer: BufferId) -> Result<(), ClientError> {
        let window = self.window;
        self.attach_buffer_as(buffer, window)
    }

    /// As [`attach_buffer`](Self::attach_buffer), as the window numbered
    /// `window` (see [`Request::AttachBuffer`]), as one does again on a new
    /// connection.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn attach_buffer_as(&mut self, buffer: BufferId, window: u64) -> Result<(), ClientError> {
        match self.request(&Request::AttachBuffer {
            buffer,
            window,
            resume: None,
        })? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Installs `language`'s grammar if `grammar`, and the language server
    /// `server`, as a [`BufferUpdate::Offer`](clackatoo_protocol::BufferUpdate::Offer)
    /// offered. The daemon says how it went in an [`Event::Installed`].
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn install(
        &mut self,
        language: &str,
        grammar: bool,
        server: Option<&str>,
    ) -> Result<(), ClientError> {
        let request = Request::Install {
            language: language.to_owned(),
            grammar,
            server: server.map(str::to_owned),
        };
        match self.request(&request)? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Has `language` never offered again.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn decline(&mut self, language: &str) -> Result<(), ClientError> {
        let request = Request::Decline {
            language: language.to_owned(),
        };
        match self.request(&request)? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Sends a change to a buffer the client follows, numbered `seq`. The
    /// daemon says how it went in an [`Event::Buffer`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn edit(&mut self, buffer: BufferId, seq: u64, change: Change) -> Result<(), ClientError> {
        self.send(&ClientMessage::Edit {
            buffer,
            seq,
            change,
        })
    }

    /// Asks the language server of a buffer the client follows `query`
    /// about byte `at` of its text at `version`, numbered `id`. The answer
    /// comes in an [`Event::Buffer`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn query(
        &mut self,
        buffer: BufferId,
        id: u64,
        (version, at): (u64, usize),
        query: Query,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Query {
            buffer,
            id,
            version,
            at,
            query,
        })
    }

    /// Asks the language server of a buffer the client follows to rename
    /// what's at byte `at` of its text at `version` to `name`, numbered
    /// `id`. The answer comes in an [`Event::Buffer`], after the edits.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn rename(
        &mut self,
        buffer: BufferId,
        id: u64,
        (version, at): (u64, usize),
        name: &str,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Rename {
            buffer,
            id,
            version,
            at,
            name: name.to_owned(),
        })
    }

    /// Asks the daemon to stage the hunk at byte `at` of the text at
    /// `version` of a buffer the client follows, numbered `id`. The answer
    /// comes in an [`Event::Buffer`], after the buffer's new hunks.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn stage(
        &mut self,
        buffer: BufferId,
        id: u64,
        (version, at): (u64, usize),
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Stage {
            buffer,
            id,
            version,
            at,
        })
    }

    /// Asks the daemon to find `search` in the project `pane` is in,
    /// numbered `id`. What's found comes in [`Event::Found`]s.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn find(
        &mut self,
        id: u64,
        pane: Option<PaneId>,
        search: Search,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Find { id, pane, search })
    }

    /// Stops the search numbered `id`.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn stop_finding(&mut self, id: u64) -> Result<(), ClientError> {
        self.send(&ClientMessage::StopFinding { id })
    }

    /// Answers [`Event::ClipboardAsked`] numbered `id`, from `terminal`'s
    /// program, with what's on the clipboard, or `None` to refuse.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn answer_clipboard(
        &mut self,
        terminal: TerminalId,
        id: u64,
        text: Option<String>,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Clipboard { terminal, id, text })
    }

    /// Asks `action` of the file explorer `files`, which the client
    /// attached to with [`Request::AttachFiles`]. What comes of it comes in
    /// [`Event::Files`]s.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn files(&mut self, files: FilesId, action: FilesAction) -> Result<(), ClientError> {
        self.send(&ClientMessage::Files { files, action })
    }

    /// Does `action` to the board `board`, which the client attached to
    /// with [`Request::AttachBoard`], as the user. What comes of it comes
    /// in [`Event::Board`]s.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn board(
        &mut self,
        board: clackatoo_protocol::BoardId,
        action: clackatoo_protocol::BoardAction,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::Board { board, action })
    }

    /// Asks the daemon to do `op` to files, numbered `id`. How it went
    /// comes in an [`Event::FileOp`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn file_op(&mut self, id: u64, op: FileOp) -> Result<(), ClientError> {
        self.send(&ClientMessage::FileOp { id, op })
    }

    /// Limits how long [`next_event`](Self::next_event) and requests wait.
    /// A wait that runs out fails with an I/O error of kind `WouldBlock`.
    ///
    /// # Errors
    ///
    /// If the socket refuses the setting.
    pub fn set_read_timeout(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    /// Sends `message`. If the QUIC connection under the stream has ended,
    /// as when the daemon stopped, the error says why, as receiving would:
    /// what's sent after its thread hung up finds the stream closed.
    fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        let bytes = frame::encode(message)?;
        (self.stream.write_all(&bytes)).map_err(|err| why(err.into(), self.carried.as_ref()))
    }

    fn receive(&mut self) -> Result<ServerMessage, ClientError> {
        receive(&mut self.stream, &mut self.decoder).map_err(|err| why(err, self.carried.as_ref()))
    }

    /// Lists every terminal.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn list_terminals(&mut self) -> Result<Vec<TerminalInfo>, ClientError> {
        match self.request(&Request::ListTerminals)? {
            Response::Terminals(terminals) => Ok(terminals),
            other => Err(unexpected(other)),
        }
    }

    /// Which release of clackatoo the daemon is, such as `2026.10.0`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn daemon_version(&mut self) -> Result<String, ClientError> {
        match self.request(&Request::Version)? {
            Response::Text(version) => Ok(version),
            other => Err(unexpected(other)),
        }
    }

    /// Starts a terminal.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn new_terminal(&mut self, request: NewTerminal) -> Result<TerminalId, ClientError> {
        match self.request(&Request::NewTerminal(request))? {
            Response::TerminalCreated(id) => Ok(id),
            other => Err(unexpected(other)),
        }
    }

    /// Sends input to a terminal.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn send_input(&mut self, terminal: TerminalId, bytes: &[u8]) -> Result<(), ClientError> {
        let request = Request::SendInput {
            terminal,
            bytes: bytes.to_vec(),
        };
        match self.request(&request)? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Asks a chat with an agent to do something: send a message, stop
    /// the agent answering, or answer its question.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn chat(&mut self, terminal: TerminalId, action: ChatAction) -> Result<(), ClientError> {
        match self.request(&Request::Chat { terminal, action })? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Captures what a terminal is showing.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn capture(&mut self, terminal: TerminalId) -> Result<View, ClientError> {
        match self.request(&Request::Capture { terminal })? {
            Response::View(view) => Ok(view),
            other => Err(unexpected(other)),
        }
    }

    /// Fetches up to `count` history lines ending before line `end`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn history(
        &mut self,
        terminal: TerminalId,
        end: u64,
        count: u16,
    ) -> Result<HistoryLines, ClientError> {
        match self.request(&Request::History {
            terminal,
            end,
            count,
        })? {
            Response::History(lines) => Ok(lines),
            other => Err(unexpected(other)),
        }
    }

    /// Finds the shell's block at line `line`, or the one before or after.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn block(
        &mut self,
        terminal: TerminalId,
        line: u64,
        which: Which,
    ) -> Result<Option<Block>, ClientError> {
        match self.request(&Request::Block {
            terminal,
            line,
            which,
        })? {
            Response::Block(block) => Ok(block),
            other => Err(unexpected(other)),
        }
    }

    /// Closes a terminal.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn close_terminal(&mut self, terminal: TerminalId) -> Result<(), ClientError> {
        match self.request(&Request::CloseTerminal { terminal })? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }

    /// Changes a terminal's size.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn resize(&mut self, terminal: TerminalId, size: Size) -> Result<(), ClientError> {
        self.done(&Request::Resize { terminal, size })
    }

    /// Fetches every workspace, and asks for an [`Event::Workspaces`]
    /// whenever they change.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn watch_workspaces(&mut self) -> Result<Workspaces, ClientError> {
        match self.request(&Request::WatchWorkspaces)? {
            Response::Workspaces(workspaces) => Ok(workspaces),
            other => Err(unexpected(other)),
        }
    }

    /// Makes a workspace whose one pane shows `terminal`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn new_workspace(
        &mut self,
        name: Option<String>,
        terminal: PaneTerminal,
    ) -> Result<PaneId, ClientError> {
        self.pane_created(&Request::NewWorkspace { name, terminal })
    }

    /// Adds a tab to `workspace`, showing `terminal`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn new_tab(
        &mut self,
        workspace: WorkspaceId,
        terminal: PaneTerminal,
    ) -> Result<PaneId, ClientError> {
        self.pane_created(&Request::NewTab {
            workspace,
            terminal,
        })
    }

    /// Splits `pane`, putting a pane showing `terminal` on `side` of it.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn split_pane(
        &mut self,
        pane: PaneId,
        side: Side,
        terminal: PaneTerminal,
    ) -> Result<PaneId, ClientError> {
        self.pane_created(&Request::Split {
            pane,
            side,
            terminal,
        })
    }

    /// Closes `pane` and its terminal.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn close_pane(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.done(&Request::ClosePane { pane })
    }

    /// Records a switch to `pane`.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn focus(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.done(&Request::Focus { pane })
    }

    fn pane_created(&mut self, request: &Request) -> Result<PaneId, ClientError> {
        match self.request(request)? {
            Response::PaneCreated(pane) => Ok(pane),
            other => Err(unexpected(other)),
        }
    }

    fn done(&mut self, request: &Request) -> Result<(), ClientError> {
        match self.request(request)? {
            Response::Done => Ok(()),
            other => Err(unexpected(other)),
        }
    }
}

/// The sending half of a [split](Client::split) connection.
#[derive(Debug)]
pub struct Sender {
    stream: UnixStream,
}

impl Sender {
    /// Sends `message` without waiting for anything.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        self.stream.write_all(&frame::encode(message)?)?;
        Ok(())
    }
}

/// The receiving half of a [split](Client::split) connection.
#[derive(Debug)]
pub struct Receiver {
    stream: UnixStream,
    decoder: FrameDecoder,
    /// Events that arrived before the split.
    events: VecDeque<Event>,
    /// What carries the connection to a remote daemon.
    carried: Option<Carried>,
}

impl Receiver {
    /// Waits for the daemon's next message.
    ///
    /// # Errors
    ///
    /// If the connection fails or closes.
    pub fn receive(&mut self) -> Result<ServerMessage, ClientError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(ServerMessage::Event(event));
        }
        receive(&mut self.stream, &mut self.decoder).map_err(|err| why(err, self.carried.as_ref()))
    }
}

/// `err`, from a connection that closed, or else why the QUIC connection
/// under it ended, if it was one and it did.
fn why(err: ClientError, carried: Option<&Carried>) -> ClientError {
    let closed = matches!(&err, ClientError::Disconnected | ClientError::Io(_));
    match carried.and_then(Carried::ended) {
        Some(reason) if closed => ClientError::Lost(reason),
        _ => err,
    }
}

/// Greets the daemon on the other end of `stream`, checking that it speaks
/// our protocol version, then turns the connection into a carrier of QUIC
/// packets. Returns the certificate the daemon's QUIC endpoint presents,
/// and where it takes packets over UDP, if it does.
///
/// The request waits for the greeting's answer: a daemon from an older
/// build may hang up on a request it can't decode before answering a
/// greeting that came with it, and then couldn't say which version it
/// speaks.
pub(crate) fn open_carrier(stream: &mut UnixStream) -> Result<(Vec<u8>, Option<Udp>), ClientError> {
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    stream.write_all(&frame::encode(&ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
    })?)?;
    let mut decoder = FrameDecoder::new();
    match receive(stream, &mut decoder)? {
        ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {}
        ServerMessage::Hello { protocol_version } => {
            return Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                daemon: protocol_version,
            });
        }
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    }
    stream.write_all(&frame::encode(&ClientMessage::Request(Request::CarryQuic))?)?;
    let carrying = match receive(stream, &mut decoder)? {
        ServerMessage::Response(Response::Carrying { certificate, udp }) => (certificate, udp),
        ServerMessage::Response(response) => return Err(unexpected(response)),
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    };
    // Nothing comes before the first packet, which is ours to send.
    if decoder.has_partial_frame() {
        return Err(ClientError::Disconnected);
    }
    stream.set_read_timeout(None)?;
    Ok(carrying)
}

/// Reads until a whole message has arrived.
pub(crate) fn receive(
    stream: &mut UnixStream,
    decoder: &mut FrameDecoder,
) -> Result<ServerMessage, ClientError> {
    let mut buffer = [0; 16 * 1024];
    loop {
        if let Some(message) = decoder.decode()? {
            return Ok(message);
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Err(ClientError::Disconnected),
            Ok(n) => decoder.push(&buffer[..n]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
}

/// Converts an answer that doesn't fit the request into an error, unpacking
/// error answers.
fn unexpected(response: Response) -> ClientError {
    match response {
        Response::Error(err) => ClientError::Daemon(err),
        other => ClientError::UnexpectedMessage(Box::new(ServerMessage::Response(other))),
    }
}

/// Talking to the daemon failed.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing is listening on the socket, or it can't be reached.
    #[error("could not connect to the daemon")]
    Connect(#[source] io::Error),
    /// The daemon speaks a different protocol version.
    #[error("the daemon speaks protocol version {daemon}, but this client speaks {ours}")]
    VersionMismatch {
        /// Our version.
        ours: u32,
        /// The daemon's version.
        daemon: u32,
    },
    /// The daemon refused or failed the request.
    #[error(transparent)]
    Daemon(Error),
    /// The daemon sent something that doesn't fit what was asked.
    #[error("the daemon sent an unexpected message: {0:?}")]
    UnexpectedMessage(Box<ServerMessage>),
    /// The daemon closed the connection.
    #[error("the daemon closed the connection")]
    Disconnected,
    /// The connection to a daemon on another machine ended, for this
    /// reason.
    #[error("{0}")]
    Lost(String),
    /// Reading or writing the socket failed.
    #[error("connection to the daemon failed")]
    Io(#[from] io::Error),
    /// A message couldn't be encoded or decoded.
    #[error(transparent)]
    Frame(#[from] FrameError),
}
