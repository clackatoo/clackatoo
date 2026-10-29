//! What an agent says of a turn beyond its words (CLACKA-196): that it's
//! trying its request to the model again, that it compacted its context,
//! the exit codes of the commands it ran, what the turn spent, the images
//! in its words, and why the turn failed, in plain words (see
//! `failure.rs`); and sending a message that failed again.
//!
//! How each adapter says these, as read from their sources
//! (`@agentclientprotocol/claude-agent-acp` 0.98, `codex-acp` 2.2.2):
//!
//! - Trying again: Claude Code's, to a client that asks for AIR's
//!   `sessionFailure`, a `session_info_update` whose
//!   `_meta.jetbrains.air.sessionFailure` has `severity` `warning` and a
//!   title such as "Retrying Claude, attempt 2 of 10."; Codex's,
//!   `_meta.codex.error` with `willRetry`, and Codex's words, such as
//!   "Reconnecting... 2/5". It's over when the agent says anything more of
//!   the turn, as both adapters take it.
//! - Compacting: Claude Code's, to a client without ACP's compaction
//!   updates, a tool call "Compact conversation" with
//!   `_meta.jetbrains.air.contextCompaction` (AIR) or
//!   `_meta.claudeCode.toolName` `compact`, whose facts (trigger and
//!   tokens) come as it ends, in that `_meta` or its `rawOutput`; Codex's,
//!   a notice "Context compacted", or to a client that can't take one, the
//!   words "*Context compacted to fit the model's context window.*".
//! - Exit codes: `_meta.terminal_exit.exit_code`, which both send for a
//!   command run as a terminal (Claude Code's says 1 for any that failed,
//!   to AIR), and Codex's `rawOutput.exit_code` for one that isn't.
//! - Spending: the prompt's answer's `usage`, ACP's unstable one, the
//!   turn's tokens; and `usage_update`'s `cost`, the session's cost so
//!   far, which only Claude Code's says: a turn's is what it grew by.

use std::collections::VecDeque;

use agent_client_protocol_schema::v1::{ContentBlock, Meta, StopReason};
use clackatoo_protocol::{ChatPatch, Cost, Item, Outcome, Spent, Tool, ToolKind, ToolStatus};
use serde_json::Value;

use super::{Action, HostedChat, Prompt, failure, one_line_notice};
use crate::agent::{Answer, Failure};

/// What Codex's adapter says, as words, to a client that can't take a
/// notice, when it compacted its context.
const CODEX_COMPACTED: &str = "*Context compacted to fit the model's context window.*\n\n";

/// The title of the notice Codex's adapter gives, to a client that can take
/// one, when it compacted its context: "Context compacted", with "Conversation
/// compacted to fit the model's context window." as what.
const CODEX_COMPACTED_NOTICE: &str = "Context compacted";

/// The most characters of what an agent says as it tries again kept.
const RETRYING: usize = 200;

/// What a chat keeps to say these.
#[derive(Debug, Default)]
pub(super) struct Facts {
    /// The last message the user sent, and the turn it started, to send
    /// again as it was, images and all, if it fails. Not handed over:
    /// after an upgrade only a message of text alone can be.
    last_prompt: Option<(usize, Prompt)>,
    /// What the session had cost as the turn answering started.
    cost_before: Option<Cost>,
    /// Whether the agent has said what the session cost since.
    cost_seen: bool,
    /// How many images the agent has shown in its words: each is a row of
    /// its own, numbered.
    images: u64,
    /// The row the image in the agent's words being taken goes in, once
    /// it's been taken to be made a picture of (see `image_sources`).
    image_taken: Option<String>,
    /// The tool calls that are the agent compacting its context, the
    /// latest few: the turn each is in, and the item it's shown as once
    /// it has finished, a line across the turn.
    compactions: VecDeque<Compaction>,
    /// The role or brief the message of a turn went with, and that turn,
    /// to send with it again if it failed.
    briefed: Option<(usize, String)>,
}

/// A tool call that's the agent compacting its context.
#[derive(Debug)]
struct Compaction {
    id: String,
    /// The turn it's in.
    turn: Option<usize>,
    /// The item it's shown as, a line across the turn, once it's finished.
    line: Option<usize>,
}

/// How many compactions are kept: more than come in a few turns.
const COMPACTIONS: usize = 16;

impl HostedChat {
    /// A turn starts.
    pub(super) fn facts_begin(&mut self) {
        self.facts.cost_before = self.node.usage.and_then(|usage| usage.cost);
        self.facts.cost_seen = false;
        self.set_retrying(None);
    }

