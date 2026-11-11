//! Specialists: the kinds of chat an agent starts another chat as, such as
//! `explore`, for quick code lookups, and `research`, for deeper investigation. They're
//! clackatoo's, not any agent's: a specialist maps onto Claude Code, Codex
//! or an agent of the user's alike.
//!
//! A specialist has a name, a role, which goes before the chat's first
//! message, limits on what the chat may do, and the agent, model and
//! effort it starts with by default. Four are built in:
//!
//! - `explore`: the agent's cheapest model, read-only, answering briefly;
//! - `research`: the parent's model and effort, read-only, investigating deeply;
//! - `plan`: a strong model that changes nothing and returns a plan;
//! - `general`: the parent's agent and model, with no role or limits.
//!
//! Custom specialists are Markdown packages in .agents/specialists or the
//! clackatoo config's specialists directory. See `skills::specialist_files`.

use clackatoo_protocol::SettingChoice;
use serde::{Deserialize, Serialize};

/// Optional capabilities and launch restrictions shared by configured and
/// packaged specialists. None means inherit, while an empty list selects none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Extras {
    pub(crate) when_to_use: String,
    pub(crate) user_invocable: bool,
    pub(crate) agent_invocable: bool,
    pub(crate) delegates_to: Option<Vec<String>>,
    pub(crate) skills: Option<Vec<String>>,
    pub(crate) skillsets: Option<Vec<String>>,
    pub(crate) mcp_servers: Option<Vec<String>>,
    pub(crate) budget: Budget,
    pub(crate) directories: Vec<std::path::PathBuf>,
}

impl Default for Extras {
    fn default() -> Self {
        Self {
            when_to_use: String::new(),
            user_invocable: true,
            agent_invocable: true,
            delegates_to: None,
            skills: None,
            skillsets: None,
            mcp_servers: None,
            budget: Budget::default(),
            directories: Vec::new(),
        }
    }
}

/// Per-request ceilings. USD is kept in millionths to avoid rounding while
/// inheriting a ceiling. Turn and cost limits require native runtime support.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Budget {
    pub(crate) seconds: Option<u64>,
    pub(crate) turns: Option<u32>,
    pub(crate) usd_micros: Option<u64>,
}

impl Budget {
    pub(crate) fn capped(self, parent: Self) -> Self {
        fn cap<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        Self {
            seconds: cap(self.seconds, parent.seconds),
            turns: cap(self.turns, parent.turns),
            usd_micros: cap(self.usd_micros, parent.usd_micros),
        }
    }

    pub(crate) fn check(self, agent: &str) -> Result<(), String> {
        if agent != "claude" && (self.turns.is_some() || self.usd_micros.is_some()) {
            return Err(format!(
                "{agent} cannot enforce specialist turn or USD budgets through its adapter; use a time budget or Claude Code"
            ));
        }
        Ok(())
    }
}

impl Extras {
    pub(crate) fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let mut extra = Self::default();
        if let Some(value) = value.get("when_to_use") {
            extra.when_to_use = value.as_str().ok_or("when_to_use must be a string")?.into();
        }
        for (name, target) in [
            ("user_invocable", &mut extra.user_invocable),
            ("agent_invocable", &mut extra.agent_invocable),
        ] {
            if let Some(value) = value.get(name) {
                *target = value
                    .as_bool()
                    .ok_or_else(|| format!("{name} must be true or false"))?;
            }
        }
        for (name, target) in [
            ("delegates_to", &mut extra.delegates_to),
            ("skills", &mut extra.skills),
            ("skillsets", &mut extra.skillsets),
            ("mcp_servers", &mut extra.mcp_servers),
        ] {
            if let Some(value) = value.get(name) {
                let values = value
                    .as_array()
                    .ok_or_else(|| format!("{name} must be a list of names"))?;
                if values.len() > 256 {
                    return Err(format!("{name} has more than 256 entries"));
                }
                *target = Some(
                    values
                        .iter()
                        .map(|value| {
                            value
                                .as_str()
                                .filter(|value| !value.trim().is_empty())
                                .map(str::to_owned)
                                .ok_or_else(|| format!("{name} entries must be nonempty strings"))
                        })
                        .collect::<Result<_, _>>()?,
                );
            }
        }
        if let Some(limits) = value.get("limits").and_then(serde_json::Value::as_object) {
            for key in limits.keys() {
                if !["access", "seconds", "turns", "cost_usd"].contains(&key.as_str()) {
                    return Err(format!("unsupported limit {key}"));
                }
            }
            let number = |name: &str, max: u64| -> Result<Option<u64>, String> {
                limits
                    .get(name)
                    .map(|value| {
                        value
                            .as_u64()
                            .filter(|value| *value > 0 && *value <= max)
                            .ok_or_else(|| format!("limits.{name} must be between 1 and {max}"))
                    })
                    .transpose()
            };
            extra.budget.seconds = number("seconds", 7 * 24 * 60 * 60)?;
            extra.budget.turns =
                number("turns", 1_000_000)?.map(|value| u32::try_from(value).expect("bounded"));
            if let Some(cost) = limits.get("cost_usd") {
                let cost = cost
                    .as_f64()
                    .filter(|cost| cost.is_finite() && *cost >= 0.000_001 && *cost <= 1_000_000.0)
                    .ok_or("limits.cost_usd must be a positive USD amount up to 1000000")?;
                extra.budget.usd_micros =
                    clackatoo_protocol::Cost::of(cost, "USD").map(|cost| cost.micros);
            }
        }
        Ok(extra)
    }
}

