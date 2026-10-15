//! Forwarding a port: a window on another machine passes the connections
//! made to a port on its own machine to one on the daemon's, as
//! `ssh -L` does.
//!
//! The window listens on its machine's loopback. For each connection it
//! takes, it opens a QUIC stream to the daemon of its own, beside the one
//! it talks over, and sends one message on it first:
//! [`Request::Forward`](crate::Request::Forward), naming where to connect.
//! The daemon connects, and answers with
//! [`Response::Done`](crate::Response::Done), or
//! [`Error::Forward`](crate::Error::Forward) saying why it couldn't. After
//! that, the stream carries the connection's bytes as they are, both
//! ways, until either side finishes it, or abandons it as a connection is
//! reset. New in protocol 82: a window asks
//! [`wire::daemon_has`](crate::wire::daemon_has) with
//! [`wire::FORWARDS`](crate::wire::FORWARDS) before opening one, as an
//! older daemon stops any stream but the first.
//!
//! The daemon sends what comes from the connection only as fast as the
//! link takes it without a queue growing ahead of keys, unless the window
//! asks otherwise ([`Forward::unpaced`], protocol 83).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::wire;

/// Where a forwarded connection goes, on the daemon's machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Forward {
    /// The machine: `localhost`, an address, or a name the daemon's
    /// machine looks up.
    pub host: String,
    /// The port there.
    pub port: u16,
    /// Send what comes from the connection as fast as it goes, even
    /// filling a slow link's queue ahead of keys, rather than as
    /// background traffic, as the window's `forwardpace` option off asks.
    /// `false` from a window older than protocol 83, whose forwards the
    /// daemon paces.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::FORWARD_PACE }, _>",
        deserialize_with = "wire::since::<{ wire::FORWARD_PACE }, _, _>"
    )]
    pub unpaced: bool,
}

/// Why the daemon couldn't make a forwarded connection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForwardFailure {
    /// Nothing is listening there: the connection was refused.
    Refused,
    /// The daemon's machine couldn't find the host by that name.
    NoSuchHost,
    /// Nothing answered in time.
    TimedOut,
    /// Something else, in words.
    Other(String),
}

impl fmt::Display for ForwardFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => f.write_str("nothing is listening there"),
            Self::NoSuchHost => f.write_str("no machine has that name"),
            Self::TimedOut => f.write_str("nothing answered in time"),
            Self::Other(why) => f.write_str(why),
        }
    }
}
