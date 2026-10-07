//! The protocol clackatoo clients use to talk to the daemon.
//!
//! A connection is a byte stream (a Unix socket today, other transports
//! later) carrying [frames](frame): a length prefix, then one message
//! serialized with `postcard`. The client sends [`ClientMessage`]s and the
//! daemon sends [`ServerMessage`]s.
//!
//! The client's first message must be [`ClientMessage::Hello`]. The daemon
//! answers with [`ServerMessage::Hello`], giving its own
//! [`PROTOCOL_VERSION`], and closes the connection if the versions differ.
//! Both `Hello`s are variant 0 of their enums and must stay that way. That
//! keeps them decodable across versions, so a version mismatch is always
//! reported as one, not as a garbled message.
//!
//! After that, the daemon answers each [`Request`], in order, with exactly
//! one [`Response`]. In between, it sends [`Event`]s: changes to the views
//! the client has attached to. A client acknowledges each view update with
//! [`ClientMessage::FrameDone`] once it has drawn it, and the daemon sends
//! no more updates for that view until then. Changes in the meantime are
//! merged into the next update, so a client gets updates as fast as it can
//! draw them and no faster, and a flood of output costs it one update per
//! frame.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod frame;
mod view;

use std::fmt;

pub use clackatoo_term::{Attributes, Color, InputModes, Size, Style, Underline};
use serde::{Deserialize, Serialize};

pub use view::{CursorState, GridNode, Line, Node, NodeId, Patch, Run, View, ViewUpdate};

/// The protocol version this crate speaks. Bump it on any incompatible
/// change to a message.
pub const PROTOCOL_VERSION: u32 = 3;

/// Everything a client sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// Opens the conversation. Must be the first message, and must stay
    /// variant 0 (see the crate docs).
    Hello {
        /// The client's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
    /// A request, which the daemon answers with one [`Response`].
    Request(Request),
    /// The client has drawn the last view update for `terminal`, and is
    /// ready for the next. Not answered.
    FrameDone {
        /// The terminal whose view was drawn.
        terminal: TerminalId,
    },
}

/// Everything the daemon sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Answers [`ClientMessage::Hello`]. Must stay variant 0 (see the crate
    /// docs).
    Hello {
        /// The daemon's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
    /// The answer to the oldest unanswered request.
    Response(Response),
    /// Something the client attached to changed.
    Event(Event),
}

/// Something the daemon tells a client without being asked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// The view of a terminal the client attached to changed.
    View {
        /// The terminal.
        terminal: TerminalId,
        /// The change.
        update: ViewUpdate,
    },
}

/// A message from a client to the daemon that expects an answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Lists every terminal.
    ListTerminals,
    /// Starts a terminal. Answered with [`Response::TerminalCreated`].
    NewTerminal(NewTerminal),
    /// Writes bytes to a terminal, as if typed.
    SendInput {
        /// The terminal to write to.
        terminal: TerminalId,
        /// The bytes, already encoded as a terminal expects them (`\r` for
        /// Enter, `\x1b[A` for Up…).
        bytes: Vec<u8>,
    },
    /// Asks for what a terminal is showing. Answered with [`Response::View`].
    Capture {
        /// The terminal to capture.
        terminal: TerminalId,
    },
    /// Closes a terminal. Its program gets a hangup signal, as when a
    /// terminal window closes.
    CloseTerminal {
        /// The terminal to close.
        terminal: TerminalId,
    },
    /// Changes a terminal's size. Its program is told, and the screen's
    /// lines are rewrapped to the new width.
    Resize {
        /// The terminal to resize.
        terminal: TerminalId,
        /// The new size.
        size: Size,
    },
    /// Starts sending the terminal's view: a snapshot, then a patch
    /// whenever it changes (see the crate docs). Attaching twice is
    /// harmless and sends a fresh snapshot.
    Attach {
        /// The terminal to follow.
        terminal: TerminalId,
    },
    /// Stops sending the terminal's view.
    Detach {
        /// The terminal to stop following.
        terminal: TerminalId,
    },
}

/// How to start a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTerminal {
    /// The terminal's size.
    pub size: Size,
    /// The program and its arguments. `None` runs the daemon's `$SHELL`.
    pub command: Option<Vec<String>>,
    /// The working directory. `None` uses the daemon's home directory.
    pub cwd: Option<String>,
    /// A file to record the terminal's output to, for tests and benchmarks
    /// (see `clackatoo_term::recording`). The path is on the daemon's machine.
    pub record_to: Option<String>,
}

/// The daemon's answer to a [`Request`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// The request failed.
    Error(Error),
    /// The request succeeded and has nothing to return.
    Done,
    /// Answers [`Request::ListTerminals`].
    Terminals(Vec<TerminalInfo>),
    /// Answers [`Request::NewTerminal`].
    TerminalCreated(TerminalId),
    /// Answers [`Request::Capture`].
    View(View),
}

/// Why the daemon refused or failed a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum Error {
    /// The connection's first message wasn't [`ClientMessage::Hello`].
    #[error("the first message must be Hello")]
    HelloRequired,
    /// No terminal has this ID. It may have been closed.
    #[error("no terminal {0}")]
    NoSuchTerminal(TerminalId),
    /// The terminal's program has exited, so it takes no more input.
    #[error("terminal {0} has exited")]
    TerminalExited(TerminalId),
    /// The terminal's program isn't reading its input, and too much is
    /// already queued for it.
    #[error("terminal {0} is not reading its input; too much is already queued")]
    InputBacklogFull(TerminalId),
    /// The terminal could not be started.
    #[error("could not start the terminal: {0}")]
    SpawnFailed(String),
    /// The output recording could not be set up.
    #[error("could not record to {path}: {reason}")]
    RecordingFailed {
        /// The requested recording path.
        path: String,
        /// What went wrong.
        reason: String,
    },
}

/// Identifies a terminal within one daemon. IDs are never reused while
/// the daemon runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TerminalId(pub u32);

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A summary of one terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// The terminal's ID.
    pub id: TerminalId,
    /// The process ID of the program the terminal started.
    pub pid: u32,
    /// The terminal's size.
    pub size: Size,
    /// The window title the program set, or empty.
    pub title: String,
    /// Whether the program is still running.
    pub status: TerminalStatus,
}

/// Whether a terminal's program is still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalStatus {
    /// The program is running.
    Running,
    /// The program exited with this status code.
    Exited(i32),
    /// The program was killed by this signal.
    Killed(i32),
}

impl fmt::Display for TerminalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Exited(code) => write!(f, "exited ({code})"),
            Self::Killed(signal) => write!(f, "killed (signal {signal})"),
        }
    }
}
