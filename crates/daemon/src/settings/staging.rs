//! What a change to the daemon's config made outside clackatoo never
//! changes by itself: what runs programs, and what lets the web in. That's
//! the agents (every field of every one), the profiles, the limits on
//! chats agents start, raised, the web app's options, and its password.
//!
//! The user approves it: what's approved is kept in the state directory
//! ([`APPROVED`], and the password's hash in [`APPROVED_PASSWORD`]), and
//! the daemon goes by it. A config that says otherwise, as one an agent
//! wrote would, is *pending*: the daemon goes on by what's approved,
//! lowering its limits if the config lowers them, and every window says
//! what's pending, for the user to apply or discard. A change discarded is
//! kept too ([`DISCARDED`]), so it isn't pending again until the config
//! says something else. The daemon goes by this as it starts and upgrades
//! as well, so a restart never takes in what wasn't approved.
//!
//! It fails closed. What's approved first is what the config says only
//! when the old TOML files move in, on that very start, or as a daemon
//! from before this first runs (see [`First`]). Otherwise, with nothing
//! approved, or what was approved gone or unreadable, nothing is: the
//! user's agents, profiles, raised limits and web app are pending, and
//! windows say why. `clackatoo web password`, and a change the user makes
//! in a window's settings menu, are the user's own, and approved as
//! they're made, each of only what it changes.
//!
//! None of this stands up to an agent that can run what it likes as the
//! user: it could write these files too, or run `clackatoo web password`.
//! What an agent may run is the agent's own permissions' to say. This
//! keeps a file an agent was let write from taking effect unseen.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::path::Path;

use super::{Dirs, Settings, When, agent_literal, lua, options, profile_literal, run_apart};
use crate::agents::{Configured, SubagentLimits};
use crate::schema::{SetBy, Source, Value};

/// The file in the state directory what's staged, as the user last
/// approved it, is kept in, as Luau: all but the password.
pub const APPROVED: &str = "daemon-approved.lua";

/// The file in the state directory the web app's password's hash, as the
/// user last approved it, is kept in, if one is: apart from [`APPROVED`],
/// so `clackatoo web password` never meets the daemon writing that.
pub const APPROVED_PASSWORD: &str = "daemon-approved-password";

/// The file in the state directory a change the user discarded is kept
/// in, as Luau, its password only as a mark of it.
pub const DISCARDED: &str = "daemon-discarded.lua";

/// What starts a comment, in [`DISCARDED`], that marks the password.
const PASSWORD_LINE: &str = "-- web password: ";

/// What's staged: what runs programs, and what lets the web in.
#[derive(Clone, Debug, PartialEq)]
pub struct Guarded {
    /// The agents, profiles and limits.
    pub configured: Configured,
    /// Each of the web app's options, `web.mode` and the rest.
    pub(crate) web: BTreeMap<&'static str, Value>,
    /// The web app's password's hash, if it has one; in what's discarded,
    /// only a mark of it (see [`mark`]).
    pub(crate) password: Option<String>,
}

impl Guarded {
    /// As `settings` say, with `password` the web app's password's hash.
    #[must_use]
    pub fn of(settings: &Settings, password: Option<String>) -> Self {
        let web = (options::ALL.iter())
            .filter(|info| info.name.starts_with("web."))
            .map(|info| (info.name, settings.value(info.name)))
            .collect();
        Self {
            configured: settings.agents(),
            web,
            password,
        }
    }

    /// Nothing approved: no agents or profiles of the config's, the
    /// limits and web app as they are by default, and no password.
    #[must_use]
    pub fn nothing() -> Self {
        Self::of(&Settings::default(), None)
    }

    /// `self`, its password only marked, as what's discarded is kept.
    fn marked(mut self) -> Self {
        self.password = self.password.as_deref().map(mark);
        self
    }
}

