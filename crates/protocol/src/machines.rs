//! Machines: which daemon is which, and the connections one daemon makes
//! to the daemons on other machines.
//!
//! Every daemon has an identity of its own ([`Machine`]): an ID chosen at
//! random the first time it starts, and kept from then on, a name for
//! people, its machine's hostname unless told otherwise, and a number for
//! the run it's in, which tells a copy of a machine's state from the
//! machine itself. A daemon says its
//! own in [`Workspaces::machine`](crate::Workspaces::machine), and each
//! workspace and pane can say which daemon it lives on, its *home*
//! ([`Workspace::home`](crate::Workspace::home),
//! [`Pane::home`](crate::Pane::home)), so a window, or another daemon,
//! always knows whose it is.
//!
//! A daemon can connect to the daemon on another machine as a client of
//! it, as a window does with `--remote`: over ssh, then QUIC inside it,
//! moving onto UDP. [`Request::Connect`](crate::Request::Connect) asks it
//! to, [`Request::Disconnect`](crate::Request::Disconnect) to stop, and
//! [`Request::WatchConnections`](crate::Request::WatchConnections) lists
//! the connections, and has the client told whenever one changes
//! ([`Event::Connections`](crate::Event::Connections)).
//!
//! ssh may ask a question as it logs in: a password, a key's passphrase,
//! or whether to trust a machine it hasn't met. The daemon passes it to
//! a window that said it answers them, the one that asked for the
//! connection if it can
//! ([`Event::Login`](crate::Event::Login)), and the answer goes back to
//! ssh ([`ClientMessage::Login`](crate::ClientMessage::Login)), never kept
//! or written down. New in protocol 91: a client asks
//! [`wire::daemon_has`](crate::wire::daemon_has) with
//! [`wire::FEDERATION`](crate::wire::FEDERATION) before sending any of it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::wire;

/// A daemon's ID: 16 bytes chosen at random the first time it starts,
/// and kept from then on, through upgrades and restarts. Written as 32
/// hex digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId(pub [u8; 16]);

impl fmt::Display for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MachineId({self})")
    }
}

impl std::str::FromStr for MachineId {
    type Err = BadMachineId;

    /// Reads the 32 hex digits [`Display`](fmt::Display) writes.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.as_bytes();
        // Hex digits alone: `from_str_radix` would take a sign too.
        if digits.len() != 32 || !digits.iter().all(u8::is_ascii_hexdigit) {
            return Err(BadMachineId);
        }
        let mut id = [0; 16];
        for (byte, pair) in id.iter_mut().zip(digits.chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| BadMachineId)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| BadMachineId)?;
        }
        Ok(Self(id))
    }
}

/// Text that isn't a [`MachineId`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a machine's ID is 32 hex digits")]
pub struct BadMachineId;

/// Which daemon a daemon is: its ID, and its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    /// Its ID, which never changes.
    pub id: MachineId,
    /// Its name, for people: its machine's hostname, such as
    /// `jack-ubuntu-desktop`, unless it was given another. Never empty,
    /// one line, with no control characters. Not unique, and it may
    /// change: tell daemons apart by their IDs.
    pub name: String,
    /// A number the daemon chose at random when it last started afresh,
    /// kept through upgrades. Two daemons with one ID and different
    /// numbers are two copies of one machine's state, as a copied home
    /// folder makes, not one daemon reached two ways.
    pub run: u64,
}

