//! A blocking client for the daemon, for the CLI, the GUI and tests.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError, Head};
use clackatoo_protocol::wire;
use clackatoo_protocol::{
    Block, BufferId, Change, ChatAction, ClientMessage, Code, Connection, Error, Event, FileOp,
    FilesAction, FilesId, HistoryLines, NewTerminal, PROTOCOL_VERSION, PaneId, PaneTerminal, Place,
    Query, Request, Response, Search, Secret, ServerMessage, Side, Signed, Size, TerminalId,
    TerminalInfo, Udp, View, Which, WorkspaceId, Workspaces,
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
    /// The protocol version the connection runs at, agreed in the
    /// `Hello`s: the daemon's, if it's older than this build.
    wire: u32,
    events: VecDeque<Event>,
    /// What carries the connection to a remote daemon.
    carried: Option<Carried>,
    /// The number this client attaches to buffers as: see
    /// [`Request::AttachBuffer`].
    window: u64,
    /// The certificate a remote daemon's QUIC endpoint presented, and its
    /// machine's signature of it, if it signed it.
    certified: Option<Box<(Vec<u8>, Option<Signed>)>>,
}

/// What a daemon answers a connection asking to carry QUIC with (see
/// [`open_carrier`]).
#[derive(Debug)]
pub(crate) struct Carrying {
    /// The certificate its QUIC endpoint presents.
    pub(crate) certificate: Vec<u8>,
    /// Where it takes packets over UDP, if it does.
    pub(crate) udp: Option<Udp>,
    /// The certificate, signed by its machine's identity key, from a
    /// daemon that signs it.
    pub(crate) signed: Option<Signed>,
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

    /// Greets the daemon on the other end of `stream`, and agrees a
    /// protocol version with it: see [`wire`].
    ///
    /// # Errors
    ///
    /// If the daemon doesn't answer, or speaks a version this build can't
    /// talk at, or won't talk to this build.
    pub fn over(stream: UnixStream) -> Result<Self, ClientError> {
        Self::over_speaking(stream, PROTOCOL_VERSION)
    }

    /// As [`over`](Self::over), saying it speaks protocol `speaks`, at
    /// most this build's, as an older build would.
    pub(crate) fn over_speaking(stream: UnixStream, speaks: u32) -> Result<Self, ClientError> {
        let mut client = Self {
            stream,
            decoder: FrameDecoder::new(),
            wire: PROTOCOL_VERSION,
            events: VecDeque::new(),
            carried: None,
            window: random_window(),
            certified: None,
        };
        // A socket nothing serves would otherwise hang the handshake.
        client.stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
        client.send(&ClientMessage::Hello {
            protocol_version: speaks.min(PROTOCOL_VERSION),
        })?;
        let wire = agree(client.receive()?)?;
        client.wire = wire;
        client.stream.set_read_timeout(None)?;
        Ok(client)
    }

    /// The protocol version the connection runs at: the older of the
    /// daemon's and this build's.
    #[must_use]
    pub fn wire(&self) -> u32 {
        self.wire
    }

    /// Whether the daemon has what arrived in protocol version `gate`, one
    /// of [`wire`]'s gates: a window asks before it offers something that
    /// needs a request new then.
    #[must_use]
    pub fn daemon_has(&self, gate: u32) -> bool {
        wire::daemon_has(self.wire, gate)
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
        Self::over_carrier(stream, None, None, None, PROTOCOL_VERSION)
    }

    /// As [`over_quic`](Self::over_quic), moving the connection onto UDP
    /// when the daemon, at `daemon_ip`, can be reached there.
    ///
    /// # Errors
    ///
    /// As [`over_quic`](Self::over_quic).
    pub fn over_quic_via(stream: UnixStream, daemon_ip: IpAddr) -> Result<Self, ClientError> {
        Self::over_carrier(
            stream,
            Some(UdpTarget::Host(daemon_ip)),
            None,
            None,
            PROTOCOL_VERSION,
        )
    }