pub(crate) fn access(value: Option<&serde_json::Value>) -> Result<Option<Limits>, String> {
    let Some(value) = value else { return Ok(None) };
    let value = if value.is_object() {
        let Some(access) = value.get("access") else {
            return Ok(None);
        };
        access
    } else {
        value
    };
    let name = value
        .as_str()
        .ok_or("limits must be an access name or table")?;
    Limits::named(name)
        .map(Some)
        .ok_or_else(|| format!("unknown access limit {name}; use none, no-commands or read-only"))
}

/// What a chat may do, beyond what its agent's own settings allow. A chat
/// started by another never has fewer limits than the chat that started
/// it.
///
/// Where they hold:
///
/// - Claude Code is given, as its session opens, only the built-in tools
///   they leave (`tools`), with those they rule out taken away as well
///   (`disallowedTools`), and no MCP servers but clackatoo's
///   (`strictMcpConfig`): that holds.
/// - Codex starts chats with either in its Read-only mode, whose sandbox
///   holds: it has no mode that edits without running commands, and in
///   its others, sandboxed commands run without asking.
/// - For every agent, clackatoo refuses writes through ACP's file system,
///   refuses its own tools that run or type commands, and turns down the
///   agent's questions about tools that would edit or run, which is
///   enough for an agent that asks first. An agent that writes files
///   itself without asking is only asked not to, by its role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) enum Limits {
    /// None of its own.
    #[default]
    None,
    /// It runs no commands.
    NoCommands,
    /// It changes no files and runs no commands: it reads, and answers.
    ReadOnly,
}

impl Limits {
    /// What it's called, in the daemon's config, and as chats keep it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::NoCommands => "no-commands",
            Self::ReadOnly => "read-only",
        }
    }

    /// The limits called `name`.
    pub(crate) fn named(name: &str) -> Option<Self> {
        [Self::None, Self::NoCommands, Self::ReadOnly]
            .into_iter()
            .find(|limits| limits.name() == name)
    }

    /// Whether it rules out running commands.
    pub(crate) fn no_commands(self) -> bool {
        self >= Self::NoCommands
    }

    /// Whether it rules out changing files.
    pub(crate) fn read_only(self) -> bool {
        self == Self::ReadOnly
    }

    /// Claude Code's built-in tools it leaves, by their names, if it
    /// rules any out: an allow-list, so a tool newer than this build
    /// (such as one that runs a command for it to watch) is ruled out
    /// too. `None` leaves them all.
    pub(crate) fn claude_allowed(self) -> Option<&'static [&'static str]> {
        const READING: [&str; 6] = ["Read", "Glob", "Grep", "WebFetch", "WebSearch", "TodoWrite"];
        const EDITING: [&str; 10] = [
            "Read",
            "Glob",
            "Grep",
            "WebFetch",
            "WebSearch",
            "TodoWrite",
            "Edit",
            "MultiEdit",
            "Write",
            "NotebookEdit",
        ];
        match self {
            Self::None => None,
            Self::NoCommands => Some(&EDITING),
            Self::ReadOnly => Some(&READING),
        }
    }

    /// Claude Code's tools it rules out, by their names, as well as
    /// leaving only [`claude_allowed`](Self::claude_allowed): its Task
    /// tool too, under both its names, as its helpers could do what the
    /// chat can't, and the tools that run commands or other agents for it.
    pub(crate) fn claude_tools(self) -> &'static [&'static str] {
        const COMMANDS: [&str; 11] = [
            "Bash",
            "BashOutput",
            "KillShell",
            "Monitor",
            "PowerShell",
            "Agent",
            "Task",
            "Workflow",
            "CronCreate",
            "ScheduleWakeup",
            "EnterWorktree",
        ];
        const CHANGES: [&str; 16] = [
            "Bash",
            "BashOutput",
            "KillShell",
            "Monitor",
            "PowerShell",
            "Agent",
            "Task",
            "Workflow",
            "CronCreate",
            "ScheduleWakeup",
            "EnterWorktree",
            "ExitWorktree",
            "Edit",
            "MultiEdit",
            "Write",
            "NotebookEdit",
        ];
        match self {
            Self::None => &[],
            Self::NoCommands => &COMMANDS,
            Self::ReadOnly => &CHANGES,
        }
    }

    /// What the agent is told of it, in its role, if anything.
    pub(crate) fn told(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::NoCommands => Some("Don't run commands: you can't, here."),
            Self::ReadOnly => Some(
                "You are read-only: don't change, create or delete files, and don't run \
                 commands. Read, search and answer.",
            ),
        }
    }
}

