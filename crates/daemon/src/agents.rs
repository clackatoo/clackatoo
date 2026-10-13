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
//!    for that version, so nothing it depends on can drift.
//! 2. On `PATH`, as `claude-agent-acp` or `codex-acp`.
//! 3. Through `npx`, at the same version, as clackatoo ran them before it
//!    installed them, so chats started then carry on.
//!
//! Run from 2 or 3, a chat offers once to install it, without waiting
//! for the answer: run from where it's installed, it starts in 140 ms
//! rather than 380, works offline, and has no npm process of about 95 MB
//! alive beside it, as `npx` keeps. Only with none of the three does a
//! chat wait for it to be installed.
//!
//! The user adds agents, or gives a known one another command, in
//! `daemon/agents.toml` in clackatoo's config directory, which is read
//! again whenever a chat starts:
//!
//! ```toml
//! [agents.opencode]
//! name = "OpenCode"
//! command = ["opencode", "acp"]
//!
//! [agents.claude]
//! command = ["claude-agent-acp"]
//! ```
//!
//! It's the daemon's, not the window's, as the daemon runs the agent:
//! it starts it again after an upgrade or a crash with no window there,
//! for a chat opened from a phone as from a desktop, on its own machine.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use clackatoo_syntax::servers::{KnownServer, Source};

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

/// An agent clackatoo knows: where its adapter comes from, and how it
/// differs from the others.
#[derive(Debug)]
pub(crate) struct Known {
    /// What chats ask for it by.
    pub(crate) name: &'static str,
    /// What to show.
    pub(crate) title: &'static str,
    /// Its adapter, which npm installs.
    pub(crate) adapter: KnownServer,
    /// The adapter's version, as its package is pinned to.
    pub(crate) version: &'static str,
    /// The adapter's arguments.
    pub(crate) args: &'static [&'static str],
    /// About how much room it takes installed, to say when offering it.
    pub(crate) size: &'static str,
    /// The mode a new chat asks for, if the agent offers it.
    pub(crate) starting_mode: Option<&'static str>,
    /// What logs in to it, to say when it isn't logged in.
    pub(crate) login: &'static str,
}