    /// The user's `prompt` started turn `turn`: kept, to send again.
    pub(super) fn remember_prompt(&mut self, turn: usize, prompt: &Prompt) {
        self.facts.last_prompt = Some((turn, prompt.clone()));
    }

    /// The message of turn `turn` went with `role`, the chat's role or
    /// brief: kept, to send with it again if it fails.
    pub(super) fn remember_brief(&mut self, turn: usize, role: &str) {
        self.facts.briefed = Some((turn, role.to_owned()));
    }

    pub(super) fn set_retrying(&mut self, retrying: Option<String>) {
        if self.node.retrying != retrying {
            self.node.retrying.clone_from(&retrying);
            self.journal.push(ChatPatch::Retrying(retrying));
        }
    }

    /// The agent said more of the turn: whatever it was trying again
    /// went through.
    pub(super) fn progressed(&mut self) {
        if self.node.retrying.is_some() {
            self.set_retrying(None);
        }
    }

    /// Takes what a `session_info_update`'s `meta` says of a failure, or
    /// of trying again.
    pub(super) fn session_info(&mut self, meta: Option<&Meta>) {
        let Some(meta) = meta else { return };
        // What a session told again says of failures is long past.
        if self.told_again() {
            return;
        }
        let typed = (meta.get("jetbrains"))
            .and_then(|jetbrains| jetbrains.pointer("/air/sessionFailure"))
            .filter(|record| record.is_object());
        if let Some(record) = typed {
            let severity = record.get("severity").and_then(Value::as_str);
            let title = (record.get("title").and_then(Value::as_str)).unwrap_or_default();
            match severity {
                Some("warning") if self.answering() && !title.is_empty() => {
                    self.set_retrying(Some(retrying_text(title)));
                }
                Some("error") => self.failure_said(record),
                _ => {}
            }
            return;
        }
        let Some(error) = meta.get("codex").and_then(|codex| codex.get("error")) else {
            return;
        };
        let message = (error.get("message").and_then(Value::as_str)).unwrap_or_default();
        let again = error.get("willRetry").and_then(Value::as_bool) == Some(true);
        if message.trim().is_empty() {
            return;
        }
        if again && self.answering() {
            self.set_retrying(Some(retrying_text(message)));
        } else if !self.answering() {
            let said = format!(
                "{} said: {}",
                self.node.agent,
                clackatoo_protocol::code_span(&failure::agent_text(message))
            );
            self.set_notice(Some(one_line_notice(&said)));
        }
    }

    /// A failure the agent gave by itself, as AIR's `sessionFailure`,
    /// `record`, rather than on a turn's answer. It ends a turn the agent
    /// took by itself, whose end no answer says, as failed: Claude Code's
    /// adapter, to a client that asks for these, leaves its error out of
    /// the turn's words. Any other is of the session, such as its worker
    /// going, or its connection lost: a notice says so. None goes unsaid.
    fn failure_said(&mut self, record: &Value) {
        let said = failure::typed(record, &self.node.agent, None)
            .unwrap_or_else(|| failure::untyped(record));
        if self.woken() {
            self.answered_as(Outcome::Failed(said.text), said.raw, None);
        } else {
            self.set_notice(Some(failure_notice(&said.text)));
        }
    }

    /// Whether tool call `id` is the agent compacting its context, shown
    /// already as the line across the turn, or in a turn that's over:
    /// what more is said of it goes to that line, or nowhere (see
    /// [`compaction`](Self::compaction)), never to a tool.
    pub(super) fn compacted(&self, id: &str) -> bool {
        let turn = self.answering_turn();
        (self.facts.compactions.iter())
            .any(|known| known.id == id && (known.line.is_some() || known.turn != turn))
    }

