//! Turns no one typed: a message another chat's agent sent, and a notice
//! from clackatoo itself.
//!
//! A message from another agent is headed with where it came from, "From
//! chat 7 · explore", in the subagents' colour, and shown as a quote down
//! the left, not as a bubble of the user's. A notice that chats the agent
//! started have finished shows as a card instead of its text, which is
//! for the agent: headed "↳ 2 subagents finished", with a card for each
//! chat saying what it was asked, how long it took and how it went, with
//! the first line of its answer. They're tools' cards (see
//! `Turn::notice_cards`), so they open and fold as tools' do: open, the
//! whole answer shows under its card, as the agent's words do, to be read,
//! searched and yanked. A click on a chat's name opens the chat.
//!
//! Each chat's card is piece `NOTICE + 2 × row` of the turn, and its
//! answer the next.

use std::collections::HashMap;

use clackatoo_protocol::{Sender, TerminalId, Turn};

use super::cards::CardLaid;
use super::select::{Position, TextKey, x_of};
use super::{AGENTS, Canvas, Drawn, Laid, Look, NOTICE, Piece, Span, TurnLaid};
use crate::chat::{ChatView, Hit, Tone};
use crate::fonts::Fonts;
use crate::markdown::layout::Measures;
use crate::render::{Fill, Paint};

/// How far in from the column's left a quote's text, and a notice's
/// cards, are, in ems: the rule down their left is in that space.
pub(super) const INDENT: f32 = 1.2;

/// How much further in a chat's answer is than its card, in ems: as far
/// as the card's text.
const ANSWER: f32 = 0.8;

/// Where the rule down the left of a quote or a notice's cards is, from
/// the column's left, in ems.
const RULE: f32 = 0.4;

/// The gap over each of a notice's cards, and over an answer, in ems.
const GAP: f32 = 0.3;

/// Which of a notice's chats `piece` is of, and whether it's that chat's
/// answer rather than its card; or `None` if it's no notice's.
pub(in crate::chat) fn notice_piece(piece: usize) -> Option<(usize, bool)> {
    let at = piece
        .checked_sub(NOTICE)
        .filter(|&at| at < AGENTS - NOTICE)?;
    Some((at / 2, at % 2 == 1))
}

/// The piece of the card of a notice's chat `row`.
pub(in crate::chat) fn card_piece(row: usize) -> usize {
    NOTICE + 2 * row
}

/// The piece of the answer of a notice's chat `row`, under its card.
pub(in crate::chat) fn answer_piece(row: usize) -> usize {
    card_piece(row) + 1
}

impl ChatView {
    /// What heads `turn`, a message from another chat's agent or a notice
    /// from clackatoo, and in what: "From chat 7 · explore", with the
    /// specialist the chat was started as, if it's one of this chat's own,
    /// or else its agent; or what the notice says.
    pub(super) fn sender_line(&self, turn: &Turn, from: &Sender) -> (String, Tone) {
        if let Some(notice) = &turn.notice {
            let tone = if turn.shows_finished() {
                Tone::Subagent
            } else {
                Tone::Muted
            };
            return (notice.heading(&from.title), tone);
        }
        let specialist = (self.node.subagents.iter())
            .find(|entry| entry.chat == from.chat)
            .and_then(|entry| entry.specialist.as_deref());
        let what = specialist.unwrap_or(&from.agent);
        (format!("From chat {} · {what}", from.chat), Tone::Subagent)
    }

