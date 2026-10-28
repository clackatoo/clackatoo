//! The clackatoo daemon, and the client that talks to it.
//!
//! The daemon owns every terminal: the PTYs, the programs running on them,
//! and the grids their output builds. Windows, the CLI, remote viewers and
//! agents are all clients, speaking [`clackatoo_protocol`] over a Unix
//! socket, or over SSH to a daemon on another machine (see [`remote`]).
//! Terminals outlive any client, so closing a window closes nothing.

// The only unsafe code is the pre-exec hooks in `pty::spawn` and
// `browser::cdp::spawn`, the declaration of glibc's `malloc_trim` in
// `memory`, and the signal handling in `signals`, which opt out of this
// explicitly.
#![deny(unsafe_code)]
#![warn(missing_docs)]
// The list of the tools agents get is one long `json!`.
#![recursion_limit = "256"]

mod agent;
mod agents;
pub mod antigravity;
mod boards;
mod browser;
mod buffer;
mod changes;
mod chat;
pub mod client;
pub mod display;
mod explorer;
mod find;
pub mod forward;
mod git;
mod group;
mod handoff;
mod handover;
mod hosted;
mod known;
pub mod launch;
mod lsp;
mod machine;
pub mod mcp;
mod memory;
mod ports;
mod profiles;
mod provider_usage;
mod pty;
mod quic;
mod recovery;
pub mod remote;
mod rpc;
mod runtime;
mod saved;
mod server;
mod shell;
mod signals;
mod tool_images;
pub mod upgrade;
pub mod web;
mod workspaces;

use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub use agents::{AgentConfig, CustomAgent};
pub use browser::{BrowserConfig, check_browser, snap_of};
pub use client::{Client, ClientError, Receiver, Sender, UdpTarget, random_window};
pub use display::DisplayConfig;
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
    /// A file every message to and from language servers is written to,
    /// a line each, stamped with the time and the server, to see what was
    /// said. `None` writes none.
    pub lsp_trace: Option<PathBuf>,
    /// How long a language server is kept with no files open in it, so
    /// coming back to its project soon doesn't start it again.
    pub server_idle: std::time::Duration,
    /// How long a command an agent runs waits for its shell's first
    /// prompt, when the shell marks its prompts, before the agent is told
    /// the shell never became ready.
    pub prompt_wait: std::time::Duration,
    /// How long a command kept running always waits to run again once it
    /// ends: twice as long each time it ends again soon after it started,
    /// up to a minute. A setting for tests: [`DEFAULT_KEEP_WAIT`] for the
    /// rest.
    pub keep_wait: std::time::Duration,
    /// How long an agent asked what models it has, for a chat's model
    /// menu, or for the sessions it kept, for the start page, has to say,
    /// before it's given up on and stopped, unless a chat needs it.
    pub probe_wait: std::time::Duration,
    /// How long an agent asked for a chat's session has to open it before
    /// the chat says it hasn't, and what it may be waiting on, and offers
    /// to start it again. The chat goes on waiting.
    pub session_wait: std::time::Duration,
    /// How long an agent that took a turn by itself, without saying when
    /// it ends, is quiet, with no tool running, before the turn is taken
    /// to have ended (see `chat/woken.rs`).
    pub quiet_turn: std::time::Duration,
    /// A setting for tests: how long an agent or language server that has
    /// gone is kept for what it sent last, from when it went or the
    /// parser's thread last got on with its messages (see
    /// `rpc::Inbox::gone_due`); never less than ten seconds while the
    /// thread is busy with them. One that exits is let go as soon as what
    /// it sent has been read. Tests lengthen it, so a program that has gone
    /// is kept until it exits. It's kept between [`DEFAULT_GONE_WAIT`] and
    /// a day.
    pub gone_wait: std::time::Duration,
    /// How long a chat an agent started, in the background, keeps its
    /// session once it has finished, before letting it go until its next
    /// message.
    pub park_after: std::time::Duration,
    /// How long a terminal stays fitted to a client, as a phone fits one,
    /// unless the client asks for the fit again: see `Request::Fit`.
    pub fit_lasts: std::time::Duration,
    /// For tests: a file a rename's thread waits for once it has written
    /// the new text of the files not open beside them, before they're put
    /// in place, so a test can act while it's under way. `None`, but in
    /// tests.
    pub rename_gate: Option<PathBuf>,
    /// The file the workspaces are kept in from one run of the daemon to
    /// the next: written as they change, and read by
    /// [`Daemon::restore_workspaces`] as a daemon starts afresh. `None`
    /// keeps none.
    pub workspaces: Option<PathBuf>,
    /// For tests: a file the thread writing the workspaces waits for
    /// before each write, so a test can hold it up, as a stuck disk would.
    /// `None`, but in tests.
    pub keep_gate: Option<PathBuf>,
    /// The home trash, as the freedesktop.org trash spec has it, where
    /// files deleted on its file system go. `None` has none: files there
    /// can only be deleted for good.
    pub trash: Option<PathBuf>,
    /// Where boards are kept, one file each, from one run of the daemon
    /// to the next (see `boards::disk`). `None` keeps none: there are no
    /// boards.
    pub boards: Option<PathBuf>,
    /// How buffers' changes that aren't saved are kept on disk, to outlive
    /// a crash or a stop (see `recovery`). `None` keeps none.
    pub recovery: Option<RecoveryConfig>,
    /// The browser agents drive for chats whose Browser setting is on
    /// (see `browser`). `None` gives them none.
    pub browser: Option<BrowserConfig>,
    /// The web app phones reach the daemon through, if it's on (see
    /// [`web`]).
    pub web: Option<web::WebConfig>,
    /// The file this daemon's identity is kept in, its ID chosen at random
    /// the first time it starts (see
    /// [`Machine`](clackatoo_protocol::Machine)). `None` chooses a new one
    /// each time it starts, as tests do.
    pub machine: Option<PathBuf>,
    /// The name this daemon goes by, for people. `None` takes the
    /// machine's hostname.
    pub machine_name: Option<String>,
    /// How the daemon connects to the daemons on other machines (see
    /// [`Request::Connect`](clackatoo_protocol::Request::Connect)). `None`
    /// connects to none: it's refused.
    pub links: Option<LinkConfig>,
    /// How the programs the daemon starts follow the desktop's display as
    /// you log in and out (see [`display`]). `None` leaves them the
    /// daemon's own environment.
    pub display: Option<DisplayConfig>,
}

