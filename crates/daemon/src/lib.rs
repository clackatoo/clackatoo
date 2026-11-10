//! The clackatoo daemon, and the client that talks to it.
//!
//! The daemon owns every terminal: the PTYs, the programs running on them,
//! and the grids their output builds. Windows, the CLI, remote viewers and
//! agents are all clients, speaking [`clackatoo_protocol`] over a Unix
//! socket, or over SSH to a daemon on another machine (see [`remote`]).
//! Terminals outlive any client, so closing a window closes nothing.

// The only unsafe code is the pre-exec hooks in `pty::spawn`,
// `browser::cdp::spawn` and `priority`, the declaration of glibc's
// `malloc_trim` in `memory`, the scheduling flag `priority` sets, and the
// signal handling in `signals` and `secret`, which opt out of this
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
pub mod dbus;
pub mod display;
mod elicit;
mod explorer;
mod find;
pub mod forward;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
mod git;
mod group;
mod handoff;
mod handover;
mod history;
mod hosted;
mod ignores;
mod images;
mod inhibit;
mod known;
pub mod launch;
mod lsp;
mod machine;
pub mod mcp;
mod memory;
mod ports;
pub mod priority;
mod profiles;
mod provider_usage;
mod pty;
mod quic;
mod recovery;
mod redact;
mod regular;
pub mod remote;
mod rpc;
mod runtime;
mod saved;
pub mod schema;
pub mod secret;
mod server;
pub mod settings;
mod shell;
mod signals;
mod tool_images;
mod trust;
mod unlisted;
pub mod upgrade;
pub mod web;
mod workspaces;

pub use images::{Limits as ImageLimits, id_of as image_id};

use std::ffi::OsString;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

pub use agents::{AgentConfig, Configured as ConfiguredAgents, CustomAgent};
pub use browser::{BrowserConfig, check_browser, snap_of};
pub use client::{Client, ClientError, Receiver, Sender, UdpTarget, random_window};
pub use display::DisplayConfig;
pub use quic::{Contact, Redial, Redialed, Route};
pub use runtime::{DaemonLock, LockError, RuntimeDir};
pub use server::{Daemon, ShutdownHandle};
pub use trust::Trusting;

/// How many times inotify has been refused in this process, as the user's
/// instances or watches were all in use.
static INOTIFY_REFUSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// How many times inotify has been refused in this process, as the user's
/// instances (`fs.inotify.max_user_instances`) or watches were all in use:
/// for a test waiting for a change a daemon never heard of to say why.
#[must_use]
pub fn inotify_refused() -> usize {
    INOTIFY_REFUSED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Numbers every chat's turns from `from` from now on, not 0, so a turn's
/// uid, which what's kept of it is keyed by, is never its index: for
/// tests, which would otherwise not tell the two apart (CLACKA-198).
#[doc(hidden)]
pub fn number_turns_from(from: u32) {
    chat::number_turns_from(from);
}

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
    /// Where the folders trusted to run their language servers are kept
    /// (see [`Trusting`]): a server, which may run its project's code, is
    /// started only for a folder the user has trusted.
    pub trusted: Trusting,
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
    /// How long a command must have run for a notification to say it
    /// ended, while no one is looking at its terminal (see
    /// `server/notices.rs`). `None` says none. `daemon.terminals.notify_after`
    /// in the daemon's config, or else [`DEFAULT_LONG_COMMAND`].
    pub long_commands: Option<std::time::Duration>,
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
    /// Where images pasted into terminals are kept, and how one is put on
    /// the desktop's clipboard (see `Request::PasteImage`). `None` keeps
    /// none: pasting an image is refused.
    pub pastes: Option<PasteConfig>,
    /// The system bus logind is asked on to wait for the workspaces to be
    /// kept before the machine shuts down or sleeps, as a D-Bus address
    /// (see `inhibit.rs`). `None` asks nothing.
    pub logind: Option<String>,
    /// How much memory the images panes show may take: each terminal's,
    /// chat's, picture pane's or buffer's, and all of them together (see
    /// `images`).
    pub images: ImageLimits,
    /// What the hints programs give their output mean (see
    /// [`clackatoo_term::regions`]), as config says.
    pub hints: HintRules,
    /// How terminals' history is kept from one run of the daemon to the
    /// next, as the workspaces are (see `history`). `None` keeps none.
    pub history: Option<HistoryConfig>,
    /// What the first window the user is at is told of the daemon's
    /// config as it started: what was moved into it from the old TOML
    /// files, and what went wrong (see [`settings`]).
    pub notes: Vec<String>,
}

