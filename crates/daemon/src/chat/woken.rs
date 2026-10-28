//! Turns an agent takes by itself, with no message (CLACKA-180): Claude
//! Code takes one when something it ran in the background ends, to look at
//! what it printed, and may commit and write a summary in it.
//!
//! What it says then comes while the chat answers nothing, so the chat
//! opens a turn of the agent's own for it: one with no message, saying why
//! when the agent says so.
//!
//! Claude Code's adapter, asked for its background tasks, says when such
//! a turn starts and ends, as a task of kind `turn` (see `tasks.rs`), and
//! says it before anything of the turn. Then that alone opens and ends
//! one: words it says with no turn open are a notice (as "Task stopped by
//! user" is to a client that can't show notices), and stopping the turn
//! waits for it to say the turn ended, for a few seconds at most.
//!
//! An agent that says nothing of the kind opens one with its words, a
//! thought or a tool it starts, and the server ends it once it has been
//! quiet a while with no tool running (see `DaemonConfig::quiet_turn`).
//! Stopping ends it at once; what was in flight then opens no new turn for
//! [`WAKE_HELD`]. The agent stopping ends either.

use std::time::{Duration, Instant};

use agent_client_protocol_schema::v1::{ContentBlock, SessionUpdate};
use clackatoo_protocol::{ChatState, Item, ToolStatus, Turn};

use super::{HostedChat, Link, now};

/// How long after a turn the agent took by itself is stopped what it says
/// opens no new turn, nor shows as a notice: it was in flight.
const WAKE_HELD: Duration = Duration::from_secs(2);

impl HostedChat {
    /// Whether `update`, come while the chat answers nothing, is the agent
    /// at work by itself: thinking or a tool it starts, or words from an
    /// agent that doesn't say when it takes a turn by itself (`!tells`).
    /// What's said of a tool of a turn that has ended, or of settings and
    /// usage, isn't, nor a notice (see [`said_while_idle`]).
    ///
    /// [`said_while_idle`]: Self::said_while_idle
    pub(crate) fn wakes(&self, update: &SessionUpdate, tells: bool) -> bool {
        if self.answering() || self.link != Link::Open || self.told_again() {
            return false;
        }
        if self.wake_held.is_some_and(|until| Instant::now() < until) {
            return false;
        }
        match update {
            // Its words with no turn open are a notice, from an agent that
            // says when it takes a turn; its thinking or a tool it starts
            // is it at work, as one whose turn's start went missing.
            SessionUpdate::AgentMessageChunk(_) if tells => false,
            SessionUpdate::AgentMessageChunk(chunk) | SessionUpdate::AgentThoughtChunk(chunk) => {
                matches!(&chunk.content, ContentBlock::Text(text) if !text.text.trim().is_empty())
            }
            SessionUpdate::ToolCall(_) => true,
            _ => false,
        }
    }

    /// Opens a turn the agent takes by itself, saying `why` if the agent
    /// said, if the chat answers nothing. Returns whether it did.
    pub(crate) fn wake(&mut self, why: Option<String>) -> bool {
        if self.answering() || self.link != Link::Open {
            return false;
        }
        self.woke_by_task = None;
        self.push_turn(Turn {
            previews: Vec::new(),
            prompt: String::new(),
            sent: now(),
            items: Vec::new(),
            end: None,
            files: None,
            from: None,
            switch: None,
            woke: Some(why.unwrap_or_default()),
        });
        self.set_state(ChatState::Answering);
        true
    }

    /// Takes `update` if it's something the agent says with no turn open
    /// that's a notice, not the agent at work: the chat shows it as its
    /// notice, the latest only. From an agent that `tells` when it takes a
    /// turn by itself, that's any words (as "Task stopped by user", from an
    /// adapter that can't send it as a notice); from another, only words
    /// shaped as the adapters shape their notices (see [`notice_shaped`]).
    /// What was in flight as a turn of its own was stopped goes nowhere.
    /// Returns whether it took it.
    pub(crate) fn said_while_idle(&mut self, update: &SessionUpdate, tells: bool) -> bool {
        if self.answering() || self.told_again() {
            return false;
        }
        let (SessionUpdate::AgentMessageChunk(chunk) | SessionUpdate::AgentThoughtChunk(chunk)) =
            update
        else {
            return false;
        };
        let thought = matches!(update, SessionUpdate::AgentThoughtChunk(_));
        let ContentBlock::Text(text) = &chunk.content else {
            return !thought;
        };
        if self.wake_held.is_some_and(|until| Instant::now() < until) {
            return true;
        }
        let said = match notice_shaped(&text.text) {
            Some(said) => said,
            None if tells && !thought => text.text.clone(),
            None => return false,
        };
        let said = super::clean(&said);
        let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
        if !said.is_empty() {
            self.set_notice(Some(super::one_line_notice(&said)));
        }
        true
    }

    /// The user stopped the turn the agent took by itself, which ends once
    /// the agent says so, if it says when it does; or else now, and what
    /// was in flight opens no new one for a moment. Returns whether it
    /// waits for the agent.
    pub(crate) fn stop_woken(&mut self) -> bool {
        if self.woken_says_when_it_ends() {
            self.woken_stopping = true;
            return true;
        }
        self.wake_held = Some(Instant::now() + WAKE_HELD);
        false
    }

    /// Whether the user stopped the turn the agent took by itself, and it
    /// waits for the agent to say it has ended.
    pub(crate) fn woken_stopping(&self) -> bool {
        self.woken() && self.woken_stopping
    }

    /// Whether the turn being answered is one the agent took by itself.
    pub(crate) fn woken(&self) -> bool {
        (self.node.turns.last()).is_some_and(|turn| turn.end.is_none() && turn.woke.is_some())
    }

    /// Whether the agent says when the turn it took by itself ends, as
    /// Claude Code's adapter does to a client that asked for its tasks.
    pub(crate) fn woken_says_when_it_ends(&self) -> bool {
        self.woken() && self.woke_by_task.is_some()
    }

    /// Whether a tool of the turn being answered is still running, so a
    /// quiet agent is still at work.
    pub(crate) fn tool_running(&self) -> bool {
        let Some(turn) = self.node.turns.last().filter(|turn| turn.end.is_none()) else {
            return false;
        };
        (turn.items.iter())
            .any(|item| matches!(item, Item::Tool(tool) if tool.status == ToolStatus::Running))
    }
}

/// What `text` says, if it's shaped as the adapters shape a notice to a
/// client that can't take one: Claude Code's `**Title:** what` or
/// `**Title**`, and Codex's `Config warning: …`, `Warning: …`,
/// `*Context compacted …*` and `Model rerouted from …`. Its bold and
/// slant markers are left out.
fn notice_shaped(text: &str) -> Option<String> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("**")
        && let Some((title, after)) = rest.split_once("**")
        && !title.is_empty()
    {
        return Some(format!("{title}{after}"));
    }
    let codex = ["Config warning: ", "Warning: ", "Model rerouted from "];
    if codex.iter().any(|start| text.starts_with(start)) {
        return Some(text.to_owned());
    }
    (text.starts_with("*Context compacted") && text.ends_with('*'))
        .then(|| text.trim_matches('*').to_owned())
}