    /// As [`over_quic`](Self::over_quic), moving the connection onto UDP
    /// to `udp` when it can be reached there.
    ///
    /// # Errors
    ///
    /// As [`over_quic`](Self::over_quic).
    pub fn over_quic_to(stream: UnixStream, udp: UdpTarget) -> Result<Self, ClientError> {
        Self::over_carrier(stream, Some(udp), None, None, PROTOCOL_VERSION)
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
        Self::over_carrier(stream, udp, None, Some(redial), PROTOCOL_VERSION)
    }

    /// As [`over_quic_redialing`](Self::over_quic_redialing), with `ssh`
    /// carrying `stream`, kept running as long as it's in use, saying it
    /// speaks protocol `speaks`.
    pub(crate) fn over_carrier(
        mut stream: UnixStream,
        udp_target: Option<UdpTarget>,
        ssh: Option<Ssh>,
        redial: Option<Redial>,
        speaks: u32,
    ) -> Result<Self, ClientError> {
        let Carrying {
            certificate,
            udp,
            signed,
        } = open_carrier(&mut stream)?;
        let udp = udp_target.zip(udp).map(|(target, udp)| {
            let address = match target {
                UdpTarget::Host(ip) => SocketAddr::new(ip, udp.port),
                UdpTarget::Exactly(address) => address,
            };
            (address, udp.probe_key)
        });
        let (local, carried) = quic::carry(stream, certificate.clone(), udp, ssh, redial)?;
        let mut client = Self::over_speaking(local, speaks)?;
        client.carried = Some(carried);
        client.certified = Some(Box::new((certificate, signed)));
        Ok(client)
    }

    /// The certificate a remote daemon's QUIC endpoint presented, and its
    /// machine's signature of it, if it signed it: `None` for a daemon on
    /// this machine.
    pub(crate) fn certified(&self) -> Option<(&[u8], Option<&Signed>)> {
        (self.certified.as_deref()).map(|(certificate, signed)| (&certificate[..], signed.as_ref()))
    }

    /// Whether a daemon on another machine is in contact, for showing when
    /// it isn't. `None` for a daemon on this one. The handle outlives a
    /// [`split`](Self::split).
    #[must_use]
    pub fn contact(&self) -> Option<Contact> {
        self.carried.as_ref().map(Carried::contact)
    }

    /// A way to forward ports from this machine to the daemon's, over
    /// the connection: see [`crate::forward`]. `None` for a daemon on this
    /// machine, which needs none. The handle outlives a
    /// [`split`](Self::split), and does nothing once the connection ends.
    #[must_use]
    pub fn forwarder(&self) -> Option<crate::forward::Forwarder> {
        self.carried
            .as_ref()
            .map(|carried| carried.forwarder(self.wire))
    }

    /// Asks the daemon on another machine this connection reaches to
    /// carry a stream on to the daemon on `machine`, one it passes on (see
    /// [`Request::Reach`]): the stream, once it says it does, to greet
    /// that daemon over, as a carrier of QUIC. Only a client that said
    /// which machine it is ([`Request::RelayAs`]) may ask.
    ///
    /// # Errors
    ///
    /// [`ClientError::Daemon`] with why the daemon won't; or if this
    /// connection is to a daemon on this machine, or has ended.
    pub fn reach(&self, machine: clackatoo_protocol::MachineId) -> Result<UnixStream, ClientError> {
        let forwarder = self.forwarder().ok_or(ClientError::Disconnected)?;
        reach(&forwarder, machine)
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
            match self.receive() {
                Ok(ServerMessage::Response(response)) => return Ok(response),
                Ok(ServerMessage::Event(event)) => self.events.push_back(event),
                Ok(other @ (ServerMessage::Hello { .. } | ServerMessage::NotUpgrading { .. })) => {
                    return Err(ClientError::UnexpectedMessage(Box::new(other)));
                }
                // An answer from a newer daemon this build can't read: still
                // the answer, so the next request's is its own.
                Err(ClientError::Unreadable(head)) if head.is_response() => {
                    return Ok(Response::Error(Error::Unsupported));
                }
                Err(ClientError::Unreadable(_)) => {}
                Err(err) => return Err(err),
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
        loop {
            match self.receive() {
                Ok(ServerMessage::Event(event)) => return Ok(event),
                Ok(other) => return Err(ClientError::UnexpectedMessage(Box::new(other))),
                // An event from a newer daemon this build can't read.
                Err(ClientError::Unreadable(_)) => {}
                Err(err) => return Err(err),
            }
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
            wire: self.wire,
        };
        let receiver = Receiver {
            stream: self.stream,
            decoder: self.decoder,
            wire: self.wire,
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
            buffer: None,
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
            buffer: None,
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

    /// Asks the daemon to do `op` to files, numbered `id`, on its own
    /// machine. How it went comes in an [`Event::FileOp`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn file_op(&mut self, id: u64, op: FileOp) -> Result<(), ClientError> {
        self.send(&ClientMessage::FileOp { id, op, pane: None })
    }

    /// Asks the daemon to do `op` to files, numbered `id`, as asked from
    /// `pane`, on the machine it's on: the daemon's own, or another the
    /// daemon is connected to. How it went comes in an [`Event::FileOp`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn file_op_from(&mut self, id: u64, op: FileOp, pane: PaneId) -> Result<(), ClientError> {
        let pane = Some(pane);
        self.send(&ClientMessage::FileOp { id, op, pane })
    }

    /// Asks the agent called `agent` for the sessions it kept that work in
    /// `cwd`, numbered `id`. They come in an [`Event::Sessions`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn list_sessions(&mut self, id: u64, agent: &str, cwd: &str) -> Result<(), ClientError> {
        self.list_sessions_on(id, agent, cwd, None)
    }

