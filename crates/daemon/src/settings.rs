//! The daemon's config, in Luau: `daemon/settings.lua`, which clackatoo
//! writes, then `daemon/init.lua`, the user's own, which outdoes it, in
//! clackatoo's config directory. Modules they `require` are found in
//! `daemon/lua/`, then the `lua/` shared with the window's config.
//!
//! ```lua
//! daemon.machine.name = "jack-desktop"
//! daemon.web.mode = "tailscale"
//! daemon.history.lines = 20000
//! daemon.agents.opencode = { name = "OpenCode", command = { "opencode", "acp" } }
//! daemon.profiles.review = { about = "Reviews a change for bugs", limits = "read-only" }
//! ```
//!
//! Its options are declared in [`options`]. It's run as the daemon
//! starts or upgrades, on a thread of its own, in a sandbox (see
//! [`lua`]), never on the loop. A config that goes wrong never stops the
//! daemon: it keeps what the config last gave without a mistake, kept in
//! the state directory, or else the defaults, says what's wrong in the
//! log, and tells the first window the user is at.
//!
//! Before it, the daemon was set in TOML: `daemon/agents.toml`,
//! `machine.toml` and `web.toml`. The first build with this config moves
//! what they set into `settings.lua`, once (see [`migrate`]).

mod literal;
mod lua;
mod migrate;
pub mod options;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use literal::Literal;

use crate::agents::{self, CustomAgent, SubagentLimits};
use crate::profiles::Change;
use crate::schema::{Place, SetBy, Source, Value};

/// What the daemon's config set: its options, its agents and its
/// profiles, each with what set it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    options: BTreeMap<&'static str, (Value, SetBy)>,
    agents: Vec<(CustomAgent, SetBy)>,
    profiles: Vec<(Change, SetBy)>,
}

/// What went wrong running the daemon's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What, as it's said.
    pub said: String,
    /// Where, if it's known.
    pub at: Option<Place>,
}

/// The daemon's config as it starts: what it set, and what windows are
/// told of it.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    /// What it set, or the last that ran without a mistake, or none.
    pub settings: Settings,
    /// What the first window the user is at is told: what was moved from
    /// the old TOML files, and what went wrong.
    pub notes: Vec<String>,
}

/// The file in the state directory the settings that last ran without a
/// mistake are kept in, as Luau, to run in their place when they go
/// wrong.
pub const LAST_GOOD: &str = "daemon-config.lua";

/// What `settings.lua` starts with.
pub(crate) const HEADER: &str = "\
-- Written by clackatoo: the daemon's settings.
-- Your own config goes in init.lua, which runs after this and outdoes it.
";

/// How long the daemon waits for its config: past it, what's still
/// running is left to stop by itself.
const WAIT: Duration = Duration::from_secs(1);

impl Settings {
    /// What `text` sets, run as the daemon's `init.lua` alone, with no
    /// modules: for tests, and tools.
    ///
    /// # Errors
    ///
    /// What went wrong running it.
    pub fn from_lua(text: &str) -> Result<Self, Failure> {
        let chunk = lua::Chunk {
            path: "daemon/init.lua".into(),
            text: text.to_owned(),
            source: Source::Config,
        };
        lua::run(&[chunk], &[])
    }

    /// The value of the option called `name`: what the config set, or its
    /// default.
    ///
    /// # Panics
    ///
    /// If there's no option called `name`.
    #[must_use]
    pub fn value(&self, name: &str) -> Value {
        let info = options::find(name).expect("an option the daemon has");
        (self.options.get(info.name)).map_or_else(|| options::default_of(info), |(v, _)| v.clone())
    }

    /// What set the option called `name`: [`SetBy::DEFAULT`] if nothing
    /// did.
    #[must_use]
    pub fn set_by(&self, name: &str) -> SetBy {
        (self.options.get(name)).map_or(SetBy::DEFAULT, |(_, by)| by.clone())
    }

    /// Whether config set the option called `name`.
    fn is_set(&self, name: &str) -> bool {
        self.options.contains_key(name)
    }

    fn on(&self, name: &str) -> bool {
        matches!(self.value(name), Value::Bool(true))
    }

    fn number(&self, name: &str) -> u32 {
        match self.value(name) {
            Value::Number(number) => number,
            _ => 0,
        }
    }

    fn text(&self, name: &str) -> String {
        match self.value(name) {
            Value::Text(text) => text,
            Value::Choice(word) => word.to_owned(),
            _ => String::new(),
        }
    }

