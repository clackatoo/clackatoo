//! The clackatoo daemon, and the client that talks to it.
//!
//! The daemon owns every terminal: the PTYs, the programs running on them,
//! and the grids their output builds. Windows, the CLI, remote viewers and
//! agents are all clients, speaking [`clackatoo_protocol`] over a Unix
//! socket, or over SSH to a daemon on another machine (see [`remote`]).
//! Terminals outlive any client, so closing a window closes nothing.

// The only unsafe code is the pre-exec hooks in `pty::spawn` and
// `browser::cdp::spawn`, and the declaration of glibc's `malloc_trim` in
// `memory`, which opt out of this explicitly.
#![deny(unsafe_code)]
#![warn(missing_docs)]

mod agent;
mod agents;
mod boards;
mod browser;
mod buffer;
mod changes;
mod chat;
pub mod client;
mod explorer;
mod find;
mod git;
mod handoff;
mod handover;
mod hosted;
pub mod launch;
mod lsp;
pub mod mcp;
mod memory;
mod profiles;
mod pty;
mod quic;
pub mod remote;
mod rpc;
mod runtime;
mod server;
mod shell;
pub mod upgrade;
pub mod web;
mod workspaces;

use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::path::PathBuf;

pub use agents::{AgentConfig, CustomAgent};
pub use browser::{BrowserConfig, check_browser, snap_of};
pub use client::{Client, ClientError, Receiver, Sender, UdpTarget, random_window};
pub use quic::{Contact, Redial, Redialed, Route};
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
    /// The program, and its arguments, that agents start to reach the
    /// tools clackatoo gives them (see [`mcp`]): `clackatoo mcp`, which
    /// is given `--chat ID --socket PATH` after them. `None` gives agents
    /// no tools.
    pub mcp: Option<Vec<OsString>>,
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
    /// How long a command an agent runs waits for its shell's first
    /// prompt, when the shell marks its prompts, before the agent is told
    /// the shell never became ready.
    pub prompt_wait: std::time::Duration,
    /// How long an agent asked what models it has, for a chat's model
    /// menu, has to say, before it's given up on and stopped.
    pub probe_wait: std::time::Duration,
    /// How long a chat an agent started, in the background, keeps its
    /// session once it has finished, before letting it go until its next
    /// message.
    pub park_after: std::time::Duration,
    /// For tests: a file a rename's thread waits for once it has written
    /// the new text of the files not open beside them, before they're put
    /// in place, so a test can act while it's under way. `None`, but in
    /// tests.
    pub rename_gate: Option<PathBuf>,
    /// The home trash, as the freedesktop.org trash spec has it, where
    /// files deleted on its file system go. `None` has none: files there
    /// can only be deleted for good.
    pub trash: Option<PathBuf>,
    /// Where boards are kept, one file each, from one run of the daemon
    /// to the next (see `boards::disk`). `None` keeps none: there are no
    /// boards.
    pub boards: Option<PathBuf>,
    /// The browser agents drive for chats whose Browser setting is on
    /// (see `browser`). `None` gives them none.
    pub browser: Option<BrowserConfig>,
    /// The web app phones reach the daemon through, if it's on (see
    /// [`web`]).
    pub web: Option<web::WebConfig>,
}

/// How long a language server is kept with no files open in it, unless
/// told otherwise.
pub const DEFAULT_SERVER_IDLE: std::time::Duration = std::time::Duration::from_mins(5);

/// How long a command an agent runs waits for its shell's first prompt,
/// unless told otherwise: zsh with plugins can take seconds to start.
pub const DEFAULT_PROMPT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long an agent asked what models it has gets to say, unless the
/// daemon is told otherwise.
pub const DEFAULT_PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a chat an agent started keeps its session once it has
/// finished in the background, unless the daemon is told otherwise.
/// Claude Code's adapter runs a process of about 250 MB for each
/// session; opening it again takes about half a second, with the chat's
/// next message.
pub const DEFAULT_PARK_AFTER: std::time::Duration = std::time::Duration::from_mins(2);

/// How long the browser keeps running with no chat using it, unless
/// told otherwise: about 150 MB, and a second to start again.
pub const DEFAULT_BROWSER_IDLE: std::time::Duration = std::time::Duration::from_mins(5);

/// The UDP ports a daemon uses unless told otherwise: mosh's, so a
/// firewall opened for mosh lets clackatoo through too.
pub const DEFAULT_UDP_PORTS: RangeInclusive<u16> = 60000..=61000;

impl DaemonConfig {
    /// Defaults taken from the environment: `$CLACKATOO_BOARDS`, where
    /// boards are kept, or `off` for none (or else `clackatoo/boards` in
    /// the data directory), `$SHELL` (or `/bin/sh`),
    /// `$HOME` (or `/`), `$CLACKATOO_AGENT`: the command of the agent a
    /// chat gets when it asks for none, split at spaces (or else Claude
    /// Code's adapter), `$CLACKATOO_UDP_PORTS`: a port, a range such as
    /// `60000-61000` (the default), or `off`, the language packs in
    /// [`languages_dir`], agents added in `daemon/agents.toml` in
    /// [`config_dir`], and `$CLACKATOO_BROWSER`: the browser agents drive,
    /// or `off` for none (or else Chromium or Chrome, as found on `PATH`),
    /// with its profile in `$CLACKATOO_BROWSER_PROFILE` (or else
    /// `clackatoo/browser` in the data directory, or for a snap, in its
    /// own folder), and the web app as `daemon/web.toml` in [`config_dir`]
    /// says, keeping its devices in `clackatoo/web` in the data directory.
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
            // Only the `clackatoo` program knows it can relay them.
            mcp: None,
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
            prompt_wait: DEFAULT_PROMPT_WAIT,
            probe_wait: DEFAULT_PROBE_WAIT,
            park_after: DEFAULT_PARK_AFTER,
            rename_gate: None,
            trash: data_dir().map(|dir| dir.join("Trash")),
            boards: match non_empty("CLACKATOO_BOARDS") {
                Some(off) if off == "off" => None,
                Some(dir) => Some(PathBuf::from(dir)),
                None => data_dir().map(|dir| dir.join("clackatoo/boards")),
            },
            browser: match non_empty("CLACKATOO_BROWSER") {
                Some(off) if off == "off" => None,
                program => Some(BrowserConfig {
                    program: program.map(PathBuf::from),
                    profile: non_empty("CLACKATOO_BROWSER_PROFILE").map(PathBuf::from),
                    data: data_dir().map(|dir| dir.join("clackatoo/browser")),
                    home: non_empty("HOME").map_or_else(|| "/".into(), PathBuf::from),
                    idle: DEFAULT_BROWSER_IDLE,
                }),
            },
            web: config_dir().zip(data_dir()).and_then(|(config, data)| {
                web::config::read(&config.join("daemon/web.toml"), data.join("clackatoo/web"))
            }),
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

/// clackatoo's state directory, for what it keeps between runs that isn't
/// worth backing up, such as windows' logs: `clackatoo` in
/// `$XDG_STATE_HOME`, or else in `~/.local/state`.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let state = non_empty("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("clackatoo"))
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
