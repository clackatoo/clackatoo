//! Profiles: the kinds of chat an agent starts another chat as, such as
//! `explore`, for cheap research that changes nothing. They're
//! clackatoo's, not any agent's: a profile maps onto Claude Code, Codex
//! or an agent of the user's alike.
//!
//! A profile has a name, a role, which goes before the chat's first
//! message, limits on what the chat may do, and the agent, model and
//! effort it starts with by default. Three are built in:
//!
//! - `explore`: the agent's cheapest model, read-only, answering briefly;
//! - `plan`: a strong model that changes nothing and returns a plan;
//! - `general`: the parent's agent and model, with no role or limits.
//!
//! The user adds their own, or changes the built-in ones, in
//! `agents.toml`, read again whenever a chat is started:
//!
//! ```toml
//! [profiles.review]
//! about = "Reviews a change for bugs"
//! agent = "codex"
//! model = "gpt-6-luna"          # or a table, by agent: { claude = "haiku" }
//! effort = "low"
//! limits = "read-only"          # or "no-commands", or "none"
//! prompt = "You review changes for bugs. Say what's wrong, briefly."
//! ```
//!
//! How far limits hold depends on the agent (see [`Limits`]).

use clackatoo_protocol::SettingChoice;

/// What a chat may do, beyond what its agent's own settings allow. A chat
/// started by another never has fewer limits than the chat that started
/// it.
///
/// Where they hold:
///
/// - Claude Code is told, as its session opens, that the tools they rule
///   out don't exist (`disallowedTools`): that holds.
/// - Codex starts read-only chats in its Read-only mode, whose sandbox
///   holds; it has no mode without commands, so `NoCommands` is only
///   asked of it.
/// - For every agent, clackatoo refuses writes through ACP's file system,
///   refuses its own tools that run or type commands, and turns down the
///   agent's questions about tools that would edit or run, which is
///   enough for an agent that asks first. An agent that writes files
///   itself without asking is only asked not to, by its role.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
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
    /// What it's called, in `agents.toml`, and as chats keep it.
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

    /// Claude Code's tools it rules out, by their names: its Task tool
    /// too, under both its names, as its helpers could do what the chat
    /// can't.
    pub(crate) fn claude_tools(self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::NoCommands => &["Bash", "BashOutput", "KillShell", "Agent", "Task"],
            Self::ReadOnly => &[
                "Bash",
                "BashOutput",
                "KillShell",
                "Agent",
                "Task",
                "Edit",
                "MultiEdit",
                "Write",
                "NotebookEdit",
            ],
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Profile {
    pub(crate) name: String,
    /// What it's for, in a line, for agents choosing one.
    pub(crate) about: String,
    /// Its role, which goes before the chat's first message.
    pub(crate) prompt: String,
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
}

impl Profile {
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

/// The profile a chat is started as when it's asked for none.
pub(crate) const DEFAULT: &str = "general";

/// The profiles clackatoo has, before the user's.
pub(crate) fn built_in() -> Vec<Profile> {
    let pairs = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
        (pairs.iter())
            .map(|&(agent, value)| (agent.to_owned(), value.to_owned()))
            .collect()
    };
    vec![
        Profile {
            name: "explore".to_owned(),
            about: "fast, cheap research on the cheapest model: finds and reads code, changes \
                    nothing, answers briefly"
                .to_owned(),
            prompt: "You are an explorer, started by another agent to find something out. Find \
                     and read what you need, quickly: search before you read whole files. \
                     Answer briefly: what you found, with file paths and line numbers, and \
                     nothing else. Your answer goes back to the agent that started you."
                .to_owned(),
            limits: Limits::ReadOnly,
            agent: None,
            models: pairs(&[("claude", "haiku"), ("codex", "gpt-6-luna")]),
            inherit_model: false,
            efforts: pairs(&[("codex", "low")]),
        },
        Profile {
            name: "plan".to_owned(),
            about: "a strong model that reads and thinks, changes nothing, and returns a plan"
                .to_owned(),
            prompt: "You are a planner, started by another agent. Read what you need, think it \
                     through, and answer with a plan: the steps, in order, the files each \
                     touches, and the risks. Change nothing. Your answer goes back to the \
                     agent that started you."
                .to_owned(),
            limits: Limits::ReadOnly,
            agent: None,
            models: pairs(&[("claude", "opus")]),
            inherit_model: false,
            efforts: pairs(&[("codex", "high")]),
        },
        Profile {
            name: DEFAULT.to_owned(),
            about: "the parent's agent and model, free to do whatever the parent may".to_owned(),
            prompt: String::new(),
            limits: Limits::None,
            agent: None,
            models: Vec::new(),
            inherit_model: true,
            efforts: Vec::new(),
        },
    ]
}

/// The profiles an `agents.toml` adds or changes, from its `profiles`
/// table, over `base`, the built-in ones: a profile of a built-in one's
/// name changes only what it gives.
pub(crate) fn parse(
    document: &toml_edit::DocumentMut,
    base: &mut Vec<Profile>,
) -> Result<(), String> {
    let Some(profiles) = document.get("profiles") else {
        return Ok(());
    };
    let profiles = profiles
        .as_table_like()
        .ok_or("profiles is a table, of profiles by name")?;
    for (name, table) in profiles.iter() {
        let table = (table.as_table_like()).ok_or_else(|| format!("profiles.{name} is a table"))?;
        let text = |key: &str| -> Result<Option<String>, String> {
            table.get(key).map_or(Ok(None), |item| {
                (item.as_str().map(|text| Some(text.to_owned())))
                    .ok_or_else(|| format!("profiles.{name}.{key} is a string"))
            })
        };
        // A string for any agent, or a table by agent.
        let by_agent = |key: &str| -> Result<Option<Vec<(String, String)>>, String> {
            let Some(item) = table.get(key) else {
                return Ok(None);
            };
            if let Some(value) = item.as_str() {
                return Ok(Some(vec![("*".to_owned(), value.to_owned())]));
            }
            let wrong =
                || format!("profiles.{name}.{key} is a string, or a table of them by agent");
            let values = item.as_table_like().ok_or_else(wrong)?;
            (values.iter())
                .map(|(agent, value)| {
                    let value = value.as_str().ok_or_else(wrong)?;
                    Ok((agent.to_owned(), value.to_owned()))
                })
                .collect::<Result<Vec<_>, String>>()
                .map(Some)
        };
        let known = base.iter().position(|profile| profile.name == name);
        let at = known.unwrap_or_else(|| {
            base.push(Profile {
                name: name.to_owned(),
                about: String::new(),
                prompt: String::new(),
                limits: Limits::None,
                agent: None,
                models: Vec::new(),
                inherit_model: false,
                efforts: Vec::new(),
            });
            base.len() - 1
        });
        let profile = &mut base[at];
        if let Some(about) = text("about")? {
            profile.about = about;
        }
        if let Some(prompt) = text("prompt")? {
            profile.prompt = prompt;
        }
        if let Some(limits) = text("limits")? {
            profile.limits = Limits::named(&limits).ok_or_else(|| {
                format!("profiles.{name}.limits is \"read-only\", \"no-commands\" or \"none\"")
            })?;
        }
        if let Some(agent) = text("agent")? {
            profile.agent = Some(agent);
        }
        if let Some(models) = by_agent("model")? {
            profile.inherit_model = models.iter().any(|(_, model)| model == "inherit");
            profile.models = models
                .into_iter()
                .filter(|(_, model)| model != "inherit")
                .collect();
        }
        if let Some(efforts) = by_agent("effort")? {
            profile.efforts = efforts;
        }
    }
    Ok(())
}

/// The choice among `choices` that `wanted` names: by its value, or else
/// its name, or else the first whose value or name has it in, ignoring
/// case, so `haiku` picks "Haiku 5.5" and `luna` picks `gpt-6-luna`.
pub(crate) fn pick<'a>(choices: &'a [SettingChoice], wanted: &str) -> Option<&'a SettingChoice> {
    let lower = wanted.to_lowercase();
    (choices.iter().find(|choice| choice.value == wanted))
        .or_else(|| (choices.iter()).find(|choice| choice.name.eq_ignore_ascii_case(wanted)))
        .or_else(|| {
            (choices.iter()).find(|choice| {
                choice.value.to_lowercase().contains(&lower)
                    || choice.name.to_lowercase().contains(&lower)
            })
        })
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
    }

    #[test]
    fn limits_go_from_none_to_read_only_and_say_what_claude_code_loses() {
        assert!(Limits::ReadOnly > Limits::NoCommands && Limits::NoCommands > Limits::None);
        assert!(Limits::ReadOnly.no_commands() && Limits::ReadOnly.read_only());
        assert!(Limits::NoCommands.no_commands() && !Limits::NoCommands.read_only());
        assert!(Limits::ReadOnly.claude_tools().contains(&"Write"));
        assert!(Limits::NoCommands.claude_tools().contains(&"Bash"));
        assert!(Limits::None.claude_tools().is_empty());
        for limits in [Limits::None, Limits::NoCommands, Limits::ReadOnly] {
            assert_eq!(Limits::named(limits.name()), Some(limits));
        }
    }

    #[test]
    fn agents_toml_adds_profiles_and_changes_the_built_in_ones() {
        let document = r#"
            [profiles.review]
            about = "Reviews a change"
            agent = "codex"
            model = "gpt-6-luna"
            effort = "low"
            limits = "read-only"
            prompt = "You review."

            [profiles.explore]
            model = { claude = "sonnet" }

            [profiles.mine]
            model = "inherit"
        "#
        .parse()
        .unwrap();
        let mut profiles = built_in();
        parse(&document, &mut profiles).unwrap();
        let named = |name: &str| {
            profiles
                .iter()
                .find(|profile| profile.name == name)
                .unwrap()
        };
        let review = named("review");
        assert_eq!(review.agent.as_deref(), Some("codex"));
        assert_eq!(review.model_for("codex"), Some("gpt-6-luna"));
        assert_eq!(review.model_for("claude"), Some("gpt-6-luna"));
        assert_eq!(review.effort_for("codex"), Some("low"));
        assert_eq!(review.limits, Limits::ReadOnly);
        assert_eq!(review.prompt, "You review.");
        // Only what's given changes.
        let explore = named("explore");
        assert_eq!(explore.model_for("claude"), Some("sonnet"));
        assert_eq!(explore.model_for("codex"), None);
        assert_eq!(explore.limits, Limits::ReadOnly);
        assert!(!explore.prompt.is_empty());
        assert!(named("mine").inherit_model);

        let wrong = "[profiles.x]\nlimits = \"loose\"".parse().unwrap();
        let refused = parse(&wrong, &mut built_in()).unwrap_err();
        assert!(refused.contains("profiles.x.limits"), "{refused}");
    }
}