    /// A new chat's Subagents setting: as set, or else the default, as
    /// far as `subagents.working` lets it.
    fn subagents_default(&self) -> u32 {
        let default = self.number("subagents.default");
        if self.is_set("subagents.default") {
            default
        } else {
            default.min(self.number("subagents.working"))
        }
    }

    /// What's wrong between options, which are each checked as they're
    /// set: at the line that set the one that can't be as it is.
    fn checked(&self) -> Result<(), Failure> {
        let wrong = |name: &str, said: String| {
            Err(Failure {
                said: match self.set_by(name).at {
                    Some(at) => format!("{}:{}: {said}", at.file, at.line),
                    None => said,
                },
                at: self.set_by(name).at,
            })
        };
        if self.is_set("subagents.default")
            && self.number("subagents.default") > self.number("subagents.working")
        {
            return wrong(
                "subagents.default",
                "daemon.subagents.default is at most daemon.subagents.working".to_owned(),
            );
        }
        if self.number("images.terminal_mb") > self.number("images.total_mb") {
            return wrong(
                "images.terminal_mb",
                "daemon.images.terminal_mb is at most daemon.images.total_mb".to_owned(),
            );
        }
        if let Err((name, said)) = self.web_checked() {
            return wrong(name, said);
        }
        Ok(())
    }

    /// What's wrong with the web app's settings together, and which one
    /// to blame.
    fn web_checked(&self) -> Result<(), (&'static str, String)> {
        let mode = self.text("web.mode");
        if mode == "off" {
            return Ok(());
        }
        let blame = |name: &'static str, said: &str| Err((name, said.to_owned()));
        if mode == "tailscale" {
            for public in ["web.port", "web.address", "web.certificate", "web.key"] {
                if self.is_set(public) {
                    return blame(
                        public,
                        "daemon.web.port, address, certificate and key are for public: \
                         tailscale listens on a unix socket, which \
                         `tailscale serve --bg unix:PATH` serves",
                    );
                }
            }
        } else {
            if self.is_set("web.socket") {
                return blame(
                    "web.socket",
                    "daemon.web.socket is for tailscale: public listens on address and port",
                );
            }
            if self.text("web.certificate").is_empty() || self.text("web.key").is_empty() {
                return blame(
                    "web.mode",
                    "public needs daemon.web.certificate and daemon.web.key: the paths of the \
                     certificate's PEM file and its private key's",
                );
            }
            if self.text("web.origin").is_empty() {
                return blame(
                    "web.mode",
                    "public needs daemon.web.origin: the address the app is reached at, as \
                     https://host:port",
                );
            }
            let address = self.text("web.address");
            if address.parse::<std::net::IpAddr>().is_err() {
                return Err((
                    "web.address",
                    format!("daemon.web.address is an IP address to listen on, not {address:?}"),
                ));
            }
        }
        let origin = self.text("web.origin");
        if !origin.is_empty() {
            let origin = origin.trim_end_matches('/').to_ascii_lowercase();
            if !origin.starts_with("https://") || origin[8..].contains('/') {
                return Err((
                    "web.origin",
                    format!(
                        "daemon.web.origin is the address the app is reached at, as \
                         https://host:port, not {origin:?}"
                    ),
                ));
            }
        }
        Ok(())
    }

    /// What it says of agents: those it adds, the profiles it adds or
    /// changes, the limits on chats agents start, and whether agents go
    /// on through upgrades.
    #[must_use]
    pub fn agents(&self) -> agents::Configured {
        let small = |name| u8::try_from(self.number(name)).unwrap_or(u8::MAX);
        agents::Configured {
            agents: self.agents.iter().map(|(agent, _)| agent.clone()).collect(),
            profiles: self
                .profiles
                .iter()
                .map(|(change, _)| change.clone())
                .collect(),
            subagents: SubagentLimits {
                working: small("subagents.working"),
                default: u8::try_from(self.subagents_default()).unwrap_or(u8::MAX),
                open: self.number("subagents.open") as usize,
                started: self.number("subagents.started"),
                depth: small("subagents.depth"),
                hops: small("subagents.hops"),
                notice_turns: self.number("subagents.notice_turns"),
            },
            keep_agents: self.on("upgrade.keep_agents"),
        }
    }

    /// The name other machines show this one by, if it sets one.
    pub(crate) fn machine_name(&self) -> Option<String> {
        Some(self.text("machine.name")).filter(|name| !name.is_empty())
    }

    /// Whether the daemon takes a higher priority and a place of its own.
    #[must_use]
    pub fn priority(&self) -> bool {
        self.on("machine.priority")
    }

