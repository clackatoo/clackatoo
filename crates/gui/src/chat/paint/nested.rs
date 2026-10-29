//! What an agent's own subagents did, under the tool call that started
//! each, as Claude Code's Task tool starts them (CLACKA-195).
//!
//! The daemon keeps such items where they came among the turn's, so a
//! subagent's work stays in order with what else came meanwhile. Each
//! shows indented under its tool's card, and only while that card is
//! open: folded, as it starts, the card says what the subagent does now
//! and how many tools it used ("↳ Reading src/main.rs · 3 tool uses").
//! The card opens and folds as any does, by a click, Enter or `za`.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{Item, NestedWork, Tool, Turn};

/// How far each level of nesting is indented, in ems.
pub(super) const INDENT: f32 = 1.2;
/// How deep subagents' subagents are followed: a peer could say each is
/// the other's.
const DEEPEST: usize = 8;

/// Where the items of a turn stand among what subagents did in it.
#[derive(Default)]
pub(in crate::chat) struct Nesting<'a> {
    /// The turn's tools, by ID: each one's index, and the tool call
    /// whose subagent used it, if a subagent did.
    tools: HashMap<&'a str, (usize, Option<&'a str>)>,
    /// What the subagent each tool call started did, by its ID.
    work: HashMap<&'a str, NestedWork>,
}

impl<'a> Nesting<'a> {
    /// `turn`'s: nothing to look up, and nothing spent, for a turn with
    /// nothing nested.
    pub(in crate::chat) fn of(turn: &'a Turn) -> Self {
        let work = turn.nested_work();
        if work.is_empty() {
            return Self::default();
        }
        let tools = (turn.items.iter().enumerate())
            .filter_map(|(at, item)| Some((item.tool()?.id.as_str(), (at, item.parent()))))
            .collect();
        Self { tools, work }
    }

    /// The tools `item` is under, nearest first: none for the agent's
    /// own, or a subagent's of no tool of the turn.
    fn above(&self, item: &'a Item) -> impl Iterator<Item = &'a str> + '_ {
        let mut next = item.parent();
        std::iter::from_fn(move || {
            let id = next.filter(|id| self.tools.contains_key(id))?;
            next = self.tools[id].1;
            Some(id)
        })
        .take(DEEPEST)
    }

    /// How deep `item` is nested: 0 for the agent's own.
    pub(in crate::chat) fn depth(&self, item: &'a Item) -> usize {
        self.above(item).count()
    }

    /// The tools above `item` whose cards are folded, of those `open`:
    /// it shows only if there are none.
    pub(in crate::chat) fn folded_above(
        &self,
        item: &'a Item,
        open: &HashSet<String>,
    ) -> Vec<String> {
        (self.above(item))
            .filter(|id| !open.contains(*id))
            .map(str::to_owned)
            .collect()
    }

    /// Whether `item` is folded away under a tool's card, of those not
    /// `open`.
    pub(in crate::chat) fn hidden(&self, item: &'a Item, open: &HashSet<String>) -> bool {
        self.above(item).any(|id| !open.contains(id))
    }

    /// The outermost of `item`'s tools whose card is folded, by its index
    /// in the turn: the card that shows in its place, where a cursor on
    /// it goes as it's folded away.
    pub(in crate::chat) fn shown_above(
        &self,
        item: &'a Item,
        open: &HashSet<String>,
    ) -> Option<usize> {
        let mut shown = None;
        for id in self.above(item) {
            if !open.contains(id) {
                shown = Some(self.tools[id].0);
            }
        }
        shown
    }

    /// What `tool`'s card says of its subagent's work, under its title,
    /// if its subagent did anything.
    pub(in crate::chat) fn line(&self, turn: &Turn, tool: &Tool) -> Option<String> {
        let work = self.work.get(tool.id.as_str())?;
        Some(turn.nested_line(tool, work))
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{ChatNode, ChatState, Item, Tool, ToolKind, ToolStatus, Turn};
    use winit::keyboard::NamedKey;

    use super::super::{FOLD, ITEMS, Position, TextKey};
    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, paint, read, read_named};

    fn tool(id: &str, title: &str, status: ToolStatus) -> Item {
        Item::Tool(Tool {
            id: id.into(),
            title: title.into(),
            status,
            kind: ToolKind::Read,
            content: Vec::new(),
            locations: Vec::new(),
            exit: None,
        })
    }

    /// A turn going: the agent handed a search to a subagent, which is
    /// reading a file, and said something itself meanwhile.
    fn delegating() -> ChatNode {
        let nested = |item| Item::nested("task".into(), item);
        ChatNode {
            state: ChatState::Answering,
            turns: vec![Turn {
                woke: None,
                previews: Vec::new(),
                from: None,
                prompt: "Where's the parser?".into(),
                sent: 1_000,
                items: vec![
                    tool("task", "Find the parser", ToolStatus::Running),
                    nested(Item::Words("I'll look for the needle in src.".into())),
                    nested(tool("grep", "grep -n parse_line", ToolStatus::Done)),
                    Item::Words("Meanwhile, a word of mine.".into()),
                    nested(tool("read", "Read src/parse.rs", ToolStatus::Running)),
                ],
                end: None,
                files: None,
                switch: None,
            }],
            ..ChatNode::default()
        }
    }