/// How the daemon connects to the daemons on other machines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkConfig {
    /// The program run as ssh.
    pub ssh: OsString,
    /// The program ssh runs to ask its questions, such as for a password
    /// (`SSH_ASKPASS`), which passes each to the daemon, to ask a window:
    /// `clackatoo` itself, which does when run with
    /// [`remote::ASKPASS_VAR`] set. `None` leaves ssh to ask on a
    /// terminal, which the daemon hasn't, so it gets no answer.
    pub askpass: Option<PathBuf>,
    /// How often a better way than the one a connection goes is tried,
    /// while one may be there: ssh straight there, for a connection that
    /// goes through another machine meanwhile ([`DEFAULT_BETTER_EVERY`]).
    pub better_every: std::time::Duration,
    /// For tests: the protocol the connections say they speak, as a build
    /// of that version would, if it's older than this build's. `None` in
    /// use.
    pub speaks: Option<u32>,
    /// For tests: a stream asking to reach another machine through this
    /// daemon is answered by this daemon itself, in that machine's place,
    /// signing its certificate as that machine with its own key, as a
    /// daemon in the way could. `false` in use.
    #[doc(hidden)]
    pub impostor: bool,
}

/// How often a better way for a connection to go is tried, unless the
/// configuration says: see [`LinkConfig::better_every`].
pub const DEFAULT_BETTER_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// How buffers' changes that aren't saved are kept on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// The directory they're kept in, a file per buffer.
    pub dir: PathBuf,
    /// How long after its last change a buffer's file is written: one
    /// write for a burst of typing.
    pub after: std::time::Duration,
    /// The longest a change waits to be written while changes keep coming.
    pub most: std::time::Duration,
    /// For tests: a file the thread writing them waits for before each
    /// write, so a test can act while one is held up. `None`, but in tests.
    pub gate: Option<PathBuf>,
}

impl RecoveryConfig {
    /// Kept in `dir`, written as usual.
    #[must_use]
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            after: DEFAULT_RECOVERY_AFTER,
            most: DEFAULT_RECOVERY_MOST,
            gate: None,
        }
    }
}

/// How long after its last change a buffer's changes not saved are
/// written to its recovery file, unless told otherwise.
pub const DEFAULT_RECOVERY_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// The longest changes not saved wait to be written to their recovery file
/// while changes keep coming, unless told otherwise.
pub const DEFAULT_RECOVERY_MOST: std::time::Duration = std::time::Duration::from_secs(4);

/// How long a language server is kept with no files open in it, unless
/// told otherwise.
pub const DEFAULT_SERVER_IDLE: std::time::Duration = std::time::Duration::from_mins(5);