    /// The limits on images' memory.
    pub(crate) fn images(&self) -> crate::ImageLimits {
        let mib = |name| u64::from(self.number(name)) << 20;
        crate::ImageLimits {
            per_owner: mib("images.terminal_mb"),
            total: mib("images.total_mb"),
        }
    }

    /// How terminals' history is kept, in `dir`, unless it isn't.
    pub(crate) fn history(&self, dir: PathBuf) -> Option<crate::HistoryConfig> {
        if !self.on("history.keep") {
            return None;
        }
        let mut config = crate::HistoryConfig::in_dir(dir);
        config.lines = self.number("history.lines") as usize;
        config.every = Duration::from_secs(self.number("history.every").into());
        Some(config)
    }

    /// The web app's settings, keeping what it keeps in `data`, with the
    /// password's hash `password`: `None` if it's off, or a setting it
    /// needs isn't there.
    ///
    /// # Errors
    ///
    /// Why it's off when it's set to be on.
    pub(crate) fn web(
        &self,
        data: PathBuf,
        password: Option<String>,
    ) -> Result<Option<crate::web::config::WebConfig>, String> {
        use crate::web::config::{WebConfig, WebMode};
        let mode = match self.text("web.mode").as_str() {
            "tailscale" => WebMode::Tailscale,
            "public" => WebMode::Public {
                certificate: self.text("web.certificate").into(),
                key: self.text("web.key").into(),
            },
            _ => return Ok(None),
        };
        let mut config = WebConfig::new(mode, data);
        let socket = self.text("web.socket");
        if !socket.is_empty() {
            config.socket = PathBuf::from(socket);
        }
        if !config.tailscale() {
            let ip = (self.text("web.address").parse())
                .map_err(|_| "daemon.web.address isn't an IP address".to_owned())?;
            config.address.set_ip(ip);
            config
                .address
                .set_port(u16::try_from(self.number("web.port")).unwrap_or_default());
        }
        config.login = Some(self.text("web.login")).filter(|login| !login.is_empty());
        if let Some(hash) = &password {
            crate::web::check_hash(hash).map_err(|err| format!("the password: {err}"))?;
        } else if !config.tailscale() {
            return Err("public needs a password: `clackatoo web password` sets one".into());
        }
        config.password = password;
        config.origin = Some(self.text("web.origin"))
            .filter(|origin| !origin.is_empty())
            .map(|origin| origin.trim_end_matches('/').to_ascii_lowercase());
        config.expire_after = Duration::from_hours(u64::from(self.number("web.expire_days")) * 24);
        config.password_every = Some(self.number("web.password_days"))
            .filter(|&days| days > 0)
            .map(|days| Duration::from_hours(u64::from(days) * 24));
        if self.is_set("web.ask") {
            config.ask = self.on("web.ask");
        }
        config.lock_after = self.number("web.lock_after");
        Ok(Some(config))
    }

    /// It, as Luau, that sets it again: what [`LAST_GOOD`] keeps.
    fn to_lua(&self) -> String {
        let mut text = String::from(
            "-- The daemon's config as it last ran without a mistake, kept by clackatoo\n\
             -- to run in its place when it goes wrong. Change daemon/init.lua, not this.\n\n",
        );
        for (name, (value, _)) in &self.options {
            let literal = match value {
                Value::Bool(on) => Literal::Bool(*on),
                Value::Number(number) => Literal::Number(number.to_string()),
                Value::Decimal(number) => Literal::Number(number.to_string()),
                Value::List(numbers) => Literal::List(
                    (numbers.iter())
                        .map(|number| Literal::Number(number.to_string()))
                        .collect(),
                ),
                Value::Choice(word) => Literal::Text((*word).to_owned()),
                Value::Text(text) => Literal::Text(text.clone()),
            };
            let _ = writeln!(text, "daemon.{name} = {}", literal.written(0));
        }
        for (agent, _) in &self.agents {
            text.push_str(&statement(
                "daemon.agents",
                &agent.name,
                &agent_literal(agent),
            ));
        }
        for (change, _) in &self.profiles {
            text.push_str(&statement(
                "daemon.profiles",
                &change.name,
                &profile_literal(change),
            ));
        }
        text
    }
}

/// `name` in `table` set to `value`, as a line of config.
fn statement(table: &str, name: &str, value: &Literal) -> String {
    let path = literal::path(table, name);
    let before = path.len() + 3;
    format!("{path} = {}\n", value.written(before))
}

