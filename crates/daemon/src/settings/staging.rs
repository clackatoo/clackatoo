//! What runs programs, which a change to the daemon's config made outside
//! clackatoo never changes by itself: the agents (every field of every
//! one), the profiles, and the limits on chats agents start, raised.
//!
//! The user approves what runs programs: what's approved is kept in the
//! state directory ([`APPROVED`]), and the daemon goes by it. A config
//! that says otherwise, as one an agent wrote would, is *pending*: the
//! daemon goes on by what's approved, lowering its limits if the config
//! lowers them, and every window says what's pending, for the user to
//! apply or discard. A change discarded is kept too ([`DISCARDED`]), so it
//! isn't pending again until the config says something else. The daemon
//! goes by this as it starts and upgrades as well, so a restart never
//! takes in what wasn't approved.
//!
//! What's approved first is whatever the config says the first time a
//! daemon with this runs, as it does as the old TOML files move in. A
//! change the user makes in a window's settings menu is approved as it's
//! made: the menu changes only limits, and only the one it changes.

use std::path::Path;

use super::{Dirs, Settings, When, agent_literal, lua, profile_literal, run_apart};
use crate::agents::{Configured, SubagentLimits};
use crate::schema::{SetBy, Source, Value};

/// The file in the state directory what runs programs, as the user last
/// approved it, is kept in, as Luau.
pub const APPROVED: &str = "daemon-approved.lua";

/// The file in the state directory a change the user discarded is kept
/// in, as Luau.
pub const DISCARDED: &str = "daemon-discarded.lua";

/// What runs programs, as the user approved it, discarded, and what the
/// config says that's neither.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Staged {
    /// What the user approved, which the daemon goes by.
    pub(crate) approved: Configured,
    /// What the user discarded, if anything.
    pub(crate) discarded: Option<Configured>,
    /// What the config says that's neither, if anything.
    pub(crate) pending: Option<Configured>,
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

/// Whether `one` and `other` say the same of what runs programs.
fn same(one: &Configured, other: &Configured) -> bool {
    one.agents == other.agents && one.profiles == other.profiles && one.subagents == other.subagents
}

