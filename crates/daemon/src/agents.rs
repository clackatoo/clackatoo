//! Which agents chats can talk to: the ones clackatoo knows, and any the
//! user adds.
//!
//! Each agent has a name, which chats ask for it by (`claude`, `codex`),
//! a title to show (`Claude Code`, `Codex`), and a command that speaks
//! ACP. The ones clackatoo knows are run through their ACP adapters, at
//! the versions tried with clackatoo, found in this order:
//!
//! 1. Where clackatoo installed it: `agents/<name>-<version>` beside the
//!    language packs, which `npm ci` fills from the lockfile kept here
//!    for that version, so nothing it depends on can drift. An adapter
//!    clackatoo patches, at a new patch level of the same upstream
//!    version (`+clackatoo.N`), is prepared from the earlier one there,
//!    unasked, as its install was agreed to (see [`upgrade`]).
//! 2. On `PATH`, as `claude-agent-acp` or `codex-acp`.
//! 3. Through `npx`, at the same version, as clackatoo ran them before it
//!    installed them, so chats started then carry on.
//!
//! An adapter clackatoo patches isn't run from 2 or 3 while the
//! provider's own MCP servers are kept out, as upstream's can't: it's
//! offered to install instead, if it can be.
//!
//! Run from 2 or 3, a chat offers once to install it, without waiting
//! for the answer: run from where it's installed, it starts in 140 ms
//! rather than 380, works offline, and has no npm process of about 95 MB
//! alive beside it, as `npx` keeps. Only with none of the three does a
//! chat wait for it to be installed.
//!
//! The user adds agents, or gives a known one another command, in the
//! daemon's config (see [`crate::settings`]), read as it starts or
//! upgrades:
//!
//! ```lua
//! daemon.agents.opencode = { name = "OpenCode", command = { "opencode", "acp" } }
//! daemon.agents.claude = { command = { "claude-agent-acp" }, tasks = false }
//! ```
//!
//! `tasks` says whether to ask the agent what it runs in the background
//! (see [`crate::agent::Task`]): Claude Code is asked unless it's `false`.
//! `daemon.subagents` sets how far chats agents start may go (see
//! [`SubagentLimits`]).
//!
//! It's the daemon's, not the window's, as the daemon runs the agent:
//! it starts it again after an upgrade or a crash with no window there,
//! for a chat opened from a phone as from a desktop, on its own machine.

mod codex_steering;
mod subagents;

pub(crate) use subagents::{DEEPEST as DEEPEST_SUBAGENTS, SubagentLimits};

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use clackatoo_syntax::install::Download;
use clackatoo_syntax::servers::{KnownServer, Packed, Source};

/// The lockfile `npm ci` installs a known agent's adapter from, and the
/// manifest beside it, by the agent's name, as `npm install
/// --package-lock-only` wrote them for the version pinned.
macro_rules! locked {
    ($name:literal, $package:literal) => {
        Source::NpmLocked {
            package: $package,
            manifest: include_str!(concat!("../agents/", $name, "/package.json")),
            lock: include_str!(concat!("../agents/", $name, "/package-lock.json")),
        }
    };
}

/// The agent a chat talks to when it asks for none.
pub(crate) const DEFAULT: &str = "claude";

pub(crate) const ANTIGRAVITY_VERSION: &str = "1.4.3";

/// An agent clackatoo knows: where its adapter comes from, and how it
/// differs from the others.
#[derive(Debug)]
pub(crate) struct Known {
    /// What chats ask for it by.
    pub(crate) name: &'static str,
    /// What to show.
    pub(crate) title: &'static str,
    /// Its pinned ACP runtime, installed from npm or a release archive.
    pub(crate) adapter: KnownServer,
    /// The adapter's version, as its package is pinned to.
    pub(crate) version: &'static str,
    /// The adapter's arguments.
    pub(crate) args: &'static [&'static str],
    /// About how much room it takes installed, to say when offering it.
    pub(crate) size: &'static str,
    /// The mode a new chat asks for, if the agent offers it.
    pub(crate) starting_mode: Option<&'static str>,
    /// Whether it's asked what it runs in the background.
    pub(crate) tasks: bool,
    /// Whether a saved file a message mentions goes to it as its text, if
    /// small, rather than as a link it reads itself (see
    /// `server/mentions.rs`).
    pub(crate) embeds_saved: bool,
    /// What logs in to it, to say when it isn't logged in.
    pub(crate) login: &'static str,
    /// Where it's told which MCP servers to start, to say when a session
    /// is slow to open.
    pub(crate) servers: Servers,
}

/// Where an agent clackatoo knows is told which MCP servers to start with
/// each session. A server waiting on a login is what most often keeps a
/// session from opening: Codex waited forever on one in jack's config
/// (CLACKA-81).
#[derive(Debug)]
pub(crate) struct Servers {
    /// Where they're set, as it's shown.
    pub(crate) set_in: &'static str,
    /// What lists them, in a terminal, saying which can't start.
    pub(crate) list: &'static str,
    /// Finds the servers set that are reached over the network, which
    /// are the ones to log in to, by name, given the home directory: for
    /// an agent whose config can be read for them.
    pub(crate) remote: Option<fn(&Path) -> Vec<String>>,
}

/// Known agents are the same if they have the same name.
impl PartialEq for Known {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Known {}

/// The agents clackatoo knows; Antigravity checked on 2026-10-25. Their packages and
/// arguments are the ACP registry's
/// (<https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json>).
pub(crate) const KNOWN: &[Known] = &[
    Known {
        name: "claude",
        title: "Claude Code",
        adapter: KnownServer {
            program: "claude-agent-acp",
            source: locked!("claude", "@agentclientprotocol/claude-agent-acp@0.98.0"),
        },
        version: "0.98.0",
        args: &[],
        size: "620 MB",
        // Auto decides itself what needs asking about (jack, 2026-10-08).
        starting_mode: Some("auto"),
        tasks: true,
        // Claude Code edits only files its Read tool has read, so a
        // file's text in the message would be read again before an edit:
        // it reads a link itself, getting line numbers too.
        embeds_saved: false,
        login: "claude",
        servers: Servers {
            set_in: "its settings (`~/.claude.json`, or the project's `.mcp.json`)",
            list: "claude mcp list",
            remote: None,
        },
    },
    Known {
        name: "codex",
        title: "Codex",
        adapter: KnownServer {
            program: "codex-acp",
            source: locked!("codex", "@agentclientprotocol/codex-acp@2.2.2"),
        },
        version: codex_steering::VERSION,
        args: &[],
        size: "450 MB",
        // Codex starts in the mode its own config gives, as it does in a
        // terminal: Auto review for most.
        starting_mode: None,
        // Asking makes Codex's adapter leave out its commands' output.
        tasks: false,
        embeds_saved: true,
        login: "codex login",
        servers: Servers {
            set_in: "`~/.codex/config.toml`",
            list: "codex mcp list",
            remote: Some(codex_remote_servers),
        },
    },
    Known {
        name: "antigravity",
        title: "Antigravity",
        adapter: KnownServer {
            program: "agy_acp_server.par",
            source: Source::Release {
                version: ANTIGRAVITY_VERSION,
                assets: &[
                    (
                        "x86_64",
                        Download {
                            url: "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-1.4.3-linux-x86_64.zip",
                            sha256: "0634ce5306ba92c932945455e7a83fda78e2d99c33fa8aa6ee0566206a382714",
                        },
                    ),
                    (
                        "aarch64",
                        Download {
                            url: "https://dl.google.com/agy-extensions/releases/linux/agy-acp-server-1.4.3-linux-arm64.zip",
                            sha256: "05c153443b0b1175151c760eeddea538950bc9843e317d54786a1f03670182bc",
                        },
                    ),
                ],
                packed: Packed::Zip,
                program: "agy_acp_server.par",
            },
        },
        version: ANTIGRAVITY_VERSION,
        args: &["--uid="],
        size: "1.1 GB, plus about 1 GB of runtime scratch",
        starting_mode: Some("default"),
        tasks: false,
        embeds_saved: true,
        login: "clackatoo agent login antigravity",
        servers: Servers {
            set_in: "its Clackatoo Antigravity profile",
            list: "clackatoo agent login antigravity",
            remote: None,
        },
    },
    Known {
        name: "gemini",
        title: "Gemini CLI",
        adapter: KnownServer {
            program: "gemini",
            source: locked!("gemini", "@google/gemini-cli@0.63.0"),
        },
        version: "0.63.0",
        args: &["--acp"],
        size: "100 MB",
        starting_mode: None,
        tasks: false,
        embeds_saved: true,
        login: "gemini",
        servers: Servers {
            set_in: "`~/.gemini/settings.json`",
            list: "gemini mcp list",
            remote: None,
        },
    },
];

/// An agent given a command of the user's, rather than the adapter
/// clackatoo installs: one of the user's own, or a known one run
/// another way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomAgent {
    /// What chats ask for it by.
    pub name: String,
    /// What to show, if not its name, or a known agent's title.
    pub title: Option<String>,
    /// Its command, which speaks ACP on its stdin and stdout.
    pub command: Vec<OsString>,
    /// The mode a new chat asks for, if the agent offers it.
    pub starting_mode: Option<String>,
    /// Whether to ask it what it runs in the background, if not as the
    /// known agent of its name is asked.
    pub tasks: Option<bool>,
}