/// How long a command an agent runs waits for its shell's first prompt,
/// unless told otherwise: zsh with plugins can take seconds to start.
pub const DEFAULT_PROMPT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a command kept running always waits to run again once it
/// ends, at first (see [`DaemonConfig::keep_wait`]).
pub const DEFAULT_KEEP_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// How long an agent asked what models it has gets to say, unless the
/// daemon is told otherwise.
pub const DEFAULT_PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// How long an agent asked for a chat's session has before the chat says
/// it's slow to open it, unless the daemon is told otherwise. Codex opens
/// one in about a second when nothing holds it up.
pub const DEFAULT_SESSION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long an agent that took a turn by itself is quiet before the turn
/// is taken to have ended, if it doesn't say: Claude Code's adapter says,
/// to a client that asks for its background tasks, so this is for others
/// only. Long, as a turn ended too soon splits in two when its agent goes
/// on.
pub const DEFAULT_QUIET_TURN: std::time::Duration = std::time::Duration::from_mins(1);

/// How long a program that has gone is kept for what it sent last, unless
/// the daemon is told otherwise (see [`DaemonConfig::gone_wait`]).
pub const DEFAULT_GONE_WAIT: std::time::Duration = crate::rpc::PARSE_WAIT;

/// How long a chat an agent started keeps its session once it has
/// finished in the background, unless the daemon is told otherwise.
/// Claude Code's adapter runs a process of about 250 MB for each
/// session; opening it again takes about half a second, with the chat's
/// next message.
pub const DEFAULT_PARK_AFTER: std::time::Duration = std::time::Duration::from_mins(2);

/// How long a terminal stays fitted to a client unless it's asked for
/// again: the web app asks every half minute while it's shown, so a phone
/// put away, whose connection may linger, lets its fit go within a minute.
pub const DEFAULT_FIT_LASTS: std::time::Duration = std::time::Duration::from_mins(1);

/// How long the browser keeps running with no chat using it, unless
/// told otherwise: about 150 MB, and a second to start again.
pub const DEFAULT_BROWSER_IDLE: std::time::Duration = std::time::Duration::from_mins(5);

/// The UDP ports a daemon uses unless told otherwise: mosh's, so a
/// firewall opened for mosh lets clackatoo through too.
pub const DEFAULT_UDP_PORTS: RangeInclusive<u16> = 60000..=61000;