/// One of a daemon's connections to the daemon on another machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    /// The machine, as ssh names it, and as the connection was asked
    /// for: `host`, `user@host`, `host:port`, or a name from the ssh
    /// config. Each daemon has one connection per host. For one to a
    /// machine another daemon passes on, which wasn't asked for here,
    /// what that daemon calls it, without the user ssh logs in as; or, if
    /// another connection has that name, the same with a number after
    /// it, as `server~2`.
    pub host: String,
    /// Which daemon is there, once it has been reached: `None` before,
    /// and from a daemon older than protocol 91, which doesn't say.
    pub machine: Option<Machine>,
    /// How it's going.
    pub state: ConnectionState,
    /// The machines it goes through, nearest first, by what they're
    /// called here: none while it goes straight there, with ssh. A
    /// connection to a machine a connected daemon passes on goes through
    /// that daemon; one asked for here goes through another only while
    /// ssh can't get there itself. Empty from a daemon older than
    /// protocol 95, whose connections all go straight there.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::CHAIN }, _>",
        deserialize_with = "wire::since::<{ wire::CHAIN }, _, _>"
    )]
    pub through: Vec<String>,
    /// The daemon doesn't pass the machine on to the daemons connected to
    /// it, as asked (see [`Request::Connect`](crate::Request::Connect)).
    /// `false` from a daemon older than protocol 95, which passes nothing
    /// on.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::CHAIN }, _>",
        deserialize_with = "wire::since::<{ wire::CHAIN }, _, _>"
    )]
    pub unshared: bool,
}

/// A machine a daemon passes on to the daemons connected to it, as one of
/// its own connections: they may show its workspaces, by connecting to it
/// through this daemon (see [`Request::Reach`](crate::Request::Reach)),
/// unless they reach it a shorter way. See
/// [`Workspaces::passed`](crate::Workspaces::passed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Passed {
    /// Its ID.
    pub id: MachineId,
    /// What the daemon passing it on calls it: its connection's host
    /// there, made safe to show.
    pub name: String,
    /// The run of its daemon, as that daemon last met it (see
    /// [`Machine::run`]).
    pub run: u64,
    /// The machines that daemon's connection to it goes through, nearest
    /// first: none when it goes straight there. A daemon never takes a
    /// way back through itself, which would go round in a loop.
    pub through: Vec<MachineId>,
    /// Whether that daemon is connected to it now.
    pub reached: bool,
    /// The identity key that daemon knows the machine by (see
    /// [`Response::Carrying`](crate::Response::Carrying)), if it knows
    /// one: what a daemon that has never met the machine checks it by,
    /// the first time it reaches it through that daemon, which so vouches
    /// for it. With none, it can't be reached through that daemon.
    pub key: Option<[u8; 32]>,
}

/// How a connection to the daemon on another machine is going.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionState {
    /// Logging in with ssh, and greeting the daemon there.
    Connecting,
    /// ssh asked a question as it logged in, such as for a password, and
    /// a window was asked it: waiting for the answer.
    Asking,
    /// ssh asked a question as it logged in, and no window that answers
    /// them was there to ask: it waits for one, and tries again once one
    /// says it answers them.
    NeedsPassword,
    /// Connected, at this protocol version: the older of the two daemons'.
    Connected {
        /// The protocol version.
        wire: u32,
    },
    /// The connection failed, or was lost, for this reason: it's tried
    /// again in a while, less and less often.
    Down {
        /// Why, in words.
        reason: String,
    },
    /// Not tried again until asked to connect again: the question ssh
    /// asked was turned down, the daemon there can't be talked to, or it's
    /// this daemon itself.
    Stopped {
        /// Why, in words.
        reason: String,
    },
    /// A state newer than the client, which a newer daemon sent in place
    /// of it.
    Other,
}

/// An answer to a question ssh asked, such as a password. Never shown in
/// debug output, or logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret(pub String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_written_and_read_as_hex() {
        let id = MachineId([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0xff, 0x10, 0x20, 0x30, 0x40, 0x50,
            0x60,
        ]);
        let text = id.to_string();
        assert_eq!(text, "0123456789abcdef00ff102030405060");
        assert_eq!(text.parse(), Ok(id));
        assert_eq!(text.to_uppercase().parse(), Ok(id));
        for bad in [
            "",
            "0123",
            &format!("{text}0"),
            "zz23456789abcdef00ff102030405060",
        ] {
            assert_eq!(bad.parse::<MachineId>(), Err(BadMachineId), "{bad}");
        }
        // Not `+1`, which from_str_radix takes.
        assert!(
            "+123456789abcdef00ff102030405060"
                .parse::<MachineId>()
                .is_err()
        );
    }

    #[test]
    fn secrets_never_show() {
        let shown = format!("{:?}", Secret("hunter2".into()));
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}