/// An agent, ready to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSpec {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) launch: Launch,
    pub(crate) starting_mode: Option<String>,
    /// Whether it's asked what it runs in the background.
    pub(crate) tasks: bool,
    /// What logs in to it, if it's known.
    pub(crate) login: Option<String>,
}

/// How an agent is started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Launch {
    /// Through the adapter clackatoo installs.
    Known(&'static Known),
    /// With the user's command.
    Command(Vec<OsString>),
}

/// What it takes to start an agent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// This command starts it. `suggest` is the known agent it is, if it
    /// runs from `PATH` or through `npx`, and clackatoo could install it.
    Ready {
        command: Vec<OsString>,
        suggest: Option<&'static Known>,
    },
    /// An earlier patch level of its adapter is installed: it's prepared
    /// again at this one, from that, fetching nothing, as the user agreed
    /// to install it already (see [`upgrade`]).
    Upgrade(&'static Known),
    /// Its adapter needs installing first, which clackatoo can do.
    Install(&'static Known),
    /// It can't be started: why, and what to do about it.
    Unavailable(String),
}

/// Where agents are found and installed.
#[derive(Clone, Debug, Default)]
pub struct AgentConfig {
    /// Agents set when the daemon started, such as by `$CLACKATOO_AGENT`:
    /// over those its config adds, and those clackatoo knows.
    pub custom: Vec<CustomAgent>,
    /// What the daemon's config says of agents, as it started.
    pub configured: Configured,
    /// Where known agents' adapters are installed, one directory each.
    /// `None` installs none.
    pub dir: Option<PathBuf>,
    /// Where adapters, `npx` and `npm`, and the programs that log in to
    /// agents, are looked for, as `PATH` lists directories. `None` is the
    /// daemon's own `PATH`.
    pub path: Option<OsString>,
    /// A directory every message to and from each agent is written to, a
    /// file for each agent started, with obvious secrets taken out (see
    /// [`crate::rpc::Peer::trace_agent_to`]). `None` writes none.
    pub trace: Option<PathBuf>,
    /// Stops every agent as the daemon upgrades, as before agents went on
    /// through upgrades, whatever the config says: the switch to turn
    /// that off, `CLACKATOO_LIVE_AGENTS=off` (see
    /// [`kept_through_upgrades`](Self::kept_through_upgrades)).
    pub restart_on_upgrade: bool,
}

/// What the daemon's config says of agents (see [`crate::settings`]): the
/// agents it adds, or gives another command, the limits on chats agents start, and whether agents go on
/// through upgrades.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent user preferences for each provider"
)]
pub struct Configured {
    pub(crate) agents: Vec<CustomAgent>,
    pub(crate) subagents: SubagentLimits,
    pub(crate) keep_agents: bool,
    pub(crate) claude_plugins: bool,
    pub(crate) codex_plugins: bool,
    pub(crate) claude_mcp: bool,
    pub(crate) codex_mcp: bool,
    pub(crate) other_mcp: bool,
}

impl Default for Configured {
    fn default() -> Self {
        Self {
            agents: Vec::new(),
            subagents: SubagentLimits::default(),
            keep_agents: true,
            claude_plugins: false,
            codex_plugins: false,
            claude_mcp: false,
            codex_mcp: false,
            other_mcp: false,
        }
    }
}

impl Configured {
    /// Whether this known runtime may use its own plugins and skills.
    pub(crate) fn native_plugins(&self, name: &str) -> bool {
        match name {
            "claude" => self.claude_plugins,
            "codex" => self.codex_plugins,
            _ => false,
        }
    }

    pub(crate) fn native_mcp(&self, name: &str) -> bool {
        match name {
            "claude" => self.claude_mcp,
            "codex" => self.codex_mcp,
            _ => self.other_mcp,
        }
    }

    /// As the daemon's config `settings` says.
    #[must_use]
    pub fn from(settings: &crate::settings::Settings) -> Self {
        settings.agents()
    }
}

impl Known {
    /// The prepared entry point. Codex's backport lives beside the original
    /// bundle; a partial install cannot accidentally enable native steering.
    pub(crate) fn installed_program(&self, dir: &Path) -> PathBuf {
        if self.name == "codex" {
            codex_steering::program(dir)
        } else {
            self.adapter.installed_program(dir)
        }
    }

    /// Whether clackatoo patches its adapter as it installs it, its
    /// version saying how (`+clackatoo.N`): upstream's, as on `PATH` or
    /// through `npx`, lacks what that adds.
    pub(crate) fn patched(&self) -> bool {
        self.version.contains("+clackatoo.")
    }

    /// Prepares the adapter npm installed in `dir`, checking what it
    /// patches is what was pinned; for most, there's nothing to.
    fn prepare(&self, dir: &Path) -> std::io::Result<()> {
        if self.name == "codex" {
            codex_steering::install(dir)
        } else {
            Ok(())
        }
    }

    /// What [`prepare`](Self::prepare) writes in `dir`, which an upgrade
    /// writes afresh rather than taking from an earlier patch level.
    fn prepared(&self, dir: &Path) -> Vec<PathBuf> {
        if self.name == "codex" {
            codex_steering::outputs(dir).to_vec()
        } else {
            Vec::new()
        }
    }

    /// What [`prepare`](Self::prepare) reads in `dir`: npm's install of
    /// it, as an earlier patch level has it too.
    fn original(&self, dir: &Path) -> PathBuf {
        if self.name == "codex" {
            codex_steering::original(dir)
        } else {
            self.adapter.installed_program(dir)
        }
    }

    /// Its adapter's package, as `name@version`.
    pub(crate) fn package(&self) -> &'static str {
        match &self.adapter.source {
            Source::NpmLocked { package, .. } => package,
            Source::Npm { packages } => packages.first().copied().unwrap_or_default(),
            _ => "",
        }
    }
}

/// The known agent called `name`, if there is one.
#[must_use]
pub(crate) fn known(name: &str) -> Option<&'static Known> {
    KNOWN.iter().find(|known| known.name == name)
}

/// Whether a saved file a message mentions goes to the agent called
/// `name` as its text, if small, rather than as a link: for any but the
/// known agents that do better reading it themselves.
pub(crate) fn embeds_saved(name: &str) -> bool {
    known(name).is_none_or(|known| known.embeds_saved)
}

/// Whether the agent called `name`, carried on through an upgrade from a
/// build that didn't hand over whether it takes files' text in a message,
/// is known to: the adapters checked, at the versions pinned.
pub(crate) fn embeds_checked(name: &str) -> bool {
    matches!(name, "claude" | "codex")
}