impl Staged {
    /// As `fresh`, what the config says now, would have it: what the
    /// daemon goes by, which is what's approved but for limits `fresh`
    /// lowers; and what's pending, if `fresh` says otherwise of what runs
    /// programs, and isn't what the user discarded.
    pub(crate) fn stage(&mut self, fresh: &Configured) -> Configured {
        let approved = &self.approved;
        let differs = fresh.agents != approved.agents
            || fresh.profiles != approved.profiles
            || !lower_or_same(&fresh.subagents, &approved.subagents);
        let discarded = (self.discarded.as_ref()).is_some_and(|discarded| same(discarded, fresh));
        self.pending = (differs && !discarded).then(|| fresh.clone());
        Configured {
            agents: approved.agents.clone(),
            profiles: approved.profiles.clone(),
            subagents: lowest(&fresh.subagents, &approved.subagents),
            keep_agents: fresh.keep_agents,
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
}

/// Approves, in `approved`, the limit `name`, a subagents option, as
/// `fresh` sets it, as the settings menu changed it: the user's own change.
pub(crate) fn approve_limit(approved: &mut Configured, name: &str, fresh: &SubagentLimits) {
    let approved = &mut approved.subagents;
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

/// What `new` changes of `old`, each named, as config names it, with what
/// it was and what it is: an agent's field, `agents.claude command`, or a
/// whole agent added or taken out, `agents.mine`; a profile, as a whole;
/// and each limit raised.
pub(crate) fn changes(old: &Configured, new: &Configured) -> Vec<(String, String, String)> {
    let mut changes = Vec::new();
    // On one line, as a window shows it.
    let written = |literal: &super::Literal| {
        let written = literal.written(0);
        written.lines().map(str::trim).collect::<Vec<_>>().join(" ")
    };
    for agent in &new.agents {
        let Some(before) = old.agents.iter().find(|each| each.name == agent.name) else {
            let added = written(&agent_literal(agent));
            changes.push((format!("agents.{}", agent.name), String::new(), added));
            continue;
        };
        let text = |words: &[std::ffi::OsString]| {
            let words: Vec<String> = (words.iter())
                .map(|word| super::literal::quoted(&word.to_string_lossy()))
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
    for agent in &old.agents {
        if !new.agents.iter().any(|each| each.name == agent.name) {
            let gone = written(&agent_literal(agent));
            changes.push((format!("agents.{}", agent.name), gone, String::new()));
        }
    }
    for profile in &new.profiles {
        let before = old.profiles.iter().find(|each| each.name == profile.name);
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
    for profile in &old.profiles {
        if !new.profiles.iter().any(|each| each.name == profile.name) {
            let gone = written(&profile_literal(profile));
            changes.push((format!("profiles.{}", profile.name), gone, String::new()));
        }
    }
    for ((name, new), (_, old)) in limits(&new.subagents).iter().zip(limits(&old.subagents)) {
        if *new > old {
            changes.push((
                format!("subagents.{name}"),
                old.to_string(),
                new.to_string(),
            ));
        }
    }
    changes
}

/// `configured`, what runs programs, as Luau that sets it again.
fn as_lua(configured: &Configured) -> String {
    let mut settings = Settings::default();
    for (name, value) in limits(&configured.subagents) {
        let info = super::options::find(&format!("subagents.{name}")).expect("a limit's option");
        let value = Value::Number(u32::try_from(value).unwrap_or(u32::MAX));
        settings.options.insert(info.name, (value, SetBy::DEFAULT));
    }
    settings.agents = (configured.agents.iter())
        .map(|agent| (agent.clone(), SetBy::DEFAULT))
        .collect();
    settings.profiles = (configured.profiles.iter())
        .map(|profile| (profile.clone(), SetBy::DEFAULT))
        .collect();
    format!(
        "-- What runs programs, as the user approved it in clackatoo: kept by clackatoo.\n\n{}",
        settings.statements()
    )
}

/// What `state`'s `file` keeps of what runs programs, if it keeps
/// something that runs.
fn read(state: &Path, file: &str) -> Option<Configured> {
    let path = state.join(file);
    let text = lua::read(&path).ok()??;
    let chunk = lua::Chunk {
        path: path.clone(),
        text,
        source: Source::Settings,
    };
    let settings = run_apart(vec![chunk], Vec::new(), When::Upgrading)
        .inspect_err(|failure| log::warn!("{}: {}", path.display(), failure.said))
        .ok()?;
    Some(settings.agents())
}

/// Keeps `configured` in `state`'s `file`, or with none, takes it away.
///
/// # Errors
///
/// What stopped it, said.
pub(crate) fn keep(
    state: &Path,
    file: &str,
    configured: Option<&Configured>,
) -> Result<(), String> {
    let path = state.join(file);
    let kept = match configured {
        Some(configured) => super::write_private(&path, &as_lua(configured)),
        None => match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    };
    kept.map_err(|err| format!("{} couldn't be written: {err}", path.display()))
}

/// What runs programs as the daemon starts, its config saying `fresh` of
/// it: what's approved, kept in the state directory, and what's pending;
/// and what the daemon goes by. With nothing approved yet, as the first
/// time a daemon with this runs, `fresh` is approved, and kept.
#[must_use]
pub fn starting(dirs: &Dirs, fresh: &Configured) -> (Staged, Configured) {
    let Some(state) = &dirs.state else {
        let staged = Staged {
            approved: fresh.clone(),
            ..Staged::default()
        };
        return (staged, fresh.clone());
    };
    let approved = read(state, APPROVED).unwrap_or_else(|| {
        if let Err(why) = keep(state, APPROVED, Some(fresh)) {
            log::warn!("{why}");
        }
        fresh.clone()
    });
    let mut staged = Staged {
        approved,
        discarded: read(state, DISCARDED),
        pending: None,
    };
    let going = staged.stage(fresh);
    if staged.pending.is_some() {
        log::warn!("the daemon's config changes what runs programs, which waits for the user");
    }
    (staged, going)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(text: &str) -> Configured {
        Settings::from_lua(text).unwrap().agents()
    }

    /// A change to what runs programs waits; limits lowered don't; and
    /// what's approved and discarded is kept, and read back, as a restart
    /// reads it.
    #[test]
    fn what_runs_programs_waits_for_the_user_through_a_restart() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            daemon: temp.path().join("config/daemon"),
            shared: temp.path().join("config/lua"),
            state: Some(temp.path().join("state")),
        };
        let first = configured("daemon.agents.mine = { command = { \"mine\" } }");
        let (staged, going) = starting(&dirs, &first);
        assert_eq!((staged.pending, going.agents), (None, first.agents.clone()));
        // An agent's command changed, and a limit raised: pending.
        let written = configured(
            "daemon.agents.mine = { command = { \"evil\" } }\ndaemon.subagents.working = 64",
        );
        let (mut staged, going) = starting(&dirs, &written);
        assert_eq!(going.agents, first.agents);
        assert_eq!(going.subagents, first.subagents);
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
        keep(state, DISCARDED, staged.pending.as_ref()).unwrap();
        let (staged_again, _) = starting(&dirs, &written);
        assert_eq!(staged_again.pending, None);
        // Applied: what the daemon goes by, through a restart.
        keep(state, APPROVED, staged.pending.as_ref()).unwrap();
        keep(state, DISCARDED, None).unwrap();
        let (applied, going) = starting(&dirs, &written);
        assert_eq!(
            (applied.pending, going.agents),
            (None, written.agents.clone())
        );
        // Lowered: at once, nothing pending.
        let lowered = configured(
            "daemon.agents.mine = { command = { \"evil\" } }\ndaemon.subagents.working = 2",
        );
        let going = staged_lowered(&mut staged, &written, &lowered);
        assert_eq!(going.subagents.working, 2);
    }

    /// `staged` with `approved` approved, staging `fresh`.
    fn staged_lowered(
        staged: &mut Staged,
        approved: &Configured,
        fresh: &Configured,
    ) -> Configured {
        staged.approved = approved.clone();
        staged.discarded = None;
        let going = staged.stage(fresh);
        assert_eq!(staged.pending, None);
        going
    }
}