/// `agent`, as config sets it.
fn agent_literal(agent: &CustomAgent) -> Literal {
    let mut fields = Vec::new();
    if let Some(title) = &agent.title {
        fields.push(("name".to_owned(), Literal::Text(title.clone())));
    }
    let command = (agent.command.iter())
        .map(|word| Literal::Text(word.to_string_lossy().into_owned()))
        .collect();
    fields.push(("command".to_owned(), Literal::List(command)));
    if let Some(mode) = &agent.starting_mode {
        fields.push(("mode".to_owned(), Literal::Text(mode.clone())));
    }
    if let Some(tasks) = agent.tasks {
        fields.push(("tasks".to_owned(), Literal::Bool(tasks)));
    }
    Literal::Table(fields)
}

/// `change`, as config sets it.
fn profile_literal(change: &Change) -> Literal {
    let mut fields = Vec::new();
    let mut text = |name: &str, value: &Option<String>| {
        if let Some(value) = value {
            fields.push((name.to_owned(), Literal::Text(value.clone())));
        }
    };
    text("about", &change.about);
    text("prompt", &change.prompt);
    text(
        "limits",
        &change.limits.map(|limits| limits.name().to_owned()),
    );
    text("agent", &change.agent);
    for (name, values) in [("model", &change.models), ("effort", &change.efforts)] {
        let Some(values) = values else { continue };
        let literal = match values.as_slice() {
            [(any, value)] if any == "*" => Literal::Text(value.clone()),
            values => Literal::Table(
                (values.iter())
                    .map(|(agent, value)| (agent.clone(), Literal::Text(value.clone())))
                    .collect(),
            ),
        };
        fields.push((name.to_owned(), literal));
    }
    Literal::Table(fields)
}

/// Where the daemon's config is, and what it keeps.
#[derive(Clone, Debug)]
pub struct Dirs {
    /// `daemon/` in clackatoo's config directory.
    pub daemon: PathBuf,
    /// The `lua/` beside it, shared with the window's config.
    pub shared: PathBuf,
    /// The state directory, where [`LAST_GOOD`] and the web app's
    /// password are kept, if there's one.
    pub state: Option<PathBuf>,
}

impl Dirs {
    /// As the environment says: in [`crate::config_dir`], or
    /// `$CLACKATOO_CONFIG`, a folder in its place, and
    /// [`crate::state_dir`]. `None` if there's no config directory, or
    /// `$CLACKATOO_CONFIG` is `off`, as for tests, whose daemons must never
    /// read, nor move, the user's own.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let config = match std::env::var_os("CLACKATOO_CONFIG").filter(|dir| !dir.is_empty()) {
            Some(off) if off == "off" => return None,
            Some(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir),
            Some(dir) => {
                log::warn!(
                    "CLACKATOO_CONFIG isn't an absolute path or `off`: {}",
                    dir.display()
                );
                return None;
            }
            None => crate::config_dir()?,
        };
        Some(Self {
            daemon: config.join("daemon"),
            shared: config.join("lua"),
            state: crate::state_dir(),
        })
    }

    /// Where `require` looks, in turn.
    fn modules(&self) -> Vec<PathBuf> {
        vec![self.daemon.join("lua"), self.shared.clone()]
    }

    /// The config's files, `settings.lua` then `init.lua`, those there.
    fn chunks(&self) -> Result<Vec<lua::Chunk>, Failure> {
        let mut chunks = Vec::new();
        for (name, source) in [
            ("settings.lua", Source::Settings),
            ("init.lua", Source::Config),
        ] {
            let path = self.daemon.join(name);
            match lua::read(&path) {
                Ok(Some(text)) => chunks.push(lua::Chunk { path, text, source }),
                Ok(None) => {}
                Err(said) => return Err(Failure { said, at: None }),
            }
        }
        Ok(chunks)
    }
}

/// Runs `chunks` on a thread of their own, waiting [`WAIT`] at most.
fn run_apart(chunks: Vec<lua::Chunk>, modules: Vec<PathBuf>) -> Result<Settings, Failure> {
    let (sender, receiver) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("daemon-config".into())
        .spawn(move || {
            let _ = sender.send(lua::run(&chunks, &modules));
        });
    if let Err(err) = spawned {
        return Err(Failure {
            said: format!("the daemon's config couldn't be run: {err}"),
            at: None,
        });
    }
    receiver.recv_timeout(WAIT).unwrap_or_else(|_| {
        Err(Failure {
            said: "the daemon's config ran too long, so it was stopped".to_owned(),
            at: None,
        })
    })
}