/// Whether the agent called `name`, carried on through an upgrade from a
/// build that didn't hand over what it can do with sessions, is known to
/// fork one at an agent's message and to resume one: the adapters checked,
/// at the versions pinned (CLACKA-198).
pub(crate) fn forks_checked(name: &str) -> bool {
    matches!(name, "claude" | "codex")
}

impl AgentConfig {
    /// Whether agents' processes go on through an upgrade, with all they
    /// run (CLACKA-188), rather than being stopped first: unless
    /// [`restart_on_upgrade`](Self::restart_on_upgrade) says otherwise, or
    /// the config does, with `daemon.upgrade.keep_agents = false`.
    ///
    /// The build upgrading asks this, and so does the build it upgrades to,
    /// which then doesn't say it takes them (see
    /// [`crate::upgrade::TAKES_ADAPTERS`]).
    pub(crate) fn kept_through_upgrades(&self) -> bool {
        !self.restart_on_upgrade && self.configured.keep_agents
    }

    /// The limits on chats agents start, as the daemon's config sets them.
    pub(crate) fn subagent_limits(&self) -> SubagentLimits {
        self.configured.subagents
    }

    /// The agent called `name`: the one clackatoo knows, changed or
    /// replaced by the user's, or why there isn't one.
    pub(crate) fn find(&self, name: &str) -> Result<AgentSpec, String> {
        self.find_among(name, &self.configured.agents)
    }

    /// The agent called `name`, as [`find`](Self::find) finds it, with
    /// `from_file` the agents the config adds.
    fn find_among(&self, name: &str, from_file: &[CustomAgent]) -> Result<AgentSpec, String> {
        let custom = (self.custom.iter().find(|custom| custom.name == name))
            .or_else(|| from_file.iter().find(|custom| custom.name == name));
        let known = known(name);
        match (custom, known) {
            (Some(custom), known) => Ok(AgentSpec {
                name: custom.name.clone(),
                title: (custom.title.clone())
                    .or_else(|| known.map(|known| known.title.to_owned()))
                    .unwrap_or_else(|| custom.name.clone()),
                launch: Launch::Command(custom.command.clone()),
                starting_mode: (custom.starting_mode.clone())
                    .or_else(|| known.and_then(|known| known.starting_mode.map(str::to_owned))),
                tasks: (custom.tasks).unwrap_or_else(|| known.is_some_and(|known| known.tasks)),
                login: known.map(|known| known.login.to_owned()),
            }),
            (None, Some(known)) => Ok(AgentSpec {
                name: known.name.to_owned(),
                title: known.title.to_owned(),
                launch: Launch::Known(known),
                starting_mode: known.starting_mode.map(str::to_owned),
                tasks: known.tasks,
                login: Some(known.login.to_owned()),
            }),
            (None, None) => {
                let mut names: Vec<&str> = KNOWN.iter().map(|known| known.name).collect();
                let added =
                    (self.custom.iter().chain(from_file)).map(|custom| custom.name.as_str());
                for added in added {
                    if !names.contains(&added) {
                        names.push(added);
                    }
                }
                Err(format!(
                    "there's no agent called {name}: the agents are {}",
                    names.join(", ")
                ))
            }
        }
    }

    /// The command a chat with the agent called `name` would run now, as
    /// the daemon finds it: its install, its adapter on `PATH`, or `npx`.
    /// For tools and tests that talk to the real agent.
    ///
    /// # Errors
    ///
    /// Why there's none: no agent is called `name`, or it's yet to be
    /// installed, or nothing can run it.
    pub fn command_for(&self, name: &str) -> Result<Vec<OsString>, String> {
        let spec = self.find(name)?;
        match self.start(&spec) {
            Start::Ready { command, .. } => Ok(command),
            Start::Upgrade(known) | Start::Install(known) => Err(format!(
                "{} isn't installed: open a chat with it to install it",
                known.package()
            )),
            Start::Unavailable(why) => Err(why),
        }
    }

    /// What it takes to start `spec`: for a known agent, the first of
    /// its install, an earlier patch level of it to upgrade, its adapter
    /// on `PATH`, and `npx` that's there.
    pub(crate) fn start(&self, spec: &AgentSpec) -> Start {
        self.starting(spec, true)
    }

    /// What it takes to start `spec`, as [`start`](Self::start) says, but
    /// never upgrading an earlier patch level if `upgrades` is `false`: one
    /// that couldn't be.
    pub(crate) fn starting(&self, spec: &AgentSpec, upgrades: bool) -> Start {
        let known = match &spec.launch {
            Launch::Command(command) if command.is_empty() => {
                return Start::Unavailable(format!("{} has no command", spec.title));
            }
            Launch::Command(command) => {
                return Start::Ready {
                    command: command.clone(),
                    suggest: None,
                };
            }
            Launch::Known(known) => *known,
        };
        let args = known.args.iter().map(OsString::from);
        let installed = (self.dir.as_deref())
            .map(|dir| known.installed_program(&install_dir(dir, known)))
            .filter(|program| program.is_file());
        if let Some(program) = installed {
            return Start::Ready {
                command: std::iter::once(program.into_os_string())
                    .chain(args)
                    .collect(),
                suggest: None,
            };
        }
        if upgrades
            && let Some(dir) = self.dir.as_deref()
            && earlier_patch(dir, known).is_some()
        {
            return Start::Upgrade(known);
        }
        let npm = !known.package().is_empty();
        let installable = self.dir.is_some()
            && if npm {
                self.program("npm").is_some()
            } else {
                known.adapter.installable()
            };
        let suggest = installable.then_some(known);
        // An adapter clackatoo patches is upstream's on `PATH` or through
        // `npx`, which can't keep the provider's own MCP servers out: with
        // those off, it's offered to install, rather than run to fail.
        if installable && known.patched() && !self.configured.native_mcp(known.name) {
            return Start::Install(known);
        }
        if let Some(program) = self.program(known.adapter.program) {
            return Start::Ready {
                command: std::iter::once(program.into_os_string())
                    .chain(args)
                    .collect(),
                suggest,
            };
        }
        if let Some(npx) = self.program("npx").filter(|_| npm) {
            let npx = [
                npx.into_os_string(),
                "--yes".into(),
                "--prefer-offline".into(),
            ];
            return Start::Ready {
                command: (npx.into_iter())
                    .chain([known.package().into()])
                    .chain(args)
                    .collect(),
                suggest,
            };
        }
        if installable {
            Start::Install(known)
        } else if self.dir.is_none() {
            Start::Unavailable(format!(
                "{} isn't installed, and this daemon installs no agents: give it a \
                 command in the daemon's config, daemon/init.lua",
                known.title
            ))
        } else if !npm {
            Start::Unavailable(format!(
                "{} is not available for this platform",
                known.title
            ))
        } else {
            Start::Unavailable(format!(
                "{}'s adapter is installed with npm, which isn't here: install Node.js, \
                 then send a message to try again, or give {} a command in the daemon's \
                 config, daemon/init.lua",
                known.title, known.name
            ))
        }
    }

    /// Every agent a chat can talk to: those clackatoo knows, then the
    /// user's own, in the order they're given.
    pub(crate) fn all(&self) -> Vec<AgentSpec> {
        let from_file = &self.configured.agents;
        let mut names: Vec<&str> = KNOWN.iter().map(|known| known.name).collect();
        for custom in self.custom.iter().chain(from_file) {
            if !names.contains(&custom.name.as_str()) {
                names.push(&custom.name);
            }
        }
        (names.into_iter())
            .filter_map(|name| self.find_among(name, from_file).ok())
            .collect()
    }

