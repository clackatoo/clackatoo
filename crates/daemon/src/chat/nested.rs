//! What an agent's own subagents do, under the tool call that started
//! each (CLACKA-195).
//!
//! Claude Code's Task tool starts a subagent in the agent's own process.
//! Its adapter, asked for `forwardSubagentText` as clackatoo asks it, sends
//! what the subagent says and every tool it uses as it does the agent's
//! own, each marked with the tool call that started it in
//! `_meta.claudeCode.parentToolUseId`; it does so again as a session loads.
//! The chat keeps such an item where it came, as [`Item::Nested`], so the
//! patches that change it still find it, and windows show it under that
//! tool, folded away until it's opened.
//!
//! A subagent busy for long says a lot: what's kept of each is bounded,
//! and what's let go of first is the oldest, which a note says.

use agent_client_protocol_schema::v1::ContentChunk;
use serde_json::Value;

use super::{HostedChat, Item, handover, index_u32, item_weight};

/// The most items kept of what one subagent did, before the oldest go.
pub(super) const MOST_NESTED: usize = 200;
/// The most bytes kept of what one subagent did, before the oldest go.
pub(super) const MOST_NESTED_BYTES: usize = 1024 * 1024;
/// How deep subagents' subagents go before a deeper one's work is taken
/// as its parent's own.
const DEEPEST: usize = 8;
/// How the note on what was let go of starts: "*12 earlier steps not
/// shown*".
const CUT_NOTE: &str = " earlier steps not shown*";

/// The tool call whose subagent sent something, as Claude Code's adapter
/// says it: `_meta.claudeCode.parentToolUseId`.
fn parent_of(meta: Option<&serde_json::Map<String, Value>>) -> Option<&str> {
    meta?.get("claudeCode")?.get("parentToolUseId")?.as_str()
}

/// Whether `chunk`, a user's message told again, is what a subagent was
/// told: as a session loads, Claude Code's adapter tells a Task's prompt
/// again so, marked with its tool call.
pub(super) fn told_a_subagent(chunk: &ContentChunk) -> bool {
    parent_of(chunk.meta.as_ref()).is_some()
}

/// How many steps a note on what was let go of says were, if `item` is
/// one.
fn cut_note(item: &Item) -> Option<usize> {
    let Item::Words(text) = item.inner() else {
        return None;
    };
    text.strip_prefix('*')?.strip_suffix(CUT_NOTE)?.parse().ok()
}

impl HostedChat {
    /// The tool call `meta` says started the subagent that sent it, if
    /// it's a tool of the turn answering. Not if it's the item `id`
    /// itself, nor nested under it: nothing is nested under itself. Nor
    /// past [`DEEPEST`], or a tool from another turn: what such a
    /// subagent did shows as the agent's own, as it did before.
    pub(super) fn parent_here(
        &self,
        meta: Option<&serde_json::Map<String, Value>>,
        id: Option<&str>,
    ) -> Option<String> {
        let parent = parent_of(meta)?;
        let items = &self.node.turns.get(self.answering_turn()?)?.items;
        let mut at = parent;
        for _ in 0..DEEPEST {
            if Some(at) == id {
                return None;
            }
            let item = (items.iter()).find(|item| item.tool().is_some_and(|tool| tool.id == at))?;
            match item.parent() {
                Some(above) => at = above,
                None => return Some(parent.to_owned()),
            }
        }
        None
    }

    /// Whether the tool `id`, in the turn answering, is a subagent's.
    pub(super) fn is_nested(&self, id: &str) -> bool {
        let Some(turn) = self.answering_turn() else {
            return false;
        };
        (self.node.turns[turn].items.iter())
            .any(|item| item.parent().is_some() && item.tool().is_some_and(|tool| tool.id == id))
    }

    /// Keeps what the subagent the tool call `parent` started did, in turn
    /// `turn`, within [`MOST_NESTED`] items and [`MOST_NESTED_BYTES`]:
    /// past either by a quarter, the oldest go, and a note in place of the
    /// first says how many have. The turn goes again whole, as its items
    /// have moved, so not for every item past the bounds.
    pub(super) fn bound_nested(&mut self, turn: usize, parent: &str) {
        let items = &self.node.turns[turn].items;
        let children: Vec<usize> = (0..items.len())
            .filter(|&at| items[at].parent() == Some(parent))
            .collect();
        let (mut count, mut bytes) = (children.len(), 0);
        for &at in &children {
            bytes += item_weight(&items[at]);
        }
        if count <= MOST_NESTED + MOST_NESTED / 4
            && bytes <= MOST_NESTED_BYTES + MOST_NESTED_BYTES / 4
        {
            return;
        }
        // The note so far goes too, and counts what it said. The newest
        // always stays, however big.
        let mut gone = 0;
        let mut cut = Vec::new();
        for &at in &children {
            if count <= 1 || count <= MOST_NESTED && bytes <= MOST_NESTED_BYTES {
                break;
            }
            gone += cut_note(&items[at]).unwrap_or(1);
            count -= 1;
            bytes -= item_weight(&items[at]);
            cut.push(at);
        }
        let Some(&first) = cut.first() else {
            return;
        };
        let note = Item::nested(parent.to_owned(), Item::Words(format!("*{gone}{CUT_NOTE}")));
        let items = &mut self.node.turns[turn].items;
        items[first] = note;
        let mut at = 0;
        items.retain(|_| {
            at += 1;
            cut[1..].binary_search(&(at - 1)).is_err()
        });
        self.patch_turn(turn);
    }

