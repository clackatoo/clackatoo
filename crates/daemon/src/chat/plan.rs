//! The agent's plan: its to-do list, such as Claude Code's `TodoWrite`
//! and Codex's plan, which ACP agents send as `plan` updates, each the
//! whole list again (CLACKA-190).
//!
//! The chat's view holds the latest, for windows to show above the box
//! and for a digest to tell the next agent the chat moves to. It's handed
//! over with the chat, and a session loaded again tells it again as it
//! replays. A plan whose every step is done stays until the user's next
//! message, then goes, as what it was for is over.

use agent_client_protocol_schema::v1::{Plan, PlanEntryStatus};
use clackatoo_protocol::{ChatPatch, PlanState, PlanStep};

use super::HostedChat;

/// The most steps a plan keeps: an agent's list is a screenful, and the
/// rest of a longer one is cut.
const MOST_STEPS: usize = 64;

/// The most characters a step keeps, the rest cut with an ellipsis: a
/// step is a line of a list.
const MOST_CHARS: usize = 500;

impl HostedChat {
    /// Takes the agent's plan, which replaces the one before.
    pub(super) fn plan_said(&mut self, plan: Plan) {
        self.set_plan(steps(plan));
        self.plan_by = Some(self.node.agent.clone());
    }

    /// Lets go of the plan, as another agent's has been told to the one
    /// the chat moved to.
    pub(super) fn forget_plan(&mut self) {
        self.set_plan(Vec::new());
        self.plan_by = None;
    }

    /// Lets go of a plan whose every step is done, as the user sends a
    /// message.
    pub(super) fn forget_finished_plan(&mut self) {
        if !self.node.plan.is_empty()
            && (self.node.plan.iter()).all(|step| step.state == PlanState::Done)
        {
            self.forget_plan();
        }
    }

    pub(super) fn set_plan(&mut self, plan: Vec<PlanStep>) {
        if self.node.plan != plan {
            self.node.plan.clone_from(&plan);
            self.journal.push(ChatPatch::Plan(plan));
        }
    }
}

/// The steps of the agent's `plan`, as many as are kept.
fn steps(plan: Plan) -> Vec<PlanStep> {
    kept(plan.entries.into_iter().map(|entry| PlanStep {
        content: entry.content,
        state: match entry.status {
            PlanEntryStatus::Completed => PlanState::Done,
            PlanEntryStatus::InProgress => PlanState::Doing,
            // Pending, or a state newer than this build knows.
            _ => PlanState::ToDo,
        },
    }))
}

/// The first [`MOST_STEPS`] of `steps`, each as windows are to show it
/// (see [`shown`]): what an agent gives, or a build before handed over.
pub(super) fn kept(steps: impl Iterator<Item = PlanStep>) -> Vec<PlanStep> {
    (steps.take(MOST_STEPS))
        .map(|step| PlanStep {
            content: shown(&step.content),
            state: step.state,
        })
        .collect()
}

/// `text` as a step's line shows it (see [`one_line`]).
fn shown(text: &str) -> String {
    one_line(text, MOST_CHARS)
}

/// `text`, which an agent gave, as a line of a list shows it: on one
/// line, every run of white space one space; what would hide or reorder
/// text shown as other agent text reaching windows is
/// (`server::unhidden`); cut to `most` characters and an ellipsis.
pub(super) fn one_line(text: &str, most: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = crate::server::unhidden(&line);
    match line.char_indices().nth(most) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_one_line_cut_short_hiding_nothing() {
        assert_eq!(shown("Write\nthe \t tests\r\n"), "Write the tests");
        // Bidi controls and tag letters show, as other agents' text does.
        let hidden = shown("a\u{202E}b\u{E0041}c");
        assert!(
            !hidden.contains('\u{202E}') && !hidden.contains('\u{E0041}'),
            "{hidden:?}"
        );
        assert_eq!(shown(&hidden), hidden, "showing it twice changes nothing");
        let long = shown(&"é".repeat(MOST_CHARS + 10));
        assert_eq!(long.chars().count(), MOST_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_plan_keeps_its_first_steps_in_their_states() {
        let entries: Vec<String> = (0..100)
            .map(|n| {
                let status = ["pending", "in_progress", "completed"][n % 3];
                format!(r#"{{"content":"Step {n}","priority":"low","status":"{status}"}}"#)
            })
            .collect();
        let plan: Plan =
            serde_json::from_str(&format!(r#"{{"entries":[{}]}}"#, entries.join(","))).unwrap();
        let steps = steps(plan);
        assert_eq!(steps.len(), MOST_STEPS);
        assert_eq!(
            steps[..3].iter().map(|s| s.state).collect::<Vec<_>>(),
            [PlanState::ToDo, PlanState::Doing, PlanState::Done]
        );
        assert_eq!(steps[63].content, "Step 63");
        // As one handed over by a build that kept more.
        let handed = kept((0..100).map(|n| PlanStep {
            content: format!("{n}\n{}", "x".repeat(MOST_CHARS)),
            state: PlanState::ToDo,
        }));
        assert_eq!(handed.len(), MOST_STEPS);
        assert!(handed[0].content.starts_with("0 x") && handed[0].content.ends_with('…'));
    }
}