    /// Notes tool call `id` as the agent compacting its context, if its
    /// `meta` says it is; and, if it is and has finished, shows it as the
    /// line across the turn that says so, with what `meta` or `raw` say
    /// of it. Claude Code's adapter says it finished once, then again with
    /// the tokens before and after, which the line then says.
    pub(super) fn compaction(&mut self, id: &str, meta: Option<&Meta>, raw: Option<&Value>) {
        let air = (meta.and_then(|meta| meta.get("jetbrains")))
            .and_then(|jetbrains| jetbrains.pointer("/air/contextCompaction"));
        let named = (meta.and_then(|meta| meta.get("claudeCode")))
            .and_then(|claude| claude.get("toolName"))
            .is_some_and(|name| name == "compact");
        let Some(turn) = self.answering_turn() else {
            return;
        };
        let known = (self.facts.compactions.iter()).position(|known| known.id == id);
        let known = match known {
            // A report on one of a turn that's over goes nowhere.
            Some(known) if self.facts.compactions[known].turn != Some(turn) => return,
            Some(known) => known,
            None if air.is_some() || named => {
                if self.facts.compactions.len() == COMPACTIONS {
                    self.facts.compactions.pop_front();
                }
                self.facts.compactions.push_back(Compaction {
                    id: id.to_owned(),
                    turn: Some(turn),
                    line: None,
                });
                self.facts.compactions.len() - 1
            }
            None => return,
        };
        let details = compaction_details(air.or(raw));
        let items = &mut self.node.turns[turn].items;
        if let Some(at) = self.facts.compactions[known].line {
            if let Some(Item::Compacted(old)) = items.get_mut(at)
                && !details.is_empty()
                && *old != details
            {
                old.clone_from(&details);
                self.patch_item(turn, at, Item::Compacted(details));
            }
            return;
        }
        let Some(at) = items.iter().position(
            |item| matches!(item, Item::Tool(tool) if tool.id == id && tool.status == ToolStatus::Done),
        ) else {
            return;
        };
        let item = Item::Compacted(details);
        items[at] = item.clone();
        self.facts.compactions[known].line = Some(at);
        self.patch_item(turn, at, item);
    }

    /// Takes `text`, a piece of the agent's words, as the line that says
    /// it compacted its context, if it's Codex's saying so to a client
    /// that can't take a notice. Returns whether it was.
    pub(super) fn codex_compacted(&mut self, text: &str) -> bool {
        self.agent == "codex" && text == CODEX_COMPACTED && self.compacted_line()
    }

    /// Takes the notice titled `title` as the line that says the agent
    /// compacted its context, if it's Codex's saying so in a turn, as it
    /// does to a client that can take notices. Returns whether it was.
    pub(super) fn codex_compacted_notice(&mut self, title: &str) -> bool {
        self.agent == "codex" && title == CODEX_COMPACTED_NOTICE && self.compacted_line()
    }

    /// Adds a line across the turn being answered saying the agent
    /// compacted its context, with nothing more of it known. Returns
    /// whether there's a turn to add it to.
    fn compacted_line(&mut self) -> bool {
        let Some(turn) = self.answering_turn() else {
            return false;
        };
        let item = Item::Compacted(String::new());
        self.node.turns[turn].items.push(item.clone());
        let at = self.node.turns[turn].items.len() - 1;
        self.patch_item(turn, at, item);
        true
    }

    /// An image in the agent's words, if it was taken to be made a picture
    /// of: a row of its own, which the picture is added to once it's made
    /// (see `image_sources`). One not taken, as too big, shows nothing.
    pub(super) fn message_image(&mut self, content: &ContentBlock) {
        let taken = self.facts.image_taken.take();
        if !matches!(content, ContentBlock::Image(_)) || self.told_again() || !self.answering() {
            return;
        }
        let id = self.next_image();
        if taken.as_ref() != Some(&id) {
            return;
        }
        self.facts.images += 1;
        self.set_tool(
            Tool {
                id,
                title: "Image".to_owned(),
                status: ToolStatus::Done,
                kind: ToolKind::Other,
                content: Vec::new(),
                locations: Vec::new(),
                exit: None,
            },
            None,
        );
    }

    /// The ID the next image in the agent's words is shown as: one no
    /// item of the turn answering has, the agent's tool calls among them.
    pub(super) fn next_image(&self) -> String {
        let items = (self.answering_turn()).map_or(&[][..], |turn| &self.node.turns[turn].items);
        // A turn of n items takes at most n numbers before one is free.
        let most = self.facts.images.saturating_add(items.len() as u64);
        (self.facts.images..=most)
            .map(|number| format!("clackatoo-agent-image:{number}"))
            .find(|id| {
                !items
                    .iter()
                    .any(|item| item.tool().is_some_and(|tool| tool.id == *id))
            })
            .unwrap_or_else(|| format!("clackatoo-agent-image:{most}+"))
    }

    /// The image in the agent's words that's to show as row `id` has
    /// been taken to be made a picture of.
    pub(super) fn image_taken(&mut self, id: String) {
        self.facts.image_taken = Some(id);
    }

    /// Takes the session's cost so far, `cost`, as a `usage_update` says
    /// it: `None` if it doesn't, which leaves what it last said.
    pub(super) fn cost_said(&mut self, cost: Option<Cost>) -> Option<Cost> {
        if cost.is_some() && self.answering() {
            self.facts.cost_seen = true;
        }
        cost.or_else(|| self.node.usage.and_then(|usage| usage.cost))
    }

