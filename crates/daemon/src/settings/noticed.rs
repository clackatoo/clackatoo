//! What a run of the daemon's config, as its files changed outside
//! clackatoo, changed that the user should know of: what runs programs,
//! what lets the web in, and what keeps lines programs mark secret. That's
//! the agents (every field of every one), the limits on
//! chats agents start, raised, the web app's options, and the `hints`
//! loosened. Every window says it, and the log; nothing waits on it.
//!
//! It's a notice, not a guard: an agent that can write the config as the
//! user could write anything else the user can. What an agent may write
//! is its own permissions' to say.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;

use super::{Settings, agent_literal, options};
use crate::agents::{Configured, SubagentLimits};
use crate::schema::Value;

/// What a run of the config says of what's noticed.
#[derive(Clone, Debug, PartialEq)]
pub struct Noticed {
    /// The agents and limits.
    configured: Configured,
    /// Each of the web app's options, `web.mode` and the rest.
    web: BTreeMap<&'static str, Value>,
    /// Each of what keeps lines programs mark secret, `hints.noagent` and
    /// the rest.
    hints: BTreeMap<&'static str, Value>,
}

impl Noticed {
    /// As `settings` say.
    #[must_use]
    pub fn of(settings: &Settings) -> Self {
        let starting = |start: &'static str| -> BTreeMap<&'static str, Value> {
            (options::ALL.iter())
                .filter(|info| info.name.starts_with(start))
                .map(|info| (info.name, settings.value(info.name)))
                .collect()
        };
        Self {
            configured: settings.agents(),
            web: starting("web."),
            hints: starting("hints."),
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

/// Whether `new`, of the hints option `name`, keeps lines programs mark
/// secret less well than `old` does: shown to agents, written to disk,
/// shown on screen, or shown longer.
fn looser(name: &str, new: &Value, old: &Value) -> bool {
    match (new, old) {
        (Value::Number(new), Value::Number(old)) => new > old,
        // The default is the stricter: `hide`, `mask`, true.
        _ => {
            new != old && options::find(name).is_some_and(|info| *old == options::default_of(info))
        }
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

/// What `new` changes of `old` that's noticed, each named, as config names
/// it, with what it was and what it is: an agent's field, `agents.claude
/// command`, or a whole agent added or taken out, `agents.mine`; a specialist,
/// as a whole; each limit raised; each of the web app's options; and each
/// hint loosened. Agents go by name, so their order is no
/// change.
#[must_use]
pub fn changes(old: &Noticed, new: &Noticed) -> Vec<(String, String, String)> {
    let mut changes = programs(&old.configured, &new.configured);
    let raised = limits(&new.configured.subagents);
    for ((name, new), (_, old)) in raised.iter().zip(limits(&old.configured.subagents)) {
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
    for (name, value) in &new.hints {
        if let Some(before) = old.hints.get(name)
            && looser(name, value, before)
        {
            changes.push(((*name).to_owned(), before.text(), value.text()));
        }
    }
    changes
}

/// What `new` changes of `old`'s agents, said as [`changes`]
/// says it.
fn programs(old: &Configured, new: &Configured) -> Vec<(String, String, String)> {
    let mut changes = Vec::new();
    for (name, before, after) in [
        ("plugins.claude", old.claude_plugins, new.claude_plugins),
        ("plugins.codex", old.codex_plugins, new.codex_plugins),
    ] {
        if before != after {
            changes.push((name.into(), before.to_string(), after.to_string()));
        }
    }
    // On one line, as a window shows it.
    let written = |literal: &super::Literal| {
        let written = literal.written(0);
        written.lines().map(str::trim).collect::<Vec<_>>().join(" ")
    };
    for agent in &new.agents {
        let Some(before) = (old.agents.iter()).find(|each| each.name == agent.name) else {
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
    for agent in &old.agents {
        if !(new.agents.iter()).any(|each| each.name == agent.name) {
            let gone = written(&agent_literal(agent));
            changes.push((format!("agents.{}", agent.name), gone, String::new()));
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noticed(text: &str) -> Noticed {
        Noticed::of(&Settings::from_lua(text).unwrap())
    }

    fn names(old: &str, new: &str) -> Vec<String> {
        (changes(&noticed(old), &noticed(new)).into_iter())
            .map(|(name, ..)| name)
            .collect()
    }

    /// What's noticed: an agent's command, a limit raised, the web app,
    /// a hint loosened; not a limit lowered, a hint tightened, nor agents
    /// put in another order.
    #[test]
    fn what_runs_programs_lets_the_web_in_or_shows_secrets_is_noticed() {
        let mine = "daemon.agents.mine = { command = { \"mine\" } }\n";
        let changed = names(
            mine,
            "daemon.agents.mine = { command = { \"evil\" } }\ndaemon.subagents.working = 64\n\
             daemon.web.mode = \"tailscale\"\ndaemon.hints.noagent = \"show\"\n",
        );
        assert_eq!(
            changed,
            [
                "agents.mine command",
                "subagents.working",
                "web.mode",
                "hints.noagent"
            ]
        );
        let (one, two) = (
            "daemon.agents.a = { command = { \"a\" } }\n",
            "daemon.agents.b = { command = { \"b\" } }\n",
        );
        assert!(names(&format!("{one}{two}"), &format!("{two}{one}")).is_empty());
        assert!(names("daemon.hints.noagent = \"show\"", "").is_empty());
        assert!(names("", "daemon.subagents.working = 2").is_empty());
        let (_, was, is) = changes(&noticed(mine), &noticed("")).remove(0);
        assert!(was.contains("mine") && is.is_empty(), "{was:?}");
    }

    /// A command that isn't UTF-8 is shown escaped, never lossily.
    #[test]
    fn a_command_not_utf8_is_shown_escaped() {
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(shown(OsStr::from_bytes(b"ok\xffno")), "ok\\xffno");
    }
}
