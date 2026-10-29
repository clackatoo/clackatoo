//! What an agent's own subagents do, under the tool call that started
//! each (CLACKA-195).
//!
//! Claude Code's Task tool, now called Agent, starts a subagent in the
//! agent's own process. Its adapter, asked for `forwardSubagentText` as
//! clackatoo asks it, sends what the subagent says and every tool it uses
//! as it does the agent's own, each marked with the tool call that started
//! it in `_meta.claudeCode.parentToolUseId`. The chat keeps such an item
//! where it came, as [`Item::Nested`], so the patches that change it still
//! find it, and windows show it under that tool, folded away until it's
//! opened. An item marked with a tool call the turn doesn't have is nested
//! all the same: windows show it on its own, never run on into the agent's
//! answer.
//!
//! A subagent busy for long says a lot: what's kept under each Task the
//! agent itself started, its subagents' subagents included, is bounded,
//! and what's let go of first is the oldest, whole subtrees at a time,
//! which a note says. What comes later for what was let go of is dropped,
//! and counted in the note.
//!
//! Clackatoo's own rows, whose IDs start `clackatoo-`, are never nested,
//! nor anything under them: an agent can't fold away what clackatoo shows.

use std::collections::{HashMap, HashSet};

use agent_client_protocol_schema::v1::ContentChunk;
use serde_json::Value;

use super::{HostedChat, Item, TurnUid, handover, index_u32, item_weight};

/// The most items kept of what one Task's subagents did, before the
/// oldest go.
pub(super) const MOST_NESTED: usize = 200;
/// The most bytes kept of what one Task's subagents did, before the
/// oldest go.
pub(super) const MOST_NESTED_BYTES: usize = 1024 * 1024;
/// How deep subagents' subagents go before a deeper one's work is taken
/// as its parent's own.
const DEEPEST: usize = 8;
/// How the note on what was let go of ends: "*12 earlier steps not
/// shown*".
const CUT_NOTE: &str = " earlier steps not shown*";
/// How the IDs of clackatoo's own rows start.
pub(super) const OWN: &str = "clackatoo-";

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

/// Where something marked as a subagent's goes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Under {
    /// In the turn as the agent's own: it's marked as no subagent's, or
    /// it may not be nested.
    Own,
    /// Under the tool call with this ID.
    Parent(String),
    /// Nowhere: what it's under was let go of to keep the bounds, under
    /// the Task with this ID, whose note counts it.
    Dropped(String),
}

/// A tool of the turn answering, as [`Index`] has it.
#[derive(Debug)]
struct Spot {
    /// The tool call whose subagent used it, if a subagent did.
    parent: Option<String>,
    /// The Task its work counts under: the outermost tool it's nested
    /// under, or itself.
    root: String,
}

/// What's kept under one Task, its subagents' subagents included.
#[derive(Debug, Default, Clone, Copy)]
struct Sum {
    items: usize,
    bytes: usize,
}

/// The turn answering's tools and what's nested under each, so finding
/// where something goes and keeping the bounds cost nothing per update.
/// Made again from the turn whenever it's changed other than through it.
#[derive(Debug, Default)]
pub(super) struct Index {
    /// The turn it's of, by its uid, and how many items it had: anything
    /// else, and it's made again.
    turn: Option<TurnUid>,
    len: usize,
    /// Set when the turn's items changed other than by being added.
    stale: bool,
    tools: HashMap<String, Spot>,
    sums: HashMap<String, Sum>,
    /// The tools of the turn let go of, with the Task each counted under:
    /// what comes later for them is dropped. Not made again: kept until
    /// the turn changes.
    cut: HashMap<String, String>,
}

impl Index {
    /// The Task what's nested under `parent` counts under.
    fn root_of(&self, parent: &str) -> String {
        (self.tools.get(parent)).map_or_else(|| parent.to_owned(), |spot| spot.root.clone())
    }

    /// Made again from `items`, turn `turn`'s.
    fn rebuild(&mut self, turn: Option<TurnUid>, items: &[Item]) {
        if self.turn != turn {
            self.cut.clear();
        }
        self.turn = turn;
        self.len = items.len();
        self.stale = false;
        self.tools.clear();
        self.sums.clear();
        for item in items {
            self.note(item);
        }
    }