/// A mark of the password's hash `hash`, which says whether it's the same
/// one without saying it.
fn mark(hash: &str) -> String {
    let mut hasher = std::hash::DefaultHasher::new();
    hash.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// What's staged, as the user approved it, discarded, and what the config
/// says that's neither.
#[derive(Clone, Debug, PartialEq)]
pub struct Staged {
    /// What the user approved, which the daemon goes by.
    pub(crate) approved: Guarded,
    /// What the user discarded, if anything, its password marked.
    pub(crate) discarded: Option<Guarded>,
    /// What the config says that's neither, if anything.
    pub(crate) pending: Option<Guarded>,
    /// Why nothing is approved, if it isn't: said with what's pending.
    pub(crate) note: Option<String>,
}

impl Default for Staged {
    fn default() -> Self {
        Self {
            approved: Guarded::nothing(),
            discarded: None,
            pending: None,
            note: None,
        }
    }
}

/// Each limit on chats agents start, by its option's name after
/// `subagents.`, as a number.
fn limits(limits: &SubagentLimits) -> [(&'static str, u64); 7] {
    [
        ("working", limits.working.into()),
        ("default", limits.default.into()),
        ("open", limits.open as u64),
        ("started", limits.started.into()),
        ("depth", limits.depth.into()),
        ("hops", limits.hops.into()),
        ("notice_turns", limits.notice_turns.into()),
    ]
}

/// Whether every limit `new` sets is at most `old`'s.
#[must_use]
fn lower_or_same(new: &SubagentLimits, old: &SubagentLimits) -> bool {
    (limits(new).iter().zip(limits(old))).all(|((_, new), (_, old))| *new <= old)
}

/// The lower of each of `one`'s and `other`'s limits.
fn lowest(one: &SubagentLimits, other: &SubagentLimits) -> SubagentLimits {
    SubagentLimits {
        working: one.working.min(other.working),
        default: one.default.min(other.default),
        open: one.open.min(other.open),
        started: one.started.min(other.started),
        depth: one.depth.min(other.depth),
        hops: one.hops.min(other.hops),
        notice_turns: one.notice_turns.min(other.notice_turns),
    }
}

/// Whether `one` and `other` have the same agents and profiles, in
/// whatever order: by name.
fn same_programs(one: &Configured, other: &Configured) -> bool {
    let agents = |configured: &Configured| -> BTreeMap<String, _> {
        (configured.agents.iter())
            .map(|agent| (agent.name.clone(), agent.clone()))
            .collect()
    };
    let profiles = |configured: &Configured| -> BTreeMap<String, _> {
        (configured.profiles.iter())
            .map(|profile| (profile.name.clone(), profile.clone()))
            .collect()
    };
    agents(one) == agents(other) && profiles(one) == profiles(other)
}

impl Staged {
    /// As `fresh`, what the config says now, would have it: what the
    /// daemon goes by, which is what's approved but for limits `fresh`
    /// lowers; and what's pending, if `fresh` says otherwise of what's
    /// staged, and isn't what the user discarded.
    pub(crate) fn stage(&mut self, fresh: &Guarded) -> Guarded {
        let approved = &self.approved;
        let differs = !same_programs(&fresh.configured, &approved.configured)
            || !lower_or_same(&fresh.configured.subagents, &approved.configured.subagents)
            || fresh.web != approved.web
            || fresh.password != approved.password;
        let discarded = (self.discarded.as_ref()).is_some_and(|discarded| {
            same_programs(&discarded.configured, &fresh.configured)
                && discarded.configured.subagents == fresh.configured.subagents
                && discarded.web == fresh.web
                && discarded.password == fresh.password.as_deref().map(mark)
        });
        self.pending = (differs && !discarded).then(|| fresh.clone());
        Guarded {
            configured: Configured {
                agents: approved.configured.agents.clone(),
                profiles: approved.configured.profiles.clone(),
                subagents: lowest(&fresh.configured.subagents, &approved.configured.subagents),
                keep_agents: fresh.configured.keep_agents,
            },
            web: approved.web.clone(),
            password: approved.password.clone(),
        }
    }

    /// What's pending, as windows are told it: what changes, each with
    /// what's approved and what's pending, written as config writes them.
    #[must_use]
    pub fn changes(&self) -> Vec<(String, String, String)> {
        let Some(pending) = &self.pending else {
            return Vec::new();
        };
        changes(&self.approved, pending)
    }

    /// Which change is pending, as windows answer it: a hash of all it
    /// says, so an answer to one never applies another.
    #[must_use]
    pub fn revision(&self) -> Option<u64> {
        let pending = self.pending.as_ref()?;
        let mut hasher = std::hash::DefaultHasher::new();
        as_lua(&pending.clone().marked(), true).hash(&mut hasher);
        Some(hasher.finish())
    }

    /// Takes in what the user answered: `approved` approved, if that
    /// changed, and `discarded` discarded. What's unapproved now is the
    /// user's doing, so the note of why nothing was goes.
    pub(crate) fn decided(&mut self, approved: Option<Guarded>, discarded: Discarded) {
        if let Some(approved) = approved {
            self.approved = approved;
            self.note = None;
        }
        match discarded {
            Discarded::Same => {}
            Discarded::Nothing => self.discarded = None,
            Discarded::Now(change) => {
                self.discarded = Some(change.marked());
                self.note = None;
            }
        }
    }
}

/// What the user discarded, after they answered.
#[derive(Debug, Default)]
pub(crate) enum Discarded {
    /// As before.
    #[default]
    Same,
    /// Nothing: a change applied.
    Nothing,
    /// This change.
    Now(Guarded),
}

/// Approves, in `approved`, the option `name` as `settings` set it, as
/// the settings menu changed it: the user's own change. Only a limit on
/// chats agents start, or one of the web app's, is staged.
pub(crate) fn approve_option(approved: &mut Guarded, name: &str, settings: &Settings) {
    if let Some(info) = options::find(name)
        && info.name.starts_with("web.")
    {
        approved.web.insert(info.name, settings.value(info.name));
        return;
    }
    let fresh = settings.agents().subagents;
    let approved = &mut approved.configured.subagents;
    match name {
        "subagents.working" => approved.working = fresh.working,
        "subagents.default" => approved.default = fresh.default,
        "subagents.open" => approved.open = fresh.open,
        "subagents.started" => approved.started = fresh.started,
        "subagents.depth" => approved.depth = fresh.depth,
        "subagents.hops" => approved.hops = fresh.hops,
        "subagents.notice_turns" => approved.notice_turns = fresh.notice_turns,
        _ => {}
    }
}

/// `word` as a window shows it: as it is, but for bytes that aren't
/// UTF-8, each written `\xNN`, so none is lost or passes for another.
fn shown(word: &OsStr) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut text = String::new();
    for chunk in word.as_bytes().utf8_chunks() {
        text.push_str(chunk.valid());
        for byte in chunk.invalid() {
            let _ = write!(text, "\\x{byte:02x}");
        }
    }
    text
}

/// What `new` changes of `old`, each named, as config names it, with what
/// it was and what it is: an agent's field, `agents.claude command`, or a
/// whole agent added or taken out, `agents.mine`; a profile, as a whole;
/// each limit raised; each of the web app's options; and its password,
/// never shown.
pub(crate) fn changes(old: &Guarded, new: &Guarded) -> Vec<(String, String, String)> {
    let mut changes = Vec::new();
    let (old_configured, new_configured) = (&old.configured, &new.configured);
    // On one line, as a window shows it.
    let written = |literal: &super::Literal| {
        let written = literal.written(0);
        written.lines().map(str::trim).collect::<Vec<_>>().join(" ")
    };
    for agent in &new_configured.agents {
        let Some(before) = (old_configured.agents.iter()).find(|each| each.name == agent.name)
        else {
            let added = written(&agent_literal(agent));
            changes.push((format!("agents.{}", agent.name), String::new(), added));
            continue;
        };
        let text = |words: &[std::ffi::OsString]| {
            let words: Vec<String> = (words.iter())
                .map(|word| super::literal::quoted(&shown(word)))
                .collect();
            format!("{{ {} }}", words.join(", "))
        };
        let fields = [
            ("command", text(&before.command), text(&agent.command)),
            (
                "name",
                format!("{:?}", before.title),
                format!("{:?}", agent.title),
            ),
            (
                "mode",
                format!("{:?}", before.starting_mode),
                format!("{:?}", agent.starting_mode),
            ),
            (
                "tasks",
                format!("{:?}", before.tasks),
                format!("{:?}", agent.tasks),
            ),
        ];
        for (field, was, is) in fields {
            if was != is {
                changes.push((format!("agents.{} {field}", agent.name), was, is));
            }
        }
    }
    for agent in &old_configured.agents {
        if !(new_configured.agents.iter()).any(|each| each.name == agent.name) {
            let gone = written(&agent_literal(agent));
            changes.push((format!("agents.{}", agent.name), gone, String::new()));
        }
    }
    for profile in &new_configured.profiles {
        let before = (old_configured.profiles.iter()).find(|each| each.name == profile.name);
        if before != Some(profile) {
            let was = before.map(|before| written(&profile_literal(before)));
            let is = written(&profile_literal(profile));
            changes.push((
                format!("profiles.{}", profile.name),
                was.unwrap_or_default(),
                is,
            ));
        }
    }
    for profile in &old_configured.profiles {
        if !(new_configured.profiles.iter()).any(|each| each.name == profile.name) {
            let gone = written(&profile_literal(profile));
            changes.push((format!("profiles.{}", profile.name), gone, String::new()));
        }
    }
    let raised = limits(&new_configured.subagents);
    for ((name, new), (_, old)) in raised.iter().zip(limits(&old_configured.subagents)) {
        if *new > old {
            changes.push((
                format!("subagents.{name}"),
                old.to_string(),
                new.to_string(),
            ));
        }
    }
    for (name, value) in &new.web {
        let before = old.web.get(name);
        if before != Some(value) {
            let was = before.map(Value::text).unwrap_or_default();
            changes.push(((*name).to_owned(), was, value.text()));
        }
    }
    if new.password != old.password {
        let said = |password: &Option<String>| {
            if password.is_some() { "set" } else { "none" }
        };
        let now = if new.password.is_some() && old.password.is_some() {
            "a new one"
        } else {
            said(&new.password)
        };
        changes.push((
            "web password".to_owned(),
            said(&old.password).to_owned(),
            now.to_owned(),
        ));
    }
    changes
}

/// `guarded` as Luau that sets it again, and with `password`, a comment
/// marking its password.
fn as_lua(guarded: &Guarded, password: bool) -> String {
    let mut settings = Settings::default();
    for (name, value) in limits(&guarded.configured.subagents) {
        let info = options::find(&format!("subagents.{name}")).expect("a limit's option");
        let value = Value::Number(u32::try_from(value).unwrap_or(u32::MAX));
        settings.options.insert(info.name, (value, SetBy::DEFAULT));
    }
    for (name, value) in &guarded.web {
        settings
            .options
            .insert(name, (value.clone(), SetBy::DEFAULT));
    }
    settings.agents = (guarded.configured.agents.iter())
        .map(|agent| (agent.clone(), SetBy::DEFAULT))
        .collect();
    settings.profiles = (guarded.configured.profiles.iter())
        .map(|profile| (profile.clone(), SetBy::DEFAULT))
        .collect();
    let mut text = String::from(
        "-- What runs programs and lets the web in, as the user approved it in clackatoo:\n\
         -- kept by clackatoo. Change daemon/init.lua, not this.\n",
    );
    if password {
        let marked = guarded.password.as_deref().unwrap_or("none");
        let _ = writeln!(text, "{PASSWORD_LINE}{marked}");
    }
    text.push('\n');
    text.push_str(&settings.statements());
    text
}

/// What `state`'s `file` keeps, run, and its text: `None` if it isn't
/// there.
///
/// # Errors
///
/// If it's there but can't be read, or run: a link to nothing, not a
/// file, too big, or a mistake in it.
fn read(state: &Path, file: &str) -> Result<Option<(Settings, String)>, String> {
    let path = state.join(file);
    match std::fs::symlink_metadata(&path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{} couldn't be read: {err}", path.display())),
        Ok(_) => {}
    }
    let text =
        (lua::read(&path)?).ok_or_else(|| format!("{} is a link to nothing", path.display()))?;
    let chunk = lua::Chunk {
        path: path.clone(),
        text: text.clone(),
        source: Source::Settings,
    };
    let settings = run_apart(vec![chunk], Vec::new(), When::Upgrading)
        .map_err(|failure| format!("{}: {}", path.display(), failure.said))?;
    Ok(Some((settings, text)))
}

/// What the user approved, kept in `state`: `None` if nothing is.
///
/// # Errors
///
/// If something is, but can't be read, or run.
fn read_approved(state: &Path) -> Result<Option<Guarded>, String> {
    let Some((settings, _)) = read(state, APPROVED)? else {
        return Ok(None);
    };
    let password = (lua::read(&state.join(APPROVED_PASSWORD))?)
        .map(|hash| hash.trim().to_owned())
        .filter(|hash| !hash.is_empty());
    Ok(Some(Guarded::of(&settings, password)))
}

/// What the user discarded, kept in `state`, if it's there and runs.
fn read_discarded(state: &Path) -> Option<Guarded> {
    let (settings, text) = read(state, DISCARDED)
        .inspect_err(|why| log::warn!("{why}"))
        .ok()??;
    let password = (text.lines())
        .find_map(|line| line.strip_prefix(PASSWORD_LINE))
        .map(str::trim)
        .filter(|marked| *marked != "none")
        .map(str::to_owned);
    Some(Guarded::of(&settings, password))
}

/// Keeps `approved` as what the user approved in `state`.
///
/// # Errors
///
/// What stopped it, said.
pub fn keep_approved(state: &Path, approved: &Guarded) -> Result<(), String> {
    approve_password(state, approved.password.as_deref())
        .map_err(|err| format!("{APPROVED_PASSWORD} couldn't be written: {err}"))?;
    let path = state.join(APPROVED);
    super::write_private(&path, &as_lua(approved, false))
        .map_err(|err| format!("{} couldn't be written: {err}", path.display()))
}

/// Keeps `discarded` as what the user discarded in `state`, or with none,
/// takes it away.
///
/// # Errors
///
/// What stopped it, said.
pub(crate) fn keep_discarded(state: &Path, discarded: Option<&Guarded>) -> Result<(), String> {
    let path = state.join(DISCARDED);
    let kept = match discarded {
        Some(discarded) => {
            let marked = discarded.clone().marked();
            super::write_private(&path, &as_lua(&marked, true))
        }
        None => remove(&path),
    };
    kept.map_err(|err| format!("{} couldn't be written: {err}", path.display()))
}

/// Takes `path` away, if it's there.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Approves `hash` as the web app's password's, or with none, no
/// password, in `state`: as `clackatoo web password` sets it, the user's
/// own doing.
///
/// # Errors
///
/// If it can't be written, or taken away.
pub fn approve_password(state: &Path, hash: Option<&str>) -> std::io::Result<()> {
    let path = state.join(APPROVED_PASSWORD);
    match hash {
        Some(hash) => super::write_private(&path, &format!("{hash}\n")),
        None => remove(&path),
    }
}

/// What may be approved as the daemon starts with nothing approved, as
/// [`super::load`] finds it before the config runs.
#[derive(Clone, Debug, Default)]
pub enum First {
    /// Nothing: with nothing approved, nothing is.
    #[default]
    No,
    /// What the config says: the old TOML files moved into it on this very
    /// start, so it's what the user had, from before there was a config
    /// an agent could write.
    Migrated,
    /// What the daemon ran before this one, from before there was staging:
    /// what it last ran without a mistake, kept in [`super::LAST_GOOD`].
    Before(Settings),
}

/// What may be approved as the daemon starts, in `dirs`, the old TOML
/// files `migrated` into its config on this start or not; found before
/// the config runs, as running it keeps what it says as the last good.
///
/// Only with nothing approved, and nothing in its place, is anything. A
/// daemon from before there was staging approved nothing, and kept no
/// mark in its [`super::LAST_GOOD`] (see [`super::STAGED`]); so the first
/// time this daemon runs after that one, what that one last ran is what
/// the user had, and is approved, once. A last good with the mark was
/// written by a daemon that stages, from whatever the config said, so it
/// never is: taking away [`APPROVED`] then approves nothing.
#[must_use]
pub fn first(dirs: &Dirs, migrated: bool) -> First {
    let Some(state) = &dirs.state else {
        return First::No;
    };
    let approved = std::fs::symlink_metadata(state.join(APPROVED));
    if !approved.is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound) {
        return First::No;
    }
    if migrated {
        return First::Migrated;
    }
    match read(state, super::LAST_GOOD) {
        Ok(Some((settings, text))) if !text.contains(super::STAGED) => First::Before(settings),
        _ => First::No,
    }
}

