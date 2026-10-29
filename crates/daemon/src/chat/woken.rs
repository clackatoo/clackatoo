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

use agent_client_protocol_schema::v1::{ContentBlock, SessionUpdate, TextContent};
use clackatoo_protocol::{ChatState, Item, ToolStatus, Turn};

use super::{HostedChat, Link, now};

/// How long after a turn is stopped what the agent says opens no new turn,
/// nor shows as a notice: it was in flight.
const WAKE_HELD: Duration = Duration::from_secs(2);

/// How soon after words taken for a notice a turn the agent opens takes
/// them back, as its start.
const PUT_BACK: Duration = Duration::from_millis(500);

/// The longest title of a notice Claude Code's adapter writes as words.
const NOTICE_TITLE: usize = 60;

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
        let taken = (self.idle_taken.take()).filter(|(at, _)| at.elapsed() < PUT_BACK);
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
        // What was taken for a notice just now was this turn's start.
        if let Some((_, text)) = taken {
            self.set_notice(None);
            self.write(&ContentBlock::Text(TextContent::new(text)), false, None);
        }
        true
    }

    /// What the agent says next, for a moment, opens no turn of its own:
    /// a turn was just stopped, and that was in flight.
    pub(super) fn hold_wake(&mut self) {
        self.wake_held = Some(Instant::now() + WAKE_HELD);
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
        let said = match notice_shaped(&self.agent, &text.text, thought) {
            Some(said) => said,
            None if tells && !thought => text.text.clone(),
            None => return false,
        };
        if !thought {
            match &mut self.idle_taken {
                Some((at, taken)) if at.elapsed() < PUT_BACK => {
                    *at = Instant::now();
                    taken.push_str(&text.text);
                }
                kept => *kept = Some((Instant::now(), text.text.clone())),
            }
        }
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
        self.hold_wake();
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
            .filter_map(Item::tool)
            .any(|tool| tool.status == ToolStatus::Running)
    }
}

/// What `text` says, if `agent`'s own adapter would have written it so for
/// a notice to a client that can't take one, exactly: Claude Code's a line
/// `**Title**` or `**Title:** what`, with a short title; Codex's
/// `Config warning: …`, `Warning: …` or `*Context compacted to fit the
/// model's context window.*`, each ending in a blank line, or a thought
/// `Model rerouted from … to … (…).` likewise. Its bold and slant markers
/// are left out. Any other agent's words are never taken for one.
fn notice_shaped(agent: &str, text: &str, thought: bool) -> Option<String> {
    match (agent, thought) {
        ("claude", false) => {
            let line = text.strip_suffix('\n').unwrap_or(text);
            let rest = line
                .strip_prefix("**")
                .filter(|rest| !rest.contains('\n'))?;
            let (title, after) = rest.split_once("**")?;
            let short = !title.is_empty() && title.chars().count() <= NOTICE_TITLE;
            if !short || title.contains('*') {
                return None;
            }
            if after.is_empty() && !title.ends_with(':') {
                return Some(title.to_owned());
            }
            let title = title.strip_suffix(':')?;
            let what = after
                .strip_prefix(' ')
                .filter(|what| !what.trim().is_empty())?;
            Some(format!("{title}: {what}"))
        }
        ("codex", false) => {
            let said = text.strip_suffix("\n\n")?;
            let compacted = "*Context compacted to fit the model's context window.*";
            if said == compacted {
                return Some(compacted.trim_matches('*').to_owned());
            }
            let config = said.starts_with("Config warning: ");
            let warning = said.starts_with("Warning: ") && !said.contains('\n');
            (config || warning).then(|| said.to_owned())
        }
        ("codex", true) => {
            let said = text.strip_suffix("\n\n")?;
            (said.starts_with("Model rerouted from ")
                && said.ends_with(").")
                && !said.contains('\n'))
            .then(|| said.to_owned())
        }
        _ => None,
    }
}