/// How terminals' history is kept across a restart of the daemon (see
/// `history`): where, how much of each, and how often what changed is
/// written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryConfig {
    /// The folder it's kept in, a file per terminal, which only the user
    /// may open: made so if it isn't there, and refused if it's anyone
    /// else's, or a link.
    pub dir: PathBuf,
    /// The most lines kept of each terminal, its screen's included.
    pub lines: usize,
    /// The most bytes of text kept of each terminal, before it's
    /// compressed: the oldest lines go first.
    pub bytes: usize,
    /// How often the history of terminals that have changed is written:
    /// a crash, or the daemon killed, loses no more than this.
    pub every: std::time::Duration,
}

impl HistoryConfig {
    /// Kept in `dir`, as much and as often as usual.
    #[must_use]
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            lines: DEFAULT_HISTORY_LINES,
            bytes: DEFAULT_HISTORY_BYTES,
            every: DEFAULT_HISTORY_EVERY,
        }
    }
}

/// The most lines of a terminal's history kept across a restart, unless
/// told otherwise: as many as a terminal keeps by default.
pub const DEFAULT_HISTORY_LINES: usize = 10_000;

/// The most bytes of text of a terminal's history kept across a restart:
/// 10,000 lines of 200 columns, styled.
pub const DEFAULT_HISTORY_BYTES: usize = 8 * 1024 * 1024;

/// How often the history of terminals that changed is written.
pub const DEFAULT_HISTORY_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Where images pasted into terminals are kept, and the programs that put
/// one on the desktop's clipboard, each reading it from its input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PasteConfig {
    /// The folder they're kept in, which only the user may open: made so
    /// if it isn't there, and refused if it's anyone else's, or a link.
    pub dir: PathBuf,
    /// On a Wayland desktop: `wl-copy`.
    pub wayland: OsString,
    /// On an X desktop: `xclip`, which is given
    /// `-selection clipboard -t image/png`.
    pub x: OsString,
}

impl PasteConfig {
    /// Kept in `dir`, put on the clipboard by `wl-copy` or `xclip`.
    #[must_use]
    pub fn in_dir(dir: PathBuf) -> Self {
        Self {
            dir,
            wayland: "wl-copy".into(),
            x: "xclip".into(),
        }
    }
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
    /// For tests: what this daemon tells the daemons connecting to it
    /// about itself that isn't so, as a modified build could. Nothing in
    /// use.
    #[doc(hidden)]
    pub lies: Lies,
}

/// For tests: what a daemon tells the daemons connecting to it about
/// itself that isn't so (see [`LinkConfig::lies`]).
#[doc(hidden)]
#[derive(Clone, Debug, Default)]
pub struct Lies {
    /// Its QUIC certificate comes unsigned, whatever protocol it speaks.
    pub unsigned: bool,
    /// It signs its QUIC certificate, with its own key, as the machine its
    /// workspaces say it is, while they lie.
    pub forges: bool,
    /// Where a test sets the machine its workspaces say it is, from the
    /// next time they're told on, while that's set. `None` in use, so
    /// nothing is looked at as they're told.
    pub says: Option<std::sync::Arc<std::sync::Mutex<Option<clackatoo_protocol::Machine>>>>,
}