/// What's staged as the daemon starts, its config saying `fresh` of it:
/// what's approved, kept in the state directory, and what's pending; and
/// what the daemon goes by. With nothing approved, only as `first` says is
/// anything; with what was approved unreadable, nothing is.
#[must_use]
pub fn starting(dirs: &Dirs, fresh: &Guarded, first: First) -> (Staged, Guarded) {
    let (approved, note) = match &dirs.state {
        None => (
            Guarded::nothing(),
            Some("there's no state directory to keep what you approve in".to_owned()),
        ),
        Some(state) => match read_approved(state) {
            Ok(Some(approved)) => (approved, None),
            Ok(None) => {
                let approved = match first {
                    First::Migrated => Some(fresh.clone()),
                    First::Before(settings) => Some(Guarded::of(&settings, fresh.password.clone())),
                    First::No => None,
                };
                match approved {
                    Some(approved) => {
                        if let Err(why) = keep_approved(state, &approved) {
                            log::warn!("{why}");
                        }
                        (approved, None)
                    }
                    None => (
                        Guarded::nothing(),
                        Some("the approved settings were missing".to_owned()),
                    ),
                }
            }
            Err(why) => {
                log::warn!("{why}");
                (
                    Guarded::nothing(),
                    Some(format!("the approved settings were unreadable: {why}")),
                )
            }
        },
    };
    let mut staged = Staged {
        approved,
        discarded: (dirs.state.as_deref()).and_then(read_discarded),
        pending: None,
        note,
    };
    let going = staged.stage(fresh);
    if staged.pending.is_some() {
        log::warn!("the daemon's config changes what runs programs, which waits for the user");
    }
    (staged, going)
}