/// A kind of chat an agent starts another chat as.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Specialist {
    pub(crate) name: String,
    /// What it's for, in a line, for agents choosing one.
    pub(crate) description: String,
    /// Its role, which goes before the chat's first message.
    pub(crate) instructions: String,
    pub(crate) limits: Limits,
    /// The agent it starts with, by name, unless asked for another:
    /// `None` for the parent's.
    pub(crate) agent: Option<String>,
    /// The model it starts on, by the agent it applies to: `*` for any.
    /// Empty, it starts on the agent's own choice.
    pub(crate) models: Vec<(String, String)>,
    /// It starts on the parent's model, if it's the parent's agent.
    pub(crate) inherit_model: bool,
    /// How hard it thinks, by the agent it applies to, as `models`.
    pub(crate) efforts: Vec<(String, String)>,
    pub(crate) extra: Extras,
}

impl Default for Specialist {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            instructions: String::new(),
            limits: Limits::None,
            agent: None,
            models: Vec::new(),
            inherit_model: false,
            efforts: Vec::new(),
            extra: Extras::default(),
        }
    }
}

impl Specialist {
    /// Supporting paths in instructions are relative to the owning bundle.
    pub(crate) fn instruction_text(&self) -> std::borrow::Cow<'_, str> {
        self.extra.directories.first().and_then(|p|p.parent()).map_or_else(||std::borrow::Cow::Borrowed(self.instructions.as_str()),|directory| {
            std::borrow::Cow::Owned(format!("Specialist directory: {}\nResolve relative supporting-file paths from this directory.\n\n{}",directory.display(),self.instructions))
        })
    }

    /// The model it starts on with the agent called `agent`, if it names
    /// one.
    pub(crate) fn model_for(&self, agent: &str) -> Option<&str> {
        for_agent(&self.models, agent)
    }

    /// How hard it thinks with the agent called `agent`, if it says.
    pub(crate) fn effort_for(&self, agent: &str) -> Option<&str> {
        for_agent(&self.efforts, agent)
    }
}

/// The value in `values` for the agent called `agent`, or for any.
fn for_agent<'a>(values: &'a [(String, String)], agent: &str) -> Option<&'a str> {
    (values.iter().find(|(name, _)| name == agent))
        .or_else(|| values.iter().find(|(name, _)| name == "*"))
        .map(|(_, value)| value.as_str())
}

/// The specialist a chat is started as when it's asked for none.
pub(crate) const DEFAULT: &str = "general";