    /// The pieces turn 0 shows, in order, and how far in each starts.
    fn shown(chat: &ChatView) -> Vec<(usize, bool)> {
        let laid = chat.cache.turns[0].laid.as_ref().unwrap();
        (laid.spans.iter())
            .filter_map(|span| match span.piece {
                super::super::Piece::Item(piece) | super::super::Piece::Card(piece) => {
                    Some((piece, span.left > 0.0))
                }
                _ => None,
            })
            .collect()
    }

    fn task_line(chat: &ChatView) -> Option<String> {
        let laid = chat.cache.turns[0].laid.as_ref().unwrap();
        laid.cards[&ITEMS].nested.clone()
    }

    #[test]
    fn a_tasks_card_folds_its_subagents_work_saying_what_it_does_now_until_opened() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), delegating());
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        // Folded, as it starts: the card says what the subagent does now.
        assert_eq!(shown(&chat), [(ITEMS, false), (ITEMS + 3, false)]);
        assert_eq!(
            task_line(&chat).as_deref(),
            Some("↳ Reading src/parse.rs · 2 tool uses")
        );
        // `za` on the card opens it: the subagent's work shows, indented,
        // in the order it came, with the agent's own words where they came.
        chat.cursor = Some(Position {
            text: TextKey {
                turn: 0,
                piece: ITEMS,
                part: 0,
            },
            byte: 0,
        });
        read(&mut chat, "za");
        paint(&mut chat, &mut fonts);
        assert_eq!(
            shown(&chat),
            [
                (ITEMS, false),
                (ITEMS + 1, true),
                (ITEMS + 2, true),
                (ITEMS + 3, false),
                (ITEMS + 4, true)
            ]
        );
        // `j` goes down through them as they show: the card's line saying
        // what the subagent does, then its words.
        read(&mut chat, "j");
        assert_eq!(
            chat.cursor.unwrap().text,
            TextKey {
                turn: 0,
                piece: ITEMS,
                part: 1
            }
        );
        read(&mut chat, "j");
        assert_eq!(chat.cursor.unwrap().text.piece, ITEMS + 1);
        // `za` in the subagent's words folds its tool's card, and the
        // cursor goes onto it.
        read(&mut chat, "za");
        paint(&mut chat, &mut fonts);
        assert_eq!(chat.cursor.unwrap().text.piece, ITEMS);
        assert_eq!(shown(&chat), [(ITEMS, false), (ITEMS + 3, false)]);
        // A finished task says so.
        let turn = &mut chat.node.turns[0];
        if let Item::Tool(task) = &mut turn.items[0] {
            task.status = ToolStatus::Done;
        }
        chat.restyled(0);
        paint(&mut chat, &mut fonts);
        assert_eq!(task_line(&chat).as_deref(), Some("↳ Done · 2 tool uses"));
    }

    #[test]
    fn a_search_opens_the_card_a_subagents_words_are_folded_under_and_yanks_see_them() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), delegating());
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg");
        read(&mut chat, "/needle");
        read_named(&mut chat, NamedKey::Enter);
        chat.settle(&mut fonts);
        let cursor = chat.cursor.unwrap();
        assert_eq!((cursor.text.turn, cursor.text.piece), (0, ITEMS + 1));
        assert!(chat.open_cards.contains("task"));
        // Copied whole, the turn has what shows of it: the subagent's
        // words now its card is open, its own nowhere else.
        let text = chat.piece_text(TextKey {
            turn: 0,
            piece: FOLD,
            part: 0,
        });
        assert!(text.is_some_and(|text| text.contains("↳")));
        let words = chat.piece_text(TextKey {
            turn: 0,
            piece: ITEMS + 1,
            part: 0,
        });
        assert_eq!(words.as_deref(), Some("I'll look for the needle in src."));
    }

    #[test]
    fn a_question_about_a_subagents_tool_opens_the_cards_its_folded_under() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), delegating());
        paint(&mut chat, &mut fonts);
        assert!(!chat.open_cards.contains("task"));
        let question = clackatoo_protocol::Question {
            id: 4,
            title: "Read src/parse.rs".into(),
            choices: Vec::new(),
            own: false,
            form: None,
            plan: None,
            tool: Some("read".into()),
        };
        let patch = clackatoo_protocol::Patch::Chat(clackatoo_protocol::ChatPatch::Question(Some(
            question,
        )));
        chat.apply(&clackatoo_protocol::ViewUpdate::Patch(vec![patch]));
        paint(&mut chat, &mut fonts);
        assert!(chat.open_cards.contains("task"));
        assert!(
            shown(&chat).contains(&(ITEMS + 4, true)),
            "{:?}",
            shown(&chat)
        );
    }

    #[test]
    fn a_folded_tasks_line_says_what_its_subagent_changed_and_ran() {
        let mut node = delegating();
        let edit = Item::Tool(Tool {
            id: "edit".into(),
            title: "Edit src/parse.rs".into(),
            status: ToolStatus::Done,
            kind: ToolKind::Edit,
            content: Vec::new(),
            locations: vec![clackatoo_protocol::Location {
                path: "src/parse.rs".into(),
                line: Some(3),
            }],
            exit: None,
        });
        node.turns[0].items.push(Item::nested("task".into(), edit));
        if let Item::Tool(task) = &mut node.turns[0].items[0] {
            task.status = ToolStatus::Done;
        }
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        assert_eq!(
            task_line(&chat).as_deref(),
            Some("↳ Done · 3 tool uses · edited 1 file")
        );
    }
}