/// Known agents are the same if they have the same name.
impl PartialEq for Known {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Known {}

/// The agents clackatoo knows, checked on 2026-10-13. Their packages and
/// arguments are the ACP registry's
/// (<https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json>).
pub(crate) const KNOWN: &[Known] = &[
    Known {
        name: "claude",
        title: "Claude Code",
        adapter: KnownServer {
            program: "claude-agent-acp",
            source: locked!("claude", "@agentclientprotocol/claude-agent-acp@0.88.0"),
        },
        version: "0.88.0",
        args: &[],
        size: "540 MB",
        // Auto decides itself what needs asking about (jack, 2026-10-08).
        starting_mode: Some("auto"),
        login: "claude",
    },
    Known {
        name: "codex",
        title: "Codex",
        adapter: KnownServer {
            program: "codex-acp",
            source: locked!("codex", "@agentclientprotocol/codex-acp@2.2.2"),
        },
        version: "2.2.2",
        args: &[],
        size: "450 MB",
        // Codex starts in the mode its own config gives, as it does in a
        // terminal: Auto review for most.
        starting_mode: None,
        login: "codex login",
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
        login: "gemini",
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
}

/// An agent, ready to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentSpec {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) launch: Launch,
    pub(crate) starting_mode: Option<String>,
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
    /// Its adapter needs installing first, which clackatoo can do.
    Install(&'static Known),
    /// It can't be started: why, and what to do about it.
    Unavailable(String),
}

/// Where agents are found and installed.
#[derive(Clone, Debug, Default)]
pub struct AgentConfig {
    /// Agents set when the daemon started, such as by `$CLACKATOO_AGENT`:
    /// over those in the file, and those clackatoo knows.
    pub custom: Vec<CustomAgent>,
    /// The user's `agents.toml`, read again whenever a chat starts.
    pub file: Option<PathBuf>,
    /// Where known agents' adapters are installed, one directory each.
    /// `None` installs none.
    pub dir: Option<PathBuf>,
    /// Where adapters, `npx` and `npm` are looked for, as `PATH` lists
    /// directories. `None` is the daemon's own `PATH`.
    pub path: Option<OsString>,
}

impl Known {
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

impl AgentConfig {
    /// The agent called `name`: the one clackatoo knows, changed or
    /// replaced by the user's, or why there isn't one.
    pub(crate) fn find(&self, name: &str) -> Result<AgentSpec, String> {
        let from_file = self.file.as_deref().map(read_file).unwrap_or_default();
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
                login: known.map(|known| known.login.to_owned()),
            }),
            (None, Some(known)) => Ok(AgentSpec {
                name: known.name.to_owned(),
                title: known.title.to_owned(),
                launch: Launch::Known(known),
                starting_mode: known.starting_mode.map(str::to_owned),
                login: Some(known.login.to_owned()),
            }),
            (None, None) => {
                let mut names: Vec<&str> = KNOWN.iter().map(|known| known.name).collect();
                let added =
                    (self.custom.iter().chain(&from_file)).map(|custom| custom.name.as_str());
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

    /// What it takes to start `spec`: for a known agent, the first of
    /// its install, its adapter on `PATH`, and `npx` that's there.
    pub(crate) fn start(&self, spec: &AgentSpec) -> Start {
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
            .map(|dir| known.adapter.installed_program(&install_dir(dir, known)))
            .filter(|program| program.is_file());
        if let Some(program) = installed {
            return Start::Ready {
                command: std::iter::once(program.into_os_string())
                    .chain(args)
                    .collect(),
                suggest: None,
            };
        }
        let installable = self.dir.is_some() && self.program("npm").is_some();
        let suggest = installable.then_some(known);
        if let Some(program) = self.program(known.adapter.program) {
            return Start::Ready {
                command: std::iter::once(program.into_os_string())
                    .chain(args)
                    .collect(),
                suggest,
            };
        }
        if let Some(npx) = self.program("npx") {
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
                 command in agents.toml",
                known.title
            ))
        } else {
            Start::Unavailable(format!(
                "{}'s adapter is installed with npm, which isn't here: install Node.js, \
                 then send a message to try again, or give {} a command in agents.toml",
                known.title, known.name
            ))
        }
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

    /// The directories the user's agents are run from, as `agents.toml`
    /// and the daemon's own settings give their commands: an install
    /// never removes them.
    pub(crate) fn named_dirs(&self) -> Vec<PathBuf> {
        let from_file = self.file.as_deref().map(read_file).unwrap_or_default();
        (self.custom.iter().chain(&from_file))
            .filter_map(|custom| custom.command.first())
            .map(PathBuf::from)
            .filter(|program| program.is_absolute())
            .collect()
    }
}

/// What logs in to `spec`, if it's an agent clackatoo knows: its login
/// command, with the program's path where clackatoo installed it if it's
/// not on `PATH`, as Codex's adapter brings its own `codex`.
pub(crate) fn login_command(spec: &AgentSpec, dir: Option<&Path>) -> Option<String> {
    let login = spec.login.as_deref()?;
    let (program, rest) = login.split_once(' ').unwrap_or((login, ""));
    if clackatoo_syntax::servers::on_path(program).is_some() {
        return Some(login.to_owned());
    }
    let Launch::Known(known) = &spec.launch else {
        return Some(login.to_owned());
    };
    let Some(dir) = dir else {
        return Some(login.to_owned());
    };
    let bin = (known.adapter.installed_program(&install_dir(dir, known))).with_file_name(program);
    Some(if bin.is_file() {
        format!("{} {rest}", bin.display()).trim_end().to_owned()
    } else {
        login.to_owned()
    })
}

/// Where `known`'s adapter is installed in `dir`: in a directory named
/// for its version, so a build pinning another installs that beside it.
pub(crate) fn install_dir(dir: &Path, known: &Known) -> PathBuf {
    dir.join(format!("{}-{}", known.name, known.version))
}

/// Installs `known`'s adapter in `dir`, unless another daemon did
/// meanwhile, and removes its other versions: all but those a process,
/// of this daemon or another, runs from, and those in `keep`, which
/// `agents.toml` names. Holds `dir`'s lock throughout, so no two daemons
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
    if known.adapter.installed_program(&installed).is_file() {
        log::info!("{} was installed meanwhile", known.title);
    } else {
        known
            .adapter
            .install(&installed)
            .map_err(|err| failed(&err))?;
    }
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
        return Ok(());
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
    Ok(())
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

/// The agents `agents.toml` at `path` adds, or none if it isn't there or
/// doesn't make sense, which is logged.
fn read_file(path: &Path) -> Vec<CustomAgent> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            log::warn!("{} couldn't be read: {err}", path.display());
            return Vec::new();
        }
    };
    parse(&text).unwrap_or_else(|err| {
        log::warn!("{}: {err}", path.display());
        Vec::new()
    })
}