/// The specialists clackatoo has, before the user's.
pub(crate) fn built_in() -> Vec<Specialist> {
    // The same Markdown format as user packages, embedded so installation needs
    // no extra files. Parse once; listing and selecting roles do no disk work.
    static BUILT_IN: std::sync::OnceLock<Vec<Specialist>> = std::sync::OnceLock::new();
    BUILT_IN
        .get_or_init(|| {
            [
                (
                    "explore",
                    include_str!("../specialists/explore/SPECIALIST.md"),
                ),
                (
                    "research",
                    include_str!("../specialists/research/SPECIALIST.md"),
                ),
                ("plan", include_str!("../specialists/plan/SPECIALIST.md")),
                (
                    DEFAULT,
                    include_str!("../specialists/general/SPECIALIST.md"),
                ),
            ]
            .into_iter()
            .map(|(expected, text)| {
                let (name, _, includes, specialist) = crate::skills::specialist_files::parse(text)
                    .expect("valid built-in specialist");
                assert_eq!(
                    name, expected,
                    "built-in specialist name matches its folder"
                );
                assert!(
                    includes.is_empty(),
                    "built-in specialists are self-contained"
                );
                specialist
            })
            .collect()
        })
        .clone()
}

/// The choice `wanted` names, among `choices`: by its value, or else its
/// name, or else the one choice whose value or name has it in, ignoring
/// case. With several that have it in, as `opus` in "Opus" and "Opus
/// (1M context)", or none, it's none: the agent's own choice stands,
/// rather than one that may cost more than the specialist meant.
pub(crate) fn pick<'a>(choices: &'a [SettingChoice], wanted: &str) -> Option<&'a SettingChoice> {
    let lower = wanted.to_lowercase();
    if let Some(exact) = (choices.iter().find(|choice| choice.value == wanted))
        .or_else(|| (choices.iter()).find(|choice| choice.name.eq_ignore_ascii_case(wanted)))
    {
        return Some(exact);
    }
    let mut partly = (choices.iter()).filter(|choice| {
        choice.value.to_lowercase().contains(&lower) || choice.name.to_lowercase().contains(&lower)
    });
    match (partly.next(), partly.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(value: &str, name: &str) -> SettingChoice {
        SettingChoice {
            value: value.to_owned(),
            name: name.to_owned(),
            description: None,
        }
    }

    #[test]
    fn a_model_is_picked_by_its_value_its_name_or_part_of_either() {
        let choices = [
            choice("default", "Opus 5.5"),
            choice("claude-haiku-4-5", "Haiku 4.5"),
            choice("haiku", "Haiku 5.5"),
            choice("gpt-6-luna", "6 Luna"),
        ];
        assert_eq!(pick(&choices, "haiku").unwrap().name, "Haiku 5.5");
        assert_eq!(pick(&choices, "opus 5.5").unwrap().value, "default");
        assert_eq!(pick(&choices, "luna").unwrap().value, "gpt-6-luna");
        assert_eq!(pick(&choices, "sonnet"), None);
        // Two with it in: neither, rather than one that may cost more.
        let opus = [
            choice("opus", "Opus"),
            choice("opus[1m]", "Opus (1M context)"),
        ];
        assert_eq!(pick(&opus, "opus").unwrap().value, "opus");
        assert_eq!(pick(&opus, "OPUS").unwrap().value, "opus");
        assert_eq!(pick(&opus, "op"), None);
        let efforts = [choice("high", "High"), choice("xhigh", "Extra high")];
        assert_eq!(pick(&efforts, "high").unwrap().value, "high");
    }

    #[test]
    fn limits_go_from_none_to_read_only_and_say_what_claude_code_loses() {
        assert!(Limits::ReadOnly > Limits::NoCommands && Limits::NoCommands > Limits::None);
        assert!(Limits::ReadOnly.no_commands() && Limits::ReadOnly.read_only());
        assert!(Limits::NoCommands.no_commands() && !Limits::NoCommands.read_only());
        assert!(Limits::ReadOnly.claude_tools().contains(&"Write"));
        assert!(Limits::NoCommands.claude_tools().contains(&"Bash"));
        assert!(Limits::None.claude_tools().is_empty());
        assert_eq!(Limits::None.claude_allowed(), None);
        let reading = Limits::ReadOnly.claude_allowed().unwrap();
        assert!(reading.contains(&"Read") && !reading.contains(&"Write"));
        assert!(
            !reading
                .iter()
                .any(|tool| Limits::ReadOnly.claude_tools().contains(tool))
        );
        let editing = Limits::NoCommands.claude_allowed().unwrap();
        assert!(editing.contains(&"Write") && !editing.contains(&"Bash"));
        assert!(
            !editing
                .iter()
                .any(|tool| Limits::NoCommands.claude_tools().contains(tool))
        );
        for limits in [Limits::None, Limits::NoCommands, Limits::ReadOnly] {
            assert_eq!(Limits::named(limits.name()), Some(limits));
        }
    }
}