/// `settings`, but with the web app's options as `going` has them: what
/// the daemon starts its web app by.
#[must_use]
pub fn applied(settings: &Settings, going: &Guarded) -> Settings {
    let mut settings = settings.clone();
    for (name, value) in &going.web {
        let by = (settings.options.get(name)).map_or(SetBy::DEFAULT, |(_, by)| by.clone());
        settings.options.insert(name, (value.clone(), by));
    }
    settings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(text: &str) -> Settings {
        Settings::from_lua(text).unwrap()
    }

    fn guarded(text: &str) -> Guarded {
        Guarded::of(&settings(text), None)
    }

    fn dirs(temp: &tempfile::TempDir) -> Dirs {
        Dirs {
            daemon: temp.path().join("config/daemon"),
            shared: temp.path().join("config/lua"),
            state: Some(temp.path().join("state")),
        }
    }

    /// A change to what runs programs waits; limits lowered don't; and
    /// what's approved and discarded is kept, and read back, as a restart
    /// reads it.
    #[test]
    fn what_runs_programs_waits_for_the_user_through_a_restart() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        let first = guarded("daemon.agents.mine = { command = { \"mine\" } }");
        let (staged, going) = starting(&dirs, &first, First::Migrated);
        assert_eq!(
            (staged.pending, going.configured.agents),
            (None, first.configured.agents.clone())
        );
        // An agent's command changed, and a limit raised: pending.
        let written = guarded(
            "daemon.agents.mine = { command = { \"evil\" } }\ndaemon.subagents.working = 64",
        );
        let (mut staged, going) = starting(&dirs, &written, First::No);
        assert_eq!(going.configured.agents, first.configured.agents);
        assert_eq!(going.configured.subagents, first.configured.subagents);
        assert_eq!(
            staged.changes(),
            [
                (
                    "agents.mine command".to_owned(),
                    "{ \"mine\" }".to_owned(),
                    "{ \"evil\" }".to_owned()
                ),
                (
                    "subagents.working".to_owned(),
                    "16".to_owned(),
                    "64".to_owned()
                ),
            ]
        );
        // Discarded: not pending again, through a restart.
        let state = dirs.state.as_ref().unwrap();
        keep_discarded(state, staged.pending.as_ref()).unwrap();
        let (staged_again, _) = starting(&dirs, &written, First::No);
        assert_eq!(staged_again.pending, None);
        // Applied: what the daemon goes by, through a restart.
        keep_approved(state, staged.pending.as_ref().unwrap()).unwrap();
        keep_discarded(state, None).unwrap();
        let (applied, going) = starting(&dirs, &written, First::No);
        assert_eq!(
            (applied.pending, going.configured.agents),
            (None, written.configured.agents.clone())
        );
        // Lowered: at once, nothing pending.
        staged.approved = written;
        staged.discarded = None;
        let going = staged.stage(&guarded(
            "daemon.agents.mine = { command = { \"evil\" } }\ndaemon.subagents.working = 2",
        ));
        assert_eq!(staged.pending, None);
        assert_eq!(going.configured.subagents.working, 2);
    }

    /// What was approved, gone or unreadable, approves nothing: the user's
    /// agents, limits and web app are pending, saying why; and a start
    /// with no state directory approves nothing either (CLACKA-237 review,
    /// H1).
    #[test]
    fn approved_settings_gone_or_broken_approve_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        let state = dirs.state.clone().unwrap();
        let fresh = Guarded::of(
            &settings(
                "daemon.agents.mine = { command = { \"mine\" } }\ndaemon.web.mode = \"tailscale\"",
            ),
            Some("$argon2id$hash".into()),
        );
        // Gone, as an agent deleting it would leave it.
        let (staged, going) = starting(&dirs, &fresh, First::No);
        assert_eq!(
            staged.note.as_deref(),
            Some("the approved settings were missing")
        );
        assert!(going.configured.agents.is_empty());
        assert_eq!(going.web, Guarded::nothing().web);
        assert_eq!(going.password, None);
        let names: Vec<String> = staged
            .changes()
            .into_iter()
            .map(|(name, ..)| name)
            .collect();
        assert_eq!(names, ["agents.mine", "web.mode", "web password"]);
        assert!(!state.join(APPROVED).exists(), "nothing was approved");
        // Broken: as gone, saying so.
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(state.join(APPROVED), "daemon.agents.x = (").unwrap();
        let (staged, going) = starting(&dirs, &fresh, First::No);
        let note = staged.note.unwrap();
        assert!(
            note.starts_with("the approved settings were unreadable"),
            "{note}"
        );
        assert!(going.configured.agents.is_empty());
        // Not a file: as broken.
        std::fs::remove_file(state.join(APPROVED)).unwrap();
        std::fs::create_dir(state.join(APPROVED)).unwrap();
        let (staged, _) = starting(&dirs, &fresh, First::No);
        assert!(staged.note.unwrap().contains("unreadable"));
        // Broken, it's no first time either.
        assert!(matches!(first(&dirs, true), First::No));
        std::fs::remove_dir(state.join(APPROVED)).unwrap();
        // No state directory: nothing approved.
        let stateless = Dirs {
            state: None,
            ..dirs.clone()
        };
        let (staged, going) = starting(&stateless, &fresh, First::Migrated);
        assert!(staged.pending.is_some() && going.configured.agents.is_empty());
    }

    /// Nothing approved is approved only as the old TOML files move in on
    /// that start, or the first time after a daemon from before staging,
    /// then as that one last ran; never from a last good this daemon wrote
    /// (CLACKA-237 review, H1).
    #[test]
    fn only_a_migration_or_the_upgrade_to_staging_approves_at_first() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        let state = dirs.state.clone().unwrap();
        std::fs::create_dir_all(&state).unwrap();
        let old = settings("daemon.agents.mine = { command = { \"mine\" } }");
        // As a daemon from before staging kept it: no mark.
        let before = old.to_lua().replace(super::super::STAGED, "");
        assert!(!before.contains(super::super::STAGED));
        std::fs::write(state.join(super::super::LAST_GOOD), before).unwrap();
        let First::Before(last) = first(&dirs, false) else {
            panic!("not the upgrade to staging");
        };
        // The config changed meanwhile: what the old daemon ran is
        // approved, and the change is pending.
        let fresh = guarded("daemon.agents.mine = { command = { \"evil\" } }");
        let (staged, going) = starting(&dirs, &fresh, First::Before(last));
        assert_eq!(going.configured.agents, old.agents().agents);
        let names: Vec<String> = staged
            .changes()
            .into_iter()
            .map(|(name, ..)| name)
            .collect();
        assert_eq!(names, ["agents.mine command"]);
        assert!(state.join(APPROVED).exists());
        // Approved now, there's no first time again.
        assert!(matches!(first(&dirs, true), First::No));
        // Taken away, a last good this daemon wrote approves nothing.
        std::fs::remove_file(state.join(APPROVED)).unwrap();
        std::fs::write(state.join(super::super::LAST_GOOD), fresh_lua(&fresh)).unwrap();
        assert!(matches!(first(&dirs, false), First::No));
        // Nor does a config with nothing moved in.
        std::fs::remove_file(state.join(super::super::LAST_GOOD)).unwrap();
        assert!(matches!(first(&dirs, false), First::No));
        assert!(matches!(first(&dirs, true), First::Migrated));
    }

    /// `guarded`'s agents as this daemon keeps its last good.
    fn fresh_lua(guarded: &Guarded) -> String {
        Settings {
            agents: (guarded.configured.agents.iter())
                .map(|agent| (agent.clone(), SetBy::DEFAULT))
                .collect(),
            ..Settings::default()
        }
        .to_lua()
    }

    /// The web app's options and password are staged too: a port or a
    /// password changed outside clackatoo waits, the daemon going by what
    /// was approved; `clackatoo web password` approves its own; a
    /// reordering alone is no change; and the revision says which change
    /// (CLACKA-237 review, M1, L2, L3).
    #[test]
    fn the_web_app_waits_too_and_order_alone_is_no_change() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = dirs(&temp);
        let state = dirs.state.clone().unwrap();
        let two = "daemon.agents.a = { command = { \"a\" } }\n\
                   daemon.agents.b = { command = { \"b\" } }\n";
        let approved = Guarded::of(&settings(two), Some("$argon2id$one".into()));
        let (staged, _) = starting(&dirs, &approved, First::Migrated);
        assert_eq!(staged.pending, None);
        // In another order: nothing pending.
        let reordered = "daemon.agents.b = { command = { \"b\" } }\n\
                         daemon.agents.a = { command = { \"a\" } }\n";
        let fresh = Guarded::of(&settings(reordered), Some("$argon2id$one".into()));
        assert_eq!(starting(&dirs, &fresh, First::No).0.pending, None);
        // A port and a password: pending, the approved ones gone by.
        let fresh = Guarded::of(
            &settings(&format!("{two}daemon.web.port = 9999")),
            Some("$argon2id$two".into()),
        );
        let (staged, going) = starting(&dirs, &fresh, First::No);
        assert_eq!(going.password.as_deref(), Some("$argon2id$one"));
        assert_ne!(going.web["web.port"], Value::Number(9999));
        let changes = staged.changes();
        assert_eq!(changes[0].0, "web.port");
        assert_eq!(
            changes[1],
            ("web password".into(), "set".into(), "a new one".into())
        );
        assert!(
            changes
                .iter()
                .all(|(_, was, is)| !was.contains("argon") && !is.contains("argon"))
        );
        // The revision is of what's pending, the same each time.
        let revision = staged.revision().unwrap();
        assert_eq!(
            starting(&dirs, &fresh, First::No).0.revision(),
            Some(revision)
        );
        let other = Guarded::of(&settings(two), Some("$argon2id$three".into()));
        assert_ne!(
            starting(&dirs, &other, First::No).0.revision(),
            Some(revision)
        );
        // Discarded, the password kept only as a mark: not pending again.
        keep_discarded(&state, staged.pending.as_ref()).unwrap();
        let kept = std::fs::read_to_string(state.join(DISCARDED)).unwrap();
        assert!(!kept.contains("argon2id"), "{kept}");
        assert_eq!(starting(&dirs, &fresh, First::No).0.pending, None);
        // `clackatoo web password` approves its own.
        approve_password(&state, Some("$argon2id$two")).unwrap();
        keep_discarded(&state, None).unwrap();
        let (staged, going) = starting(&dirs, &fresh, First::No);
        assert_eq!(going.password.as_deref(), Some("$argon2id$two"));
        assert_eq!(staged.changes()[0].0, "web.port");
        assert_eq!(staged.changes().len(), 1);
    }

    /// A command that isn't UTF-8 is shown escaped, never lossily.
    #[test]
    fn a_command_not_utf8_is_shown_escaped() {
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(shown(OsStr::from_bytes(b"ok\xffno")), "ok\\xffno");
    }
}