    /// What the turn answering spent, as `usage`, the answer's, says, and
    /// what it cost, if the agent said what the session cost as it went.
    fn spent(&self, usage: Option<&Value>) -> Option<Spent> {
        let cost = (self.facts.cost_seen)
            .then(|| self.node.usage.and_then(|usage| usage.cost))
            .flatten()
            .and_then(|cost| cost.since(self.facts.cost_before));
        let count = |key: &str| usage?.get(key)?.as_u64();
        let output = count("outputTokens").unwrap_or(0);
        let cached = count("cachedReadTokens").unwrap_or(0);
        // Claude Code's input leaves out what came from the cache, and
        // Codex's doesn't: the total says what was read either way.
        let total = count("totalTokens").unwrap_or_else(|| {
            [
                count("inputTokens"),
                Some(output),
                Some(cached),
                count("cachedWriteTokens"),
            ]
            .into_iter()
            .flatten()
            .fold(0, u64::saturating_add)
        });
        let input = total.saturating_sub(output);
        (total > 0 || cost.is_some()).then_some(Spent {
            input,
            output,
            cached: cached.min(input),
            cost,
        })
    }

    /// The agent answered the message, or refused it: why, put in plain
    /// words if it failed, with `login`, its login command, if known.
    pub(crate) fn answered_by(&mut self, result: Result<Answer, Failure>, login: Option<&str>) {
        let agent = self.node.agent.clone();
        match result {
            Ok(answer) => {
                let spent = self.spent(answer.usage.as_ref());
                // A failure on the answer is one, whatever its kind.
                let said = (answer.failure.as_ref())
                    .filter(|_| answer.stop != StopReason::Cancelled)
                    .map(|record| {
                        failure::typed(record, &agent, login)
                            .unwrap_or_else(|| failure::untyped(record))
                    });
                match said {
                    Some(said) => self.answered_as(Outcome::Failed(said.text), said.raw, spent),
                    None => self.answered_as(outcome_of(Ok(answer.stop)), None, spent),
                }
            }
            Err(refusal) => {
                let said = failure::refused(&refusal, &agent, login);
                self.answered_as(Outcome::Failed(said.text), said.raw, None);
            }
        }
    }

    /// Sends the last turn's message again, as a turn of its own, if it
    /// failed and was the user's; with the role or brief it went with if
    /// it failed before the agent did anything with it, so before it can
    /// have taken that in: the turn holds nothing the agent did. A
    /// message with images is sent with them only as long as the chat has
    /// them: after an upgrade, its words go alone, and the chat says so.
    /// Nothing is sent while a message waits for the session to load.
    pub(super) fn resend(&mut self, actions: &mut Vec<Action>) {
        if self.holding() || self.loading() {
            return;
        }
        let Some(index) = self.node.turns.len().checked_sub(1) else {
            return;
        };
        let turn = &self.node.turns[index];
        let failed = matches!(
            turn.end.as_ref().map(|end| &end.outcome),
            Some(Outcome::Failed(_))
        );
        if !failed || turn.from.is_some() || turn.switch.is_some() || turn.woke.is_some() {
            return;
        }
        let unheard = turn.items.is_empty();
        let prompt = match &self.facts.last_prompt {
            Some((at, prompt)) if *at == index => prompt.clone(),
            _ => {
                let (words, images) = without_image_labels(&turn.prompt);
                if words.trim().is_empty() {
                    self.set_notice(Some(
                        "That message can't be sent again: its images aren't kept any more. \
                         Paste them again."
                            .to_owned(),
                    ));
                    return;
                }
                if images {
                    self.set_notice(Some(
                        "Sent its words again without its images, which aren't kept any \
                         more: paste them again if they're needed."
                            .to_owned(),
                    ));
                }
                Prompt::from(words)
            }
        };
        // The role or brief it went with goes again, if the agent never
        // took it in.
        if let Some((at, role)) = self.facts.briefed.take() {
            if at == index && unheard && self.role.is_none() {
                self.role = Some(role);
            } else {
                self.facts.briefed = Some((at, role));
            }
        }
        let notice = self.node.notice.clone();
        self.send_prompt(prompt, actions);
        // What it says of its images stays over the turn just begun.
        if notice.is_some() && self.node.notice.is_none() {
            self.set_notice(notice);
        }
    }
}

/// `shown`, a message as a turn shows it, without the labels that stand
/// for its images, `[Image · 640 × 480]` or `[Image]` on lines of their
/// own; and whether it had any.
fn without_image_labels(shown: &str) -> (String, bool) {
    let label = |line: &str| {
        line == "[Image]"
            || line
                .strip_prefix("[Image · ")
                .and_then(|rest| rest.strip_suffix(']'))
                .is_some_and(|size| {
                    size.split_once(" × ").is_some_and(|(width, height)| {
                        width.parse::<u32>().is_ok() && height.parse::<u32>().is_ok()
                    })
                })
    };
    let had = shown.lines().any(label);
    let words: Vec<&str> = shown.lines().filter(|line| !label(line)).collect();
    (words.join("\n").trim().to_owned(), had)
}