    /// Whether `spec` can start as it is, without installing anything or
    /// fetching it through `npx`: it's installed, its adapter is on
    /// `PATH`, or the user gave it a command whose program is there.
    pub(crate) fn installed(&self, spec: &AgentSpec) -> bool {
        match &spec.launch {
            Launch::Command(command) => command.first().is_some_and(|program| {
                let program = Path::new(program);
                if program.components().count() > 1 {
                    is_program(program)
                } else {
                    self.program(&program.to_string_lossy()).is_some()
                }
            }),
            Launch::Known(known) => {
                let installed = (self.dir.as_deref())
                    .map(|dir| known.installed_program(&install_dir(dir, known)));
                installed.is_some_and(|program| program.is_file())
                    || self.program(known.adapter.program).is_some()
            }
        }
    }

    pub(crate) fn usage_node(&self) -> Option<PathBuf> {
        self.program("node")
    }

    /// Where the program that logs in to an agent is looked for (see
    /// [`login_command`]): [`path`](Self::path), or else the daemon's own
    /// `PATH`.
    pub(crate) fn login_path(&self) -> OsString {
        (self.path.clone())
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default()
    }

    /// The program called `name`, as the daemon finds it.
    fn program(&self, name: &str) -> Option<PathBuf> {
        match &self.path {
            None => clackatoo_syntax::servers::on_path(name),
            Some(path) => std::env::split_paths(path)
                .map(|dir| dir.join(name))
                .find(|candidate| is_program(candidate)),
        }
    }

    /// The directories the user's agents are run from, as the config and
    /// the daemon's own settings give their commands: an install never
    /// removes them.
    pub(crate) fn named_dirs(&self) -> Vec<PathBuf> {
        (self.custom.iter().chain(&self.configured.agents))
            .filter_map(|custom| custom.command.first())
            .map(PathBuf::from)
            .filter(|program| program.is_absolute())
            .collect()
    }
}

/// How to log in to an agent clackatoo knows, from its login command in
/// [`KNOWN`]: as the chat says to run it, and as the daemon runs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Login {
    /// As the user would type it in a terminal: the program by its name,
    /// or, where it's only where clackatoo installed it, as Codex's
    /// adapter brings its own `codex`, by that path; then its arguments.
    pub(crate) said: Vec<String>,
    /// As the daemon runs it, with no shell: the same, with the program
    /// by the absolute path it was found at, only ever in `PATH`'s
    /// absolute directories (see [`clackatoo_syntax::servers::find_on`])
    /// or where clackatoo installed it, never in the folder it runs in;
    /// or, for clackatoo's own, the program running. `None` if it isn't
    /// found.
    pub(crate) run: Option<Vec<String>>,
}

/// What logs in to `spec`, if it's an agent clackatoo knows (see
/// [`Login`]), looking for its program in `path`, a `$PATH`, then where
/// clackatoo installed it, in `dir`.
pub(crate) fn login_command(spec: &AgentSpec, dir: Option<&Path>, path: &OsStr) -> Option<Login> {
    let login = spec.login.as_deref()?;
    let mut said: Vec<String> = login.split(' ').map(str::to_owned).collect();
    let program = said.first()?.clone();
    // clackatoo's own login, as Antigravity's: this program, whatever's on
    // `PATH`.
    let own = (program == "clackatoo")
        .then(|| std::env::current_exe().ok().filter(|exe| exe.is_file()))
        .flatten();
    let found = own.or_else(|| clackatoo_syntax::servers::find_on(&program, path));
    let installed = || {
        let Launch::Known(known) = &spec.launch else {
            return None;
        };
        let bin = known.adapter.installed_program(&install_dir(dir?, known));
        Some(bin.with_file_name(&program)).filter(|bin| bin.is_file())
    };
    let found = found.or_else(|| {
        let bin = installed();
        if let Some(shown) = bin.as_deref().and_then(Path::to_str) {
            shown.clone_into(&mut said[0]);
        }
        bin
    });
    let run = found
        .and_then(|found| found.to_str().map(str::to_owned))
        .map(|found| {
            let mut run = said.clone();
            run[0] = found;
            run
        });
    Some(Login { said, run })
}

/// What a session the agent called `name` is slow to open may be waiting
/// on, to tell the user: for an agent clackatoo knows, an MCP server in
/// its config that needs a login, naming those reached over the network
/// where its config can be read for them (Codex's), and what lists them;
/// for any other, something it starts with each session. `home` is the
/// user's home directory.
///
/// A known agent given a command of the user's is still that agent, with
/// the same config. Reads one small file at most.
pub(crate) fn stuck_hint(name: &str, home: &Path) -> String {
    /// The most servers named: plenty to go on.
    const NAMED: usize = 3;
    let Some(servers) = known(name).map(|known| &known.servers) else {
        return "It may be waiting on something it starts with each session, such as an MCP \
                server that needs a login."
            .to_owned();
    };
    let remote = servers
        .remote
        .map(|remote| remote(home))
        .unwrap_or_default();
    let named = if remote.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = (remote.iter().take(NAMED))
            .map(|name| format!("`{name}`"))
            .collect();
        format!(" (e.g. {})", names.join(", "))
    };
    format!(
        "It may be waiting on an MCP server in {} that needs a login{named}: try `{}` in a \
         terminal.",
        servers.set_in, servers.list
    )
}

/// The MCP servers Codex's config sets that it reaches over the network,
/// by name: those are the ones that may need a login (by OAuth). The
/// config is `config.toml` in `$CODEX_HOME`, or else in `.codex` in
/// `home`, as Codex finds it.
fn codex_remote_servers(home: &Path) -> Vec<String> {
    let dir = (std::env::var_os("CODEX_HOME"))
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| home.join(".codex"), PathBuf::from);
    codex_remote_servers_in(&dir.join("config.toml"))
}

