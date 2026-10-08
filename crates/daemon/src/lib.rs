//! The clackatoo daemon, and the client that talks to it.
//!
//! The daemon owns every terminal: the PTYs, the programs running on them,
//! and the grids their output builds. Windows, the CLI, remote viewers and
//! agents are all clients, speaking [`clackatoo_protocol`] over a Unix
//! socket, or over SSH to a daemon on another machine (see [`remote`]).
//! Terminals outlive any client, so closing a window closes nothing.

// The only unsafe code is the pre-exec hook in `pty::spawn` and the
// declaration of glibc's `malloc_trim` in `memory`, which opt out of this
// explicitly.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod buffer;
mod chat;
pub mod client;
mod handover;
mod hosted;
pub mod launch;
mod memory;
mod pty;
mod quic;
pub mod remote;
mod runtime;
mod server;
mod shell;
pub mod upgrade;
mod workspaces;

use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::path::PathBuf;

pub use client::{Client, ClientError, Receiver, Sender};
pub use quic::{Contact, Route};
pub use runtime::{DaemonLock, LockError, RuntimeDir};
pub use server::{Daemon, ShutdownHandle};

/// How the daemon starts terminals.
#[derive(Clone, Debug)]
pub struct DaemonConfig {
    /// Settings for every terminal's emulator.
    pub terminal: clackatoo_term::Config,
    /// The program a terminal runs when no command is given.
    pub shell: OsString,
    /// The working directory a terminal starts in when none is given.
    pub home: PathBuf,
    /// Where the scripts are that make shells mark their prompts and
    /// commands, which [`upgrade::serve`] writes. Without them, shells
    /// start as they are.
    pub shell_integration: Option<PathBuf>,
    /// The agent chats talk to, and its arguments: a program speaking ACP,
    /// the Agent Client Protocol, on its stdin and stdout.
    pub agent: Vec<OsString>,
    /// The UDP ports clients on other machines may move their QUIC
    /// connections onto: the first free one is taken, when the first
    /// connection over SSH arrives. 0 means any port. `None` keeps every
    /// connection inside SSH.
    pub udp_ports: Option<RangeInclusive<u16>>,
}

/// The agent chats talk to unless told otherwise: Claude Code, through
/// the adapter that speaks ACP for it, at the version tried with
/// clackatoo. `npx` fetches it the first time.
pub const DEFAULT_AGENT: &[&str] = &[
    "npx",
    "--yes",
    "--prefer-offline",
    "@agentclientprotocol/claude-agent-acp@0.88.0",
];

/// The UDP ports a daemon uses unless told otherwise: mosh's, so a
/// firewall opened for mosh lets clackatoo through too.
pub const DEFAULT_UDP_PORTS: RangeInclusive<u16> = 60000..=61000;

impl DaemonConfig {
    /// Defaults taken from the environment: `$SHELL` (or `/bin/sh`),
    /// `$HOME` (or `/`), `$CLACKATOO_AGENT`: a command, split at spaces
    /// (or [`DEFAULT_AGENT`]), and `$CLACKATOO_UDP_PORTS`: a port, a range
    /// such as `60000-61000` (the default), or `off`.
    #[must_use]
    pub fn from_env() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            terminal: clackatoo_term::Config::default(),
            shell: non_empty("SHELL").unwrap_or_else(|| "/bin/sh".into()),
            home: non_empty("HOME").map_or_else(|| "/".into(), PathBuf::from),
            shell_integration: None,
            agent: match non_empty("CLACKATOO_AGENT") {
                Some(command) => command
                    .to_string_lossy()
                    .split_whitespace()
                    .map(OsString::from)
                    .collect(),
                None => DEFAULT_AGENT.iter().map(OsString::from).collect(),
            },
            udp_ports: match non_empty("CLACKATOO_UDP_PORTS") {
                None => Some(DEFAULT_UDP_PORTS),
                Some(ports) if ports == "off" => None,
                Some(ports) => ports.to_str().and_then(parse_ports).or_else(|| {
                    log::warn!(
                        "CLACKATOO_UDP_PORTS isn't a port, a range or `off`: {}",
                        ports.display()
                    );
                    Some(DEFAULT_UDP_PORTS)
                }),
            },
        }
    }
}

/// A port, or a range of them.
fn parse_ports(ports: &str) -> Option<RangeInclusive<u16>> {
    let (first, last) = ports.split_once('-').unwrap_or((ports, ports));
    let (first, last) = (first.trim().parse().ok()?, last.trim().parse().ok()?);
    (first <= last).then_some(first..=last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udp_ports_are_one_or_a_range() {
        assert_eq!(parse_ports("60001"), Some(60001..=60001));
        assert_eq!(parse_ports("60000-61000"), Some(60000..=61000));
        assert_eq!(parse_ports("61000-60000"), None);
        assert_eq!(parse_ports("udp"), None);
    }
}