/// The notice that says `failure`, a failure in plain words, by itself.
pub(super) fn failure_notice(failure: &str) -> String {
    one_line_notice(&capitalised(failure))
}

/// How a turn that ended as `result` says ended.
pub(super) fn outcome_of(result: Result<StopReason, String>) -> Outcome {
    match result {
        Ok(StopReason::Cancelled) => Outcome::Stopped,
        Ok(StopReason::MaxTokens) => Outcome::Failed("the answer hit its length limit".to_owned()),
        Ok(StopReason::MaxTurnRequests) => {
            Outcome::Failed("the answer took too many steps".to_owned())
        }
        Ok(StopReason::Refusal) => Outcome::Failed("the agent refused".to_owned()),
        // Done, or a reason newer than this build knows.
        Ok(_) => Outcome::Done,
        Err(err) => Outcome::Failed(format!("the agent failed: {err}")),
    }
}

/// The code the command a tool call ran exited with, as its `meta` or,
/// for one that runs a command, of `kind`, its `raw` output says.
pub(super) fn exit_code(meta: Option<&Meta>, raw: Option<&Value>, kind: ToolKind) -> Option<i32> {
    let code = |value: &Value| {
        value
            .get("exit_code")?
            .as_i64()
            .and_then(|code| i32::try_from(code).ok())
    };
    (meta.and_then(|meta| meta.get("terminal_exit")))
        .and_then(code)
        .or_else(|| raw.filter(|_| kind == ToolKind::Execute).and_then(code))
}

/// What the line across a turn where the agent compacted its context
/// says of it, from its `facts`: "automatic · 182k → 31k tokens".
fn compaction_details(facts: Option<&Value>) -> String {
    let Some(facts) = facts else {
        return String::new();
    };
    let mut details = Vec::new();
    match facts.get("trigger").and_then(Value::as_str) {
        Some("automatic" | "auto") => details.push("automatic".to_owned()),
        Some("manual") => details.push("asked for".to_owned()),
        _ => {}
    }
    let tokens = |key: &str| facts.get(key).and_then(Value::as_u64);
    match (tokens("preTokens"), tokens("postTokens")) {
        (Some(before), Some(after)) => details.push(format!(
            "{} → {} tokens",
            clackatoo_protocol::tokens(before),
            clackatoo_protocol::tokens(after)
        )),
        (Some(before), None) => {
            details.push(format!(
                "from {} tokens",
                clackatoo_protocol::tokens(before)
            ));
        }
        _ => {}
    }
    details.join(" · ")
}

/// What an agent says as it tries again, on one line, with nothing in it
/// hidden, cut short.
fn retrying_text(text: &str) -> String {
    let text = failure::agent_text(text);
    let mut text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((cut, _)) = text.char_indices().nth(RETRYING) {
        text.truncate(cut);
        text.push('…');
    }
    text
}

/// `text` with a capital first letter, as a notice starts.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_compaction_says_how_it_was_started_and_the_tokens_before_and_after() {
        let facts = json!({"version": 1, "trigger": "automatic", "preTokens": 182_000,
            "postTokens": 31_000, "durationMs": 9000});
        assert_eq!(
            compaction_details(Some(&facts)),
            "automatic · 182k → 31k tokens"
        );
        assert_eq!(
            compaction_details(Some(&json!({"trigger": "manual"}))),
            "asked for"
        );
        assert_eq!(compaction_details(None), "");
    }

    #[test]
    fn an_exit_code_is_read_from_the_terminal_or_a_commands_raw_output() {
        let meta: Meta = serde_json::from_value(
            json!({"terminal_exit": {"terminal_id": "t", "exit_code": 2, "signal": null}}),
        )
        .unwrap();
        assert_eq!(exit_code(Some(&meta), None, ToolKind::Other), Some(2));
        let raw = json!({"exit_code": 127});
        assert_eq!(exit_code(None, Some(&raw), ToolKind::Execute), Some(127));
        assert_eq!(exit_code(None, Some(&raw), ToolKind::Read), None);
        let unknown: Meta =
            serde_json::from_value(json!({"terminal_exit": {"exit_code": null}})).unwrap();
        assert_eq!(exit_code(Some(&unknown), None, ToolKind::Execute), None);
    }
}