    /// Takes in `item`, added at the end.
    fn note(&mut self, item: &Item) {
        let root = item.parent().map(|parent| self.root_of(parent));
        if let Some(root) = &root {
            let sum = self.sums.entry(root.clone()).or_default();
            sum.items += 1;
            sum.bytes += item_weight(item);
        }
        if let Some(tool) = item.tool() {
            let spot = Spot {
                parent: item.parent().map(str::to_owned),
                root: root.unwrap_or_else(|| tool.id.clone()),
            };
            self.tools.insert(tool.id.clone(), spot);
        }
    }
}

impl HostedChat {
    /// The index of the turn answering, made again if it isn't current;
    /// `None` while no turn is being answered.
    fn nest_index(&mut self) -> Option<&mut Index> {
        let turn = self.answering_turn()?;
        let items = &self.node.turns[turn].items;
        let uid = self.uids.of(turn);
        let index = &mut self.nest;
        if index.stale || index.turn != uid || index.len != items.len() {
            index.rebuild(uid, items);
        }
        Some(index)
    }

    /// The turn answering's items changed other than by one being added
    /// at the end: its index is made again when next needed.
    pub(super) fn nest_changed(&mut self) {
        self.nest.stale = true;
    }

    /// Where something `meta` marks goes: item `id`, if it's a tool call,
    /// or words or thinking. Clackatoo's own rows, and what's marked with
    /// one, are the agent's own; so is a tool that would be nested under
    /// itself, or deeper than [`DEEPEST`]. What was let go of, or is
    /// marked with what was, is dropped.
    pub(super) fn under(
        &mut self,
        meta: Option<&serde_json::Map<String, Value>>,
        id: Option<&str>,
    ) -> Under {
        let parent = parent_of(meta);
        let Some(index) = self.nest_index() else {
            return Under::Own;
        };
        if let Some(root) = id.and_then(|id| index.cut.get(id)) {
            return Under::Dropped(root.clone());
        }
        let Some(parent) = parent else {
            return Under::Own;
        };
        if parent.starts_with(OWN) || id.is_some_and(|id| id.starts_with(OWN)) {
            return Under::Own;
        }
        if let Some(root) = index.cut.get(parent) {
            return Under::Dropped(root.clone());
        }
        let mut at = Some(parent);
        for _ in 0..DEEPEST {
            match at {
                None => return Under::Parent(parent.to_owned()),
                Some(above) if Some(above) == id => return Under::Own,
                Some(above) => {
                    at = index
                        .tools
                        .get(above)
                        .and_then(|spot| spot.parent.as_deref());
                }
            }
        }
        if at.is_none() {
            Under::Parent(parent.to_owned())
        } else {
            Under::Own
        }
    }

    /// Whether the tool `id`, in the turn answering, is a subagent's.
    pub(super) fn is_nested(&mut self, id: &str) -> bool {
        (self.nest_index())
            .and_then(|index| index.tools.get(id))
            .is_some_and(|spot| spot.parent.is_some())
    }

    /// The item just added at the end of turn `turn`, the turn answering,
    /// is nested: it's counted, and the bounds kept.
    pub(super) fn nested_added(&mut self, turn: usize) {
        let items = &self.node.turns[turn].items;
        let Some(item) = items.last() else {
            return;
        };
        let uid = self.uids.of(turn);
        let index = &mut self.nest;
        if index.stale || index.turn != uid || index.len + 1 != items.len() {
            index.rebuild(uid, items);
        } else {
            index.len += 1;
            index.note(item);
        }
        let root = item.parent().map(|parent| index.root_of(parent));
        if let Some(root) = root {
            self.bound_nested(turn, &root);
        }
    }

    /// What's nested under `parent`, in the turn answering, grew by
    /// `bytes`, or shrank by `less`.
    pub(super) fn nested_grew(&mut self, parent: &str, bytes: usize, less: usize) {
        if let Some(index) = self.nest_index() {
            let root = index.root_of(parent);
            let sum = index.sums.entry(root).or_default();
            sum.bytes = (sum.bytes + bytes).saturating_sub(less);
        }
    }