impl DaemonConfig {
    /// Defaults taken from the environment: `$CLACKATOO_BOARDS`, where
    /// boards are kept, or `off` for none (or else `clackatoo/boards` in
    /// the data directory), `$CLACKATOO_RECOVERY`, where changes not saved
    /// are kept, or `off` for nowhere (or else `recovery` in
    /// [`state_dir`]), `$CLACKATOO_WORKSPACES`, the file the workspaces are
    /// kept in, or `off` for none (or else `workspaces` in
    /// [`state_dir`]), `$SHELL` (or `/bin/sh`),
    /// `$HOME` (or `/`), `$CLACKATOO_MACHINE`, the file this daemon's ID is
    /// kept in, or `off` for a new one each start (or else `machine` in
    /// [`state_dir`]), the name `daemon/machine.toml` in [`config_dir`]
    /// gives it, `$CLACKATOO_SSH`: the program run as ssh to connect to
    /// other daemons (or else `ssh`), `$CLACKATOO_AGENT`: the command of the agent a
    /// chat gets when it asks for none, split at spaces (or else Claude
    /// Code's adapter), `$CLACKATOO_MCP`: the command, split at spaces,
    /// agents run to reach clackatoo's tools, given `--chat ID --socket
    /// PATH` after it (or else none: the `clackatoo` program sets its own),
    /// `$CLACKATOO_UDP_PORTS`: a port, a range such as
    /// `60000-61000` (the default), or `off`, the language packs in
    /// [`languages_dir`], agents added in `daemon/agents.toml` in
    /// [`config_dir`], and `$CLACKATOO_BROWSER`: the browser agents drive,
    /// or `off` for none (or else Chromium or Chrome, as found on `PATH`),
    /// with its profile in `$CLACKATOO_BROWSER_PROFILE` (or else
    /// `clackatoo/browser` in the data directory, or for a snap, in its
    /// own folder), the web app as `daemon/web.toml` in [`config_dir`]
    /// says, keeping its devices in `clackatoo/web` in the data directory,
    /// `$CLACKATOO_LSP_TRACE`: a file to trace language servers'
    /// messages to (or else none), and `$CLACKATOO_DISPLAY`: `off` to
    /// leave the programs the daemon starts its own display, rather than
    /// follow the desktop's login (see [`display`]).
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "one field each")]
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
                        tasks: None,
                    })
                    .collect(),
                file: config_dir().map(|dir| dir.join("daemon/agents.toml")),
                dir: languages_dir().map(|dir| dir.with_file_name("agents")),
                path: None,
            },
            // Only the `clackatoo` program knows it can relay them, unless
            // the environment names a program that can (tests do).
            mcp: non_empty("CLACKATOO_MCP").map(|command| {
                (command.to_string_lossy().split_whitespace())
                    .map(OsString::from)
                    .collect()
            }),
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
            lsp_trace: non_empty("CLACKATOO_LSP_TRACE").map(PathBuf::from),
            server_idle: DEFAULT_SERVER_IDLE,
            prompt_wait: DEFAULT_PROMPT_WAIT,
            keep_wait: DEFAULT_KEEP_WAIT,
            probe_wait: DEFAULT_PROBE_WAIT,
            session_wait: DEFAULT_SESSION_WAIT,
            quiet_turn: DEFAULT_QUIET_TURN,
            gone_wait: DEFAULT_GONE_WAIT,
            park_after: DEFAULT_PARK_AFTER,
            fit_lasts: DEFAULT_FIT_LASTS,
            rename_gate: None,
            workspaces: match non_empty("CLACKATOO_WORKSPACES") {
                Some(off) if off == "off" => None,
                Some(path) => Some(PathBuf::from(path)),
                None => state_dir().map(|dir| dir.join("workspaces")),
            },
            keep_gate: None,
            trash: data_dir().map(|dir| dir.join("Trash")),
            boards: match non_empty("CLACKATOO_BOARDS") {
                Some(off) if off == "off" => None,
                Some(dir) => Some(PathBuf::from(dir)),
                None => data_dir().map(|dir| dir.join("clackatoo/boards")),
            },
            recovery: match non_empty("CLACKATOO_RECOVERY") {
                Some(off) if off == "off" => None,
                // Only from somewhere in particular, as a relative path would
                // be from wherever the daemon happened to start.
                Some(dir) if Path::new(&dir).is_absolute() => {
                    Some(RecoveryConfig::in_dir(PathBuf::from(dir)))
                }
                Some(dir) => {
                    log::warn!(
                        "CLACKATOO_RECOVERY isn't an absolute path or `off`: {}",
                        dir.display()
                    );
                    state_dir().map(|dir| RecoveryConfig::in_dir(dir.join("recovery")))
                }
                None => state_dir().map(|dir| RecoveryConfig::in_dir(dir.join("recovery"))),
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
            machine: match non_empty("CLACKATOO_MACHINE") {
                Some(off) if off == "off" => None,
                Some(path) => Some(PathBuf::from(path)),
                None => state_dir().map(|dir| dir.join("machine")),
            },
            machine_name: config_dir().and_then(|config| machine::configured_name(&config)),
            links: Some(LinkConfig {
                ssh: remote::ssh_program(),
                // Only the `clackatoo` program knows it can ask.
                askpass: None,
                better_every: DEFAULT_BETTER_EVERY,
                speaks: None,
                impostor: false,
            }),
            display: match non_empty("CLACKATOO_DISPLAY") {
                Some(off) if off == "off" => None,
                other => {
                    if let Some(other) = other {
                        log::warn!("CLACKATOO_DISPLAY isn't `off`: {}", other.display());
                    }
                    let shell = RuntimeDir::for_current_user().path().join("shell");
                    Some(DisplayConfig::from_env(Some(shell.join("display"))))
                }
            },
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
    state_dir_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// clackatoo's state directory, given `$XDG_STATE_HOME` and `$HOME`: a
/// relative `$XDG_STATE_HOME` is ignored, as the XDG spec says, since it
/// would be from wherever the program happened to start.
fn state_dir_from(state: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let state = (state.map(PathBuf::from))
        .filter(|state| state.is_absolute())
        .or_else(|| {
            (home.filter(|home| !home.is_empty()))
                .map(|home| PathBuf::from(home).join(".local/state"))
        })?;
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
    fn a_relative_state_home_is_ignored() {
        let dir = |state: Option<&str>, home: Option<&str>| {
            state_dir_from(state.map(OsString::from), home.map(OsString::from))
        };
        assert_eq!(
            dir(Some("/s"), Some("/h")),
            Some(PathBuf::from("/s/clackatoo"))
        );
        for state in [None, Some(""), Some("state"), Some("./s")] {
            assert_eq!(
                dir(state, Some("/h")),
                Some(PathBuf::from("/h/.local/state/clackatoo"))
            );
        }
        assert_eq!(dir(Some("rel"), None), None);
    }

    #[test]
    fn udp_ports_are_one_or_a_range() {
        assert_eq!(parse_ports("60001"), Some(60001..=60001));
        assert_eq!(parse_ports("60000-61000"), Some(60000..=61000));
        assert_eq!(parse_ports("61000-60000"), None);
        assert_eq!(parse_ports("udp"), None);
    }
}
