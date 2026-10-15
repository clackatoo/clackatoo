//! The card a turn shows for the chats its agent started there, its
//! subagents: one line, "3 subagents working · 12s ›", that opens to a line
//! for each, saying what it is, what it was asked, and how it's going.
//!
//! They run in the background, in no pane, so the card is where they're
//! seen: a click on one's line, or Enter on it, opens it as a pane beside
//! the chat. One that asks the user something says so, as it must be
//! opened to answer. While any works, the card counts the seconds.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use clackatoo_protocol::{Subagent, SubagentStatus, TerminalId};

use super::fold::{duration, height};
use super::select::TextKey;
use super::{AGENTS, Canvas, Drawn, Laid, Look, Measures, draw_laid, intersection, lay_out};
use crate::chat::{ChatView, Hit, Tone};
use crate::fonts::Fonts;

/// How far a chat's line on a card is indented under the card's own, in
/// ems.
pub(super) const INDENT: f32 = 1.2;

/// How far down a card its line `part` starts, in `m`: line 0 is the
/// card's own.
#[allow(
    clippy::cast_precision_loss,
    reason = "a few lines, far inside f32's range"
)]
pub(super) fn line_top(m: Measures, part: usize) -> f32 {
    height(m) * part as f32
}

/// A turn's card, laid out: its line, and when it's open, a line for each
/// chat.
pub(in crate::chat) struct AgentsLaid {
    pub(in crate::chat) header: Laid,
    pub(in crate::chat) rows: Vec<(TerminalId, Laid)>,
}

impl ChatView {
    /// The chats the agent started in turn `index`, oldest first: the last
    /// turn has any that say a turn past it.
    pub(in crate::chat) fn subagents_in(&self, index: usize) -> Vec<&Subagent> {
        // While the turns are laid out, they're out of the node: the
        // cache has a slot for each.
        let turns = self.node.turns.len().max(self.cache.turns.len());
        let last = turns.saturating_sub(1);
        (self.node.subagents.iter())
            .filter(|entry| (entry.turn as usize).min(last) == index)
            .collect()
    }

    /// Whether turn `index`'s card is open. Cards start folded.
    pub(in crate::chat) fn agents_open(&self, index: usize) -> bool {
        self.open_agents.contains(&index)
    }

    /// Opens turn `index`'s card, or folds it.
    pub(in crate::chat) fn toggle_agents(&mut self, index: usize) {
        let open = !self.agents_open(index);
        self.set_agents(index, open);
    }

    /// Opens turn `index`'s card, if `open`, or folds it.
    pub(in crate::chat) fn set_agents(&mut self, index: usize, open: bool) {
        let changed = if open {
            self.open_agents.insert(index)
        } else {
            self.open_agents.remove(&index)
        };
        if changed {
            self.restyled(index);
        }
    }

    /// Opens every turn's card, if `open`, or folds them all, as `zR` and
    /// `zM` do with the turns' work.
    pub(in crate::chat) fn set_all_agents(&mut self, open: bool) {
        let turns: std::collections::BTreeSet<usize> = (0..self.node.turns.len())
            .filter(|&index| !self.subagents_in(index).is_empty())
            .chain(self.open_agents.iter().copied())
            .collect();
        for index in turns {
            self.set_agents(index, open);
        }
    }

    /// How tall turn `index`'s card is, with `rows` chats, in `m`.
    pub(super) fn agents_height(&self, m: Measures, index: usize, rows: usize) -> f32 {
        let open = if self.agents_open(index) { rows } else { 0 };
        line_top(m, 1 + open)
    }

    /// Lays out the card of each turn in `turns` that has one, if what it
    /// says has changed.
    pub(super) fn label_agents(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        turns: std::ops::Range<usize>,
    ) {
        let now = self.now();
        for index in turns {
            if index >= self.cache.turns.len() {
                break;
            }
            let entries = self.subagents_in(index);
            if entries.is_empty() {
                self.cache.turns[index].agents = None;
                continue;
            }
            let open = self.agents_open(index);
            let header = agents_label(&entries, open, now);
            let rows: Vec<(TerminalId, String, Tone)> = if open {
                (entries.iter())
                    .map(|entry| (entry.chat, agent_row(entry, now), row_tone(&entry.status)))
                    .collect()
            } else {
                Vec::new()
            };
            let old = self.cache.turns[index].agents.take();
            let same = |laid: &Laid, text: &str| laid.source == text;
            let mut kept = old.map_or_else(
                || {
                    let empty = lay_out(
                        &mut self.layout_cx,
                        fonts,
                        m,
                        "",
                        Look::Plain(Tone::Muted),
                        f32::INFINITY,
                    );
                    (empty, Vec::new())
                },
                |old| (old.header, old.rows),
            );
            if !same(&kept.0, &header) {
                kept.0 = lay_out(
                    &mut self.layout_cx,
                    fonts,
                    m,
                    &header,
                    Look::Plain(Tone::Muted),
                    f32::INFINITY,
                );
            }
            let mut laid_rows = Vec::with_capacity(rows.len());
            for (chat, text, tone) in rows {
                let reused = (kept
                    .1
                    .iter()
                    .position(|(was, laid)| *was == chat && same(laid, &text)))
                .map(|at| kept.1.swap_remove(at).1);
                let laid = reused.unwrap_or_else(|| {
                    lay_out(
                        &mut self.layout_cx,
                        fonts,
                        m,
                        &text,
                        Look::Plain(tone),
                        f32::INFINITY,
                    )
                });
                laid_rows.push((chat, laid));
            }
            self.cache.turns[index].agents = Some(AgentsLaid {
                header: kept.0,
                rows: laid_rows,
            });
        }
    }

