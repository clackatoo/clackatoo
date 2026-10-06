//! The protocol clackatoo clients use to talk to the daemon.
//!
//! A connection is a byte stream (a Unix socket today, other transports
//! later) carrying [frames](frame): a length prefix, then one message
//! serialized with `postcard`. The client sends [`Request`]s, and the daemon
//! answers each one, in order, with exactly one [`Response`].
//!
//! The first request on every connection must be [`Request::Hello`]. The
//! daemon answers with [`Response::Hello`], giving its own
//! [`PROTOCOL_VERSION`], and closes the connection if the versions differ.
//! Both `Hello`s are variant 0 of their enums and must stay that way. That
//! keeps them decodable across versions, so a version mismatch is always
//! reported as one, not as a garbled message.
//!
//! The crate does no I/O, so a browser client compiled to WebAssembly can
//! use it as-is.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod frame;
mod screen;

use std::fmt;

pub use clackatoo_term::{Attributes, Color, Size, Style, Underline};
use serde::{Deserialize, Serialize};

pub use screen::{CursorState, Line, Run, Screen};

/// The protocol version this crate speaks. Bump it on any incompatible
/// change to a message.
pub const PROTOCOL_VERSION: u32 = 1;

/// A message from a client to the daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Opens the conversation. Must be the first request, and must stay
    /// variant 0 (see the crate docs).
    Hello {
        /// The client's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
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
    /// Asks for what a terminal is showing. Answered with [`Response::Screen`].
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

/// A message from the daemon to a client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Answers [`Request::Hello`]. Must stay variant 0 (see the crate docs).
    Hello {
        /// The daemon's [`PROTOCOL_VERSION`].
        protocol_version: u32,
    },
    /// The request failed.
    Error(Error),
    /// The request succeeded and has nothing to return.
    Done,
    /// Answers [`Request::ListTerminals`].
    Terminals(Vec<TerminalInfo>),
    /// Answers [`Request::NewTerminal`].
    TerminalCreated(TerminalId),
    /// Answers [`Request::Capture`].
    Screen(Screen),
}

/// Why the daemon refused or failed a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum Error {
    /// The connection's first request wasn't [`Request::Hello`].
    #[error("the first request must be Hello")]
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