    /// Something for what was let go of under the Task `root` was dropped:
    /// its note counts it.
    pub(super) fn nested_dropped(&mut self, root: &str) {
        let Some(turn) = self.answering_turn() else {
            return;
        };
        let items = &mut self.node.turns[turn].items;
        let Some(at) =
            (items.iter()).position(|item| item.parent() == Some(root) && cut_note(item).is_some())
        else {
            return;
        };
        let gone = cut_note(&items[at]).unwrap_or(0) + 1;
        items[at] = Item::nested(root.to_owned(), Item::Words(format!("*{gone}{CUT_NOTE}")));
        let note = items[at].clone();
        self.patch_item(turn, at, note);
    }

    /// Records that the tool call `id`, which `under` dropped, is let go
    /// of under the Task `root`, so what comes for it later is too, and
    /// counts it.
    pub(super) fn drop_tool(&mut self, id: &str, root: &str) {
        if let Some(index) = self.nest_index()
            && !index.cut.contains_key(id)
        {
            index.cut.insert(id.to_owned(), root.to_owned());
            self.nested_dropped(root);
        }
    }

    /// Keeps what's under the Task `root`, in turn `turn`, within
    /// [`MOST_NESTED`] items and [`MOST_NESTED_BYTES`]: past either by a
    /// quarter, the oldest go, each with all that's under it, and a note
    /// in place of the first says how many have. The newest stays, unless
    /// what it's under goes. The turn goes again whole, as its items have
    /// moved: so only once for every quarter of the bounds.
    fn bound_nested(&mut self, turn: usize, root: &str) {
        let sum = self.nest.sums.get(root).copied().unwrap_or_default();
        if sum.items <= MOST_NESTED + MOST_NESTED / 4
            && sum.bytes <= MOST_NESTED_BYTES + MOST_NESTED_BYTES / 4
        {
            return;
        }
        let items = &self.node.turns[turn].items;
        let uid = self.uids.of(turn);
        let index = &mut self.nest;
        index.rebuild(uid, items);
        // What's under the root, oldest first.
        let members: Vec<usize> = (0..items.len())
            .filter(|&at| items[at].parent().is_some_and(|p| index.root_of(p) == root))
            .collect();
        let (mut count, mut bytes) = (members.len(), 0);
        for &at in &members {
            bytes += item_weight(&items[at]);
        }
        let newest = members.last().copied();
        let mut gone = 0;
        let mut cut = Vec::new();
        let mut cut_tools = HashSet::new();
        for &at in &members {
            let item = &items[at];
            let forced = item
                .parent()
                .is_some_and(|parent| cut_tools.contains(parent));
            let within = count <= MOST_NESTED && bytes <= MOST_NESTED_BYTES;
            if !forced && (within || Some(at) == newest) {
                continue;
            }
            gone += cut_note(item).unwrap_or(1);
            count -= 1;
            bytes -= item_weight(item);
            if let Some(tool) = item.tool() {
                cut_tools.insert(tool.id.clone());
            }
            cut.push(at);
        }
        let Some(&first) = cut.first() else {
            return;
        };
        for id in cut_tools {
            index.cut.insert(id, root.to_owned());
        }
        let note = Item::nested(root.to_owned(), Item::Words(format!("*{gone}{CUT_NOTE}")));
        let items = &mut self.node.turns[turn].items;
        items[first] = note;
        let mut at = 0;
        items.retain(|_| {
            at += 1;
            cut[1..].binary_search(&(at - 1)).is_err()
        });
        self.nest.stale = true;
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
    /// Never one of clackatoo's own rows, whatever a record says.
    pub(crate) fn resume_nesting(&mut self, records: &[handover::NestedItem]) {
        for record in records.iter().filter(|record| record.chat == self.id.0) {
            let item = (self.node.turns.get_mut(record.turn as usize))
                .and_then(|turn| turn.items.get_mut(record.item as usize));
            if let Some(item) = item
                && !item.tool().is_some_and(|tool| tool.id.starts_with(OWN))
                && !record.parent.starts_with(OWN)
            {
                let flat = std::mem::replace(item, Item::Words(String::new()));
                *item = Item::nested(record.parent.clone(), flat);
            }
        }
        self.nest_changed();
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
    fn what_names_no_tool_of_the_turn_stays_apart_and_a_loop_is_the_agents_own() {
        let mut chat = answering();
        // A subagent of no tool of the turn: never run on into the
        // agent's words, before or after.
        words(&mut chat, "Mine. ", None);
        words(&mut chat, "Nobody's.", Some("gone"));
        words(&mut chat, " Mine again.", None);
        let nested = Item::nested("gone".into(), Item::Words("Nobody's.".into()));
        assert_eq!(
            items(&chat),
            [
                Item::Words("Mine. ".into()),
                nested,
                Item::Words(" Mine again.".into())
            ]
        );
        let mut chat = answering();

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
        assert_eq!(items[0].parent(), None);
        assert_eq!(items[1].parent(), Some("a"));
        assert_eq!(items[2].parent(), None);
        assert_eq!(items[3].parent(), Some("b"));
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

    #[test]
    fn an_agent_can_never_fold_away_clackatoos_own_rows() {
        let mut chat = answering();
        call(&mut chat, "task", None);
        // A forged call with the ID clackatoo's row for what the agent
        // asks to type will have, marked as the Task's subagent's…
        call(&mut chat, "clackatoo-7", Some("task"));
        let row = |chat: &HostedChat| {
            let items = items(chat);
            let at = (items.iter()).position(is_tool("clackatoo-7")).unwrap();
            items[at].clone()
        };
        assert_eq!(row(&chat).parent(), None);
        // …and clackatoo's row itself, written over it, stay the agent's own.
        let asked = chat.ask_for_tool(7, "run make", Some("make all"), super::super::Choices::Tool);
        assert!(asked);
        let shown = row(&chat);
        assert_eq!(shown.parent(), None);
        assert!(shown.tool().unwrap().title.contains("in full"));
        // Nothing goes under one of clackatoo's rows, either.
        call(&mut chat, "under", Some("clackatoo-7"));
        assert!(
            items(&chat)
                .iter()
                .any(|item| is_tool("under")(item) && item.parent().is_none())
        );
        // Nor does a handed-over record nest one.
        let at = (items(&chat).iter())
            .position(is_tool("clackatoo-7"))
            .unwrap();
        chat.resume_nesting(&[handover::NestedItem {
            chat: 1,
            turn: 0,
            item: index_u32(at),
            parent: "task".into(),
        }]);
        assert_eq!(row(&chat).parent(), None);
    }

    #[test]
    fn a_tasks_bound_takes_in_its_subagents_subagents_and_cuts_whole_subtrees() {
        let mut chat = answering();
        call(&mut chat, "task", None);
        // A subagent's own subagent, first, with a step of its own.
        call(&mut chat, "inner", Some("task"));
        call(&mut chat, "deep", Some("inner"));
        // Then enough under the Task, the inner's counted with them, to go
        // past the bound by a quarter.
        let last = MOST_NESTED + MOST_NESTED / 4 - 2;
        for n in 0..=last {
            call(&mut chat, &format!("t{n}"), Some("task"));
        }
        let under_task = |chat: &HostedChat| {
            (items(chat).iter())
                .filter(|item| item.parent().is_some())
                .count()
        };
        assert!(
            under_task(&chat) <= MOST_NESTED + 1,
            "{}",
            under_task(&chat)
        );
        // The inner subagent went whole, its step with it.
        assert!(!items(&chat).iter().any(is_tool("inner")));
        assert!(!items(&chat).iter().any(is_tool("deep")));
        let note = |chat: &HostedChat| {
            (items(chat).iter())
                .find_map(|item| cut_note(item).filter(|_| item.parent() == Some("task")))
        };
        let gone = note(&chat).expect("a note");
        // What comes later for what went is dropped, and counted, never
        // shown as the agent's own.
        call(&mut chat, "later", Some("inner"));
        update(
            &mut chat,
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"deep","status":"completed"}"#,
        );
        words(&mut chat, "Still here.", Some("inner"));
        assert!(
            !items(&chat)
                .iter()
                .any(|item| is_tool("later")(item) || is_tool("deep")(item))
        );
        assert!(
            !(items(&chat).iter())
                .any(|item| matches!(item.inner(), Item::Words(w) if w.contains("Still")))
        );
        assert_eq!(note(&chat), Some(gone + 1));
        // A subagent of no tool of the turn is bounded all the same.
        let mut chat = answering();
        for n in 0..=MOST_NESTED + MOST_NESTED / 4 {
            call(&mut chat, &format!("o{n}"), Some("gone"));
        }
        assert!(items(&chat).len() <= MOST_NESTED + 1);
    }
}