impl Lies {
    /// The machine its workspaces say it is, if they lie.
    pub(crate) fn said(&self) -> Option<clackatoo_protocol::Machine> {
        let says = self.says.as_ref()?;
        (says.lock())
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Lies are told apart by where they're kept, not by what they say now.
impl PartialEq for Lies {
    fn eq(&self, other: &Self) -> bool {
        let says = match (&self.says, &other.says) {
            (Some(one), Some(other)) => std::sync::Arc::ptr_eq(one, other),
            (one, other) => one.is_none() && other.is_none(),
        };
        self.unsigned == other.unsigned && self.forges == other.forges && says
    }
}

impl Eq for Lies {}

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

/// How long a command must have run for its end to be told, unless the
/// daemon's config says otherwise (see
/// [`DaemonConfig::long_commands`]).
pub const DEFAULT_LONG_COMMAND: std::time::Duration = std::time::Duration::from_secs(10);

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
    /// [`state_dir`]), `$CLACKATOO_SSH`: the program run as ssh to connect to
    /// other daemons (or else `ssh`), `$CLACKATOO_AGENT`: the command of the agent a
    /// chat gets when it asks for none, split at spaces (or else Claude
    /// Code's adapter), `$CLACKATOO_MCP`: the command, split at spaces,
    /// agents run to reach clackatoo's tools, given `--chat ID --socket
    /// PATH` after it (or else none: the `clackatoo` program sets its own),
    /// `$CLACKATOO_UDP_PORTS`: a port, a range such as
    /// `60000-61000` (the default), or `off`, the language packs in
    /// [`languages_dir`], and `$CLACKATOO_BROWSER`: the browser agents drive,
    /// or `off` for none (or else Chromium or Chrome, as found on `PATH`),
    /// with its profile in `$CLACKATOO_BROWSER_PROFILE` (or else
    /// `clackatoo/browser` in the data directory, or for a snap, in its
    /// own folder), the folders trusted to run their language servers in `trusted` in
    /// [`state_dir`] (or for this run only, with none),
    /// `$CLACKATOO_LSP_TRACE`: a file to trace language servers'
    /// messages to (or else none), `$CLACKATOO_ACP_TRACE`: a directory to
    /// trace agents' messages to, a file each (or else none), and `$CLACKATOO_DISPLAY`: `off` to
    /// leave the programs the daemon starts its own display, rather than
    /// follow the desktop's login (see [`display`]), and
    /// `$CLACKATOO_PASTES`, the folder images pasted into terminals are
    /// kept in, or `off` for none (or else `clackatoo/pastes` in
    /// [`cache_dir`]), and `$CLACKATOO_LOGIND`: the D-Bus address of the
    /// system bus logind is asked on, or `off` to ask nothing (or else the
    /// system bus, but with `$CLACKATOO_DISPLAY` off, as for tests), and
    /// `$CLACKATOO_IMAGE_MEMORY`: how many MiB of images each terminal,
    /// chat, picture pane or buffer may hold, and all of them together, as
    /// `64,256`, the defaults. The daemon's config sets none of it: see
    /// [`configured`](Self::configured).
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_settings(&settings::Settings::default())
    }

    /// As `clackatoo daemon` starts or upgrades: the daemon's config run
    /// (see [`settings::load`]), moved from the old TOML files first if
    /// they're there, and the environment over it, as
    /// [`from_settings`](Self::from_settings) says; and what the config
    /// set, for what's read of it beyond, as the daemon's priority.
    #[must_use]
    pub fn configured(when: settings::When) -> (Self, settings::Settings) {
        let loaded = (settings::Dirs::from_env())
            .map(|dirs| settings::load(&dirs, when))
            .unwrap_or_default();
        let mut config = Self::from_settings(&loaded.settings);
        config.notes.splice(0..0, loaded.notes);
        (config, loaded.settings)
    }

