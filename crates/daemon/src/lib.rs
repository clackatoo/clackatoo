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
mod agents;
mod buffer;
mod chat;
pub mod client;
mod explorer;
mod find;
mod git;
mod handover;
mod hosted;
pub mod launch;
mod lsp;
mod memory;
mod pty;
mod quic;
pub mod remote;
mod rpc;
mod runtime;
mod server;
mod shell;
pub mod upgrade;
mod workspaces;

use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::path::PathBuf;

pub use agents::{AgentConfig, CustomAgent};
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
    /// The agents chats can talk to: programs speaking ACP, the Agent
    /// Client Protocol, on their stdin and stdout.
    pub agents: AgentConfig,
    /// The UDP ports clients on other machines may move their QUIC
    /// connections onto: the first free one is taken, when the first
    /// connection over SSH arrives. 0 means any port. `None` keeps every
    /// connection inside SSH.
    pub udp_ports: Option<RangeInclusive<u16>>,
    /// Where the language packs are that buffers are parsed with, one
    /// directory each (see [`clackatoo_syntax::pack`]). `None` parses
    /// nothing.
    pub languages: Option<PathBuf>,
    /// Where language servers are installed, one directory each (see
    /// [`clackatoo_syntax::servers`]), and looked for when they aren't on
    /// `PATH`. `None` installs none, and offers none.
    pub servers: Option<PathBuf>,
    /// The file the languages are listed in that the answer to an offer
    /// to install them was no, so they aren't offered again.
    pub declined: Option<PathBuf>,
    /// How long a language server is kept with no files open in it, so
    /// coming back to its project soon doesn't start it again.
    pub server_idle: std::time::Duration,
    /// For tests: a file a rename's thread waits for once it has written
    /// the new text of the files not open beside them, before they're put
    /// in place, so a test can act while it's under way. `None`, but in
    /// tests.
    pub rename_gate: Option<PathBuf>,
    /// The home trash, as the freedesktop.org trash spec has it, where
    /// files deleted on its file system go. `None` has none: files there
    /// can only be deleted for good.
    pub trash: Option<PathBuf>,
}

/// How long a language server is kept with no files open in it, unless
/// told otherwise.
pub const DEFAULT_SERVER_IDLE: std::time::Duration = std::time::Duration::from_mins(5);

/// The UDP ports a daemon uses unless told otherwise: mosh's, so a
/// firewall opened for mosh lets clackatoo through too.
pub const DEFAULT_UDP_PORTS: RangeInclusive<u16> = 60000..=61000;

impl DaemonConfig {
    /// Defaults taken from the environment: `$SHELL` (or `/bin/sh`),
    /// `$HOME` (or `/`), `$CLACKATOO_AGENT`: the command of the agent a
    /// chat gets when it asks for none, split at spaces (or else Claude
    /// Code's adapter), `$CLACKATOO_UDP_PORTS`: a port, a range such as
    /// `60000-61000` (the default), or `off`, the language packs in
    /// [`languages_dir`], and agents added in `daemon/agents.toml` in
    /// [`config_dir`].
    #[must_use]
    pub fn from_env() -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        // Programs name this machine in `file://` links (see
        // `clackatoo_term::Config::local_host`).
        let local_host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        Self {
            terminal: clackatoo_term::Config {
                local_host,
                ..clackatoo_term::Config::default()
            },
            shell: non_empty("SHELL").unwrap_or_else(|| "/bin/sh".into()),
            home: non_empty("HOME").map_or_else(|| "/".into(), PathBuf::from),
            shell_integration: None,
            agents: AgentConfig {
                custom: (non_empty("CLACKATOO_AGENT").into_iter())
                    .map(|command| CustomAgent {
                        name: agents::DEFAULT.to_owned(),
                        title: None,
                        command: (command.to_string_lossy().split_whitespace())
                            .map(OsString::from)
                            .collect(),
                        starting_mode: None,
                    })
                    .collect(),
                file: config_dir().map(|dir| dir.join("daemon/agents.toml")),
                dir: languages_dir().map(|dir| dir.with_file_name("agents")),
                path: None,
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
            languages: languages_dir(),
            servers: languages_dir().map(|dir| dir.with_file_name("servers")),
            declined: languages_dir().map(|dir| dir.with_file_name("declined")),
            server_idle: DEFAULT_SERVER_IDLE,
            rename_gate: None,
            trash: data_dir().map(|dir| dir.join("Trash")),
        }
    }
}

/// Where language packs are installed: `clackatoo/languages` in
/// `$XDG_DATA_HOME`, or else in `~/.local/share`. Language servers are
/// installed beside it, in `servers`.
#[must_use]
pub fn languages_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("clackatoo/languages"))
}

/// Where programs keep their data: `$XDG_DATA_HOME`, or else
/// `~/.local/share`.
fn data_dir() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    non_empty("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

/// clackatoo's config directory: `clackatoo` in `$XDG_CONFIG_HOME`, or
/// else in `~/.config`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let config = non_empty("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("clackatoo"))
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