    /// Lays out the cards of the chats `turn`'s notice says finished, in
    /// place of its message, into `laid`, from `y` down, and the answers
    /// of those open, keeping what of `old` hasn't changed. Returns where
    /// they end.
    pub(super) fn lay_out_notice(
        &mut self,
        fonts: &mut Fonts,
        (m, column): (Measures, f32),
        turn: &Turn,
        (laid, old_cards, old_pieces): (
            &mut TurnLaid,
            &mut HashMap<usize, CardLaid>,
            &mut HashMap<usize, Laid>,
        ),
        mut y: f32,
    ) -> f32 {
        let left = m.em(INDENT);
        let width = (column - left).max(m.em(8.0));
        let finished = turn.notice.as_ref().map(|notice| &notice.finished);
        for (row, tool) in turn.notice_cards().iter().enumerate() {
            if row > 0 {
                y += m.em(GAP);
            }
            let key = card_piece(row);
            let card = self.card(fonts, m, old_cards.remove(&key), (tool, None), width);
            laid.spans
                .push(Span::of(Piece::Card(key), y, card.height).left(left));
            y += card.height;
            laid.cards.insert(key, card);
            let answer = (finished.and_then(|finished| finished.get(row)))
                .map_or("", |chat| chat.answer.as_str());
            if !self.open_cards.contains(&tool.id) || answer.trim().is_empty() {
                continue;
            }
            let key = answer_piece(row);
            let left = left + m.em(ANSWER);
            let wide = (column - left).max(m.em(8.0));
            let piece = self.reuse(fonts, m, old_pieces.remove(&key), answer, Look::Words, wide);
            y += m.em(GAP * 2.0);
            laid.spans
                .push(Span::of(Piece::Item(key), y, piece.height).left(left));
            y += piece.height;
            laid.pieces.insert(key, piece);
        }
        y + m.em(1.0)
    }

    /// Draws what goes with the card of row `row` of turn `turn`'s notice,
    /// drawn at `at`, `height` tall: the rule down its left, and over the
    /// chat's name, where a click opens it, if it's one this chat's agent
    /// started.
    pub(super) fn draw_notice_row(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        (turn, row): (usize, usize),
        card: &CardLaid,
        (at, height): ([f32; 2], f32),
        drawn: &mut Drawn<'_>,
    ) {
        notice_rule(canvas, m, [at[0] - m.em(INDENT), at[1]], height);
        let chat = (self.node.turns.get(turn))
            .and_then(|turn| turn.notice.as_ref()?.finished.get(row))
            .map(|finished| finished.chat);
        let Some(chat) = chat.filter(|&chat| self.started(chat)) else {
            return;
        };
        let Some((head, [x, y])) = card.part(m, 0) else {
            return;
        };
        let Some(block) = head.blocks.first() else {
            return;
        };
        let name = format!("chat {chat}");
        if !block.text.starts_with(&name) {
            return;
        }
        let wide = x_of(&block.layout, name.len());
        let rect = [at[0] + x + block.left, at[1] + y, wide, head.height];
        drawn.hit(canvas.clip, Hit::Subagent(chat), rect);
    }

    /// Whether the chat `chat` is one this chat's agent started, which the
    /// daemon opens beside it when asked.
    fn started(&self, chat: TerminalId) -> bool {
        (self.node.subagents.iter()).any(|entry| entry.chat == chat)
    }

    /// The chat of the card of a notice the cursor's on, or in whose
    /// answer it is, if it's one this chat's agent started: `gx` and `gf`
    /// open it.
    pub(in crate::chat) fn notice_chat(&self) -> Option<TerminalId> {
        let cursor = self.cursor?;
        let (row, _) = notice_piece(cursor.text.piece)?;
        let turn = self.node.turns.get(cursor.text.turn)?;
        let chat = turn.notice.as_ref()?.finished.get(row)?.chat;
        self.started(chat).then_some(chat)
    }

    /// The card of the notice's chat the cursor's on, or whose answer
    /// it's in: its turn, its row and its id.
    fn answer_card(&self) -> Option<(usize, usize, String)> {
        let cursor = self.cursor?;
        let (row, _) = notice_piece(cursor.text.piece)?;
        let turn = self.node.turns.get(cursor.text.turn)?;
        let card = turn.notice_cards().into_iter().nth(row)?;
        Some((cursor.text.turn, row, card.id))
    }