    /// As [`from_env`](Self::from_env) says, over what the daemon's config
    /// `settings` sets: the agents it adds and their limits, this
    /// machine's name, the web app, keeping its devices in
    /// `clackatoo/web` in the data directory and its password's hash in
    /// [`state_dir`], how terminals' history is kept and how much memory
    /// images may take. The environment's `$CLACKATOO_HISTORY*` and
    /// `$CLACKATOO_IMAGE_MEMORY` outdo it, for tests.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "one field each")]
    pub fn from_settings(settings: &settings::Settings) -> Self {
        let non_empty = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        // Programs name this machine in `file://` links (see
        // `clackatoo_term::Config::local_host`).
        let local_host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty());
        let mut widths = clackatoo_term::Config::default().widths;
        widths.vs16 = settings.on("terminals.vs16");
        let mut notes = Vec::new();
        Self {
            terminal: clackatoo_term::Config {
                local_host,
                widths,
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
                configured: settings.agents(),
                dir: languages_dir().map(|dir| dir.with_file_name("agents")),
                path: None,
                trace: non_empty("CLACKATOO_ACP_TRACE").map(PathBuf::from),
                restart_on_upgrade: restart_agents_on_upgrade(),
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
            trusted: state_dir()
                .map_or(Trusting::ThisRun, |dir| Trusting::File(dir.join("trusted"))),
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
            long_commands: match settings.number("terminals.notify_after") {
                0 => None,
                seconds => Some(std::time::Duration::from_secs(seconds.into())),
            },
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
            web: web_from(settings, &mut notes),
            machine: match non_empty("CLACKATOO_MACHINE") {
                Some(off) if off == "off" => None,
                Some(path) => Some(PathBuf::from(path)),
                None => state_dir().map(|dir| dir.join("machine")),
            },
            machine_name: settings.machine_name(),
            links: Some(LinkConfig {
                ssh: remote::ssh_program(),
                // Only the `clackatoo` program knows it can ask.
                askpass: None,
                better_every: DEFAULT_BETTER_EVERY,
                speaks: None,
                impostor: false,
                lies: Lies::default(),
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
            pastes: match non_empty("CLACKATOO_PASTES") {
                Some(off) if off == "off" => None,
                // Only from somewhere in particular, as for recovery.
                Some(dir) if Path::new(&dir).is_absolute() => {
                    Some(PasteConfig::in_dir(PathBuf::from(dir)))
                }
                Some(dir) => {
                    log::warn!(
                        "CLACKATOO_PASTES isn't an absolute path or `off`: {}",
                        dir.display()
                    );
                    cache_dir().map(|dir| PasteConfig::in_dir(dir.join("clackatoo/pastes")))
                }
                None => cache_dir().map(|dir| PasteConfig::in_dir(dir.join("clackatoo/pastes"))),
            },
            logind: match non_empty("CLACKATOO_LOGIND") {
                Some(off) if off == "off" => None,
                Some(address) => Some(address.to_string_lossy().into_owned()),
                None if non_empty("CLACKATOO_DISPLAY").is_some_and(|off| off == "off") => None,
                None => Some(dbus::system_address()),
            },
            images: images_from(settings, &non_empty),
            hints: HintRules {
                hide: settings.value("hints.hide") == schema::Value::Choice("mask"),
                nosave: settings.on("hints.nosave"),
                noagent: settings.value("hints.noagent") == schema::Value::Choice("hide"),
                shown_for: std::time::Duration::from_secs(
                    settings.number("hints.shown_for").into(),
                ),
            },
            history: history_from(settings, &non_empty),
            notes,
        }
    }
}

/// What the hints programs give the lines they print mean (see
/// [`clackatoo_term::regions`]): each is honoured unless config says to
/// ignore it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each hint is honoured or not"
)]
pub struct HintRules {
    /// Lines marked `hide` are masked in what windows and the web app are
    /// sent, until the user asks to see them: `daemon.hints.hide`,
    /// `mask` or `show`.
    pub hide: bool,
    /// Lines marked `nosave` are never written to disk:
    /// `daemon.hints.nosave`.
    pub nosave: bool,
    /// Lines marked `noagent` are kept from agents: `daemon.hints.noagent`,
    /// `hide` or `show`.
    pub noagent: bool,
    /// How long lines the user asked to see stay shown before they're
    /// masked again: `daemon.hints.shown_for`, in seconds.
    pub shown_for: std::time::Duration,
}

