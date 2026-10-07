//! A blocking client for the daemon, for the CLI, the GUI and tests.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError};
use clackatoo_protocol::{
    ClientMessage, Error, Event, HistoryLines, NewTerminal, PROTOCOL_VERSION, PaneId, PaneTerminal,
    Request, Response, ServerMessage, Side, Size, TerminalId, TerminalInfo, View, WorkspaceId,
    Workspaces,
};

use crate::quic::{self, Carried};
use crate::remote::Ssh;

/// How long the daemon may take to answer a new connection's hello.
const HELLO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
        Self::over_carrier(stream, None)
    }

    /// As [`over_quic`](Self::over_quic), with `ssh` carrying `stream`, kept
    /// running as long as the connection is open.
    pub(crate) fn over_carrier(
        mut stream: UnixStream,
        ssh: Option<Ssh>,
    ) -> Result<Self, ClientError> {
        let certificate = open_carrier(&mut stream)?;
        let (local, carried) = quic::carry(stream, certificate, ssh)?;
        let mut client = Self::over(local)?;
        client.carried = Some(carried);
        Ok(client)
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
                hello @ ServerMessage::Hello { .. } => {
                    return Err(ClientError::UnexpectedMessage(Box::new(hello)));
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
            _carried: self.carried,
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

    /// Limits how long [`next_event`](Self::next_event) and requests wait.
    /// A wait that runs out fails with an I/O error of kind `WouldBlock`.
    ///
    /// # Errors
    ///
    /// If the socket refuses the setting.
    pub fn set_read_timeout(&mut self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), ClientError> {
        self.stream.write_all(&frame::encode(message)?)?;
        Ok(())
    }

    fn receive(&mut self) -> Result<ServerMessage, ClientError> {
        receive(&mut self.stream, &mut self.decoder)
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
    _carried: Option<Carried>,
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
        receive(&mut self.stream, &mut self.decoder)
    }
}

/// Greets the daemon on the other end of `stream`, checking that it speaks
/// our protocol version, and turns the connection into a carrier of QUIC
/// packets. Both are sent at once, to save a round trip: a daemon speaking
/// another version hangs up after its greeting, ignoring the rest. Returns
/// the certificate the daemon's QUIC endpoint presents.
fn open_carrier(stream: &mut UnixStream) -> Result<Vec<u8>, ClientError> {
    let mut opening = frame::encode(&ClientMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
    })?;
    opening.extend(frame::encode(&ClientMessage::Request(Request::CarryQuic))?);
    stream.set_read_timeout(Some(HELLO_TIMEOUT))?;
    stream.write_all(&opening)?;
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
    let certificate = match receive(stream, &mut decoder)? {
        ServerMessage::Response(Response::Carrying { certificate }) => certificate,
        ServerMessage::Response(response) => return Err(unexpected(response)),
        other => return Err(ClientError::UnexpectedMessage(Box::new(other))),
    };
    // Nothing comes before the first packet, which is ours to send.
    if decoder.has_partial_frame() {
        return Err(ClientError::Disconnected);
    }
    stream.set_read_timeout(None)?;
    Ok(certificate)
}

/// Reads until a whole message has arrived.
fn receive(
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
    /// Reading or writing the socket failed.
    #[error("connection to the daemon failed")]
    Io(#[from] io::Error),
    /// A message couldn't be encoded or decoded.
    #[error(transparent)]
    Frame(#[from] FrameError),
}