    /// As [`list_sessions`](Self::list_sessions), of the agent on machine
    /// `on`, another the daemon is connected to, or its own with `None`.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn list_sessions_on(
        &mut self,
        id: u64,
        agent: &str,
        cwd: &str,
        on: Option<clackatoo_protocol::MachineId>,
    ) -> Result<(), ClientError> {
        self.send(&ClientMessage::ListSessions {
            id,
            agent: agent.to_owned(),
            cwd: cwd.to_owned(),
            on,
        })
    }

    /// Asks how `code`, blocks of code from an agent's words, is coloured,
    /// numbered `id`. The colours come in an [`Event::Coloured`].
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn colour(&mut self, id: u64, code: Vec<Code>) -> Result<(), ClientError> {
        self.send(&ClientMessage::Colour { id, code })
    }

    /// Tells the daemon where editor `pane`'s cursor and view are, to keep
    /// for a restart.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn place(&mut self, pane: PaneId, place: Place) -> Result<(), ClientError> {
        self.send(&ClientMessage::Place { pane, place })
    }

    /// Dismisses the offer to run again the command `pane`'s terminal was
    /// running when the daemon last stopped.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn dismiss(&mut self, pane: PaneId) -> Result<(), ClientError> {
        self.send(&ClientMessage::Dismiss { pane })
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
        let bytes = frame::encode(self.wire, message)?;
        (self.stream.write_all(&bytes)).map_err(|err| why(err.into(), self.carried.as_ref()))
    }

    fn receive(&mut self) -> Result<ServerMessage, ClientError> {
        receive(&mut self.stream, &mut self.decoder, self.wire)
            .map_err(|err| why(err, self.carried.as_ref()))
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

    /// Takes the message `queued` out of chat `terminal`'s queue and into
    /// its draft (`ChatAction::TakeBack`), returning the draft it makes.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn take_back(&mut self, terminal: TerminalId, queued: u64) -> Result<String, ClientError> {
        let action = ChatAction::TakeBack { queued };
        match self.request(&Request::Chat { terminal, action })? {
            Response::Text(made) => Ok(made),
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
        self.pane_created(&Request::NewWorkspace {
            name,
            terminal,
            on: None,
        })
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
            on: None,
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
            on: None,
        })
    }

    /// Closes `pane` and its terminal; but a chat an agent started, whose
    /// chat is still open, goes on in the background, as a card in it.
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

    /// Has the daemon connect to the daemon on another machine, `host`, as
    /// ssh names it, running `command` there to reach clackatoo (see
    /// [`Request::Connect`]). How it goes comes in
    /// [`Event::Connections`], to a client watching them.
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn connect_to(&mut self, host: &str, command: Option<&str>) -> Result<(), ClientError> {
        self.connect_sharing(host, command, None)
    }

    /// As [`connect_to`](Self::connect_to), passing the machine on to the
    /// daemons connected to this one, or not, as `share` says, if it says
    /// (see [`Request::Connect`]). A daemon too old to pass machines on
    /// is asked nothing, as it would connect and keep the connection as
    /// one to pass on once it's upgraded.
    ///
    /// # Errors
    ///
    /// See [`ClientError`]: [`ClientError::Daemon`] with
    /// [`Error::Unsupported`] for `share` asked of a daemon too old.
    pub fn connect_sharing(
        &mut self,
        host: &str,
        command: Option<&str>,
        share: Option<bool>,
    ) -> Result<(), ClientError> {
        if share.is_some() && !self.daemon_has(wire::CHAIN) {
            return Err(ClientError::Daemon(Error::Unsupported));
        }
        self.done(&Request::Connect {
            host: host.to_owned(),
            command: command.map(str::to_owned),
            share,
        })
    }

    /// Ends the daemon's connection to `host`, and forgets it.
    ///
    /// # Errors
    ///
    /// See [`ClientError`]: [`ClientError::Daemon`] if there's none.
    pub fn disconnect(&mut self, host: &str) -> Result<(), ClientError> {
        self.done(&Request::Disconnect {
            host: host.to_owned(),
        })
    }

    /// Lists the daemon's connections to daemons on other machines, and
    /// has the client told whenever they change. With `answer`, the client
    /// answers the questions ssh asks for them too: see
    /// [`Request::WatchConnections`].
    ///
    /// # Errors
    ///
    /// See [`ClientError`].
    pub fn watch_connections(&mut self, answer: bool) -> Result<Vec<Connection>, ClientError> {
        match self.request(&Request::WatchConnections { answer })? {
            Response::Connections(connections) => Ok(connections),
            other => Err(unexpected(other)),
        }
    }

    /// Answers the question numbered `ask` ssh asked ([`Event::Login`]),
    /// or with `None`, turns it down.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn answer_login(&mut self, ask: u64, answer: Option<String>) -> Result<(), ClientError> {
        self.send(&ClientMessage::Login {
            ask,
            answer: answer.map(Secret),
        })
    }

    /// The connection, taken apart, for the daemon's loop to read itself:
    /// the stream, what has been read of it and not yet decoded, the
    /// protocol version it runs at, and what carries it to another
    /// machine, which ends it when dropped. Events already read and not
    /// taken are dropped.
    pub(crate) fn into_parts(self) -> (UnixStream, FrameDecoder, u32, Option<Carried>) {
        (self.stream, self.decoder, self.wire, self.carried)
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
    /// The protocol version the connection runs at.
    wire: u32,
}