impl Default for HintRules {
    fn default() -> Self {
        Self {
            hide: true,
            nosave: true,
            noagent: true,
            shown_for: std::time::Duration::from_secs(30),
        }
    }
}

/// How much memory images may take, as the daemon's config `settings`
/// says, or `$CLACKATOO_IMAGE_MEMORY` over it.
fn images_from(
    settings: &settings::Settings,
    non_empty: &dyn Fn(&str) -> Option<OsString>,
) -> ImageLimits {
    match non_empty("CLACKATOO_IMAGE_MEMORY") {
        Some(value) => ImageLimits::from_env(Some(&value)),
        None => settings.images(),
    }
}

/// The web app, as the daemon's config `settings` says, with the
/// password's hash in [`state_dir`]: why it's off when it's set to be on
/// goes in `notes`.
fn web_from(settings: &settings::Settings, notes: &mut Vec<String>) -> Option<web::WebConfig> {
    if settings.value("web.mode") == schema::Value::Choice("off") {
        return None;
    }
    let data = data_dir()?.join("clackatoo/web");
    let password = match settings::state_dir().map(|state| settings::password(&state)) {
        Some(Ok(password)) => password,
        Some(Err(why)) => {
            log::error!("{why}; the web app is off");
            notes.push(format!("the web app is off: {why}"));
            return None;
        }
        None => None,
    };
    settings
        .web(data, password)
        .inspect_err(|why| {
            log::error!("{why}; the web app is off");
            notes.push(format!("the web app is off: {why}"));
        })
        .ok()
        .flatten()
}

/// How terminals' history is kept, as the daemon's config `settings`
/// says, in `history` in [`state_dir`], and the environment over it:
/// `$CLACKATOO_HISTORY`, the folder it's kept in, or `off` for none,
/// `$CLACKATOO_HISTORY_LINES`, the most lines kept of each terminal, and
/// `$CLACKATOO_HISTORY_EVERY`, how often in seconds what changed is
/// written.
fn history_from(
    settings: &settings::Settings,
    non_empty: &dyn Fn(&str) -> Option<OsString>,
) -> Option<HistoryConfig> {
    let mut config = match non_empty("CLACKATOO_HISTORY") {
        Some(off) if off == "off" => return None,
        // Only from somewhere in particular, as a relative path would be
        // from wherever the daemon happened to start.
        Some(dir) if Path::new(&dir).is_absolute() => {
            let mut config = settings.history(PathBuf::from(&dir));
            config.get_or_insert_with(|| HistoryConfig::in_dir(PathBuf::from(dir)));
            config?
        }
        Some(dir) => {
            log::warn!(
                "CLACKATOO_HISTORY isn't an absolute path or `off`: {}",
                dir.display()
            );
            settings.history(state_dir()?.join("history"))?
        }
        None => settings.history(state_dir()?.join("history"))?,
    };
    let number = |name: &str| {
        let value = non_empty(name)?;
        let number = value.to_str().and_then(|value| value.parse::<u64>().ok());
        if number.is_none() {
            log::warn!("{name} isn't a number: {}", value.display());
        }
        number
    };
    if let Some(lines) = number("CLACKATOO_HISTORY_LINES") {
        config.lines = usize::try_from(lines).unwrap_or(usize::MAX).min(1_000_000);
    }
    if let Some(seconds) = number("CLACKATOO_HISTORY_EVERY") {
        config.every = std::time::Duration::from_secs(seconds.clamp(1, 24 * 60 * 60));
    }
    Some(config)
}

