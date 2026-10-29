//! The agent's slash commands: its own, its skills and its MCP servers'
//! prompts, which ACP agents send as `available_commands_update`, each
//! the whole list again (CLACKA-189).
//!
//! The chat's view holds the latest, for windows to offer as `/` is typed
//! at the start of a message. What an agent says of them is cut down and
//! cleaned first, as what it says reaches every window: at most
//! [`MOST_COMMANDS`], each name a word that can be typed, each line short
//! and showing what would hide or reorder text.
//!
//! They're the agent's, so they go when the chat moves to another agent,
//! which says its own once its session opens. They aren't handed over in
//! an upgrade: the agent starts again after one, and says them again as
//! the chat's session opens.

use agent_client_protocol_schema::v1::{AvailableCommand, AvailableCommandInput};
use clackatoo_protocol::{AgentCommand, ChatPatch};

use super::HostedChat;
use super::plan::one_line;

/// The most commands kept: Claude Code with many skills and MCP servers
/// offers a few hundred at most.
const MOST_COMMANDS: usize = 256;

/// The most of what an agent says are commands looked at, kept or not.
const MOST_LOOKED_AT: usize = 4 * MOST_COMMANDS;

/// The most characters of a command's name: longer isn't a name anyone
/// types.
const MOST_NAME: usize = 64;

/// The most characters of what a command does, the rest cut.
const MOST_DESCRIPTION: usize = 300;

/// The most characters of the hint of what to type after a command.
const MOST_HINT: usize = 100;

impl HostedChat {
    /// Takes the agent's commands, which replace those before.
    pub(super) fn commands_said(&mut self, commands: Vec<AvailableCommand>) {
        self.set_commands(kept(commands));
    }

    /// Lets go of the agent's commands, as the chat moves to another.
    pub(super) fn forget_commands(&mut self) {
        self.set_commands(Vec::new());
    }

    fn set_commands(&mut self, commands: Vec<AgentCommand>) {
        if self.node.commands != commands {
            self.node.commands.clone_from(&commands);
            self.journal.push(ChatPatch::Commands(commands));
        }
    }
}

/// The commands of `said` that are kept, as windows are to show them:
/// the first [`MOST_COMMANDS`] with a name that can be typed, each name
/// once.
fn kept(said: Vec<AvailableCommand>) -> Vec<AgentCommand> {
    let mut kept: Vec<AgentCommand> = Vec::new();
    let mut names = std::collections::HashSet::new();
    // Past this many, an agent filling the list with names that can't be
    // typed or come again isn't looked at further.
    for command in said.into_iter().take(MOST_LOOKED_AT) {
        if kept.len() == MOST_COMMANDS {
            break;
        }
        let Some(name) = name(&command.name) else {
            continue;
        };
        if !names.insert(name.clone()) {
            continue;
        }
        let hint = match command.input {
            Some(AvailableCommandInput::Unstructured(input)) => Some(input.hint),
            // A kind of input newer than this build knows.
            _ => None,
        };
        kept.push(AgentCommand {
            name,
            description: one_line(&command.description, MOST_DESCRIPTION),
            hint: (hint.map(|hint| one_line(&hint, MOST_HINT))).filter(|hint| !hint.is_empty()),
        });
    }
    kept
}

/// `said`, a command's name as the agent gave it, as it's typed after the
/// `/`, which some agents give it with: `None` if it isn't a word that
/// can be typed, as one with white space, a `/`, or characters that
/// would hide or reorder text isn't, nor one too long.
fn name(said: &str) -> Option<String> {
    let name = said.strip_prefix('/').unwrap_or(said);
    let typed = !name.is_empty()
        && name.chars().count() <= MOST_NAME
        && !name.contains('/')
        && !name.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        && crate::server::unhidden(name) == name;
    typed.then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(json: &str) -> Vec<AvailableCommand> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn commands_keep_their_names_hints_and_one_line_descriptions() {
        let commands = kept(said(
            r#"[{"name":"compact","description":"Summarise\nthe   chat"},
                {"name":"/review-branch","description":"Review","input":{"hint":"branch name"}},
                {"name":"init","description":"Write AGENTS.md","input":{"hint":""}}]"#,
        ));
        assert_eq!(
            commands,
            [
                AgentCommand {
                    name: "compact".into(),
                    description: "Summarise the chat".into(),
                    hint: None,
                },
                AgentCommand {
                    name: "review-branch".into(),
                    description: "Review".into(),
                    hint: Some("branch name".into()),
                },
                AgentCommand {
                    name: "init".into(),
                    description: "Write AGENTS.md".into(),
                    hint: None,
                },
            ]
        );
    }

    #[test]
    fn names_that_cant_be_typed_or_come_twice_are_left_out() {
        let long = "x".repeat(MOST_NAME + 1);
        let json = format!(
            r#"[{{"name":"two words","description":""}},{{"name":"a/b","description":""}},
                {{"name":"","description":""}},{{"name":"/","description":""}},
                {{"name":"rtl\u202Eevil","description":""}},{{"name":"{long}","description":""}},
                {{"name":"ok","description":"first"}},{{"name":"ok","description":"second"}}]"#
        );
        let commands = kept(said(&json));
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert_eq!(commands[0].description, "first");
        // A list of nothing but those isn't read to its end.
        let junk = vec![r#"{"name":"a b","description":""}"#; MOST_LOOKED_AT];
        let json = format!(r#"[{},{{"name":"late","description":""}}]"#, junk.join(","));
        assert!(kept(said(&json)).is_empty());
    }

    #[test]
    fn commands_are_capped_and_their_lines_cut_hiding_nothing() {
        let many: Vec<String> = (0..MOST_COMMANDS + 50)
            .map(|n| {
                format!(
                    r#"{{"name":"c{n}","description":"\u202E{}end","input":{{"hint":"{}"}}}}"#,
                    "d".repeat(MOST_DESCRIPTION),
                    "h".repeat(MOST_HINT + 5)
                )
            })
            .collect();
        let commands = kept(said(&format!("[{}]", many.join(","))));
        assert_eq!(commands.len(), MOST_COMMANDS);
        let first = &commands[0];
        assert_eq!(first.description.chars().count(), MOST_DESCRIPTION + 1);
        assert!(first.description.ends_with('…'));
        assert!(!first.description.contains('\u{202E}'));
        assert_eq!(
            first.hint.as_deref().map(|hint| hint.chars().count()),
            Some(MOST_HINT + 1)
        );
    }
}