impl Sender {
    /// Sends `message` without waiting for anything.
    ///
    /// # Errors
    ///
    /// If the connection fails.
    pub fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        self.stream.write_all(&frame::encode(self.wire, message)?)?;
        Ok(())
    }
}

/// The receiving half of a [split](Client::split) connection.
#[derive(Debug)]
pub struct Receiver {
    stream: UnixStream,
    decoder: FrameDecoder,
    /// The protocol version the connection runs at.
    wire: u32,
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
    /// If the connection fails or closes; or, leaving it open,
    /// [`ClientError::Unreadable`] for a message from a newer daemon that
    /// this build can't read, which the caller makes up for: by taking it
    /// as an error answer to the oldest request waiting, or attaching
    /// again to what changed.
    pub fn receive(&mut self) -> Result<ServerMessage, ClientError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(ServerMessage::Event(event));
        }
        receive(&mut self.stream, &mut self.decoder, self.wire)
            .map_err(|err| why(err, self.carried.as_ref()))
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

/// Greets the daemon on the other end of `stream`, agreeing a protocol
/// version with it, then turns the connection into a carrier of QUIC
/// packets. Returns the certificate the daemon's QUIC endpoint presents,
/// and where it takes packets over UDP, if it does.
///
/// The request waits for the greeting's answer: a daemon from an older
/// build may hang up on a request it can't decode before answering a
/// greeting that came with it, and then couldn't say which version it
/// speaks.
pub(crate) fn open_carrier(stream: &mut UnixStream) -> Result<Carrying, ClientError> {
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    stream.write_all(&frame::encode(
        PROTOCOL_VERSION,
        &ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )?)?;
    let mut decoder = FrameDecoder::new();
    let wire = agree(receive(stream, &mut decoder, PROTOCOL_VERSION)?)?;
    let carry = ClientMessage::Request(Request::CarryQuic);
    stream.write_all(&frame::encode(wire, &carry)?)?;
    let carrying = match receive(stream, &mut decoder, wire)? {
        ServerMessage::Response(Response::Carrying {
            certificate,
            udp,
            signed,
        }) => Carrying {
            certificate,
            udp,
            signed,
        },
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

/// How long the daemon a stream reaching another machine goes through may
/// take to answer.
const REACH_WAIT: Duration = Duration::from_secs(10);

/// Opens a stream on the connection `forwarder` hands things to, asking
/// its daemon to carry it on to the daemon on `machine` (see
/// [`Request::Reach`]). Returns it once that daemon answers that it did.
pub(crate) fn reach(
    forwarder: &crate::forward::Forwarder,
    machine: clackatoo_protocol::MachineId,
) -> Result<UnixStream, ClientError> {
    let (mut ours, theirs) = UnixStream::pair()?;
    forwarder.carry(theirs);
    ours.set_read_timeout(Some(REACH_WAIT))?;
    ours.set_write_timeout(Some(REACH_WAIT))?;
    let wire = forwarder.wire();
    let request = ClientMessage::Request(Request::Reach { machine });
    ours.write_all(&frame::encode(wire, &request)?)?;
    let mut decoder = FrameDecoder::new();
    match receive(&mut ours, &mut decoder, wire)? {
        ServerMessage::Response(Response::Done) => {}
        ServerMessage::Response(Response::Error(err)) => return Err(ClientError::Daemon(err)),
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    }
    // Nothing comes before this end greets the daemon it reached.
    if decoder.has_partial_frame() {
        return Err(ClientError::Disconnected);
    }
    ours.set_read_timeout(None)?;
    ours.set_write_timeout(None)?;
    Ok(ours)
}

/// The protocol version to run a connection at, from the daemon's answer
/// to its `Hello`, or why it can't be run.
///
/// A daemon older than [`wire::MIN_DAEMON`] isn't talked to even if it
/// seems to agree: one older than [`wire::FALLBACKS`] refuses a client
/// by answering with its own version and hanging up, which is just what
/// agreeing would look like. Taking it would leave a remote window
/// connecting again and again into the same refusal, never saying the
/// daemon needs updating.
pub(crate) fn agree(reply: ServerMessage) -> Result<u32, ClientError> {
    match reply {
        ServerMessage::Hello { protocol_version } if wire::takes_reply(protocol_version) => {
            Ok(protocol_version)
        }
        ServerMessage::Hello { protocol_version } => Err(ClientError::VersionMismatch {
            ours: PROTOCOL_VERSION,
            daemon: protocol_version,
        }),
        other => Err(ClientError::UnexpectedMessage(Box::new(other))),
    }
}

/// Reads until a whole message has arrived, as a build of protocol `wire`
/// wrote it.
pub(crate) fn receive(
    stream: &mut UnixStream,
    decoder: &mut FrameDecoder,
    wire: u32,
) -> Result<ServerMessage, ClientError> {
    let mut buffer = [0; 16 * 1024];
    loop {
        match decoder.decode(wire) {
            Ok(Some(message)) => return Ok(message),
            Ok(None) => {}
            Err(err) => {
                return Err(match err.skipped() {
                    Some(head) => {
                        log::warn!("the daemon sent a message this build can't read: {err}");
                        ClientError::Unreadable(head)
                    }
                    None => err.into(),
                });
            }
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
    /// The daemon, a newer one, sent a message this build can't read,
    /// which began so. The connection goes on.
    #[error("the daemon sent a message this build can't read")]
    Unreadable(Head),
}