    /// Which items of the chat's turns are subagents', and the tool call
    /// that started each, for the next build: the turns go as
    /// `handover::ChatTurn`, which holds nothing nested.
    pub(crate) fn hand_over_nesting(&self) -> Vec<handover::NestedItem> {
        let mut nested = Vec::new();
        for (turn, value) in self.node.turns.iter().enumerate() {
            for (item, value) in value.items.iter().enumerate() {
                if let Some(parent) = value.parent() {
                    nested.push(handover::NestedItem {
                        chat: self.id.0,
                        turn: index_u32(turn),
                        item: index_u32(item),
                        parent: parent.to_owned(),
                    });
                }
            }
        }
        nested
    }

    /// Nests again the items `records` say are subagents', of this chat's.
    pub(crate) fn resume_nesting(&mut self, records: &[handover::NestedItem]) {
        for record in records.iter().filter(|record| record.chat == self.id.0) {
            let item = (self.node.turns.get_mut(record.turn as usize))
                .and_then(|turn| turn.items.get_mut(record.item as usize));
            if let Some(item) = item {
                let flat = std::mem::replace(item, Item::Words(String::new()));
                *item = Item::nested(record.parent.clone(), flat);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::SessionNotification;
    use clackatoo_protocol::{AgentTools, Size, TerminalId, ToolStatus};

    use super::super::{Prompt, turn_from_handover, turn_to_handover};
    use super::*;

    /// A Claude Code chat, answering a message.
    fn answering() -> HostedChat {
        let agent = crate::agents::AgentConfig::default()
            .find("claude")
            .unwrap();
        let mut chat = HostedChat::new(
            TerminalId(1),
            Size::new(40, 12).unwrap(),
            "/tmp".to_owned(),
            &agent,
            AgentTools::Ask,
            true,
            false,
        );
        let prompt = Prompt {
            text: "look".into(),
            images: Vec::new(),
        };
        chat.begin("look".into(), prompt, None, &mut Vec::new());
        chat
    }

    fn update(chat: &mut HostedChat, json: &str) {
        let json = format!(r#"{{"sessionId":"s","update":{json}}}"#);
        let notification: SessionNotification = serde_json::from_str(&json).unwrap();
        chat.update(notification.update);
    }

    /// `_meta` saying the Task tool call `parent` started what sends it.
    fn from(parent: &str) -> String {
        format!(r#""_meta":{{"claudeCode":{{"parentToolUseId":"{parent}"}}}}"#)
    }

    fn call(chat: &mut HostedChat, id: &str, parent: Option<&str>) {
        let meta = parent
            .map(|parent| format!(",{}", from(parent)))
            .unwrap_or_default();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"tool_call","toolCallId":"{id}","title":"Read {id}.rs","kind":"read","status":"in_progress"{meta}}}"#
            ),
        );
    }

    fn says(chat: &mut HostedChat, kind: &str, text: &str, parent: Option<&str>) {
        let meta = parent
            .map(|parent| format!(",{}", from(parent)))
            .unwrap_or_default();
        update(
            chat,
            &format!(
                r#"{{"sessionUpdate":"{kind}","content":{{"type":"text","text":"{text}"}}{meta}}}"#
            ),
        );
    }

    fn words(chat: &mut HostedChat, text: &str, parent: Option<&str>) {
        says(chat, "agent_message_chunk", text, parent);
    }

    fn items(chat: &HostedChat) -> &[Item] {
        &chat.node.turns.last().unwrap().items
    }

    fn is_tool(id: &str) -> impl Fn(&Item) -> bool + '_ {
        move |item| item.tool().is_some_and(|tool| tool.id == id)
    }