    /// Draws turn `index`'s card, with the column's left and its top at
    /// `origin`.
    pub(super) fn draw_agents(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        index: usize,
        origin: [f32; 2],
        drawn: &mut Drawn<'_>,
    ) {
        let Some(card) = (self.cache.turns.get(index)).and_then(|slot| slot.agents.as_ref()) else {
            return;
        };
        let column = self.cache.width;
        let line = height(m);
        let lines = std::iter::once((None, &card.header))
            .chain((card.rows.iter()).map(|(chat, laid)| (Some(*chat), laid)));
        for (part, (chat, laid)) in lines.enumerate() {
            let top = origin[1] + line_top(m, part);
            // Indented under the card's line.
            let left = origin[0] + if part == 0 { 0.0 } else { m.em(INDENT) };
            let width = (column - (left - origin[0])).max(0.0);
            let view = canvas.clip;
            canvas.clip = intersection(view, [left, top, width, line]);
            let text_top = top + (line - laid.height) / 2.0;
            draw_laid(canvas, m, laid, [left, text_top], width);
            canvas.clip = view;
            if let Some(block) = laid.blocks.first() {
                let key = TextKey {
                    turn: index,
                    piece: AGENTS,
                    part,
                };
                drawn.note_text(key, [left, text_top], &block.layout, false);
            }
            let row = [left, top, laid.width.min(width), line];
            let [x, y, wide, high] = intersection(view, row);
            if wide > 0.0 && high > 0.0 {
                let hit = chat.map_or(Hit::Agents(index), Hit::Subagent);
                let pane = drawn.pane;
                drawn
                    .hits
                    .push((hit, [x - pane[0], y - pane[1], wide, high]));
            }
        }
    }

    /// When a card in view next counts on a second, if one with a chat
    /// still going was drawn.
    pub(super) fn agents_live(&self) -> Option<Instant> {
        let now = self.now();
        (self.shown.iter())
            .filter(|shown| shown.key.piece == AGENTS && shown.at[1] < self.view_height)
            .flat_map(|shown| self.subagents_in(shown.key.turn))
            .filter(|entry| entry.status.going())
            .map(|entry| {
                let into = now.saturating_sub(entry.started) % 1000;
                Instant::now() + Duration::from_millis(1000 - into)
            })
            .min()
    }

    /// The chat on line `part` of turn `turn`'s card, if that line is a
    /// chat's: line 0 is the card's own.
    pub(in crate::chat) fn agent_on(&self, turn: usize, part: usize) -> Option<TerminalId> {
        let card = self.cache.turns.get(turn)?.agents.as_ref()?;
        card.rows.get(part.checked_sub(1)?).map(|(chat, _)| *chat)
    }

    /// What turn `turn`'s card says, a line each, to copy.
    pub(in crate::chat) fn agents_text(&self, turn: usize) -> String {
        let now = self.now();
        let entries = self.subagents_in(turn);
        let mut lines = vec![agents_label(&entries, false, now)];
        lines.extend(entries.iter().map(|entry| agent_row(entry, now)));
        lines.join("\n")
    }
}