/// The agents an `agents.toml` adds.
fn parse(text: &str) -> Result<Vec<CustomAgent>, String> {
    let document = (text.parse::<toml_edit::DocumentMut>()).map_err(|err| err.to_string())?;
    let Some(agents) = document.get("agents") else {
        return Ok(Vec::new());
    };
    let agents = agents
        .as_table_like()
        .ok_or("agents is a table, of agents by name")?;
    let mut parsed = Vec::new();
    for (name, agent) in agents.iter() {
        let agent = (agent.as_table_like()).ok_or_else(|| format!("agents.{name} is a table"))?;
        let text = |key: &str| -> Result<Option<String>, String> {
            agent.get(key).map_or(Ok(None), |item| {
                (item.as_str().map(|text| Some(text.to_owned())))
                    .ok_or_else(|| format!("agents.{name}.{key} is a string"))
            })
        };
        let command = (agent.get("command").and_then(toml_edit::Item::as_array))
            .ok_or_else(|| format!("agents.{name} needs a command, a list of strings"))?;
        let command = (command.iter())
            .map(|word| word.as_str().map(OsString::from))
            .collect::<Option<Vec<_>>>()
            .filter(|command| !command.is_empty())
            .ok_or_else(|| format!("agents.{name}.command is a list of strings"))?;
        parsed.push(CustomAgent {
            name: name.to_owned(),
            title: text("name")?,
            command,
            starting_mode: text("mode")?,
        });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(name: &str, command: &[&str]) -> CustomAgent {
        CustomAgent {
            name: name.to_owned(),
            title: None,
            command: command.iter().map(OsString::from).collect(),
            starting_mode: None,
        }
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
                panic!("{} isn't from a lockfile", known.name);
            };
            assert!(package.ends_with(&format!("@{}", known.version)));
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
            Err("there's no agent called nope: the agents are claude, codex, gemini, mine".into())
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
    fn agents_toml_adds_agents_and_says_what_it_gets_wrong() {
        let parsed = parse(
            r#"
                [agents.opencode]
                name = "OpenCode"
                command = ["opencode", "acp"]
                mode = "build"

                [agents.claude]
                command = ["claude-agent-acp"]
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
                },
                custom("claude", &["claude-agent-acp"]),
            ]
        );
        assert_eq!(parse("").unwrap(), Vec::new());
        assert!(
            parse("[agents.x]\nname = \"X\"")
                .unwrap_err()
                .contains("needs a command")
        );
        assert!(
            parse("[agents.x]\ncommand = []")
                .unwrap_err()
                .contains("list of strings")
        );
        assert!(
            parse("[agents.x]\ncommand = [\"a\"]\nmode = 1")
                .unwrap_err()
                .contains("mode")
        );
        assert!(parse("agents = 3").is_err());
    }

    #[test]
    fn the_file_is_read_again_whenever_an_agent_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents.toml");
        let config = AgentConfig {
            file: Some(file.clone()),
            ..AgentConfig::default()
        };
        assert!(config.find("mine").is_err());
        fs::write(&file, "[agents.mine]\ncommand = [\"mine-acp\"]\n").unwrap();
        assert_eq!(config.find("mine").unwrap().title, "mine");
        // Set by the daemon, over the file.
        let config = AgentConfig {
            custom: vec![custom("mine", &["other"])],
            ..config
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
        let config = AgentConfig {
            dir: Some(agents.clone()),
            path: Some(bin.clone().into()),
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
        let installed = codex
            .adapter
            .installed_program(&install_dir(&agents, codex));
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
        // Nor without a directory to install in, which says to use
        // agents.toml when there's nothing else.
        let config = AgentConfig {
            path: Some(dir.path().join("none").into()),
            ..AgentConfig::default()
        };
        let Start::Unavailable(why) = config.start(&spec) else {
            panic!()
        };
        assert!(why.contains("agents.toml"), "{why}");
    }

    #[test]
    fn an_install_keeps_what_runs_and_what_agents_toml_names() {
        let dir = tempfile::tempdir().unwrap();
        let codex = known("codex").unwrap();
        let installed = codex
            .adapter
            .installed_program(&install_dir(dir.path(), codex));
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
        assert!(old("1.2.0").exists(), "agents.toml names it");
        assert!(dir.path().join("codex-mine").exists(), "not a version");
    }
}
