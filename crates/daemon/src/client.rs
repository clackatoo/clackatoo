//! A blocking client for the daemon, for the CLI, the GUI and tests.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError};
use clackatoo_protocol::{
    ClientMessage, Error, Event, NewTerminal, PROTOCOL_VERSION, Request, Response, ServerMessage,
    Size, TerminalId, TerminalInfo, View,
};

/// A connection to the daemon that sends one request at a time and waits for
/// each answer. Events that arrive while it waits are kept for
/// [`next_event`](Self::next_event).
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    events: VecDeque<Event>,
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
        let mut client = Self {
            stream,
            decoder: FrameDecoder::new(),
            events: VecDeque::new(),
        };
        client.send(&ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        })?;
        match client.receive()? {
            ServerMessage::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Ok(client)
            }
            ServerMessage::Hello { protocol_version } => Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                daemon: protocol_version,
            }),
            other => Err(ClientError::UnexpectedMessage(Box::new(other))),
        }
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

    /// Waits for the next event: changes to the views attached to.
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
        match self.request(&Request::Resize { terminal, size })? {
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