    #[test]
    fn a_subagents_words_and_tools_go_under_the_task_that_started_it() {
        let mut chat = answering();
        words(&mut chat, "Let me delegate. ", None);
        call(&mut chat, "task", None);
        words(&mut chat, "I'll grep", Some("task"));
        words(&mut chat, " first.", Some("task"));
        says(&mut chat, "agent_thought_chunk", "hmm", Some("task"));
        // A call asked about first, whose subagent an update names, and
        // updates that say nothing of it after.
        call(&mut chat, "grep", None);
        update(
            &mut chat,
            &format!(
                r#"{{"sessionUpdate":"tool_call_update","toolCallId":"grep",{}}}"#,
                from("task")
            ),
        );
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"grep","status":"completed"}"#,
        );
        words(&mut chat, "Found it.", Some("task"));
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"task","status":"completed"}"#,
        );
        words(&mut chat, "It's in parse.rs.", None);
        let items = items(&chat);
        assert_eq!(items.len(), 7, "{items:#?}");
        assert_eq!(items[0], Item::Words("Let me delegate. ".into()));
        assert!(items[1].parent().is_none() && is_tool("task")(&items[1]));
        let nested = |item: Item| Item::nested("task".into(), item);
        assert_eq!(items[2], nested(Item::Words("I'll grep first.".into())));
        assert_eq!(items[3], nested(Item::Thought("hmm".into())));
        assert_eq!(items[4].parent(), Some("task"));
        assert_eq!(items[4].tool().unwrap().status, ToolStatus::Done);
        // The subagent's words don't run on from before its tool, nor the
        // agent's from the subagent's.
        assert_eq!(items[5], nested(Item::Words("Found it.".into())));
        assert_eq!(items[6], Item::Words("It's in parse.rs.".into()));
        assert!(chat.is_nested("grep") && !chat.is_nested("task"));
    }

    #[test]
    fn what_names_no_tool_of_the_turn_or_a_loop_is_the_agents_own() {
        let mut chat = answering();
        words(&mut chat, "Nobody's.", Some("gone"));
        assert_eq!(items(&chat), [Item::Words("Nobody's.".into())]);
        call(&mut chat, "a", None);
        call(&mut chat, "b", Some("a"));
        // "a" can't go under "b", which is under "a"…
        update(
            &mut chat,
            &format!(
                r#"{{"sessionUpdate":"tool_call_update","toolCallId":"a","status":"completed",{}}}"#,
                from("b")
            ),
        );
        // …nor anything under itself.
        call(&mut chat, "c", Some("c"));
        // A subagent's own subagent goes under its tool.
        call(&mut chat, "d", Some("b"));
        let items = items(&chat);
        assert_eq!(items[1].parent(), None);
        assert_eq!(items[2].parent(), Some("a"));
        assert_eq!(items[3].parent(), None);
        assert_eq!(items[4].parent(), Some("b"));
    }

    #[test]
    fn a_busy_subagent_keeps_its_newest_steps_and_says_how_many_went() {
        let mut chat = answering();
        call(&mut chat, "task", None);
        let last = MOST_NESTED + MOST_NESTED / 4;
        for n in 0..=last {
            call(&mut chat, &format!("t{n}"), Some("task"));
        }
        let kept = |chat: &HostedChat| {
            (items(chat).iter())
                .filter(|item| item.parent() == Some("task"))
                .count()
        };
        // Past the bound by a quarter, back to it, and a note first.
        assert_eq!(kept(&chat), MOST_NESTED + 1);
        let gone = MOST_NESTED / 4 + 1;
        let note = |gone| {
            Item::nested(
                "task".into(),
                Item::Words(format!("*{gone} earlier steps not shown*")),
            )
        };
        assert_eq!(items(&chat)[1], note(gone));
        assert!(!items(&chat).iter().any(is_tool("t0")));
        assert!(items(&chat).iter().any(is_tool(&format!("t{gone}"))));
        assert!((items(&chat).last()).is_some_and(is_tool(&format!("t{last}"))));
        assert!(is_tool("task")(&items(&chat)[0]));
        // Again, the note one of what's past the bound: it counts all
        // that went.
        for n in 0..MOST_NESTED / 4 {
            call(&mut chat, &format!("u{n}"), Some("task"));
        }
        assert_eq!(kept(&chat), MOST_NESTED + 1);
        assert_eq!(items(&chat)[1], note(gone + MOST_NESTED / 4));
        // And bytes: a few big steps push the oldest out.
        let big = "x".repeat(MOST_NESTED_BYTES / 3);
        for n in 0..4 {
            words(&mut chat, &big, Some("task"));
            call(&mut chat, &format!("v{n}"), Some("task"));
        }
        let bytes: usize = (items(&chat).iter())
            .filter(|item| item.parent() == Some("task"))
            .map(item_weight)
            .sum();
        assert!(
            bytes <= MOST_NESTED_BYTES + MOST_NESTED_BYTES / 4,
            "{bytes}"
        );
        assert!((items(&chat).last()).is_some_and(is_tool("v3")));
    }

    #[test]
    fn what_subagents_did_is_handed_over_beside_the_turns() {
        let mut chat = answering();
        call(&mut chat, "task", None);
        words(&mut chat, "Inside.", Some("task"));
        words(&mut chat, " Outside.", None);
        let nesting = chat.hand_over_nesting();
        assert_eq!(nesting.len(), 1);
        assert_eq!((nesting[0].turn, nesting[0].item), (0, 1));
        let before = chat.node.turns.clone();
        let flat = turn_from_handover(turn_to_handover(before[0].clone()));
        assert_eq!(flat.items[1], Item::Words("Inside.".into()));
        chat.node.turns = vec![flat];
        chat.resume_nesting(&nesting);
        assert_eq!(chat.node.turns[0].items, before[0].items);
        // Records of other chats, or of items not there, change nothing.
        let mut other = nesting;
        other[0].chat = 9;
        other.push(handover::NestedItem {
            chat: 1,
            turn: 7,
            item: 0,
            parent: "x".into(),
        });
        chat.resume_nesting(&other);
        assert_eq!(chat.node.turns[0].items, before[0].items);
    }
}