/// Where language packs are installed: `clackatoo/languages` in
/// `$XDG_DATA_HOME`, or else in `~/.local/share`. Language servers are
/// installed beside it, in `servers`.
#[must_use]
pub fn languages_dir() -> Option<PathBuf> {
    Some(data_dir()?.join("clackatoo/languages"))
}

/// Where programs keep files they can do without: `$XDG_CACHE_HOME`, or
/// else `~/.cache`. A relative path in either is ignored.
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))
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

/// Whether the environment says to stop agents as the daemon upgrades, as
/// before they went on through upgrades: `CLACKATOO_LIVE_AGENTS=off` (see
/// [`AgentConfig::restart_on_upgrade`]).
pub(crate) fn restart_agents_on_upgrade() -> bool {
    std::env::var_os("CLACKATOO_LIVE_AGENTS").is_some_and(|live| live == "off")
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

    /// What the daemon's terminals do, as its config says.
    #[test]
    fn the_terminals_go_by_the_config() {
        let config = DaemonConfig::from_settings(&settings::Settings::default());
        assert!(config.terminal.widths.vs16);
        assert_eq!(config.long_commands, Some(DEFAULT_LONG_COMMAND));
        let settings = settings::Settings::from_lua(
            "daemon.terminals.vs16 = false\ndaemon.terminals.notify_after = 30\n",
        )
        .unwrap();
        let config = DaemonConfig::from_settings(&settings);
        assert!(!config.terminal.widths.vs16);
        assert_eq!(
            config.long_commands,
            Some(std::time::Duration::from_secs(30))
        );
        let never = settings::Settings::from_lua("daemon.terminals.notify_after = 0").unwrap();
        assert_eq!(DaemonConfig::from_settings(&never).long_commands, None);
        assert!(settings::Settings::from_lua("daemon.terminals.notify_after = -3").is_err());
        assert!(settings::Settings::from_lua("daemon.terminals.vs16 = \"no\"").is_err());
    }

    /// The environment's `$CLACKATOO_HISTORY*` and `$CLACKATOO_IMAGE_MEMORY`
    /// outdo the daemon's config, for tests and CI.
    #[test]
    fn the_environment_outdoes_the_config() {
        let settings = settings::Settings::from_lua(
            "daemon.history.lines = 500\ndaemon.history.every = 60\n\
             daemon.images.terminal_mb = 8\ndaemon.images.total_mb = 16\n",
        )
        .unwrap();
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                (pairs.iter())
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };
        let none = env(&[]);
        if let Some(history) = history_from(&settings, &none) {
            assert_eq!(history.lines, 500);
            assert_eq!(history.every, std::time::Duration::from_mins(1));
        }
        let images = images_from(&settings, &none);
        assert_eq!((images.per_owner, images.total), (8 << 20, 16 << 20));
        let over = env(&[
            ("CLACKATOO_HISTORY", "/kept"),
            ("CLACKATOO_HISTORY_LINES", "7"),
            ("CLACKATOO_IMAGE_MEMORY", "1,2"),
        ]);
        let history = history_from(&settings, &over).unwrap();
        assert_eq!(history.dir, PathBuf::from("/kept"));
        assert_eq!(history.lines, 7);
        assert_eq!(
            history.every,
            std::time::Duration::from_mins(1),
            "the config's"
        );
        let images = images_from(&settings, &over);
        assert_eq!((images.per_owner, images.total), (1 << 20, 2 << 20));
        assert_eq!(
            history_from(&settings, &env(&[("CLACKATOO_HISTORY", "off")])),
            None
        );
        // Off in the config, the environment can still keep it.
        let off = settings::Settings::from_lua("daemon.history.keep = false").unwrap();
        assert!(history_from(&off, &none).is_none());
        assert!(history_from(&off, &env(&[("CLACKATOO_HISTORY", "/kept")])).is_some());
    }

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