/// What the line of a card for `entries` says: how many are working, or
/// have finished, how long it's been, and whether any asks the user.
fn agents_label(entries: &[&Subagent], open: bool, now: u64) -> String {
    let chevron = if open { "⌄" } else { "›" };
    let count = entries.len();
    let noun = if count == 1 { "subagent" } else { "subagents" };
    let going = entries.iter().filter(|entry| entry.status.going()).count();
    let asking = (entries.iter())
        .filter(|entry| entry.status == SubagentStatus::Asking)
        .count();
    let failed = (entries.iter())
        .filter(|entry| matches!(entry.status, SubagentStatus::Failed(_)))
        .count();
    let first = entries
        .iter()
        .map(|entry| entry.started)
        .min()
        .unwrap_or(now);
    let mut label = if going > 0 {
        let working = if going == count {
            format!("{count} {noun} working")
        } else {
            format!("{going} of {count} {noun} working")
        };
        format!("{working} · {}", duration(now.saturating_sub(first)))
    } else {
        let last = (entries.iter().filter_map(|entry| entry.ended)).max();
        let took = last.map_or_else(String::new, |last| {
            format!(" in {}", duration(last.saturating_sub(first)))
        });
        format!("{count} {noun} finished{took}")
    };
    if asking > 0 {
        let verb = if asking == 1 { "needs" } else { "need" };
        let _ = write!(label, " · {asking} {verb} you");
    }
    if failed > 0 {
        let _ = write!(label, " · {failed} failed");
    }
    format!("{label}  {chevron}")
}

/// What a card's line for `entry` says: how it's going, what it is, what
/// it was asked, and for how long.
fn agent_row(entry: &Subagent, now: u64) -> String {
    let elapsed = || duration(now.saturating_sub(entry.started));
    let took = || {
        entry.ended.map_or_else(String::new, |ended| {
            format!(" in {}", duration(ended.saturating_sub(entry.started)))
        })
    };
    let (mark, status) = match &entry.status {
        SubagentStatus::Starting => ("●", format!("starting {}", elapsed())),
        SubagentStatus::Working => ("●", format!("working {}", elapsed())),
        SubagentStatus::Asking => ("?", "needs you: open it to answer".to_owned()),
        SubagentStatus::Done => ("✓", format!("done{}", took())),
        SubagentStatus::Failed(why) => ("✗", format!("failed: {why}")),
        SubagentStatus::Closed => ("·", "closed".to_owned()),
    };
    let mut what = vec![entry.profile.clone().unwrap_or_else(|| entry.agent.clone())];
    if entry.profile.is_some() {
        what.push(entry.agent.clone());
    }
    if let Some(model) = &entry.model {
        what.push(model.clone());
    }
    let shown = if entry.shown { " · in a pane" } else { "" };
    format!(
        "{mark} {} · {} — {status}{shown}",
        what.join(" · "),
        entry.task
    )
}

