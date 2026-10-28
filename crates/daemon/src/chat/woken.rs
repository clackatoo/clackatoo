//! Turns an agent takes by itself, with no message (CLACKA-180): Claude
//! Code takes one when something it ran in the background ends, to look at
//! what it printed, and may commit and write a summary in it.
//!
//! What it says then comes while the chat answers nothing, so the chat
//! opens a turn of the agent's own for it: one with no message, saying why
//! when the agent says so. It ends as the agent says it has, if it says:
//! Claude Code's adapter tells a client that asked for its background
//! tasks of each such turn as a task of kind `turn` (see `tasks.rs`).
//! Otherwise the server ends it once the agent has been quiet a while with
//! no tool running (see `DaemonConfig::quiet_turn`). Stopping the chat
//! ends it, and so does the agent stopping.

use agent_client_protocol_schema::v1::{ContentBlock, SessionUpdate};
use clackatoo_protocol::{ChatState, Item, ToolStatus, Turn};

use super::{HostedChat, Link, now};

impl HostedChat {
    /// Whether `update`, come while the chat answers nothing, is the agent
    /// at work by itself: words, thinking, or a tool it starts. What's
    /// said of a tool of a turn that has ended, or of settings and usage,
    /// isn't.
    pub(crate) fn wakes(&self, update: &SessionUpdate) -> bool {
        if self.answering() || self.link != Link::Open || self.told_again() {
            return false;
        }
        match update {
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