/// The daemon's config, as the daemon starts or upgrades: moving what the
/// old TOML files set into `settings.lua` first, if they're there and it
/// isn't, then running it. What it couldn't run is said, and what last
/// ran without a mistake is used, or the defaults.
#[must_use]
pub fn load(dirs: &Dirs) -> Loaded {
    let mut notes = Vec::new();
    let migrated = migrate::once(dirs, &mut notes);
    let chunks = match migrated {
        // What couldn't be written is run as it would have been.
        Some(text) => Ok(vec![lua::Chunk {
            path: dirs.daemon.join("settings.lua"),
            text,
            source: Source::Settings,
        }]),
        None => dirs.chunks(),
    };
    let ran = chunks.and_then(|chunks| run_apart(chunks, dirs.modules()));
    let settings = match ran {
        Ok(settings) => {
            keep_good(dirs, &settings);
            settings
        }
        Err(failure) => {
            let said = failure.said.replace(&format!("{}/", config(dirs)), "");
            log::error!("the daemon's config: {said}");
            let (settings, kept) = last_good(dirs);
            notes.push(format!(
                "the daemon's config went wrong, so it {kept}: {said}",
                kept = if kept {
                    "kept its settings from the last time it ran"
                } else {
                    "went by its defaults"
                },
            ));
            settings
        }
    };
    Loaded { settings, notes }
}

/// The daemon's config as it would be loaded now, writing nothing: for
/// the build an upgrade is to, asked what it takes, before it's started.
#[must_use]
pub fn peek(dirs: &Dirs) -> Settings {
    let chunks = match migrate::text(dirs) {
        Some(text) => Ok(vec![lua::Chunk {
            path: dirs.daemon.join("settings.lua"),
            text,
            source: Source::Settings,
        }]),
        None => dirs.chunks(),
    };
    match chunks.and_then(|chunks| run_apart(chunks, dirs.modules())) {
        Ok(settings) => settings,
        Err(_) => last_good(dirs).0,
    }
}

/// The config directory `dirs` is in, as a path's text.
fn config(dirs: &Dirs) -> String {
    (dirs.daemon.parent().unwrap_or(&dirs.daemon))
        .display()
        .to_string()
}

/// Keeps `settings` in [`LAST_GOOD`], if they've changed.
fn keep_good(dirs: &Dirs, settings: &Settings) {
    let Some(state) = &dirs.state else { return };
    let path = state.join(LAST_GOOD);
    let text = settings.to_lua();
    if std::fs::read_to_string(&path).is_ok_and(|kept| kept == text) {
        return;
    }
    if let Err(err) = write_private(&path, &text) {
        log::warn!("{} couldn't be written: {err}", path.display());
    }
}

/// What [`LAST_GOOD`] keeps, and whether it kept anything: else the
/// defaults.
fn last_good(dirs: &Dirs) -> (Settings, bool) {
    let Some(path) = dirs.state.as_ref().map(|state| state.join(LAST_GOOD)) else {
        return (Settings::default(), false);
    };
    let Ok(Some(text)) = lua::read(&path) else {
        return (Settings::default(), false);
    };
    let chunk = lua::Chunk {
        path: path.clone(),
        text,
        source: Source::Settings,
    };
    match run_apart(vec![chunk], Vec::new()) {
        Ok(settings) => (settings, true),
        Err(failure) => {
            log::warn!("{}: {}", path.display(), failure.said);
            (Settings::default(), false)
        }
    }
}

/// Writes `text` to `path`, which only the user may read, whole or not
/// at all: through a file beside it, renamed over it.
pub(crate) fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("partial");
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&partial, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    written
}

/// The file in the state directory the web app's password is kept in, as
/// an Argon2id hash, which only the user may read: never in the config,
/// which people keep in git.
pub const PASSWORD: &str = "web-password";

/// The web app's password's hash, as `state`'s [`PASSWORD`] keeps it, if
/// it's there.
///
/// # Errors
///
/// If it can't be read.
pub fn password(state: &Path) -> Result<Option<String>, String> {
    let path = state.join(PASSWORD);
    let text = lua::read(&path)?;
    Ok(text
        .map(|text| text.trim().to_owned())
        .filter(|hash| !hash.is_empty()))
}

/// Keeps `hash` as the web app's password's in `state`'s [`PASSWORD`], or
/// with none, takes it away.
///
/// # Errors
///
/// If it can't be written, or taken away.
pub fn set_password(state: &Path, hash: Option<&str>) -> std::io::Result<()> {
    let path = state.join(PASSWORD);
    match hash {
        Some(hash) => write_private(&path, &format!("{hash}\n")),
        None => match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    }
}

/// `agents.toml`'s `text` moved into `settings.lua`, as the first build
/// with the Luau config moves it: for the fuzzer.
#[cfg(feature = "fuzzing")]
pub(crate) fn migrate_agents_toml(text: &str) -> String {
    migrate::agents_text(text)
}

#[cfg(test)]
mod tests;
