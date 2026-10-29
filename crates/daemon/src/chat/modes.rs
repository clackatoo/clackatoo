//! How much the modes of the agents clackatoo knows let them do, by the
//! IDs their adapters give them (CLACKA-197), so the daemon never takes an
//! agent's own word, nor the order it lists its modes in, for which is
//! safer: an agent that moves itself to a mode that may do more isn't
//! followed, and a chat an agent started gets the safest.
//!
//! - Claude Code's adapter (`@agentclientprotocol/claude-agent-acp` 0.98,
//!   `session-mode.js`): Plan, which changes nothing; Manual (`default`);
//!   Accept edits; Auto; and Bypass permissions, where allowed. It lists
//!   Plan third.
//! - Codex's (`codex-acp` 2.2.2, `AgentMode`): Read-only; Workspace
//!   access; Auto review (`agent`); Full access. Its Plan mode is another
//!   setting, `collaboration_mode`.
//!
//! Any other agent's modes aren't known: its own changes of them are never
//! followed.

/// The modes of the agent called `agent`, by their IDs, from the one that
/// may do least, if clackatoo knows them.
fn ranked(agent: &str) -> Option<&'static [&'static str]> {
    match agent {
        "claude" => Some(&[
            "plan",
            "default",
            "acceptEdits",
            "auto",
            "bypassPermissions",
        ]),
        "codex" => Some(&["read-only", "workspace-write", "agent", "agent-full-access"]),
        _ => None,
    }
}

/// Where the mode `value` is among `agent`'s modes, from 0, the one that
/// may do least, if clackatoo knows it.
pub(super) fn rank(agent: &str, value: &str) -> Option<usize> {
    ranked(agent)?.iter().position(|known| *known == value)
}

/// Whether the mode `value` is `agent`'s Plan mode, as its adapter names
/// it: only Claude Code's has one among its modes.
pub(super) fn is_plan(agent: &str, value: &str) -> bool {
    agent == "claude" && value == "plan"
}

/// The mode `agent`'s adapter leaves Plan mode for when the user answers
/// its plan's question with the option `option`: Claude Code's
/// `exit-plan-*` options (`permissions/effects.js`), with or without a
/// fresh context.
pub(super) fn leaving_plan_for(agent: &str, option: &str) -> Option<&'static str> {
    if agent != "claude" {
        return None;
    }
    match option {
        "exit-plan-bypass" | "exit-plan-clear-bypass" => Some("bypassPermissions"),
        "exit-plan-auto" | "exit-plan-clear-auto" => Some("auto"),
        "exit-plan-accept-edits" | "exit-plan-clear-accept-edits" => Some("acceptEdits"),
        "exit-plan-default" => Some("default"),
        _ => None,
    }
}

/// The safest of `offered`, the values of `agent`'s modes, to work in,
/// one that can still do something: Manual for Claude Code, Read-only for
/// Codex. `None` for an agent whose modes aren't known.
pub(super) fn safest<'a>(agent: &str, offered: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    offered
        .filter(|value| !is_plan(agent, value))
        .filter_map(|value| Some((rank(agent, value)?, value)))
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, value)| value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_modes_are_ranked_by_what_they_let_the_agent_do_not_as_listed() {
        assert!(rank("claude", "plan") < rank("claude", "default"));
        assert!(rank("claude", "acceptEdits") < rank("claude", "auto"));
        assert!(rank("codex", "read-only") < rank("codex", "agent-full-access"));
        assert_eq!(rank("mine", "read-only"), None);
        assert!(is_plan("claude", "plan") && !is_plan("mine", "plan"));
        let claude = ["default", "acceptEdits", "plan", "auto"];
        assert_eq!(safest("claude", claude.into_iter()), Some("default"));
        let codex = ["agent-full-access", "agent", "read-only"];
        assert_eq!(safest("codex", codex.into_iter()), Some("read-only"));
        assert_eq!(safest("mine", ["yolo", "careful"].into_iter()), None);
        assert_eq!(leaving_plan_for("claude", "exit-plan-auto"), Some("auto"));
        assert_eq!(leaving_plan_for("codex", "implement_plan"), None);
    }
}
