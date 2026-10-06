//! A blocking client for the daemon, for the CLI and tests.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use clackatoo_protocol::frame::{self, FrameDecoder, FrameError};
use clackatoo_protocol::{
    Error, NewTerminal, PROTOCOL_VERSION, Request, Response, Screen, TerminalId, TerminalInfo,
};

/// A connection to the daemon that sends one request at a time and waits for
/// each answer.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
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
        };
        match client.request(&Request::Hello {
            protocol_version: PROTOCOL_VERSION,
        })? {
            Response::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                Ok(client)
            }
            Response::Hello { protocol_version } => Err(ClientError::VersionMismatch {
                ours: PROTOCOL_VERSION,
                daemon: protocol_version,
            }),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
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
        self.stream.write_all(&frame::encode(request)?)?;
        let mut buffer = [0; 16 * 1024];
        loop {
            if let Some(response) = self.decoder.decode()? {
                return Ok(response);
            }
            match self.stream.read(&mut buffer) {
                Ok(0) => return Err(ClientError::Disconnected),
                Ok(n) => self.decoder.push(&buffer[..n]),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err.into()),
            }
        }
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
    pub fn capture(&mut self, terminal: TerminalId) -> Result<Screen, ClientError> {
        match self.request(&Request::Capture { terminal })? {
            Response::Screen(screen) => Ok(screen),
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
}

/// Converts an answer that doesn't fit the request into an error, unpacking
/// error answers.
fn unexpected(response: Response) -> ClientError {
    match response {
        Response::Error(err) => ClientError::Daemon(err),
        other => ClientError::UnexpectedResponse(Box::new(other)),
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
    /// The daemon answered with something that doesn't fit the request.
    #[error("the daemon sent an unexpected response: {0:?}")]
    UnexpectedResponse(Box<Response>),
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