/// The MCP servers Codex's config at `path` sets that have a `url`, and
/// aren't turned off, by name. None if it isn't there, or isn't TOML.
fn codex_remote_servers_in(path: &Path) -> Vec<String> {
    /// The most of a config read: one is a few kilobytes.
    const MOST: u64 = 1024 * 1024;
    // Read on the event loop: a FIFO, or anything else that isn't a file,
    // could keep it waiting.
    let read = crate::regular::open(path, crate::regular::Links::Follow).and_then(|file| {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(file, MOST), &mut text)?;
        Ok(text)
    });
    let Some(document) = (read.ok()).and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
    else {
        return Vec::new();
    };
    let Some(servers) = (document.get("mcp_servers")).and_then(toml_edit::Item::as_table_like)
    else {
        return Vec::new();
    };
    let remote = |server: &toml_edit::Item| {
        server.as_table_like().is_some_and(|server| {
            server
                .get("url")
                .and_then(toml_edit::Item::as_str)
                .is_some()
                && server.get("enabled").and_then(toml_edit::Item::as_bool) != Some(false)
        })
    };
    (servers.iter())
        .filter(|(_, server)| remote(server))
        .map(|(name, _)| name)
        // Codex's own rule for a server's name, which keeps anything else
        // out of what's shown.
        .filter(|name| {
            !name.is_empty()
                && (name.bytes()).all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(str::to_owned)
        .collect()
}

/// Where `known`'s adapter is installed in `dir`: in a directory named
/// for its version, so a build pinning another installs that beside it.
pub(crate) fn install_dir(dir: &Path, known: &Known) -> PathBuf {
    dir.join(format!("{}-{}", known.name, known.version))
}

/// Installs `known`'s adapter in `dir`, unless another daemon did
/// meanwhile, and removes its other versions: all but those a process,
/// of this daemon or another, runs from, and those in `keep`, which the
/// config names. Holds `dir`'s lock throughout, so no two daemons
/// install or remove at once. Takes a while: it's for a thread of its
/// own.
///
/// # Errors
///
/// If npm can't install it, saying why.
pub(crate) fn install(dir: &Path, known: &Known, keep: &[PathBuf]) -> Result<(), String> {
    let failed =
        |err: &dyn std::fmt::Display| format!("{} couldn't be installed: {err}", known.title);
    fs::create_dir_all(dir).map_err(|err| failed(&err))?;
    let _locked = clackatoo_syntax::servers::lock(dir).map_err(|err| failed(&err))?;
    let installed = install_dir(dir, known);
    if known.installed_program(&installed).is_file() {
        log::info!("{} was installed meanwhile", known.title);
    } else {
        known
            .adapter
            .install(&installed)
            .map_err(|err| failed(&err))?;
        known.prepare(&installed).map_err(|err| failed(&err))?;
    }
    remove_others(dir, known, &installed, keep);
    Ok(())
}

/// The directory in `dir` of the latest of `known`'s earlier patch levels
/// at the same upstream version, as `codex-2.2.2+clackatoo.1` is of
/// `2.2.2+clackatoo.2`, with npm's install of it whole, if there is one:
/// a build patching the adapter anew prepares it from that (see
/// [`upgrade`]).
pub(crate) fn earlier_patch(dir: &Path, known: &Known) -> Option<PathBuf> {
    let (upstream, level) = known.version.split_once("+clackatoo.")?;
    let level: u32 = level.parse().ok()?;
    let prefix = format!("{}-{upstream}+clackatoo.", known.name);
    (fs::read_dir(dir).into_iter().flatten().flatten())
        .filter_map(|entry| {
            let name = entry.file_name();
            let earlier: u32 = name.to_str()?.strip_prefix(&prefix)?.parse().ok()?;
            (earlier < level).then(|| (earlier, entry.path()))
        })
        .filter(|(_, path)| known.original(path).is_file())
        .max_by_key(|&(earlier, _)| earlier)
        .map(|(_, path)| path)
}

/// Prepares `known`'s adapter at its patch level from an earlier one in
/// `dir` (see [`earlier_patch`]), with nothing fetched: npm's install of
/// it is linked into a directory beside it, the patch made there, checking
/// what it patches is what was pinned, and that put in place at once.
/// Then removes other versions as [`install`] does, keeping those in
/// `keep` and those running. Takes a while: it's for a thread of its own.
///
/// # Errors
///
/// If there's no earlier patch level, or it can't be prepared, as when
/// it isn't what was pinned: it's then installed as any other.
pub(crate) fn upgrade(dir: &Path, known: &Known, keep: &[PathBuf]) -> Result<(), String> {
    #[cfg(test)]
    if let Some(program) = tests::prepared_in(dir) {
        return upgrade_with(dir, known, keep, |dir| {
            let path = known.installed_program(dir);
            fs::write(&path, program)?;
            fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        });
    }
    upgrade_with(dir, known, keep, |dir| known.prepare(dir))
}

/// As [`upgrade`] does, preparing it with `prepare`.
fn upgrade_with(
    dir: &Path,
    known: &Known,
    keep: &[PathBuf],
    prepare: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let failed = |err: &dyn std::fmt::Display| {
        format!(
            "{}'s adapter couldn't be brought up to date: {err}",
            known.title
        )
    };
    let _locked = clackatoo_syntax::servers::lock(dir).map_err(|err| failed(&err))?;
    let installed = install_dir(dir, known);
    if known.installed_program(&installed).is_file() {
        log::info!("{} was installed meanwhile", known.title);
    } else {
        let from = earlier_patch(dir, known)
            .ok_or_else(|| failed(&"no earlier version of it is installed"))?;
        log::info!("preparing {} from {}", installed.display(), from.display());
        // Left by an upgrade cut off before it finished: none is under
        // way, as this one holds the lock.
        let staged = dir.join(format!(".{}-{}", known.name, known.version));
        remove_all(&staged).map_err(|err| failed(&err))?;
        let skip = known.prepared(&from);
        let prepared = link_tree(&from, &staged, &skip).and_then(|()| prepare(&staged));
        if let Err(err) = prepared {
            let _ = fs::remove_dir_all(&staged);
            return Err(failed(&err));
        }
        // What an install cut off left there, with no program.
        remove_all(&installed).map_err(|err| failed(&err))?;
        fs::rename(&staged, &installed).map_err(|err| failed(&err))?;
    }
    remove_others(dir, known, &installed, keep);
    Ok(())
}

/// Removes `path`, and all in it, if it's there.
fn remove_all(path: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Makes `to` as `from` is, but for the files in `skip`: each file a hard
/// link to `from`'s, or a copy where it can't be one, and each symbolic
/// link the same. Nothing writes an installed adapter's files in place,
/// so the two share them safely; anything else, as a FIFO, is left out.
fn link_tree(from: &Path, to: &Path, skip: &[PathBuf]) -> std::io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if skip.contains(&source) {
            continue;
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            link_tree(&source, &target, skip)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
        } else if kind.is_file() && fs::hard_link(&source, &target).is_err() {
            fs::copy(&source, &target)?;
        }
    }
    fs::set_permissions(to, fs::metadata(from)?.permissions())
}

/// Removes `known`'s other versions in `dir` than `installed`: all but
/// those a process, of this daemon or another, runs from, and those in
/// `keep`, which the config names.
fn remove_others(dir: &Path, known: &Known, installed: &Path, keep: &[PathBuf]) {
    let prefix = format!("{}-", known.name);
    let others = (fs::read_dir(dir).into_iter().flatten().flatten())
        .filter(|other| {
            (other.file_name().to_str()).is_some_and(|name| {
                name.strip_prefix(&prefix)
                    .is_some_and(|version| version.starts_with(|ch: char| ch.is_ascii_digit()))
            })
        })
        .map(|other| other.path())
        .filter(|path| *path != installed && !keep.iter().any(|kept| kept.starts_with(path)))
        .collect::<Vec<_>>();
    if others.is_empty() {
        return;
    }
    let running = running_from();
    for path in others {
        if running.iter().any(|program| program.starts_with(&path)) {
            log::info!("keeping {}, which is running", path.display());
            continue;
        }
        log::info!("removing {}, an older version", path.display());
        if let Err(err) = fs::remove_dir_all(&path) {
            log::warn!("{} couldn't be removed: {err}", path.display());
        }
    }
}

/// What every process this user can see runs: its executable, and its
/// arguments that are paths, as Node's script is.
fn running_from() -> Vec<PathBuf> {
    let mut running = Vec::new();
    let processes = fs::read_dir("/proc").into_iter().flatten().flatten();
    for process in processes {
        let path = process.path();
        if !(process.file_name().to_str())
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        if let Ok(exe) = fs::read_link(path.join("exe")) {
            running.push(exe);
        }
        let command = fs::read(path.join("cmdline")).unwrap_or_default();
        running.extend(
            (command.split(|&byte| byte == 0))
                .filter(|arg| arg.first() == Some(&b'/'))
                .map(|arg| PathBuf::from(String::from_utf8_lossy(arg).into_owned())),
        );
    }
    running
}