    /// `za`, `zo` or `zc`, or Enter, if `open` is `None`, on the card of
    /// a notice's chat, or in its answer: opens the answer under the card,
    /// or folds it, the cursor going to the card. Returns false if the
    /// cursor's on no such card.
    pub(in crate::chat) fn fold_answer(&mut self, open: Option<bool>) -> bool {
        let Some((turn, row, id)) = self.answer_card() else {
            return false;
        };
        let open = open.unwrap_or_else(|| !self.open_cards.contains(&id));
        if open {
            self.open_cards.insert(id);
        } else {
            self.open_cards.remove(&id);
        }
        self.restyled(turn);
        self.cursor = Some(Position {
            text: TextKey {
                turn,
                piece: card_piece(row),
                part: 0,
            },
            byte: 0,
        });
        self.picked = None;
        self.reveal = true;
        true
    }

    /// Opens every answer of the chats notices say finished, if `open`, or
    /// folds them all, as `zR` and `zM` do with the turns' work.
    pub(in crate::chat) fn set_all_answers(&mut self, open: bool) {
        for index in 0..self.node.turns.len() {
            let cards = self.node.turns[index].notice_cards();
            if cards.is_empty() {
                continue;
            }
            for card in cards {
                if open {
                    self.open_cards.insert(card.id);
                } else {
                    self.open_cards.remove(&card.id);
                }
            }
            self.restyled(index);
        }
    }

    /// Puts the cursor, if it's in the answer of a notice's chat that's
    /// folded away, onto the chat's card.
    pub(in crate::chat) fn cursor_off_folded_answer(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        let Some((row, true)) = notice_piece(cursor.text.piece) else {
            return;
        };
        let open = (self.answer_card()).is_some_and(|(_, _, id)| self.open_cards.contains(&id));
        if !open {
            self.cursor = Some(Position {
                text: TextKey {
                    piece: card_piece(row),
                    part: 0,
                    ..cursor.text
                },
                byte: 0,
            });
            self.picked = None;
        }
    }
}

/// The text of `piece` of `turn`, if it's one of a notice's: a chat's
/// card's line and the first line of its answer, as it shows folded, and
/// if `whole`, the whole answer after; or its answer.
pub(in crate::chat) fn notice_text(turn: &Turn, piece: usize, whole: bool) -> Option<String> {
    let (row, answer) = notice_piece(piece)?;
    let chat = turn.notice.as_ref()?.finished.get(row)?;
    Some(match (answer, whole) {
        (true, _) => chat.answer.clone(),
        (false, true) => format!("{}\n{}", chat.label(), chat.answer)
            .trim_end()
            .to_owned(),
        (false, false) => format!("{}\n{}", chat.label(), chat.first_line()),
    })
}

/// Draws the rule down the left of a notice's cards and the answers
/// under them, with the column's left at `at`, `height` tall, joined to
/// what's over it.
pub(super) fn notice_rule(canvas: &mut Canvas<'_>, m: Measures, at: [f32; 2], height: f32) {
    let above = m.em(GAP);
    let rule = [at[0] + m.em(RULE), at[1] - above, 2.0, height + above];
    canvas.fill(Fill::solid(rule, Paint::SubagentEdge));
}