/// The colour of a card's line for a chat that's `status`: one asking the
/// user stands out.
fn row_tone(status: &SubagentStatus) -> Tone {
    match status {
        SubagentStatus::Asking | SubagentStatus::Failed(_) => Tone::Warning,
        SubagentStatus::Starting | SubagentStatus::Working => Tone::Plain,
        SubagentStatus::Done | SubagentStatus::Closed => Tone::Muted,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        ChatAction, ChatNode, ChatPatch, ChatState, Outcome, Patch, Subagent, SubagentStatus,
        TerminalId, Turn, TurnEnd, ViewUpdate,
    };

    use super::super::AGENTS;
    use winit::keyboard::NamedKey;

    use crate::chat::fixtures::{fonts, paint, read, read_named};
    use crate::chat::{ChatView, Hit};

    fn entry(chat: u32, status: SubagentStatus) -> Subagent {
        Subagent {
            chat: TerminalId(chat),
            agent: "Claude Code".into(),
            profile: Some("explore".into()),
            model: Some("Haiku 5.5".into()),
            task: format!("Find thing {chat}"),
            status,
            started: 100_000,
            ended: None,
            turn: 0,
            shown: false,
        }
    }

    /// A turn going, whose agent started two chats.
    fn starting() -> ChatNode {
        ChatNode {
            state: ChatState::Answering,
            turns: vec![Turn {
                from: None,
                prompt: "Look around".into(),
                sent: 99_000,
                items: Vec::new(),
                end: None,
                files: None,
                switch: None,
            }],
            subagents: vec![
                entry(5, SubagentStatus::Working),
                entry(6, SubagentStatus::Starting),
            ],
            ..ChatNode::default()
        }
    }

    fn header(chat: &ChatView) -> Option<String> {
        let card = chat.cache.turns[0].agents.as_ref()?;
        Some(card.header.source.clone())
    }

    fn hit(chat: &ChatView, wanted: &Hit) -> Option<[f32; 4]> {
        (chat.hits.iter())
            .find(|(hit, _)| hit == wanted)
            .map(|(_, rect)| *rect)
    }

    #[test]
    fn a_turns_subagents_show_as_one_line_that_counts_and_opens_to_each() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), starting());
        chat.set_clock(112_400);
        paint(&mut chat, &mut fonts);
        assert_eq!(
            header(&chat).as_deref(),
            Some("2 subagents working · 12s  ›")
        );
        assert!(chat.live().is_some(), "it counts the seconds");
        // Folded, it's one line; a click opens it, a line a chat.
        let rect = hit(&chat, &Hit::Agents(0)).expect("the line, clickable");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        chat.settle(&mut fonts);
        paint(&mut chat, &mut fonts);
        let rows: Vec<String> = (chat.cache.turns[0].agents.as_ref().unwrap().rows.iter())
            .map(|(_, laid)| laid.source.clone())
            .collect();
        assert_eq!(
            rows,
            [
                "● explore · Claude Code · Haiku 5.5 · Find thing 5 — working 12s",
                "● explore · Claude Code · Haiku 5.5 · Find thing 6 — starting 12s",
            ]
        );
        // A click on a chat's line asks for it in a pane.
        let rect = hit(&chat, &Hit::Subagent(TerminalId(6))).expect("its line");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        let settled = chat.settle(&mut fonts);
        assert_eq!(
            settled.actions,
            [ChatAction::Open {
                chat: TerminalId(6)
            }]
        );
    }

    #[test]
    fn the_card_says_who_needs_the_user_and_how_long_they_all_took() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), starting());
        chat.set_clock(112_400);
        paint(&mut chat, &mut fonts);
        let mut asking = chat.node.subagents.clone();
        asking[1].status = SubagentStatus::Asking;
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Subagents(
            asking,
        ))]));
        paint(&mut chat, &mut fonts);
        assert_eq!(
            header(&chat).as_deref(),
            Some("2 subagents working · 12s · 1 needs you  ›")
        );
        let mut done = chat.node.subagents.clone();
        for (entry, ended) in done.iter_mut().zip([130_000, 160_000]) {
            entry.status = SubagentStatus::Done;
            entry.ended = Some(ended);
        }
        done[0].status = SubagentStatus::Failed("the agent stopped".into());
        let mut turn = chat.node.turns[0].clone();
        turn.end = Some(TurnEnd {
            at: 161_000,
            outcome: Outcome::Done,
        });
        chat.apply(&ViewUpdate::Patch(vec![
            Patch::Chat(ChatPatch::Turn { index: 0, turn }),
            Patch::Chat(ChatPatch::Subagents(done)),
        ]));
        paint(&mut chat, &mut fonts);
        assert_eq!(
            header(&chat).as_deref(),
            Some("2 subagents finished in 1m 0s · 1 failed  ›")
        );
        assert!(chat.live().is_none(), "nothing counts");
    }

    #[test]
    fn enter_on_a_chats_line_in_normal_mode_opens_it() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), starting());
        chat.set_clock(112_400);
        chat.toggle_agents(0);
        paint(&mut chat, &mut fonts);
        assert_eq!(chat.agent_on(0, 2), Some(TerminalId(6)));
        assert_eq!(chat.agent_on(0, 0), None);
        // The card's lines are text the cursor goes on: the last chat's is
        // the chat's end, and the first's is the line above.
        chat.reading = true;
        read(&mut chat, "Gk");
        let cursor = chat.cursor.map(|cursor| cursor.text);
        assert_eq!(cursor.map(|key| (key.piece, key.part)), Some((AGENTS, 1)));
        assert_eq!(read_named(&mut chat, NamedKey::Enter), (true, false));
        let settled = chat.settle(&mut fonts);
        assert_eq!(
            settled.actions,
            [ChatAction::Open {
                chat: TerminalId(5)
            }]
        );
    }

    #[test]
    fn zr_and_zm_open_and_fold_the_card_too_and_za_on_it_toggles_it() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), starting());
        chat.set_clock(112_400);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        assert!(!chat.agents_open(0));
        read(&mut chat, "zR");
        assert!(chat.agents_open(0), "open, with the turns' work");
        paint(&mut chat, &mut fonts);
        // On a chat's line, `zM` folds it, and the cursor goes to the
        // card's own line.
        read(&mut chat, "G");
        assert_eq!(chat.cursor.map(|cursor| cursor.text.part), Some(2));
        read(&mut chat, "zM");
        assert!(!chat.agents_open(0));
        let cursor = chat
            .cursor
            .map(|cursor| (cursor.text.piece, cursor.text.part));
        assert_eq!(cursor, Some((AGENTS, 0)));
        paint(&mut chat, &mut fonts);
        // On the card's line, `za` opens it and folds it, and `zo` and
        // `zc` likewise; the turn's work stays as it was.
        let work = |chat: &ChatView| chat.shape(0, &chat.node.turns[0]).open;
        let before = work(&chat);
        read(&mut chat, "za");
        assert!(chat.agents_open(0));
        read(&mut chat, "za");
        assert!(!chat.agents_open(0));
        read(&mut chat, "zo");
        assert!(chat.agents_open(0));
        read(&mut chat, "zc");
        assert!(!chat.agents_open(0));
        assert_eq!(work(&chat), before);
    }
}