/// Whether `path` is a file that can be run.
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The agents directories whose upgrades tests prepare themselves,
    /// with the program each writes: a made-up bundle never passes the
    /// pinned one's check.
    static PREPARED: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

    /// Has upgrades in `dir` write `program` as the prepared adapter,
    /// rather than patch the bundle, for this test.
    pub(crate) fn prepare_in(dir: &Path, program: String) {
        PREPARED.lock().unwrap().push((dir.to_owned(), program));
    }

    pub(super) fn prepared_in(dir: &Path) -> Option<String> {
        let prepared = PREPARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (prepared.iter())
            .find(|(each, _)| each == dir)
            .map(|(_, program)| program.clone())
    }

    /// The agents `text`, the daemon's config, adds, or what's wrong with
    /// it, if anything is.
    fn parse(text: &str) -> Result<Vec<CustomAgent>, String> {
        crate::settings::Settings::from_lua(text)
            .map(|settings| settings.agents().agents)
            .map_err(|failure| failure.said)
    }

    /// The agents `text`, the daemon's config, sets.
    fn configured(text: &str) -> Configured {
        crate::settings::Settings::from_lua(text).unwrap().agents()
    }

    fn custom(name: &str, command: &[&str]) -> CustomAgent {
        CustomAgent {
            name: name.to_owned(),
            title: None,
            command: command.iter().map(OsString::from).collect(),
            starting_mode: None,
            tasks: None,
        }
    }

    /// A login is run by the absolute path its program was found at, only
    /// in `PATH`'s absolute directories: never one planted in a chat's
    /// folder, which a `.` or an empty entry would find there (CLACKA-233).
    /// What the chat says to type keeps the bare name.
    #[test]
    fn a_login_runs_its_program_by_its_absolute_path_never_from_the_chats_folder() {
        use std::os::unix::fs::PermissionsExt;
        let chat = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        // Never run: only looked for.
        for dir in [chat.path(), bin.path()] {
            let program = dir.join("claude");
            fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let spec = AgentConfig::default().find("claude").unwrap();
        let login = |path: &str| login_command(&spec, None, OsStr::new(path)).unwrap();
        // Run in the chat's folder, `.`, an empty entry or a relative one
        // would find the one planted there: none is looked in.
        let relative = login(".::claude-bin");
        assert_eq!(relative.said, ["claude"]);
        assert_eq!(relative.run, None);
        let bin = bin.path().display().to_string();
        let found = login(&format!(".:{bin}"));
        assert_eq!(found.said, ["claude"]);
        assert_eq!(found.run, Some(vec![format!("{bin}/claude")]));
        assert!(!found.run.unwrap()[0].starts_with(&*chat.path().to_string_lossy()));
        // clackatoo's own login runs this program, not one on `PATH`.
        let spec = AgentConfig::default().find("antigravity").unwrap();
        let own = login_command(&spec, None, OsStr::new(&bin)).unwrap();
        assert_eq!(own.said, ["clackatoo", "agent", "login", "antigravity"]);
        let exe = std::env::current_exe().unwrap().display().to_string();
        assert_eq!(
            own.run.unwrap(),
            [exe.as_str(), "agent", "login", "antigravity"]
        );
    }

    /// Agents go on through upgrades unless the config or the switch
    /// says not to (CLACKA-188).
    #[test]
    fn agents_go_on_through_upgrades_unless_told_not_to() {
        assert!(AgentConfig::default().kept_through_upgrades(), "no config");
        let config = |text| AgentConfig {
            configured: configured(text),
            ..AgentConfig::default()
        };
        assert!(config("daemon.upgrade.keep_agents = true").kept_through_upgrades());
        assert!(!config("daemon.upgrade.keep_agents = false").kept_through_upgrades());
        assert!(parse("daemon.upgrade.keep_agents = \"no\"").is_err());
        let switched = AgentConfig {
            restart_on_upgrade: true,
            ..AgentConfig::default()
        };
        assert!(!switched.kept_through_upgrades());
    }

    /// The limits on chats agents start come from `daemon.subagents`
    /// (CLACKA-209).
    #[test]
    fn subagent_limits_come_from_the_config() {
        assert_eq!(
            AgentConfig::default().subagent_limits(),
            SubagentLimits::default()
        );
        let config = AgentConfig {
            configured: configured(
                "daemon.subagents.working = 4\ndaemon.subagents.default = 4\ndaemon.subagents.depth = 2",
            ),
            ..AgentConfig::default()
        };
        let limits = config.subagent_limits();
        assert_eq!((limits.working, limits.default, limits.depth), (4, 4, 2));
    }

    #[test]
    fn antigravity_installs_without_npm_and_never_uses_npx() {
        let temp = tempfile::tempdir().unwrap();
        let config = AgentConfig {
            dir: Some(temp.path().join("agents")),
            path: Some(temp.path().as_os_str().to_owned()),
            ..AgentConfig::default()
        };
        let spec = config.find("antigravity").unwrap();
        assert!(matches!(config.start(&spec), Start::Install(k) if k.name == "antigravity"));
        assert!(!config.installed(&spec));
    }

    #[test]
    fn every_known_agent_is_found_by_its_name() {
        for known in KNOWN {
            let spec = AgentConfig::default().find(known.name).unwrap();
            assert_eq!(spec.title, known.title);
            assert_eq!(
                spec.launch,
                Launch::Known(super::known(known.name).unwrap())
            );
            let Source::NpmLocked { package, lock, .. } = &known.adapter.source else {
                let Source::Release {
                    version, assets, ..
                } = &known.adapter.source
                else {
                    panic!("unpinned agent");
                };
                assert_eq!(*version, known.version);
                assert_eq!(assets.len(), 2);
                for (_, asset) in *assets {
                    assert_eq!(asset.sha256.len(), 64);
                }
                continue;
            };
            let upstream = known.version.split('+').next().unwrap();
            assert!(package.ends_with(&format!("@{upstream}")));
            // The lockfile pins that version, and what it depends on.
            let (name, version) = package.rsplit_once('@').unwrap();
            let lock: serde_json::Value = serde_json::from_str(lock).unwrap();
            let pinned = &lock["packages"][format!("node_modules/{name}")];
            assert_eq!(pinned["version"], version, "{}", known.name);
            assert!(pinned["integrity"].as_str().unwrap().starts_with("sha512-"));
        }
        assert!(known(DEFAULT).is_some());
    }

    #[test]
    fn an_unknown_agent_says_which_there_are() {
        let config = AgentConfig {
            custom: vec![custom("mine", &["mine-acp"])],
            ..AgentConfig::default()
        };
        assert_eq!(
            config.find("nope"),
            Err("there's no agent called nope: the agents are claude, codex, antigravity, gemini, mine".into())
        );
    }

    #[test]
    fn the_users_command_replaces_a_known_agents_and_keeps_the_rest() {
        let config = AgentConfig {
            custom: vec![custom("claude", &["fake", "--quick"])],
            ..AgentConfig::default()
        };
        let spec = config.find("claude").unwrap();
        assert_eq!(spec.title, "Claude Code");
        assert_eq!(spec.starting_mode.as_deref(), Some("auto"));
        assert_eq!(
            config.start(&spec),
            Start::Ready {
                command: vec!["fake".into(), "--quick".into()],
                suggest: None,
            }
        );
    }

    #[test]
    fn the_config_adds_agents_and_says_what_it_gets_wrong() {
        let parsed = parse(
            r#"
                daemon.agents.opencode = {
                  name = "OpenCode",
                  command = { "opencode", "acp" },
                  mode = "build",
                  tasks = false,
                }
                daemon.agents.claude = { command = { "claude-agent-acp" } }
            "#,
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                CustomAgent {
                    name: "opencode".into(),
                    title: Some("OpenCode".into()),
                    command: vec!["opencode".into(), "acp".into()],
                    starting_mode: Some("build".into()),
                    tasks: Some(false),
                },
                custom("claude", &["claude-agent-acp"]),
            ]
        );
        assert_eq!(parse("").unwrap(), Vec::new());
        let wrong = |text: &str| parse(text).unwrap_err();
        assert!(wrong("daemon.agents.x = { name = \"X\" }").contains("needs a command"));
        assert!(wrong("daemon.agents.x = { command = {} }").contains("list of strings"));
        assert!(wrong("daemon.agents.x = { command = { \"a\" }, mode = 1 }").contains("mode"));
        assert!(wrong("daemon.agents.x = { command = { \"a\" }, moed = \"m\" }").contains("moed"));
        assert!(wrong("daemon.agents = 3").contains("can't be set"));
        assert!(wrong("daemon.agents.x = 3").contains("is a table"));
    }

    /// An agent set can be changed after, and taken out, and reads back.
    #[test]
    fn an_agent_set_can_be_changed_and_taken_out() {
        let parsed = parse(
            r#"
                daemon.agents.mine = { command = { "mine-acp" } }
                daemon.agents.mine.tasks = false
                daemon.agents.gone = { command = { "gone" } }
                daemon.agents.gone = nil
                daemon.agents["my agent"] = { command = { "spaced" } }
                local names = {}
                for name in daemon.agents do table.insert(names, name) end
                assert(#names == 2, "two agents")
                assert(daemon.agents.mine.command[1] == "mine-acp")
            "#,
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].tasks, Some(false));
        assert_eq!(parsed[1].name, "my agent");
        // Set by the daemon, over the config.
        let config = AgentConfig {
            custom: vec![custom("mine", &["other"])],
            configured: configured("daemon.agents.mine = { command = { \"mine-acp\" } }"),
            ..AgentConfig::default()
        };
        let spec = config.find("mine").unwrap();
        assert_eq!(spec.launch, Launch::Command(vec!["other".into()]));
    }

    /// Makes an empty program at `path`, which can be run.
    fn program(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn a_known_agent_is_run_from_its_install_then_path_then_npx_and_else_installed() {
        let dir = tempfile::tempdir().unwrap();
        let (agents, bin) = (dir.path().join("agents"), dir.path().join("bin"));
        fs::create_dir_all(&bin).unwrap();
        // With its own MCP servers allowed, which upstream's adapter
        // can't keep out (see `upstreams_codex_is_offered_to_install_when_mcp_is_kept_out`).
        let config = AgentConfig {
            dir: Some(agents.clone()),
            path: Some(bin.clone().into()),
            configured: configured("daemon.mcp.codex = \"allow\""),
            ..AgentConfig::default()
        };
        let codex = known("codex").unwrap();
        let spec = config.find("codex").unwrap();
        // None of them, and no npm to install it with.
        assert!(matches!(config.start(&spec), Start::Unavailable(why) if why.contains("npm")));
        // npm: it's to be installed.
        program(&bin.join("npm"));
        assert_eq!(config.start(&spec), Start::Install(codex));
        // npx: run through it, as before, at the version pinned, and the
        // install is suggested.
        program(&bin.join("npx"));
        let npx = [
            bin.join("npx").into_os_string(),
            "--yes".into(),
            "--prefer-offline".into(),
        ];
        assert_eq!(
            config.start(&spec),
            Start::Ready {
                command: npx
                    .into_iter()
                    .chain(["@agentclientprotocol/codex-acp@2.2.2".into()])
                    .collect(),
                suggest: Some(codex),
            }
        );
        // On PATH: run from there.
        program(&bin.join("codex-acp"));
        assert_eq!(
            config.start(&spec),
            Start::Ready {
                command: vec![bin.join("codex-acp").into()],
                suggest: Some(codex),
            }
        );
        // Installed: run from there, with nothing to suggest.
        let installed = codex.installed_program(&install_dir(&agents, codex));
        program(&installed);
        assert_eq!(
            config.start(&spec),
            Start::Ready {
                command: vec![installed.into()],
                suggest: None,
            }
        );
        // With its arguments.
        let gemini = config.find("gemini").unwrap();
        let installed = known("gemini").unwrap();
        let installed = installed
            .adapter
            .installed_program(&install_dir(&agents, installed));
        program(&installed);
        assert_eq!(
            config.start(&gemini),
            Start::Ready {
                command: vec![installed.into(), "--acp".into()],
                suggest: None,
            }
        );
    }

    #[test]
    fn without_npm_to_install_with_it_isnt_suggested() {
        let dir = tempfile::tempdir().unwrap();
        program(&dir.path().join("npx"));
        let config = AgentConfig {
            dir: Some(dir.path().join("agents")),
            path: Some(dir.path().into()),
            ..AgentConfig::default()
        };
        let spec = config.find("claude").unwrap();
        assert!(matches!(
            config.start(&spec),
            Start::Ready { suggest: None, .. }
        ));
        // Nor without a directory to install in, which says to use the
        // config when there's nothing else.
        let config = AgentConfig {
            path: Some(dir.path().join("none").into()),
            ..AgentConfig::default()
        };
        let Start::Unavailable(why) = config.start(&spec) else {
            panic!()
        };
        assert!(why.contains("daemon/init.lua"), "{why}");
    }

    #[test]
    fn an_install_keeps_what_runs_and_what_the_config_names() {
        let dir = tempfile::tempdir().unwrap();
        let codex = known("codex").unwrap();
        let installed = codex.installed_program(&install_dir(dir.path(), codex));
        program(&installed);
        let old = |version: &str| dir.path().join(format!("codex-{version}"));
        for version in ["1.0.0", "1.1.0", "1.2.0"] {
            fs::create_dir_all(old(version).join("bin")).unwrap();
        }
        fs::create_dir_all(dir.path().join("codex-mine")).unwrap();
        // A process running from 1.1.0, as another daemon's agent would.
        let script = old("1.1.0").join("bin/agent");
        fs::write(&script, "#!/bin/sh\nread line\n").unwrap();
        let mut running = std::process::Command::new("sh")
            .arg(&script)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Installed already, by another daemon: nothing's run, and the
        // others are cleared but for those kept.
        let named = old("1.2.0").join("bin/acp");
        install(dir.path(), codex, &[named]).unwrap();
        running.kill().unwrap();
        running.wait().unwrap();
        assert!(installed.is_file());
        assert!(!old("1.0.0").exists());
        assert!(old("1.1.0").exists(), "it's running");
        assert!(old("1.2.0").exists(), "the config names it");
        assert!(dir.path().join("codex-mine").exists(), "not a version");
    }

    #[test]
    fn codexs_remote_mcp_servers_are_found_by_their_url() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        assert!(codex_remote_servers_in(&config).is_empty(), "no config");
        fs::write(
            &config,
            r#"
model = "gpt-5"

[mcp_servers.acta]
url = "https://acta.peios.org/mcp"

[mcp_servers.files]
command = "npx"
args = ["files-mcp"]

[mcp_servers.linear]
url = "https://mcp.linear.app/mcp"
enabled = false

[mcp_servers."we`ird"]
url = "https://example.com"

[mcp_servers.sentry]
url = "https://mcp.sentry.dev/mcp"
"#,
        )
        .unwrap();
        assert_eq!(codex_remote_servers_in(&config), ["acta", "sentry"]);
        fs::write(&config, "[mcp_servers\nurl =").unwrap();
        assert!(codex_remote_servers_in(&config).is_empty(), "not TOML");
        // A FIFO, which a read would wait on for a writer, is passed over
        // at once.
        let fifo = dir.path().join("fifo.toml");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            0,
        )
        .unwrap();
        assert!(codex_remote_servers_in(&fifo).is_empty(), "not a file");
    }

    #[test]
    fn a_slow_session_names_where_each_agent_is_told_its_mcp_servers() {
        let home = tempfile::tempdir().unwrap();
        let claude = stuck_hint("claude", home.path());
        assert!(claude.contains("`claude mcp list`"), "{claude}");
        assert!(claude.contains("~/.claude.json"), "{claude}");
        assert!(stuck_hint("gemini", home.path()).contains("`gemini mcp list`"));
        let mine = stuck_hint("opencode", home.path());
        assert_eq!(
            mine,
            "It may be waiting on something it starts with each session, such as an MCP server \
             that needs a login."
        );
    }

    /// Codex's adapter on `PATH` or through `npx` is upstream's, which
    /// can't keep its own MCP servers out: with those off, as they are
    /// unless the config allows them, it's offered to install rather than
    /// run to fail. Claude Code's through `npx` is at the version pinned,
    /// which can, so it's run as before.
    #[test]
    fn upstreams_codex_is_offered_to_install_when_mcp_is_kept_out() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let config = AgentConfig {
            dir: Some(dir.path().join("agents")),
            path: Some(bin.clone().into()),
            ..AgentConfig::default()
        };
        let codex = known("codex").unwrap();
        let spec = config.find("codex").unwrap();
        program(&bin.join("npx"));
        program(&bin.join("codex-acp"));
        // No npm: nothing to offer, so it runs as it can, and says why
        // it can't go on (see `server/chats.rs`).
        assert!(matches!(
            config.start(&spec),
            Start::Ready { suggest: None, .. }
        ));
        program(&bin.join("npm"));
        assert_eq!(config.start(&spec), Start::Install(codex));
        let claude = config.find("claude").unwrap();
        let Start::Ready { command, suggest } = config.start(&claude) else {
            panic!("Claude Code's pinned adapter keeps them out");
        };
        assert!(command.contains(&"@agentclientprotocol/claude-agent-acp@0.98.0".into()));
        assert_eq!(suggest, known("claude"));
        assert!(!known("claude").unwrap().patched());
        assert!(codex.patched());
    }

    /// The `+clackatoo.N` directories of `name` at `upstream` in `dir`
    /// for each level in `levels`, with npm's install of it whole.
    fn earlier(dir: &Path, known: &Known, levels: &[&str]) -> Vec<PathBuf> {
        let upstream = known.version.split('+').next().unwrap();
        (levels.iter())
            .map(|level| {
                let path = dir.join(format!("{}-{upstream}+clackatoo.{level}", known.name));
                let original = known.original(&path);
                fs::create_dir_all(original.parent().unwrap()).unwrap();
                fs::write(&original, "bundle").unwrap();
                path
            })
            .collect()
    }

    /// Only an earlier patch level of the same upstream version, installed
    /// whole, is brought up: the latest of those.
    #[test]
    fn only_a_whole_earlier_patch_level_is_brought_up() {
        let dir = tempfile::tempdir().unwrap();
        let codex = known("codex").unwrap();
        assert_eq!(earlier_patch(dir.path(), codex), None);
        let (_, level) = codex.version.split_once("+clackatoo.").unwrap();
        let level: u32 = level.parse().unwrap();
        // The same level, a later one, another upstream version, and one
        // npm never finished.
        earlier(
            dir.path(),
            codex,
            &[&level.to_string(), &(level + 1).to_string()],
        );
        fs::create_dir_all(dir.path().join("codex-2.2.1+clackatoo.1/codex-acp")).unwrap();
        fs::create_dir_all(dir.path().join("codex-2.2.2+clackatoo.0")).unwrap();
        assert_eq!(earlier_patch(dir.path(), codex), None);
        let older = earlier(dir.path(), codex, &["0", &(level - 1).to_string()]);
        assert_eq!(earlier_patch(dir.path(), codex), Some(older[1].clone()));
        // Nor any agent clackatoo doesn't patch.
        assert_eq!(earlier_patch(dir.path(), known("claude").unwrap()), None);
    }

    /// An earlier patch level is linked into place at the one pinned,
    /// with nothing fetched: npm's files are shared, its links kept, what
    /// the patch wrote there written afresh, and nothing left beside it.
    /// The earlier one stays while something runs from it, as an agent
    /// carried through the daemon's upgrade does.
    #[test]
    fn an_earlier_patch_level_is_brought_up_without_npm() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let (agents, bin) = (dir.path().join("agents"), dir.path().join("bin"));
        fs::create_dir_all(&bin).unwrap();
        let config = AgentConfig {
            dir: Some(agents.clone()),
            path: Some(bin.into()),
            ..AgentConfig::default()
        };
        let codex = known("codex").unwrap();
        let spec = config.find("codex").unwrap();
        let from = earlier(&agents, codex, &["1"]).remove(0);
        let [old_program, old_staged] = codex_steering::outputs(&from);
        program(&old_program);
        fs::write(&old_staged, "half written").unwrap();
        let link = from.join("codex-acp/node_modules/.bin/codex-acp");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink("../@agentclientprotocol/codex-acp/dist/index.js", &link)
            .unwrap();
        // Nor npm nor npx: it's brought up all the same.
        assert_eq!(config.start(&spec), Start::Upgrade(codex));
        assert_eq!(
            config.starting(&spec, false),
            Start::Unavailable(
                "Codex's adapter is installed with npm, which isn't here: install Node.js, \
                 then send a message to try again, or give codex a command in the daemon's \
                 config, daemon/init.lua"
                    .into()
            )
        );
        // Left by an upgrade cut off.
        let staged = agents.join(format!(".codex-{}", codex.version));
        fs::create_dir_all(staged.join("stale")).unwrap();
        let prepared = |dir: &Path| {
            assert!(!codex.installed_program(dir).exists(), "not carried over");
            assert!(
                !codex_steering::outputs(dir)[1].exists(),
                "not carried over"
            );
            fs::write(codex.installed_program(dir), "prepared")
        };
        upgrade_with(&agents, codex, &[from.join("codex-acp")], prepared).unwrap();
        let installed = install_dir(&agents, codex);
        let program = codex.installed_program(&installed);
        assert_eq!(fs::read_to_string(&program).unwrap(), "prepared");
        let inode = |path: &Path| fs::metadata(path).unwrap().ino();
        assert_eq!(
            inode(&codex.original(&installed)),
            inode(&codex.original(&from))
        );
        let link = installed.join("codex-acp/node_modules/.bin/codex-acp");
        assert_eq!(fs::read_to_string(link).unwrap(), "bundle");
        assert!(!staged.exists());
        assert!(from.exists(), "kept, as it's named");
        assert_eq!(
            config.start(&spec),
            Start::Ready {
                command: vec![program.into()],
                suggest: None,
            }
        );
        // Done already, it's done no more, and the earlier one goes once
        // nothing needs it.
        upgrade_with(&agents, codex, &[], |_| panic!("prepared again")).unwrap();
        assert!(!from.exists());
    }

    /// An earlier patch level whose bundle isn't the one pinned leaves
    /// nothing at the level pinned: Codex is then offered to install, as
    /// any other agent is.
    #[test]
    fn an_earlier_patch_level_that_fails_its_check_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (agents, bin) = (dir.path().join("agents"), dir.path().join("bin"));
        program(&bin.join("npm"));
        let config = AgentConfig {
            dir: Some(agents.clone()),
            path: Some(bin.into()),
            ..AgentConfig::default()
        };
        let codex = known("codex").unwrap();
        let spec = config.find("codex").unwrap();
        let from = earlier(&agents, codex, &["1"]).remove(0);
        assert_eq!(config.start(&spec), Start::Upgrade(codex));
        let why = upgrade(&agents, codex, &[]).unwrap_err();
        assert!(why.contains("differs from the pinned"), "{why}");
        assert!(!install_dir(&agents, codex).exists());
        assert!(!agents.join(format!(".codex-{}", codex.version)).exists());
        assert!(from.exists(), "untouched");
        assert_eq!(config.starting(&spec, false), Start::Install(codex));
    }

    /// An earlier patch level of the real bundle pinned, linked from an
    /// install of it, is brought up to the one pinned.
    #[test]
    #[ignore = "needs the pinned installed npm bundle; set CLACKATOO_CODEX_BACKPORT_DIR"]
    fn bring_up_the_pinned_adapter() {
        let from =
            std::env::var_os("CLACKATOO_CODEX_BACKPORT_DIR").expect("adapter install directory");
        let dir = tempfile::tempdir().unwrap();
        let codex = known("codex").unwrap();
        let earlier = dir.path().join("codex-2.2.2+clackatoo.1");
        link_tree(Path::new(&from), &earlier, &[]).unwrap();
        upgrade(dir.path(), codex, &[]).unwrap();
        let program = codex.installed_program(&install_dir(dir.path(), codex));
        let prepared = fs::read_to_string(program).unwrap();
        assert!(prepared.contains(&format!("version: \"{}\"", codex.version)));
        assert!(!earlier.exists());
    }

    /// The adapters that keep their own MCP servers out are those pinned:
    /// a new pin is never locked out by a version written down elsewhere.
    #[test]
    fn the_pinned_adapters_keep_their_own_mcp_servers_out() {
        for name in ["claude", "codex"] {
            let known = known(name).unwrap();
            let package = known.package().rsplit_once('@').unwrap().0;
            assert!(
                crate::agent::keeps_mcp_out(package, known.version),
                "{name}"
            );
        }
        assert!(!crate::agent::keeps_mcp_out(
            "@agentclientprotocol/codex-acp",
            "2.2.2+clackatoo.1"
        ));
        assert!(!crate::agent::keeps_mcp_out(
            "@agentclientprotocol/codex-acp",
            "2.2.2"
        ));
    }
}