/// Draws the rule down the left of the quote of `turn`'s message, with
/// the column's left and the quote's top at `at`, `height` tall: in the
/// subagents' colour for a message from another chat's agent, and quiet
/// for a notice of clackatoo's.
pub(super) fn quote_rule(
    canvas: &mut Canvas<'_>,
    m: Measures,
    turn: Option<&Turn>,
    at: [f32; 2],
    height: f32,
) {
    let paint = if turn.is_some_and(|turn| turn.notice.is_none()) {
        Paint::SubagentEdge
    } else {
        Paint::RaisedEdge
    };
    let rule = [at[0] + m.em(RULE), at[1], 2.0, height];
    canvas.fill(Fill::solid(rule, paint));
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        ChatAction, ChatNode, FinishedChat, Outcome, Sender, Subagent, SubagentStatus, TerminalId,
        TurnNotice,
    };
    use winit::keyboard::NamedKey;

    use super::{answer_piece, card_piece};
    use crate::chat::fixtures::{fonts, paint, read, read_named, talk};
    use crate::chat::paint::{PROMPT, Position, TextKey};
    use crate::chat::{ChatView, Hit};
    use crate::fonts::Fonts;
    use crate::markdown::layout::Measures;

    fn finished(chat: u32, outcome: Outcome, answer: &str) -> FinishedChat {
        FinishedChat {
            chat: TerminalId(chat),
            agent: "Claude Code".into(),
            specialist: (chat == 5).then(|| "explore".into()),
            asked: format!("Find thing {chat}"),
            started: 10_000,
            ended: 90_000,
            outcome: Some(outcome),
            answer: answer.into(),
        }
    }

    /// A chat whose agent started chat 5, and was told it and chat 9,
    /// which it didn't start, finished.
    fn told() -> ChatNode {
        let mut node = talk(&[
            ("Look around", "Started one."),
            ("[chat 5 finished] what the agent reads", "Both done."),
        ]);
        node.turns[1].from = Some(Sender {
            chat: TerminalId(1),
            agent: "Clackatoo".into(),
            title: "Subagents finished".into(),
        });
        node.turns[1].notice = Some(TurnNotice {
            finished: vec![
                finished(5, Outcome::Done, "It's in `paint.rs`.\n\nAnd more past it."),
                finished(9, Outcome::Failed("it broke".into()), "Gave up."),
            ],
        });
        node.subagents = vec![Subagent {
            chat: TerminalId(5),
            agent: "Claude Code".into(),
            specialist: Some("explore".into()),
            model: None,
            task: "Find thing 5".into(),
            status: SubagentStatus::Done,
            started: 10_000,
            ended: Some(90_000),
            turn: 0,
            shown: false,
            tasks: Vec::new(),
        }];
        node
    }

    fn reading(node: ChatNode) -> (ChatView, Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        (chat, fonts)
    }

    fn on(chat: &mut ChatView, piece: usize) {
        chat.cursor = Some(Position {
            text: TextKey {
                turn: 1,
                piece,
                part: 0,
            },
            byte: 0,
        });
    }

    fn answer_open(chat: &ChatView, row: usize) -> bool {
        let id = chat.node.turns[1].notice_cards()[row].id.clone();
        chat.open_cards.contains(&id)
    }

    #[test]
    fn a_notice_is_a_card_of_the_chats_not_what_the_agent_was_told() {
        let (chat, _) = reading(told());
        let laid = chat.cache.turns[1].laid.as_ref().unwrap();
        assert_eq!(
            laid.badge.as_ref().map(|badge| badge.source.as_str()),
            Some("↳ 2 subagents finished in 1m 20s · 1 failed")
        );
        assert!(!laid.pieces.contains_key(&PROMPT), "not the agent's text");
        let heads: Vec<&str> = [card_piece(0), card_piece(1)]
            .iter()
            .map(|piece| laid.cards[piece].tool.title.as_str())
            .collect();
        assert_eq!(
            heads,
            [
                "chat 5 · explore · 1m 20s — Find thing 5",
                "chat 9 · Claude Code · 1m 20s · failed: it broke — Find thing 9",
            ]
        );
        // Folded, each shows the first line of its answer, and no more.
        let m = Measures::of(chat.cache.size);
        let first = laid.cards[&card_piece(0)].part(m, 1).unwrap().0;
        assert_eq!(first.source, "It's in `paint.rs`.");
        assert!(!laid.pieces.contains_key(&answer_piece(0)));
    }

    #[test]
    fn za_and_enter_open_an_answer_under_its_card_and_fold_it() {
        let (mut chat, mut fonts) = reading(told());
        on(&mut chat, card_piece(0));
        read(&mut chat, "za");
        assert!(answer_open(&chat, 0));
        paint(&mut chat, &mut fonts);
        let laid = chat.cache.turns[1].laid.as_ref().unwrap();
        assert_eq!(
            laid.pieces[&answer_piece(0)].source,
            "It's in `paint.rs`.\n\nAnd more past it."
        );
        // In the answer, Enter folds it, and the cursor goes to the card.
        on(&mut chat, answer_piece(0));
        read_named(&mut chat, NamedKey::Enter);
        assert!(!answer_open(&chat, 0));
        assert_eq!(chat.cursor.unwrap().text.piece, card_piece(0));
        // `zR` opens every answer, and `zM` folds them, the cursor off
        // the answers folded away.
        read(&mut chat, "zR");
        assert!(answer_open(&chat, 0) && answer_open(&chat, 1));
        paint(&mut chat, &mut fonts);
        on(&mut chat, answer_piece(1));
        read(&mut chat, "zM");
        assert!(!answer_open(&chat, 0) && !answer_open(&chat, 1));
        assert_eq!(chat.cursor.unwrap().text.piece, card_piece(1));
    }

    #[test]
    fn search_finds_and_opens_an_answer_and_a_yank_has_the_card_whole() {
        let (mut chat, mut fonts) = reading(told());
        read(&mut chat, "gg/past it");
        read_named(&mut chat, NamedKey::Enter);
        chat.settle(&mut fonts);
        let cursor = chat.cursor.unwrap().text;
        assert_eq!((cursor.turn, cursor.piece), (1, answer_piece(0)));
        assert!(answer_open(&chat, 0), "opened to show it");
        // What the agent was told isn't searched.
        read(&mut chat, "gg/what the agent reads");
        read_named(&mut chat, NamedKey::Enter);
        chat.settle(&mut fonts);
        assert_ne!(chat.cursor.unwrap().text.turn, 1);
        let key = TextKey {
            turn: 1,
            piece: card_piece(1),
            part: 0,
        };
        assert_eq!(
            chat.piece_text(key).as_deref(),
            Some("chat 9 · Claude Code · 1m 20s · failed: it broke — Find thing 9\nGave up.")
        );
    }

    #[test]
    fn a_click_on_a_chats_name_opens_it_if_this_chat_started_it() {
        let (mut chat, mut fonts) = reading(told());
        let hit = |chat: &ChatView, wanted: &Hit| {
            (chat.hits.iter())
                .find(|(hit, _)| hit == wanted)
                .map(|(_, rect)| *rect)
        };
        assert!(
            hit(&chat, &Hit::Subagent(TerminalId(9))).is_none(),
            "not its own"
        );
        let rect = hit(&chat, &Hit::Subagent(TerminalId(5))).expect("its name");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        let settled = chat.settle(&mut fonts);
        assert_eq!(
            settled.actions,
            [ChatAction::Open {
                chat: TerminalId(5)
            }]
        );
        assert!(!answer_open(&chat, 0), "the card stays as it was");
        // `gx` on its line does the same, and on one it didn't start,
        // nothing.
        on(&mut chat, card_piece(0));
        read(&mut chat, "gx");
        let open = ChatAction::Open {
            chat: TerminalId(5),
        };
        assert_eq!(chat.settle(&mut fonts).actions, [open]);
        on(&mut chat, card_piece(1));
        read(&mut chat, "gx");
        assert!(chat.settle(&mut fonts).actions.is_empty());
    }

    #[test]
    fn a_message_from_another_agent_is_quoted_under_where_it_came_from() {
        let mut node = told();
        node.turns[1].notice = None;
        node.turns[1].from = Some(Sender {
            chat: TerminalId(5),
            agent: "Claude Code".into(),
            title: "chat 5".into(),
        });
        node.turns[1].prompt = "Can you look at this?".into();
        let (chat, _) = reading(node);
        let laid = chat.cache.turns[1].laid.as_ref().unwrap();
        assert_eq!(
            laid.badge.as_ref().map(|badge| badge.source.as_str()),
            Some("From chat 5 · explore")
        );
        assert!(laid.quoted);
        let text = chat.text(TextKey {
            turn: 1,
            piece: PROMPT,
            part: 0,
        });
        assert_eq!(text.map(|text| text.text), Some("Can you look at this?"));
    }
}
