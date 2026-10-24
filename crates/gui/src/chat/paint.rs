//! Laying out a chat and drawing it: its turns in a column down the
//! middle of the pane, scrolled, and the box a message is typed in below.
//!
//! Every size is a multiple of the terminal font's size, so a chat grows
//! and shrinks with the text around it. Turns are laid out once for a
//! width, as they come into view, and again only when they change, as
//! the turn being answered does, or are opened or folded.

mod agents;
mod cards;
mod divider;
mod edge;
mod fold;
mod models;
mod select;
mod tables;
mod usage;
mod working;

use std::collections::HashMap;

use clackatoo_protocol::{
    ChatState, ChoiceKind, Item, Outcome, Said, Setting, SettingKind, ToolStatus, Turn, TurnFiles,
    TurnFilesState, code_span,
};
use parley::Layout;

use super::picker::{self, ChoiceRow, Row, SliderRow, Spot};
use super::{Brush, ChatView, Hit, Menu, Tone};
use crate::fonts::Fonts;
use crate::markdown::layout::{
    Canvas, LINE, Laid, Look, Measures, TextPaint, draw_laid, draw_scrolled, intersection, lay_out,
    lay_out_coloured,
};
pub(super) use crate::markdown::layout::{body_style, text_size};
use crate::markdown::tables::TableLaid;
use crate::render::{Fill, Paint};
// What the tests below use of what was moved to `markdown`.
#[cfg(test)]
use crate::markdown::{
    layout::{LaidBlock, code_inset, quote_inset, warm_fonts},
    tables::Cells,
};
pub(super) use select::{
    How, Picked, Position, Shown, TextKey, after, before, line_middle, line_of, within, x_of,
};
pub(super) use usage::Detail;
#[cfg(test)]
use {crate::render::Ink, parley::PositionedLayoutItem};

/// The widest the column of turns gets, in ems.
const COLUMN: f32 = 46.0;
/// The most lines the box a message is typed in grows to.
const EDITOR_LINES: f32 = 10.0;
/// A turn's message is its piece 0.
pub(super) const PROMPT: usize = 0;
/// The line its work folds under is its piece 1.
pub(super) const FOLD: usize = 1;
/// Item `i` of a turn is its piece `ITEMS + i`.
pub(super) const ITEMS: usize = 2;
/// The card for the chats its agent started is its last piece: line 0 is
/// the card's own, and line `n` the `n`th chat's.
pub(super) const AGENTS: usize = usize::MAX;

/// Whether `piece` is a line that opens and folds, rather than text the
/// conversation says: what's selected and copied leaves it out.
pub(super) fn control(piece: usize) -> bool {
    piece == FOLD || piece == AGENTS
}

/// The turns, laid out for one width, kept while they stay the same.
///
/// Only turns in view, or near it, are laid out: the rest are taken to be
/// as tall as they were when last laid out, or until they have been, as
/// tall as a guess from their text. A long chat opens, and scrolls, at the
/// cost of what's on the screen.
#[derive(Default)]
pub(super) struct Cache {
    /// The column's width and the text's size they were laid out for.
    pub(super) width: f32,
    pub(super) size: f32,
    /// The fonts' [`generation`](Fonts::generation) they were laid out in.
    fonts: u32,
    /// By turn, in order.
    pub(super) turns: Vec<TurnSlot>,
    /// Each turn's top and bottom, from the conversation's top, as far as
    /// `bounded` turns: from there on, they're worked out again when next
    /// asked for, as a turn's height has changed.
    bounds: Vec<[f32; 2]>,
    bounded: usize,
    /// The turns laid out, to drop again once far from the view.
    laid: Vec<usize>,
    /// Text of the chat's own, such as that the agent couldn't start, by
    /// what it says.
    notes: HashMap<(String, Tone), Laid>,
    /// The line above the box that says what the agent is doing, laid
    /// out, while it works: what it says changes as it goes on.
    working: Option<working::WorkingLaid>,
    /// The box's own short lines, as its chips say, laid out.
    labels: Labels,
}

/// Short lines of the box's own, such as its chips' labels and the
/// meter, laid out for the frame before and this one, by what they say
/// and their tone: drawing the box lays out only what's changed, and
/// what isn't drawn again is let go, so labels that come and go, as the
/// meter's do, don't pile up.
#[derive(Default)]
struct Labels {
    last: HashMap<(String, Tone), Laid>,
    now: HashMap<(String, Tone), Laid>,
    shortened: HashMap<(String, u32, Tone), String>,
    previous_shortened: HashMap<(String, u32, Tone), String>,
}

/// What's kept of one turn between frames.
#[derive(Default)]
pub(super) struct TurnSlot {
    /// The turn, as it was laid out when last it was.
    laid: Option<TurnLaid>,
    /// The turn has changed since, or how it's shown has.
    pub(super) stale: bool,
    /// How tall it is: as laid out, or until it has been, a guess. `None`
    /// until guessed.
    height: Option<f32>,
    /// The line its work folds under, laid out: what it says changes as
    /// the turn goes on.
    header: Option<Laid>,
    /// The card for the chats its agent started, laid out, if it started
    /// any: what it says changes as they go on.
    agents: Option<agents::AgentsLaid>,
    /// The time above it, laid out, if it was sent long after the turn
    /// before: what it says changes as the days go by.
    divider: Option<Laid>,
}

/// A turn, laid out.
struct TurnLaid {
    /// Its pieces, top to bottom, from the turn's top.
    spans: Vec<Span>,
    /// Its message and the agent's items, by piece.
    pieces: HashMap<usize, Laid>,
    /// Tools shown as cards, by piece.
    cards: HashMap<usize, cards::CardLaid>,
    /// What's said under it, as how it ended.
    note: Option<Laid>,
    /// What it changed in files, and the buttons that undo and redo it.
    files: Option<FilesLaid>,
    /// Whether the button that copies its answer is under it.
    copy: bool,
    /// Which chat sent its message, if another agent did.
    badge: Option<Laid>,
    height: f32,
}

/// What a turn changed in files, laid out: "2 files changed", and its
/// buttons, "Undo" or "Redo", each with what clicking it does.
struct FilesLaid {
    label: Laid,
    buttons: Vec<(Hit, Laid)>,
    height: f32,
    /// Where it starts, from the column's left: after the button that
    /// copies the answer, if that's there.
    left: f32,
}

impl ChatView {
    /// Lays the turns whose code was waiting for colours out again, when
    /// next they're near the view, as some have come.
    pub(super) fn recolour(&mut self) {
        for index in 0..self.cache.turns.len() {
            let waiting = (self.cache.turns[index].laid.as_ref())
                .is_some_and(|laid| laid.pieces.values().any(|piece| piece.waiting));
            if waiting {
                self.restyled(index);
            }
        }
    }
}

impl ChatView {
    /// Lays the chat out in a pane `size` pixels, whose top left is at
    /// `origin`, and says what to draw. `caret` says the pane has the
    /// keyboard: the cursor is drawn in the message being typed, or in
    /// normal mode, in the conversation.
    pub(crate) fn paint(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        caret: bool,
    ) -> TextPaint {
        self.previews.begin();
        self.style_editor(fonts);
        let em = text_size(fonts);
        let m = Measures::of(em);
        self.line_height = (em * LINE).round();
        let margin = m.em(1.5);
        let column = (size[0] - 2.0 * margin).clamp(m.em(8.0), m.em(COLUMN));
        let left = ((size[0] - column) / 2.0).max(0.0).round();
        // Laid out for another width, size or font: again, as each turn
        // comes into view. How tall each was is still the best guess.
        if self.cache.width.to_bits() != column.to_bits()
            || self.cache.size.to_bits() != em.to_bits()
            || self.cache.fonts != fonts.generation
        {
            let turns = (std::mem::take(&mut self.cache.turns).into_iter())
                .map(|slot| TurnSlot {
                    height: slot.height,
                    ..TurnSlot::default()
                })
                .collect();
            self.cache = Cache {
                width: column,
                size: em,
                fonts: fonts.generation,
                turns,
                ..Cache::default()
            };
        }

        // What the box said last frame, to say again without laying it
        // out; what it doesn't say again goes.
        let labels = &mut self.cache.labels;
        labels.last = std::mem::take(&mut labels.now);
        labels.previous_shortened = std::mem::take(&mut labels.shortened);

        let mut paint = TextPaint::default();
        self.hits.clear();
        self.shown.clear();

        // The box below, first, as the conversation gets what's left.
        let place = Place {
            origin,
            size,
            left,
            column,
        };
        let box_height = self.paint_box(fonts, &mut paint, m, place, caret && !self.reading);
        // Just above the box, a row for what's gone wrong, and under it,
        // nearest the box, a row for what the agent is doing while it
        // works: always in view, wherever the conversation's scrolled to.
        let status = self.status();
        self.moving = caret;
        let now = self.now();
        let working = self.label_working(fonts, m, now);
        let rows = u8::from(status.is_some()) + u8::from(working);
        let status_height = m.em(1.8) * f32::from(rows);
        let view_height = (size[1] - box_height - status_height - m.em(1.0)).max(0.0);
        let row = [left, view_height, column, m.em(1.8)];
        self.draw_above_box((&mut paint, fonts), m, (origin, size), row, (status, now));

        // The conversation: the turns in view are laid out, and drawn.
        self.view_height = view_height;
        self.scroll_past_edge();
        self.arrange(fonts, m, column);
        let shown = in_view(&self.cache.bounds, self.scroll, self.scroll + view_height);
        self.label_folds(fonts, m, self.near());
        self.label_agents(fonts, m, self.near());
        self.label_dividers(fonts, m, self.near());
        let clip = [origin[0], origin[1], size[0], view_height];
        let mut canvas = Canvas {
            paint: &mut paint,
            fonts,
            clip,
            em,
            over: false,
        };
        let top = [origin[0] + left, origin[1] - self.scroll];
        // The turns may have moved under the mouse, as when the wheel
        // scrolls them or one grows as the agent writes.
        self.hovered_turn = self.mouse.and_then(|at| self.turn_to_copy_at(at));
        self.draw_turns(&mut canvas, m, shown, top, origin);
        self.pick_past_edge();
        self.note_live();
        self.note_edge();
        self.column_right = left + column;
        self.cursor_onto_text();
        self.cursor_into_view();
        self.cursor_on_arrival();
        let label = self.copy_label(canvas.fonts, m);
        self.draw_reading(
            &mut canvas,
            m,
            origin,
            (caret && self.menu.is_none(), label),
        );
        if self.node.turns.is_empty() && matches!(self.node.state, ChatState::Ready) {
            self.draw_empty(
                &mut canvas,
                m,
                [origin[0] + left, origin[1]],
                [column, view_height],
            );
        }
        // The menu goes over everything, the box included, and the
        // meter's detail too, unless the menu's open.
        if let Some(anchor) = self.menu_anchor.take() {
            self.draw_menu(&mut paint, fonts, m, place, anchor);
        } else if let Some(anchor) = self.detail_anchor.take() {
            self.draw_detail(&mut paint, fonts, m, place, anchor);
        }
        paint
    }

    /// Draws `turns`, with the column's left and the conversation's top
    /// at `origin`, and notes what of them takes a click, and their text,
    /// from the pane's top left, `pane`.
    fn draw_turns(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        turns: std::ops::Range<usize>,
        origin: [f32; 2],
        pane: [f32; 2],
    ) {
        // The button under each answer says "Copy", or "Copied" once it
        // has.
        let copy = COPY.map(|label| self.note(canvas.fonts, m, label, Tone::Muted, f32::INFINITY));
        let (mut hits, mut texts) = (Vec::new(), Vec::new());
        let mut drawn = Drawn {
            pane,
            view: canvas.clip,
            hits: &mut hits,
            texts: &mut texts,
            copy: &copy,
        };
        for index in turns {
            let at = [origin[0], origin[1] + self.cache.bounds[index][0]];
            self.draw_turn(canvas, m, index, at, &mut drawn);
        }
        for laid in copy {
            self.cache
                .notes
                .insert((laid.source.clone(), Tone::Muted), laid);
        }
        self.hits.extend(hits);
        self.shown = texts;
    }

    /// The agent, as the chat names it: "Codex", or "the agent" for a
    /// chat from before chats said which.
    pub(super) fn agent(&self) -> &str {
        if self.node.agent.is_empty() {
            "the agent"
        } else {
            &self.node.agent
        }
    }

    /// Draws what's said just above the box, in the pane at `origin` of
    /// `size`: `status`, and under it, what the agent is doing at `now`,
    /// if it's working. `row` is where the first goes, from the pane's top
    /// left: the column's left, the top, the column's width and a row's
    /// height.
    fn draw_above_box(
        &mut self,
        (paint, fonts): (&mut TextPaint, &mut Fonts),
        m: Measures,
        (origin, size): ([f32; 2], [f32; 2]),
        row: [f32; 4],
        (status, now): (Option<(String, Tone)>, u64),
    ) {
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [origin[0], origin[1], size[0], size[1]],
            em: m.em,
            over: false,
        };
        let mut row = [origin[0] + row[0], origin[1] + row[1], row[2], row[3]];
        if let Some(status) = status {
            self.draw_status(&mut canvas, m, row, status);
            row[1] += row[3];
        }
        self.draw_working(&mut canvas, m, row, now);
    }

    /// Draws `status`, what's said just above the box, in the middle of
    /// `row`: the column's left, top, width and height.
    fn draw_status(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        row: [f32; 4],
        (text, tone): (String, Tone),
    ) {
        let [left, top, column, height] = row;
        let laid = self.note(canvas.fonts, m, &text, tone, column);
        let top = top + (height - laid.height) / 2.0;
        let x = left + (column - laid.width) / 2.0;
        for block in &laid.blocks {
            canvas.text(&block.layout, [x, top + block.top]);
        }
        self.cache.notes.insert((text, tone), laid);
    }

    /// What's said just above the box, if anything, in place of what the
    /// agent is doing: a notice, such as that the agent is being
    /// installed, or the agent failing to start.
    fn status(&self) -> Option<(String, Tone)> {
        match (&self.node.state, &self.node.notice) {
            (ChatState::Failed(why), _) => Some((
                format!("{} couldn't start: {why}", capitalised(self.agent())),
                Tone::Warning,
            )),
            (_, Some(notice)) => Some((notice.clone(), Tone::Muted)),
            _ => None,
        }
    }

    /// Works out where each turn goes, from the conversation's top, laying
    /// out the turns in view, or near it, that need it, and scrolls: to
    /// the end, if following it, or else so what's at the top of the view
    /// stays where it is as the turns above it find their heights. Turns
    /// far from the view are let go of. What it costs depends on what's
    /// near the view, not on how long the chat is.
    fn arrange(&mut self, fonts: &mut Fonts, m: Measures, column: f32) {
        // Out while they're laid out, which changes the cache, and back
        // after: not copied, as a long chat is a lot to copy every frame.
        let turns = std::mem::take(&mut self.node.turns);
        let had = self.cache.turns.len();
        self.cache.turns.resize_with(turns.len(), TurnSlot::default);
        self.cache.bounded = self.cache.bounded.min(turns.len());
        // New turns, guessed at until they're laid out.
        for (index, turn) in turns.iter().enumerate().skip(had) {
            let open = self.shape(index, turn).open;
            let divided = divider::divided(&turns, index).is_some();
            self.set_height(index, guess(m, column, turn, open, divided));
        }
        loop {
            self.update_bounds(m);
            let content = (self.cache.bounds.last()).map_or(0.0, |bound| bound[1]) + m.em(1.5);
            self.max_scroll = (content - self.view_height).max(0.0);
            if self.follow {
                self.scroll = self.max_scroll;
            }
            self.reveal_cursor();
            self.scroll = self.scroll.clamp(0.0, self.max_scroll);
            let stale: Vec<usize> = (self.near())
                .filter(|&index| !self.cache.turns[index].current())
                .collect();
            if stale.is_empty() {
                break;
            }
            // What's at the top of the view stays there.
            let anchor = (!self.follow).then(|| {
                let index = in_view(&self.cache.bounds, self.scroll, self.scroll).start;
                let top = (self.cache.bounds.get(index)).map_or(0.0, |bound| bound[0]);
                (index, self.scroll - top)
            });
            for index in stale {
                self.lay_out_turn(fonts, m, column, index, &turns);
            }
            self.update_bounds(m);
            if let Some((index, offset)) = anchor
                && let Some(bound) = self.cache.bounds.get(index)
            {
                self.scroll = bound[0] + offset;
            }
        }
        self.let_go_far();
        self.reveal = false;
        self.node.turns = turns;
    }

    /// Lets go of the layouts of turns far from the view, a few views'
    /// heights away, keeping how tall they are: a long chat read from end
    /// to end keeps only what's near where it's read.
    fn let_go_far(&mut self) {
        let keep = in_view(
            &self.cache.bounds,
            self.scroll - 4.0 * self.view_height,
            self.scroll + 5.0 * self.view_height,
        );
        let cache = &mut self.cache;
        cache.laid.retain(|&index| {
            let near = keep.contains(&index);
            if !near && let Some(slot) = cache.turns.get_mut(index) {
                slot.laid = None;
                slot.header = None;
                slot.agents = None;
                slot.divider = None;
            }
            near
        });
        // Nor how far their tables were scrolled sideways.
        (self.table_scrolls).retain(|&(turn, _, _), _| keep.contains(&turn));
    }

    /// The turns near enough the view to be laid out: in it, and within a
    /// view's height above it or below, so scrolling a little, or moving
    /// the cursor, finds them ready.
    fn near(&self) -> std::ops::Range<usize> {
        in_view(
            &self.cache.bounds,
            self.scroll - self.view_height,
            self.scroll + 2.0 * self.view_height,
        )
    }

    /// Turn `index` is `height` tall: where the turns after it go is
    /// worked out again when next it's needed.
    fn set_height(&mut self, index: usize, height: f32) {
        if let Some(slot) = self.cache.turns.get_mut(index) {
            slot.height = Some(height);
            self.cache.bounded = self.cache.bounded.min(index);
        }
    }

    /// Works out each turn's top and bottom, from the conversation's top,
    /// from the first whose height changed on.
    fn update_bounds(&mut self, m: Measures) {
        let cache = &mut self.cache;
        let count = cache.turns.len();
        cache.bounds.resize(count, [0.0; 2]);
        let from = cache.bounded.min(count);
        let mut y = match from.checked_sub(1) {
            Some(before) => cache.bounds[before][1] + m.em(2.0),
            None => m.em(1.5),
        };
        for index in from..count {
            let top = y;
            let bottom = top + cache.turns[index].height.unwrap_or(0.0);
            cache.bounds[index] = [top, bottom];
            y = bottom + m.em(2.0);
        }
        cache.bounded = count;
    }

    /// Lays out turn `index` of `turns`, keeping what of it was laid out
    /// already and hasn't changed, as the words of an answer before more
    /// came.
    #[allow(
        clippy::too_many_lines,
        reason = "each piece of a turn in display order"
    )]
    fn lay_out_turn(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        column: f32,
        index: usize,
        turns: &[Turn],
    ) {
        let turn = &turns[index];
        let shape = self.shape(index, turn);
        let old = self.cache.turns[index].laid.take();
        let (mut old_pieces, mut old_cards) = old
            .map(|laid| (laid.pieces, laid.cards))
            .unwrap_or_default();
        let mut laid = TurnLaid {
            spans: Vec::new(),
            pieces: HashMap::new(),
            cards: HashMap::new(),
            note: None,
            files: None,
            copy: false,
            badge: None,
            height: 0.0,
        };
        let mut y = self.lay_out_head(fonts, (m, column), (index, turns), &mut laid);
        if !turn.previews.is_empty() {
            let side =
                super::images::side(column * 0.78, turn.previews.len(), m.em(12.0), m.em(0.5));
            let height = super::images::height(side, turn.previews.iter().map(Some));
            laid.spans.push(Span::of(Piece::Previews, y, height));
            y += height + m.em(1.0);
        }
        if !turn.prompt.is_empty() {
            let width = (column * 0.78 - m.em(2.0)).max(m.em(4.0));
            let old = old_pieces.remove(&PROMPT);
            let piece = self.reuse(fonts, m, old, &turn.prompt, Look::Plain(Tone::Plain), width);
            let height = piece.height + m.em(1.2);
            laid.spans.push(Span::of(Piece::Prompt, y, height));
            laid.pieces.insert(PROMPT, piece);
            y += height + m.em(1.0);
        }
        if shape.fold {
            let height = fold::height(m);
            laid.spans.push(Span::of(Piece::Fold, y, height));
            y += height;
        }
        for (item_index, item) in turn.items.iter().enumerate() {
            // Folded, only the answer shows.
            if item_index < shape.answer && !shape.open {
                continue;
            }
            let key = ITEMS + item_index;
            let last = laid.spans.last().map(|span| span.piece);
            let follows = last.is_some_and(|piece| piece != Piece::Prompt);
            if let Item::Tool(tool) = item {
                if follows {
                    y += m.em(0.15);
                }
                let card = self.card(fonts, m, old_cards.remove(&key), tool, column);
                let height = card.height;
                laid.spans.push(Span::of(Piece::Card(key), y, height));
                laid.cards.insert(key, card);
                y += height;
                continue;
            }
            let (text, look) = match item {
                Item::Words(words) | Item::Other(words) => (words.as_str(), Look::Words),
                Item::Thought(words) => (words.as_str(), Look::Thought),
                Item::Tool(_) => continue, // Laid out as a card above.
            };
            if text.trim().is_empty() {
                continue;
            }
            let piece = self.reuse(fonts, m, old_pieces.remove(&key), text, look, column);
            let gap = match item {
                _ if matches!(last, Some(Piece::Card(_))) => m.em(1.0),
                _ if matches!(last, Some(Piece::Fold)) => m.em(0.6),
                _ => m.em(0.75),
            };
            if follows {
                y += gap;
            }
            laid.spans.push(Span::of(Piece::Item(key), y, piece.height));
            y += piece.height;
            laid.pieces.insert(key, piece);
        }
        // The chats its agent started, as a card under its work.
        let started = self.subagents_in(index).len();
        if started > 0 {
            if !laid.spans.is_empty() {
                y += m.em(0.6);
            }
            let height = self.agents_height(m, index, started);
            laid.spans.push(Span::of(Piece::Agents, y, height));
            y += height;
        }
        y = self.lay_out_ending(fonts, (m, column), (index, turn), &mut laid, y);
        laid.height = y;
        let slot = &mut self.cache.turns[index];
        if slot.laid.is_none() {
            self.cache.laid.push(index);
        }
        slot.stale = false;
        slot.laid = Some(laid);
        self.set_height(index, y);
    }

    /// Lays out what goes over turn `index` of `turns`, into `laid`: when
    /// it was sent, if long after the turn before, and which chat sent it,
    /// if another agent did. Returns where that ends.
    fn lay_out_head(
        &mut self,
        fonts: &mut Fonts,
        (m, column): (Measures, f32),
        (index, turns): (usize, &[Turn]),
        laid: &mut TurnLaid,
    ) -> f32 {
        let mut y = 0.0;
        if divider::divided(turns, index).is_some() {
            let height = divider::height(m);
            laid.spans.push(Span::of(Piece::Divider, y, height));
            y += height + divider::below(m);
        }
        if let Some(from) = &turns[index].from {
            let text = format!("From chat {} · {} · {}", from.chat, from.agent, from.title);
            // Quiet: it says where the message came from, which is
            // neither focus nor something to act on.
            let look = Look::Plain(Tone::Muted);
            let badge = lay_out(&mut self.layout_cx, fonts, m, &text, look, column * 0.78);
            laid.spans.push(Span::of(Piece::Badge, y, badge.height));
            y += badge.height + m.em(0.3);
            laid.badge = Some(badge);
        }
        y
    }

    /// Lays out what goes under turn `index`, `turn`, into `laid`, from
    /// `y` down: how it ended, if not well, and a line with the button
    /// that copies its answer and what it changed in files. Returns where
    /// that ends.
    fn lay_out_ending(
        &mut self,
        fonts: &mut Fonts,
        (m, column): (Measures, f32),
        (index, turn): (usize, &Turn),
        laid: &mut TurnLaid,
        mut y: f32,
    ) -> f32 {
        if let Some((text, tone)) = turn_note(turn) {
            y += m.em(0.75);
            let note = lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                &text,
                Look::Plain(tone),
                column,
            );
            laid.spans.push(Span::of(Piece::Note, y, note.height));
            y += note.height;
            laid.note = Some(note);
        }
        // Once it's done, a line under its answer, folded or not: the
        // button that copies the answer, and what it changed in files,
        // with the buttons that undo and redo it.
        laid.copy = answer_to_copy(turn).is_some();
        let files = turn.files.filter(|_| turn.end.is_some());
        if laid.copy || files.is_some() {
            y += m.em(0.75);
            // After the button, if it's there, and no wider than what's
            // left of the column.
            let left = if laid.copy {
                self.copy_width(fonts, m) + m.em(1.2)
            } else {
                0.0
            };
            let files = files.map(|files| {
                let wide = (column - left).max(m.em(4.0));
                let mut files = self.lay_out_files(fonts, m, wide, (index, files));
                files.left = left;
                files
            });
            let height = (files.as_ref()).map_or(0.0, |files| files.height);
            let height = height.max(fold::height(m));
            laid.spans.push(Span::of(Piece::Footer, y, height));
            y += height;
            laid.files = files;
        }
        y
    }

    /// The width of the button under an answer, however it's labelled.
    fn copy_width(&mut self, fonts: &mut Fonts, m: Measures) -> f32 {
        let mut widest: f32 = 0.0;
        for label in COPY {
            let laid = self.note(fonts, m, label, Tone::Muted, f32::INFINITY);
            widest = widest.max(laid.width);
            self.cache
                .notes
                .insert((label.to_owned(), Tone::Muted), laid);
        }
        widest
    }

    /// Lays out what turn `turn` changed in files, `files`, and the
    /// buttons beside it.
    fn lay_out_files(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        column: f32,
        (turn, files): (usize, TurnFiles),
    ) -> FilesLaid {
        let (label, buttons) = files_line(turn, files);
        let mut text = |text: &str, tone| {
            lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                text,
                Look::Plain(tone),
                column,
            )
        };
        let label = text(&label, Tone::Muted);
        let buttons: Vec<(Hit, Laid)> = (buttons.into_iter())
            .map(|(hit, name)| (hit, text(name, Tone::Accent)))
            .collect();
        let height = (buttons.iter().map(|(_, laid)| laid.height)).fold(label.height, f32::max);
        FilesLaid {
            label,
            buttons,
            height,
            left: 0.0,
        }
    }

    /// `old`, if it was laid out from `text`, or else `text` laid out
    /// afresh.
    fn reuse(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        old: Option<Laid>,
        text: &str,
        look: Look,
        width: f32,
    ) -> Laid {
        match old {
            // Not if its code was waiting for colours, which may have come.
            Some(laid) if laid.source == text && !laid.waiting => laid,
            old => {
                let colours = matches!(look, Look::Words).then_some(&mut self.colours);
                let mut cells = old.map(Laid::into_cells).unwrap_or_default();
                let (layout_cx, words) = (&mut self.layout_cx, (text, look));
                lay_out_coloured(layout_cx, fonts, m, words, (width, &mut cells), colours)
            }
        }
    }

    /// One of the box's short lines, `tone`, laid out `width` wide, or as
    /// it was for the frame before: given back with
    /// [`keep_label`](Self::keep_label) once drawn.
    fn box_label(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        text: &str,
        tone: Tone,
        width: f32,
    ) -> Laid {
        let key = (text.to_owned(), tone);
        let labels = &mut self.cache.labels;
        match labels.now.remove(&key).or_else(|| labels.last.remove(&key)) {
            Some(laid) => laid,
            None => lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                text,
                Look::Plain(tone),
                width,
            ),
        }
    }

    /// Keeps `laid`, from [`box_label`](Self::box_label), for the next frame.
    fn keep_label(&mut self, laid: Laid) {
        let key = (laid.source.clone(), laid.tone);
        self.cache.labels.now.insert(key, laid);
    }

    /// Text of the chat's own, laid out.
    fn note(&mut self, fonts: &mut Fonts, m: Measures, text: &str, tone: Tone, width: f32) -> Laid {
        match self.cache.notes.remove(&(text.to_owned(), tone)) {
            Some(laid) => laid,
            None => lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                text,
                Look::Plain(tone),
                width,
            ),
        }
    }

    /// Draws what a chat with no turns says (see [`empty_note`]) in the
    /// middle of the view, whose left and top are `origin`, `size` wide
    /// and high.
    fn draw_empty(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        origin: [f32; 2],
        size: [f32; 2],
    ) {
        let [with, start] = empty_note(self.agent(), self.folder.as_deref(), self.reading);
        let lines = [(with, Tone::Plain), (start, Tone::Muted)];
        let laid =
            (lines.each_ref()).map(|(text, tone)| self.note(canvas.fonts, m, text, *tone, size[0]));
        let gap = m.em(0.4);
        let height = laid[0].height + gap + laid[1].height;
        let mut y = origin[1] + (size[1] - height) / 2.0;
        for (laid, (text, tone)) in laid.into_iter().zip(lines) {
            let x = origin[0] + (size[0] - laid.width) / 2.0;
            for block in &laid.blocks {
                canvas.text(&block.layout, [x, y + block.top]);
            }
            y += laid.height + gap;
            self.cache.notes.insert((text, tone), laid);
        }
    }

    /// The turn under `at`, from the pane's top left, if its answer has a
    /// button to copy it: from its top to the next turn's, so the button
    /// stays while the mouse crosses the gap under it.
    pub(super) fn turn_to_copy_at(&self, at: [f32; 2]) -> Option<usize> {
        if at[1] < 0.0 || at[1] >= self.view_height {
            return None;
        }
        let y = at[1] + self.scroll;
        let bounds = &self.cache.bounds;
        let turn = bounds
            .partition_point(|bound| bound[0] <= y)
            .checked_sub(1)?;
        let bottom = bounds.get(turn + 1).map_or(bounds[turn][1], |next| next[0]);
        let copies = (self.node.turns.get(turn)).is_some_and(|turn| answer_to_copy(turn).is_some());
        (y < bottom && copies && self.menu.is_none()).then_some(turn)
    }

    /// Draws turn `index` with the column's left and the turn's top at
    /// `origin`: what of it is in view.
    fn draw_turn(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        index: usize,
        origin: [f32; 2],
        drawn: &mut Drawn<'_>,
    ) {
        let Some(laid) = (self.cache.turns.get(index)).and_then(|slot| slot.laid.as_ref()) else {
            return;
        };
        let column = self.cache.width;
        let [view_top, view_height] = [canvas.clip[1], canvas.clip[3]];
        for span in &laid.spans {
            let top = origin[1] + span.top;
            // Out of view: nothing to draw.
            if origin[1] + span.bottom < view_top || top > view_top + view_height {
                continue;
            }
            match span.piece {
                Piece::Card(piece) => {
                    if let Some(card) = laid.cards.get(&piece) {
                        let key = TextKey {
                            turn: index,
                            piece,
                            part: 0,
                        };
                        cards::draw_card(canvas, m, card, key, [origin[0], top], column, drawn);
                    }
                }
                Piece::Prompt => {
                    let Some(prompt) = laid.pieces.get(&PROMPT) else {
                        continue;
                    };
                    let left = origin[0] + bubble_left(m, prompt, column);
                    canvas.fill(Fill::rounded(
                        [left, top, bubble_width(m, prompt), span.bottom - span.top],
                        Paint::Raised,
                        m.em(1.1),
                    ));
                    let at = [left + m.em(1.0), top + m.em(0.6)];
                    draw_laid(canvas, m, prompt, at, column);
                    drawn.note_laid(prompt, at, (index, PROMPT));
                }
                Piece::Previews => {
                    let previews = &self.node.turns[index].previews;
                    let height = span.bottom - span.top;
                    let side =
                        super::images::side(column * 0.78, previews.len(), m.em(12.0), m.em(0.5));
                    let count = f32::from(u8::try_from(previews.len()).unwrap_or(4));
                    let width = side * count + m.em(0.5) * (count - 1.0);
                    for (image, preview) in previews.iter().enumerate().take(4) {
                        let left = origin[0] + column - width
                            + (side + m.em(0.5)) * f32::from(u8::try_from(image).unwrap_or(0));
                        self.previews
                            .draw(canvas, preview, [left, top, side, height]);
                    }
                }
                Piece::Fold => self.draw_fold(canvas, m, index, [origin[0], top], drawn),
                Piece::Agents => self.draw_agents(canvas, m, index, [origin[0], top], drawn),
                Piece::Item(piece) => {
                    if let Some(item) = laid.pieces.get(&piece) {
                        let scrolled = |table| self.table_scroll(index, piece, table);
                        draw_scrolled(canvas, m, item, [origin[0], top], column, &scrolled);
                        drawn.note_scrolled(item, [origin[0], top], (index, piece), &scrolled);
                    }
                }
                Piece::Note => {
                    if let Some(note) = &laid.note {
                        draw_laid(canvas, m, note, [origin[0], top], column);
                    }
                }
                // On the right, over the message it's about.
                Piece::Badge => {
                    if let Some(badge) = &laid.badge {
                        let left = origin[0] + column - badge.width;
                        draw_laid(canvas, m, badge, [left, top], badge.width);
                    }
                }
                Piece::Divider => self.draw_divider(canvas, m, index, [origin[0], top]),
                Piece::Footer => {
                    // Only while the mouse is over the turn, or it says it
                    // copied: its place is kept, so nothing moves.
                    let copied = self.copied_answer == Some(index);
                    if laid.copy && (copied || self.hovered_turn == Some(index)) {
                        let label = &drawn.copy[usize::from(copied)];
                        draw_laid(canvas, m, label, [origin[0], top], label.width);
                        let rect = [origin[0], top, label.width, span.bottom - span.top];
                        drawn.hit(canvas.clip, Hit::CopyAnswer(index), rect);
                    }
                    // Laid out after the wider label, so it stays put as
                    // the button says "Copied".
                    if let Some(files) = &laid.files {
                        draw_files(canvas, m, files, [origin[0] + files.left, top], drawn);
                    }
                }
            }
        }
    }

    /// Draws the box a message is typed in, or the agent's question in
    /// its place, at the bottom of the pane. Returns its height, with the
    /// space below it.
    fn paint_box(
        &mut self,
        fonts: &mut Fonts,
        paint: &mut TextPaint,
        m: Measures,
        place: Place,
        caret: bool,
    ) -> f32 {
        let pad = m.em(1.1);
        let inner = (place.column - 2.0 * pad).max(m.em(4.0));
        let question = self.lay_out_question(fonts, m, inner);
        let editor_height = if question.is_none() {
            self.size_editor(inner);
            let layout = self.editor.layout(&mut fonts.font_cx, &mut self.layout_cx);
            layout.height().max(m.em(LINE))
        } else {
            0.0
        };
        let content_height = question.as_ref().map_or(
            editor_height.min(m.em(LINE * EDITOR_LINES)).max(m.em(3.6)),
            |question| question.height,
        );
        let attachments = if question.is_none() {
            self.images.len()
        } else {
            0
        };
        let side = super::images::side(inner, attachments, m.em(6.0), m.em(0.5));
        let row_height = super::images::height(
            side,
            self.images
                .iter()
                .take(attachments)
                .map(|image| image.preview.as_ref()),
        );
        let image_height = if attachments == 0 {
            0.0
        } else {
            row_height + m.em(0.7)
        };
        let height = pad + content_height + image_height + m.em(0.6) + m.em(2.2) + m.em(0.7);
        let footer = if self.node.usage.is_some() {
            m.em(2.4)
        } else {
            m.em(0.8)
        };
        let frame = BoxFrame {
            place,
            left: place.origin[0] + place.left,
            top: place.origin[1] + place.size[1] - footer - height,
            height,
            inner,
            pad,
            attachments_height: image_height,
        };
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [
                place.origin[0],
                place.origin[1],
                place.size[0],
                place.size[1],
            ],
            em: m.em,
            over: false,
        };
        let bounds = [frame.left, frame.top, place.column, height];
        let shadow = [bounds[0], bounds[1] + m.em(0.15), bounds[2], bounds[3]];
        canvas.fill(Fill::rounded(shadow, Paint::Shadow, m.em(1.5)));
        canvas
            .fill(Fill::rounded(bounds, Paint::Raised, m.em(1.5)).bordered(1.0, Paint::RaisedEdge));
        match question {
            Some(question) => self.draw_question(&mut canvas, m, &frame, question),
            None => self.draw_editor(
                &mut canvas,
                m,
                &frame,
                editor_height,
                caret && self.menu.is_none(),
            ),
        }
        for index in 0..attachments {
            let x =
                frame.left + pad + (side + m.em(0.5)) * f32::from(u8::try_from(index).unwrap_or(0));
            let y = frame.top + pad;
            let rect = [x, y, side, row_height];
            self.draw_attachment(&mut canvas, m, &frame, index, rect);
        }
        self.draw_send(&mut canvas, m, &frame);
        self.draw_controls(&mut canvas, m, &frame);
        self.draw_meter(
            &mut canvas,
            m,
            &frame,
            (
                frame.left + place.column - pad,
                frame.left + pad,
                frame.top + height + m.em(0.25),
                m.em(1.6),
            ),
        );
        height + footer
    }

    /// One draft thumbnail and its remove button, above the image pass.
    fn draw_attachment(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        index: usize,
        rect: [f32; 4],
    ) {
        let [x, y, side, row_height] = rect;
        canvas.fill(Fill::rounded(rect, Paint::Hover, m.em(0.4)));
        if let Some(preview) = &self.images[index].preview {
            self.previews.draw(canvas, preview, rect);
        } else {
            let laid = self.box_label(canvas.fonts, m, "Image", Tone::Muted, side);
            draw_laid(canvas, m, &laid, [x, y + row_height / 2.0], side);
            self.keep_label(laid);
        }
        let remove = [x + side - m.em(1.5), y, m.em(1.5), m.em(1.5)];
        // The image pass is above ordinary fills and text.
        canvas.over = true;
        canvas.fill(
            Fill::rounded(remove, Paint::Palette, m.em(0.75)).bordered(1.0, Paint::RaisedEdge),
        );
        let laid = self.box_label(canvas.fonts, m, "×", Tone::Plain, remove[2]);
        draw_laid(
            canvas,
            m,
            &laid,
            [
                remove[0] + (remove[2] - laid.width) / 2.0,
                remove[1] + (remove[3] - laid.height) / 2.0,
            ],
            remove[2],
        );
        self.keep_label(laid);
        canvas.over = false;
        self.hits
            .push((Hit::RemoveImage(index), frame.place.in_pane(remove)));
    }

    /// The agent's question, and its answers as buttons in rows, laid out
    /// `inner` wide, if it's asking one.
    fn lay_out_question(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        inner: f32,
    ) -> Option<QuestionLaid> {
        let question = self.node.question.clone()?;
        let title = lay_out(
            &mut self.layout_cx,
            fonts,
            m,
            &format!("**{}**", asked(&question, self.quoted)),
            Look::Words,
            inner,
        );
        let (gap, height) = (m.em(0.5), m.em(2.0));
        let mut rows = height;
        let mut x = 0.0;
        let mut buttons = Vec::new();
        for (index, choice) in question.choices.iter().enumerate() {
            // The first answer that says yes is the one to press.
            let primary = index == 0 && choice.kind == ChoiceKind::AllowOnce;
            let tone = match choice.kind {
                _ if primary => Tone::OnAccent,
                ChoiceKind::AllowOnce | ChoiceKind::AllowAlways => Tone::Plain,
                ChoiceKind::RejectOnce | ChoiceKind::RejectAlways => Tone::Muted,
            };
            let text = format!("{}  {}", index + 1, choice.name);
            let laid = lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                &text,
                Look::Plain(tone),
                inner,
            );
            let width = laid.width + m.em(1.6);
            if x > 0.0 && x + width > inner {
                x = 0.0;
                rows += height + gap;
            }
            buttons.push(Button {
                laid,
                id: choice.id.clone(),
                at: [x, rows - height],
                width,
                primary,
                armed: self.armed() == Some(index),
            });
            x += width + gap;
        }
        let height = title.height + m.em(0.7) + rows;
        Some(QuestionLaid {
            title,
            buttons,
            height,
        })
    }

    fn draw_question(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        question: QuestionLaid,
    ) {
        let origin = frame.text_origin();
        draw_laid(canvas, m, &question.title, origin, frame.inner);
        let top = origin[1] + question.title.height + m.em(0.7);
        for button in question.buttons {
            let rect = [
                origin[0] + button.at[0],
                top + button.at[1],
                button.width,
                m.em(2.0),
            ];
            let fill = if button.primary {
                Fill::rounded(rect, Paint::Accent, m.em(1.0))
            } else {
                Fill::rounded(rect, Paint::Hover, m.em(1.0)).bordered(1.0, Paint::RaisedEdge)
            };
            // Armed, it's ringed in the text's colour, which shows round
            // the accent's too.
            canvas.fill(if button.armed {
                fill.bordered((m.em * 0.12).round().max(2.0), Paint::Ink)
            } else {
                fill
            });
            let text_top = rect[1] + (rect[3] - button.laid.height) / 2.0;
            draw_laid(
                canvas,
                m,
                &button.laid,
                [rect[0] + m.em(0.8), text_top],
                button.width,
            );
            self.hits
                .push((Hit::Choice(button.id), frame.place.in_pane(rect)));
        }
    }

    /// Draws the message being typed, `height` tall laid out, with its
    /// selection and, with `caret`, its cursor.
    fn draw_editor(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        height: f32,
        caret: bool,
    ) {
        let origin = frame.text_origin();
        let visible = height.min(m.em(LINE * EDITOR_LINES));
        // A long message scrolls inside the box, to keep the cursor in
        // view.
        let cursor = (self.editor.cursor_geometry((m.em / 8.0).round().max(1.5)))
            .map(|cursor| rect_at(cursor, [0.0; 2]));
        let offset = cursor.map_or(0.0, |[_, top, _, cursor_height]| {
            (top + cursor_height - visible).clamp(0.0, (height - visible).max(0.0))
        });
        let clip = [origin[0] - 2.0, origin[1], frame.inner + 4.0, visible];
        let mut canvas = Canvas {
            paint: canvas.paint,
            fonts: canvas.fonts,
            clip,
            em: m.em,
            over: false,
        };
        let shown = [origin[0], origin[1] - offset];
        let [left, top, _, _] = frame.place.in_pane([shown[0], shown[1], 0.0, 0.0]);
        self.editor_origin = [left, top];
        self.hits.push((
            Hit::Editor,
            frame
                .place
                .in_pane([origin[0], origin[1], frame.inner, visible]),
        ));
        for (bounds, _) in self.editor.selection_geometry() {
            canvas.fill(Fill::solid(rect_at(bounds, shown), Paint::Picked));
        }
        if self.editor.raw_text().is_empty() {
            let placeholder = format!("Message {}", self.agent());
            let laid = self.note(canvas.fonts, m, &placeholder, Tone::Muted, frame.inner);
            for block in &laid.blocks {
                canvas.text(&block.layout, [shown[0], shown[1] + block.top]);
            }
            self.cache.notes.insert((placeholder, Tone::Muted), laid);
        } else if let Some(layout) = self.editor.try_layout() {
            canvas.text(layout, shown);
        }
        // The cursor goes over the text.
        if let Some([x, y, width, cursor_height]) = cursor {
            let rect = [shown[0] + x, shown[1] + y, width.max(1.5), cursor_height];
            canvas.paint.ime = Some(rect);
            if caret {
                canvas
                    .paint
                    .over
                    .push(Fill::solid(rect, Paint::Accent).clipped(clip));
            }
        }
    }

    /// Draws the button that sends the message, or stops the agent while
    /// it answers.
    fn draw_send(&mut self, canvas: &mut Canvas<'_>, m: Measures, frame: &BoxFrame) {
        let busy = self.stoppable();
        let ready = (!self.editor.raw_text().trim().is_empty() || !self.images.is_empty())
            && self.node.question.is_none();
        let (hit, label) = if busy {
            (Hit::Stop, "■")
        } else {
            (Hit::Send, "↑")
        };
        let lit = busy || ready;
        let size = m.em(2.2);
        let rect = [
            frame.left + frame.place.column - frame.pad - size,
            frame.top + frame.height - m.em(0.7) - size,
            size,
            size,
        ];
        canvas.fill(Fill::rounded(
            rect,
            if lit { Paint::Accent } else { Paint::Chosen },
            size / 2.0,
        ));
        let tone = if lit { Tone::OnAccent } else { Tone::Muted };
        let laid = self.box_label(canvas.fonts, m, label, tone, size);
        let text = [
            rect[0] + (size - laid.width) / 2.0,
            rect[1] + (size - laid.height) / 2.0,
        ];
        draw_laid(canvas, m, &laid, text, size);
        self.keep_label(laid);
        self.hits.push((hit, frame.place.in_pane(rect)));
    }
}

impl ChatView {
    /// Model and permissions at the left, clipboard attachment at the right.
    fn draw_controls(&mut self, canvas: &mut Canvas<'_>, m: Measures, frame: &BoxFrame) {
        let size = m.em(2.2);
        let top = frame.top + frame.height - m.em(0.7) - size;
        let right = frame.left + frame.place.column - frame.pad - 2.0 * size - m.em(0.5);
        let of = |kind| {
            self.node
                .settings
                .iter()
                .find(|setting| setting.kind == kind)
                .cloned()
        };
        let models: Vec<Setting> = of(SettingKind::Model)
            .into_iter()
            .chain(of(SettingKind::Effort))
            .collect();
        let permissions: Vec<Setting> =
            [SettingKind::Mode, SettingKind::Tools, SettingKind::Browser]
                .into_iter()
                .filter_map(of)
                .collect();
        let start = frame.left + frame.pad - m.em(0.6);
        let room = (right - start).max(m.em(2.0));
        let permissions_label = permissions
            .iter()
            .find(|s| s.kind == SettingKind::Mode)
            .map_or_else(|| "Permissions".to_owned(), chosen_name);
        let permission_text = format!("◇  {permissions_label}  ▾");
        let permission = self.box_label(
            canvas.fonts,
            m,
            &permission_text,
            Tone::Muted,
            f32::INFINITY,
        );
        let permission_width = (permission.width + m.em(1.2)).min(room * 0.42);
        self.keep_label(permission);
        let model_room = (room - permission_width - m.em(0.8)).max(m.em(2.0));
        if !models.is_empty() {
            let label = models
                .iter()
                .map(chosen_name)
                .collect::<Vec<_>>()
                .join(" · ");
            let label = format!("{}  {label}  ▾", models::symbol(self.agent()));
            let laid =
                self.short_label(canvas.fonts, m, &label, model_room - m.em(1.2), Tone::Plain);
            let width = (laid.width + m.em(1.2)).min(model_room);
            self.draw_chip(
                canvas,
                m,
                frame,
                [start, top, width, size],
                &laid,
                models.into_iter().map(|s| s.id).collect(),
            );
            self.keep_label(laid);
            let left = start + width + m.em(0.45);
            canvas.fill(Fill::solid(
                [left, top + m.em(0.5), 1.0, size - m.em(1.0)],
                Paint::RaisedEdge,
            ));
            if !permissions.is_empty() {
                let label = self.short_label(
                    canvas.fonts,
                    m,
                    &permission_text,
                    permission_width - m.em(1.2),
                    Tone::Muted,
                );
                self.draw_chip(
                    canvas,
                    m,
                    frame,
                    [left + m.em(0.35), top, permission_width, size],
                    &label,
                    permissions.into_iter().map(|s| s.id).collect(),
                );
                self.keep_label(label);
            }
        } else if !permissions.is_empty() {
            let permission = self.short_label(
                canvas.fonts,
                m,
                &permission_text,
                room - m.em(1.2),
                Tone::Muted,
            );
            self.draw_chip(
                canvas,
                m,
                frame,
                [start, top, room, size],
                &permission,
                permissions.into_iter().map(|s| s.id).collect(),
            );
            self.keep_label(permission);
        }
        self.draw_attach_button(canvas, m, frame, top);
    }

    fn draw_attach_button(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        top: f32,
    ) {
        let size = m.em(2.2);
        let rect = [
            frame.left + frame.place.column - frame.pad - 2.0 * size - m.em(0.35),
            top,
            size,
            size,
        ];
        let attach = self.box_label(canvas.fonts, m, "+", Tone::Muted, size);
        draw_laid(
            canvas,
            m,
            &attach,
            [
                rect[0] + (size - attach.width) / 2.0,
                top + (size - attach.height) / 2.0,
            ],
            size,
        );
        self.keep_label(attach);
        self.hits.push((Hit::Attach, frame.place.in_pane(rect)));
    }

    /// Draws a chip that opens the menu for `settings`, lit while it's
    /// open, which then opens from it.
    fn draw_chip(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
        laid: &Laid,
        settings: Vec<String>,
    ) {
        let open = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.settings == settings);
        // The menu opens above the box, lined up with the chip.
        if open {
            canvas.fill(Fill::rounded(rect, Paint::Hover, m.em(0.5)));
        }
        if open {
            self.menu_anchor = Some([rect[0], frame.top, rect[2], rect[3]]);
        }
        let at = [rect[0] + m.em(0.6), rect[1] + (rect[3] - laid.height) / 2.0];
        draw_laid(canvas, m, laid, at, rect[2]);
        self.hits
            .push((Hit::Chip(settings), frame.place.in_pane(rect)));
    }

    /// Draws one of a menu's choices in `rect`: lit if the keyboard or the
    /// mouse is on it, ticked if it's chosen, and for a model family, with
    /// the button that shows its versions.
    fn draw_choice(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        choice: &ChoiceLaid,
        (lit, column): (bool, f32),
    ) {
        if lit {
            canvas.fill(Fill::rounded(rect, Paint::Chosen, m.em(0.5)));
        }
        let mark = m.em(1.6);
        let mut x = rect[0] + m.em(0.4);
        let top = rect[1]
            + if choice.description.is_some() {
                m.em(0.3)
            } else {
                (rect[3] - choice.name.height) / 2.0
            };
        if choice.row.current {
            let tick = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                "✓",
                Look::Plain(Tone::Accent),
                mark,
            );
            let indent = if choice.row.nested { m.em(NESTED) } else { 0.0 };
            draw_laid(canvas, m, &tick, [x + indent, top], mark);
        }
        x += mark + if choice.row.nested { m.em(NESTED) } else { 0.0 };
        let names = x;
        draw_laid(canvas, m, &choice.name, [x, top], rect[2]);
        x += choice.name.width;
        if let Some(version) = &choice.version {
            draw_laid(canvas, m, version, [x + m.em(0.35), top], rect[2]);
        }
        if let Some(description) = &choice.description {
            draw_laid(
                canvas,
                m,
                description,
                [names, top + choice.name.height + m.em(0.1)],
                rect[2],
            );
        }
        if let Some(recommended) = &choice.recommended {
            x = names + column.max(choice.named(m)) + m.em(0.8);
            draw_laid(canvas, m, recommended, [x, top], rect[2]);
        }
        if let Some((family, open)) = &choice.row.family {
            let size = m.em(1.5);
            let button = [
                rect[0] + rect[2] - m.em(0.25) - size,
                rect[1] + (rect[3] - size) / 2.0,
                size,
                size,
            ];
            canvas.fill(Fill::rounded(
                button,
                if *open { Paint::Picked } else { Paint::Clear },
                m.em(0.4),
            ));
            let chevron = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                if *open { "▾" } else { "▸" },
                Look::Plain(Tone::Muted),
                size,
            );
            let at = [
                button[0] + (size - chevron.width) / 2.0,
                button[1] + (size - chevron.height) / 2.0,
            ];
            draw_laid(canvas, m, &chevron, at, size);
            self.hits
                .push((Hit::Expand(family.clone()), place.in_pane(button)));
        }
    }

    /// Draws a slider in `rect`: its steps along a line, filled up to the
    /// one chosen, and beside it the button that leaves it to the agent.
    #[allow(clippy::cast_precision_loss, reason = "a handful of steps")]
    fn draw_slider(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rect: [f32; 4],
        slider: &SliderRow,
        reset: Option<&Laid>,
    ) {
        let reset_width = reset.map_or(0.0, |laid| laid.width + m.em(1.4));
        let left = rect[0] + m.em(1.0);
        let right = rect[0] + rect[2]
            - m.em(1.0)
            - if reset.is_some() {
                reset_width + m.em(0.9)
            } else {
                0.0
            };
        let middle = (rect[1] + rect[3] / 2.0).round();
        let steps = slider.steps.len();
        let gap = (right - left) / (steps - 1) as f32;
        let x_of = |step: usize| left + gap * step as f32;
        let line = m.em(0.2).max(2.0);
        let track = |to: f32| [left, middle - line / 2.0, to - left, line];
        canvas.fill(Fill::rounded(track(right), Paint::PaletteEdge, line / 2.0));
        if let Some(at) = slider.at {
            canvas.fill(Fill::rounded(track(x_of(at)), Paint::Accent, line / 2.0));
        }
        for (step, (value, _)) in slider.steps.iter().enumerate() {
            let dot = m.em(0.45);
            let filled = slider.at.is_some_and(|at| step <= at);
            canvas.fill(Fill::rounded(
                [x_of(step) - dot / 2.0, middle - dot / 2.0, dot, dot],
                if filled {
                    Paint::Accent
                } else {
                    Paint::PaletteEdge
                },
                dot / 2.0,
            ));
            let share = [x_of(step) - gap / 2.0, rect[1], gap, rect[3]];
            self.hits.push((
                Hit::Step {
                    setting: slider.setting.clone(),
                    value: value.clone(),
                },
                place.in_pane(share),
            ));
        }
        if let Some(at) = slider.at {
            let knob = m.em(1.1);
            canvas.fill(
                Fill::rounded(
                    [x_of(at) - knob / 2.0, middle - knob / 2.0, knob, knob],
                    Paint::Accent,
                    knob / 2.0,
                )
                .bordered(2.0, Paint::Palette),
            );
        }
        if let (Some((value, _, on)), Some(laid)) = (&slider.reset, reset) {
            let height = m.em(1.6);
            let pill = [
                rect[0] + rect[2] - m.em(0.4) - reset_width,
                middle - height / 2.0,
                reset_width,
                height,
            ];
            canvas.fill(if *on {
                Fill::rounded(pill, Paint::Accent, height / 2.0)
            } else {
                Fill::rounded(pill, Paint::Clear, height / 2.0).bordered(1.0, Paint::PaletteEdge)
            });
            let at = [pill[0] + m.em(0.7), middle - laid.height / 2.0];
            draw_laid(canvas, m, laid, at, pill[2]);
            self.hits.push((
                Hit::Reset {
                    setting: slider.setting.clone(),
                    value: value.clone(),
                },
                place.in_pane(pill),
            ));
        }
    }

    /// The rows of `menu`, laid out at most `wide`.
    fn menu_rows(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        menu: &Menu,
        wide: f32,
    ) -> Vec<MenuRow> {
        let rows = picker::rows(&self.offer(), menu);
        let mut text = |text: &str, tone| {
            lay_out(&mut self.layout_cx, fonts, m, text, Look::Plain(tone), wide)
        };
        (rows.into_iter())
            .map(|row| match row {
                Row::Heading { name, aside } => MenuRow::Heading {
                    name: text(&name, Tone::Muted),
                    aside: aside.map(|aside| text(&aside, Tone::Plain)),
                },
                Row::Choice(row) => MenuRow::Choice(Box::new(ChoiceLaid {
                    name: text(&row.name, Tone::Plain),
                    version: (row.version.as_deref()).map(|version| text(version, Tone::Muted)),
                    description: (row.description.as_deref())
                        .map(|description| text(description, Tone::Muted)),
                    recommended: row.recommended.then(|| text("recommended", Tone::Accent)),
                    row,
                })),
                Row::Slider(row) => MenuRow::Slider {
                    reset: (row.reset.as_ref()).map(|(_, name, on)| {
                        text(name, if *on { Tone::OnAccent } else { Tone::Muted })
                    }),
                    row,
                },
            })
            .collect()
    }

    /// Draws the settings menu, opening upward from `anchor`: the left of
    /// the chip that opened it, and the top of the box.
    fn draw_menu(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        anchor: [f32; 4],
    ) {
        let Some(menu) = self.menu.clone() else {
            return;
        };
        if picker::offers_models(&self.offer(), &menu) {
            self.draw_models(paint, fonts, m, place, anchor);
            return;
        }
        let menu_width = m.em(28.0).min(place.size[0] - m.em(1.0));
        let rows = self.menu_rows(fonts, m, &menu, (menu_width - m.em(3.4)).max(m.em(2.0)));
        let pad = m.em(0.4);
        // The width of the widest name a description follows.
        let column = (rows.iter())
            .filter_map(|row| match row {
                MenuRow::Choice(choice) if !choice.row.nested => Some(choice.named(m)),
                _ => None,
            })
            .fold(0.0, f32::max);
        let width = (rows.iter())
            .map(|row| row.width(m, column))
            .fold(0.0, f32::max)
            + m.em(1.4);
        let height = 2.0 * pad + rows.iter().map(|row| row.height(m)).sum::<f32>();
        let width = width.min(menu_width);
        let left = anchor[0].clamp(
            place.origin[0] + m.em(0.5),
            (place.origin[0] + place.size[0] - width - m.em(0.5)).max(place.origin[0]),
        );
        let bottom = (anchor[1] - m.em(0.4))
            .max(place.origin[1] + m.em(17.0))
            .min(place.origin[1] + place.size[1] - m.em(0.5));
        let top = (bottom - height).max(place.origin[1] + m.em(0.5));
        let bounds = [left, top, width, bottom - top];
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [
                place.origin[0],
                place.origin[1],
                place.size[0],
                place.size[1],
            ],
            em: m.em,
            over: true,
        };
        canvas.fill(Fill::rounded(
            [bounds[0], bounds[1] + m.em(0.2), bounds[2], bounds[3]],
            Paint::Shadow,
            m.em(0.8),
        ));
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(0.8)).bordered(1.0, Paint::PaletteEdge),
        );
        canvas.clip = bounds;
        self.hits.push((Hit::MenuSurface, place.in_pane(bounds)));
        let available = (bounds[3] - 2.0 * pad).max(0.0);
        let mut offset = menu.offset.min(rows.len().saturating_sub(1));
        if menu.reveal
            && let Some(at) = rows
                .iter()
                .position(|row| row.spot() == menu.highlighted && menu.highlighted.is_some())
        {
            if at < offset {
                offset = at;
            }
            while offset < at
                && rows[offset..=at].iter().map(|r| r.height(m)).sum::<f32>() > available
            {
                offset += 1;
            }
        }
        if let Some(menu) = &mut self.menu {
            menu.offset = offset;
            menu.reveal = false;
        }
        self.draw_menu_rows(
            &mut canvas,
            m,
            place,
            rows.into_iter().skip(offset),
            menu.highlighted.as_ref(),
            column,
        );
    }

    fn draw_menu_rows(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        place: Place,
        rows: impl Iterator<Item = MenuRow>,
        highlighted: Option<&Spot>,
        column: f32,
    ) {
        let bounds = canvas.clip;
        let [left, top, width, height] = bounds;
        let bottom = top + height;
        let pad = m.em(0.4);
        let mut y = top + pad;
        for menu_row in rows {
            if y >= bottom - pad {
                break;
            }
            let hits_start = self.hits.len();
            let height = menu_row.height(m);
            let rect = [left + pad, y, width - 2.0 * pad, height];
            match menu_row {
                MenuRow::Heading { name, aside } => {
                    let middle = |laid: &Laid| y + (height - laid.height) / 2.0;
                    draw_laid(canvas, m, &name, [left + m.em(0.8), middle(&name)], width);
                    if let Some(aside) = aside {
                        let x = left + width - m.em(0.8) - aside.width;
                        draw_laid(canvas, m, &aside, [x, middle(&aside)], width);
                    }
                }
                MenuRow::Choice(choice) => {
                    let spot = Spot::Choice {
                        setting: choice.row.setting.clone(),
                        value: choice.row.value.clone(),
                    };
                    let lit = highlighted == Some(&spot);
                    self.draw_choice(canvas, m, place, rect, &choice, (lit, column));
                    // Under the versions button, which goes over it.
                    let pick = Hit::Pick {
                        setting: choice.row.setting,
                        value: choice.row.value,
                    };
                    let at = self
                        .hits
                        .len()
                        .saturating_sub(usize::from(choice.row.family.is_some()));
                    self.hits.insert(at, (pick, place.in_pane(rect)));
                }
                MenuRow::Slider { row, reset } => {
                    if highlighted == Some(&Spot::Slider(row.setting.clone())) {
                        canvas.fill(Fill::rounded(rect, Paint::Chosen, m.em(0.5)));
                    }
                    self.draw_slider(canvas, m, place, rect, &row, reset.as_ref());
                }
            }
            for (_, hit) in &mut self.hits[hits_start..] {
                *hit = intersection(*hit, place.in_pane(bounds));
            }
            y += height;
        }
    }
}

/// A setting's choice as a chip says it: the name of the choice it has.
fn chosen_name(setting: &Setting) -> String {
    setting
        .current()
        .map_or_else(|| setting.value.clone(), |choice| choice.name.clone())
}

/// One of a menu's choices, laid out.
struct ChoiceLaid {
    row: ChoiceRow,
    name: Laid,
    version: Option<Laid>,
    description: Option<Laid>,
    recommended: Option<Laid>,
}

impl ChoiceLaid {
    /// How wide its name is, with its version.
    fn named(&self, m: Measures) -> f32 {
        self.name.width + (self.version.as_ref()).map_or(0.0, |version| m.em(0.35) + version.width)
    }
}

/// One row of the settings menu, laid out.
enum MenuRow {
    Heading { name: Laid, aside: Option<Laid> },
    Choice(Box<ChoiceLaid>),
    Slider { row: SliderRow, reset: Option<Laid> },
}

/// How far a model family's versions are indented, in ems.
const NESTED: f32 = 1.2;

impl MenuRow {
    fn height(&self, m: Measures) -> f32 {
        if let Self::Choice(choice) = self
            && let Some(description) = &choice.description
        {
            return choice.name.height + description.height + m.em(0.7);
        }
        m.em(match self {
            Self::Heading { .. } => 1.6,
            Self::Choice(_) => 1.9,
            Self::Slider { .. } => 2.4,
        })
    }

    fn spot(&self) -> Option<Spot> {
        match self {
            Self::Choice(choice) => Some(Spot::Choice {
                setting: choice.row.setting.clone(),
                value: choice.row.value.clone(),
            }),
            Self::Slider { row, .. } => Some(Spot::Slider(row.setting.clone())),
            Self::Heading { .. } => None,
        }
    }

    /// The width it needs, with the menu's padding left out, and its
    /// description after `column`, if it's not nested.
    fn width(&self, m: Measures, column: f32) -> f32 {
        let width = |laid: Option<&Laid>, gap: f32| laid.map_or(0.0, |laid| gap + laid.width);
        match self {
            Self::Heading { name, aside } => name.width + width(aside.as_ref(), m.em(2.0)),
            Self::Choice(choice) => {
                m.em(1.6)
                    + if choice.row.nested { m.em(NESTED) } else { 0.0 }
                    + (if choice.row.nested {
                        choice.named(m)
                    } else {
                        column
                    })
                    .max(choice.description.as_ref().map_or(0.0, |d| d.width))
                    + width(choice.recommended.as_ref(), m.em(0.8))
                    + if choice.row.family.is_some() {
                        m.em(2.2)
                    } else {
                        0.0
                    }
            }
            Self::Slider { reset, .. } => {
                m.em(14.0) + reset.as_ref().map_or(0.0, |laid| laid.width + m.em(2.3))
            }
        }
    }
}

/// What a chat with no turns says, in two lines: who it's with and
/// where, as "Claude Code in ~/project", and how to start, which in normal
/// mode, where typing does nothing, is the key that does.
pub(super) fn empty_note(agent: &str, folder: Option<&str>, reading: bool) -> [String; 2] {
    let with = match folder {
        Some(folder) if !folder.is_empty() => format!("{agent} in {folder}"),
        _ => agent.to_owned(),
    };
    let start = if reading {
        "Press i to write a message · Shift+Enter for a new line"
    } else {
        "Send a message to start."
    };
    [with, start.to_owned()]
}

/// Where a chat is drawn: its pane, and the column of turns in it.
#[derive(Clone, Copy)]
struct Place {
    /// The pane's top left, and its size.
    origin: [f32; 2],
    size: [f32; 2],
    /// The column's left, from the pane's, and its width.
    left: f32,
    column: f32,
}

impl Place {
    /// `rect`, from the pane's top left rather than the window's.
    fn in_pane(self, [x, y, width, height]: [f32; 4]) -> [f32; 4] {
        [x - self.origin[0], y - self.origin[1], width, height]
    }
}

/// Where the box goes.
struct BoxFrame {
    place: Place,
    left: f32,
    top: f32,
    height: f32,
    /// How wide its text is.
    inner: f32,
    pad: f32,
    attachments_height: f32,
}

impl BoxFrame {
    /// Where its text starts.
    fn text_origin(&self) -> [f32; 2] {
        [
            self.left + self.pad,
            self.top + self.pad + self.attachments_height,
        ]
    }
}

/// The agent's question, laid out.
struct QuestionLaid {
    title: Laid,
    buttons: Vec<Button>,
    /// The title's height and the buttons' together.
    height: f32,
}

/// One of the answers to the agent's question, as a button.
struct Button {
    laid: Laid,
    /// The answer's ID.
    id: String,
    /// Where it goes, from below the title.
    at: [f32; 2],
    width: f32,
    /// It's the one to press, in the accent colour.
    primary: bool,
    /// A number pressed in normal mode picked it, to give with the number
    /// again or Enter: it's ringed.
    armed: bool,
}

/// `bounds`, as parley gives them, as a rectangle from `origin`: left,
/// top, width and height.
#[allow(
    clippy::cast_possible_truncation,
    reason = "pixels in a pane, far inside f32's range"
)]
fn rect_at(bounds: parley::BoundingBox, origin: [f32; 2]) -> [f32; 4] {
    [
        origin[0] + bounds.x0 as f32,
        origin[1] + bounds.y0 as f32,
        (bounds.x1 - bounds.x0) as f32,
        (bounds.y1 - bounds.y0) as f32,
    ]
}

/// One piece of a turn and where it goes, from the turn's top.
struct Span {
    piece: Piece,
    top: f32,
    bottom: f32,
}

/// What a piece of a turn is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Piece {
    /// Its message, as a bubble.
    Prompt,
    /// The pasted image thumbnails, above the message.
    Previews,
    /// The line its work folds under.
    Fold,
    /// What the agent did, by piece.
    Item(usize),
    /// A tool it used, as a card, by piece.
    Card(usize),
    /// How it ended, if not well.
    Note,
    /// The line under it, once it's done: the button that copies its
    /// answer, and what it changed in files, with what undoes or redoes
    /// it.
    Footer,
    /// Which chat sent its message, if another agent did.
    Badge,
    /// The card for the chats its agent started.
    Agents,
    /// The time it was sent, over it, as it was long after the turn
    /// before.
    Divider,
}

impl Span {
    /// `piece`, from `top` down, `height` tall.
    fn of(piece: Piece, top: f32, height: f32) -> Self {
        Self {
            piece,
            top,
            bottom: top + height,
        }
    }
}

impl TurnSlot {
    /// Whether it's laid out as it is now.
    pub(super) fn current(&self) -> bool {
        self.laid.is_some() && !self.stale
    }

    /// Whether it's been laid out, as it is now or as it was.
    pub(super) fn laid_out(&self) -> bool {
        self.laid.is_some()
    }
}

/// What a frame notes of what it drew, and where, from the pane's top
/// left: what takes a click, and the text, for the mouse to select.
struct Drawn<'a> {
    /// The pane's top left.
    pane: [f32; 2],
    /// What's in view, from the window's top left.
    view: [f32; 4],
    hits: &'a mut Vec<(Hit, [f32; 4])>,
    texts: &'a mut Vec<Shown>,
    /// The button under each answer's label, laid out: "Copy", and
    /// "Copied".
    copy: &'a [Laid; 2],
}

impl Drawn<'_> {
    /// Notes that `hit` takes a click in `rect`, from the window's top
    /// left, where it's inside `clip`, the view.
    fn hit(&mut self, clip: [f32; 4], hit: Hit, rect: [f32; 4]) {
        let [left, top, wide, high] = intersection(clip, rect);
        if wide > 0.0 && high > 0.0 {
            let pane = self.pane;
            self.hits
                .push((hit, [left - pane[0], top - pane[1], wide, high]));
        }
    }

    /// Notes `laid`'s blocks, drawn with the column's left at
    /// `origin[0]` and its top at `origin[1]`, as `piece` of `turn`.
    fn note_laid(&mut self, laid: &Laid, origin: [f32; 2], (turn, piece): (usize, usize)) {
        self.note_scrolled(laid, origin, (turn, piece), &|_| 0.0);
    }

    /// Notes `laid`'s blocks as [`note_laid`](Self::note_laid) does, with
    /// its tables scrolled sideways as far as `scrolled` says.
    fn note_scrolled(
        &mut self,
        laid: &Laid,
        origin: [f32; 2],
        (turn, piece): (usize, usize),
        scrolled: &dyn Fn(usize) -> f32,
    ) {
        for (part, block) in laid.blocks.iter().enumerate() {
            let scroll = laid.table_of(part).map_or(0.0, scrolled);
            let at = [origin[0] + block.left - scroll, origin[1] + block.top];
            // Above or below the view, as most of a long answer is.
            let (top, bottom) = (at[1], at[1] + block.layout.height());
            if bottom < self.view[1] || top > self.view[1] + self.view[3] {
                continue;
            }
            self.note_text(
                TextKey { turn, piece, part },
                at,
                &block.layout,
                block.kind.code(),
            );
        }
    }

    /// Notes text `key`, laid out as `layout`, drawn with its top left at
    /// `at`; `code` if it's a block of code.
    fn note_text(&mut self, key: TextKey, at: [f32; 2], layout: &Layout<Brush>, code: bool) {
        self.texts.push(Shown {
            key,
            at: [at[0] - self.pane[0], at[1] - self.pane[1]],
            size: [layout.width(), layout.height()],
            code,
        });
    }
}

/// A message's bubble's width, for `laid`, its text.
fn bubble_width(m: Measures, laid: &Laid) -> f32 {
    laid.width + 2.0 * m.em(1.0)
}

/// Where a message's bubble starts, from the column's left: it's on the
/// right.
fn bubble_left(m: Measures, laid: &Laid, column: f32) -> f32 {
    column - bubble_width(m, laid)
}

/// Where a message's text starts, from the column's left and the turn's
/// top, `laid` being it laid out.
fn prompt_origin(m: Measures, laid: &Laid, column: f32) -> [f32; 2] {
    [bubble_left(m, laid, column) + m.em(1.0), m.em(0.6)]
}

/// The turns whose `bounds` reach between `from` and `to`, from the
/// conversation's top.
fn in_view(bounds: &[[f32; 2]], from: f32, to: f32) -> std::ops::Range<usize> {
    let start = bounds.partition_point(|bound| bound[1] < from);
    let end = bounds.partition_point(|bound| bound[0] <= to);
    start..end.max(start)
}

/// How tall `turn` looks to be, `column` wide, before it's laid out:
/// near enough to scroll by, from how much text it has. `divided` if the
/// time it was sent is over it.
#[allow(
    clippy::cast_precision_loss,
    reason = "a guess, from counts far inside f32's range"
)]
fn guess(m: Measures, column: f32, turn: &Turn, open: bool, divided: bool) -> f32 {
    let line = m.em * LINE;
    // About two characters to an em.
    let lines = |text: &str, wide: f32| -> f32 {
        let per_line = (wide / (m.em * 0.5)).max(1.0);
        (text.lines())
            .map(|line| (line.chars().count() as f32 / per_line).ceil().max(1.0))
            .sum()
    };
    let mut height = 0.0;
    if divided {
        height += divider::height(m) + divider::below(m);
    }
    if turn.from.is_some() {
        height += line + m.em(0.3);
    }
    if !turn.prompt.is_empty() {
        height += lines(&turn.prompt, column * 0.78) * line + m.em(2.2);
    }
    if !turn.previews.is_empty() {
        let side = super::images::side(column * 0.78, turn.previews.len(), m.em(12.0), m.em(0.5));
        height += super::images::height(side, turn.previews.iter().map(Some)) + m.em(1.0);
    }
    let answer = fold::answer_start(turn);
    if answer > 0 || turn.end.is_none() {
        height += fold::height(m) + m.em(0.6);
    }
    for (index, item) in turn.items.iter().enumerate() {
        if index < answer && !open {
            continue;
        }
        height += match item {
            Item::Words(text) | Item::Thought(text) | Item::Other(text) => {
                lines(text, column) * line + m.em(0.75)
            }
            Item::Tool(_) => line + m.em(0.85),
        };
    }
    // The line with the button that copies its answer, and what it
    // changed in files.
    if (turn.files.is_some() && turn.end.is_some()) || answer_to_copy(turn).is_some() {
        height += line + m.em(0.75);
    }
    if let Some(switch) = &turn.switch {
        height += (1 + switch.notes.len()) as f32 * 2.0 * line;
    }
    height
}

/// What's said under a turn: how it ended, if not well; or for one where
/// the chat moved to another agent, a line across saying so, and what
/// didn't carry over.
fn turn_note(turn: &Turn) -> Option<(String, Tone)> {
    if let Some(switch) = &turn.switch {
        let mut note = format!("── {} ──", switch.label());
        for line in &switch.notes {
            note.push_str("\n\n");
            note.push_str(line);
        }
        return Some((note, Tone::Muted));
    }
    match &turn.end.as_ref()?.outcome {
        Outcome::Done => None,
        Outcome::Stopped => Some(("Stopped.".to_owned(), Tone::Muted)),
        Outcome::Failed(why) => Some((format!("⚠ {}", capitalised(why)), Tone::Warning)),
    }
}

/// Draws what a turn changed in files, `files`, with the column's left
/// and its top at `origin`, and notes where its buttons can be clicked,
/// where they're in view.
fn draw_files(
    canvas: &mut Canvas<'_>,
    m: Measures,
    files: &FilesLaid,
    origin: [f32; 2],
    drawn: &mut Drawn<'_>,
) {
    draw_laid(canvas, m, &files.label, origin, files.label.width);
    let mut x = origin[0] + files.label.width + m.em(0.8);
    for (hit, laid) in &files.buttons {
        draw_laid(canvas, m, laid, [x, origin[1]], laid.width);
        let rect = [x, origin[1], laid.width, laid.height];
        drawn.hit(canvas.clip, hit.clone(), rect);
        x += laid.width + m.em(0.8);
    }
}

/// What the button under each answer says: before it's clicked, and after.
const COPY: [&str; 2] = ["Copy", "Copied"];

/// Where in `turn`'s items its answer is, if it's done and has one to
/// copy: its last, the agent's words.
pub(super) fn answer_to_copy(turn: &Turn) -> Option<usize> {
    turn.end.as_ref()?;
    let last = turn.items.len().checked_sub(1)?;
    match &turn.items[last] {
        Item::Words(words) if !words.trim().is_empty() => Some(last),
        _ => None,
    }
}

/// What's said of what a turn changed in files, as `files` says, and the
/// buttons beside it: "2 files changed", then "Undo".
fn files_line(turn: usize, files: TurnFiles) -> (String, Vec<(Hit, &'static str)>) {
    let count = match files.count {
        1 => "1 file changed".to_owned(),
        count => format!("{count} files changed"),
    };
    let (label, buttons) = match files.state {
        TurnFilesState::Made => (count, vec![(Hit::Undo(turn), "Undo")]),
        TurnFilesState::Undone => (format!("{count}, undone"), vec![(Hit::Redo(turn), "Redo")]),
        TurnFilesState::Partly => (
            format!("{count}, partly undone"),
            vec![(Hit::Undo(turn), "Undo"), (Hit::Redo(turn), "Redo")],
        ),
        TurnFilesState::TooOld => (format!("{count}, too long ago to undo"), Vec::new()),
    };
    (label, buttons)
}

/// A tool call's line: a mark for how it's going, and its title.
fn tool_text(status: ToolStatus, title: &str) -> String {
    let mark = match status {
        ToolStatus::Running => "○",
        ToolStatus::Done => "✓",
        ToolStatus::Failed => "✗",
    };
    format!("{mark}  {title}")
}

/// `question`, as it's shown, in Markdown: the daemon's own, as whether
/// to install the agent, as it is; any other as asking whether to let the
/// agent do what it says. Only the code its title quotes is read as
/// Markdown: the rest is shown as it's written.
fn asked(question: &clackatoo_protocol::Question, quoted: bool) -> String {
    let mut shown = String::new();
    if !question.own {
        shown.push_str("Allow the agent to ");
    }
    // A daemon from before protocol 84 quoted what an agent wrote between
    // single backticks, which the agent's own backticks could end: read
    // as code, its words could pass for clackatoo's. Its titles are shown
    // as they are.
    if !quoted {
        shown.push_str(&escape(&question.title));
    }
    for piece in question.said().into_iter().filter(|_| quoted) {
        match piece {
            Said::Words(words) => shown.push_str(&escape(&words)),
            Said::Code(code) => shown.push_str(&code_span(&code)),
        }
    }
    if !question.own {
        shown.push('?');
    }
    shown
}

/// `text` with its first letter capital.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `text` with what Markdown would read as its own escaped.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if "\\`*_{}[]()#+-.!|<>~".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontOptions;

    #[test]
    fn an_empty_chat_says_who_and_where_and_how_to_start_in_either_mode() {
        let press = "Press i to write a message · Shift+Enter for a new line";
        assert_eq!(
            empty_note("Claude Code", Some("~/project"), true),
            ["Claude Code in ~/project", press]
        );
        assert_eq!(
            empty_note("Claude Code", Some("~/project"), false),
            ["Claude Code in ~/project", "Send a message to start."]
        );
        // A daemon too old to say where: who alone.
        assert_eq!(empty_note("Codex", None, true)[0], "Codex");
        assert_eq!(empty_note("Codex", Some(""), true)[0], "Codex");
        // The folder comes with the daemon's first view of the chat, and
        // both lines are drawn, as the mode says.
        let view = clackatoo_protocol::View {
            title: "Claude Code".into(),
            cwd: Some("~/project".into()),
            root: clackatoo_protocol::Node::Chat(Box::new(clackatoo_protocol::ChatNode {
                agent: "Claude Code".into(),
                state: ChatState::Ready,
                ..clackatoo_protocol::ChatNode::default()
            })),
        };
        let mut chat = ChatView::of_view(&view, view.chat().unwrap());
        let mut fonts = crate::chat::fixtures::fonts();
        for (reading, start) in [(true, press), (false, "Send a message to start.")] {
            chat.reading = reading;
            crate::chat::fixtures::paint(&mut chat, &mut fonts);
            let notes = &chat.cache.notes;
            assert!(notes.contains_key(&("Claude Code in ~/project".to_owned(), Tone::Plain)));
            assert!(
                notes.contains_key(&(start.to_owned(), Tone::Muted)),
                "{reading}"
            );
        }
        // Not once it has turns.
        chat.node.turns = crate::chat::fixtures::talk(&[("one", "two")]).turns;
        chat.cache.notes.clear();
        crate::chat::fixtures::paint(&mut chat, &mut fonts);
        assert!(!(chat.cache.notes.keys()).any(|(text, _)| text.starts_with("Claude Code in")));
    }

    #[test]
    fn the_chats_fonts_are_found_ahead_on_a_thread_that_ends() {
        let mut fonts = crate::chat::fixtures::fonts();
        let thread = warm_fonts(&fonts).expect("a thread");
        thread.join().expect("it finds them without panicking");
        // The window's own collection lays text out as ever.
        let mut layout_cx = parley::LayoutContext::new();
        let m = Measures::of(text_size(&fonts));
        let laid = lay_out(
            &mut layout_cx,
            &mut fonts,
            m,
            "Send ↑ · done ✓",
            Look::Words,
            1000.0,
        );
        assert!(laid.width > 0.0);
    }

    #[test]
    fn words_streaming_in_lay_out_again_only_the_block_that_changed() {
        let mut fonts = crate::chat::fixtures::fonts();
        let mut layout_cx = parley::LayoutContext::new();
        let m = Measures::of(text_size(&fonts));
        let lay = |text: &str, cells: &mut Cells, fonts: &mut Fonts, layout_cx: &mut _| {
            lay_out_coloured(
                layout_cx,
                fonts,
                m,
                (text, Look::Words),
                (600.0, cells),
                None,
            )
        };
        let before = "# Plan\n\nFirst, **this**.\n\n- one\n- two\n\nThen th";
        let laid = lay(before, &mut Cells::new(), &mut fonts, &mut layout_cx);
        assert_eq!(laid.blocks.len(), 5);
        let mut cells = laid.into_cells();
        let after = format!("{before}at, which goes on.");
        let again = lay(&after, &mut cells, &mut fonts, &mut layout_cx);
        // Every block but the last was kept: only the last is left over.
        assert_eq!(cells.len(), 1, "only the paragraph that grew is laid out");
        assert_eq!(again.blocks[4].text, "Then that, which goes on.");
        // Laid out at another width, or as thinking, nothing is kept.
        let mut cells = again.into_cells();
        lay_out_coloured(
            &mut layout_cx,
            &mut fonts,
            m,
            (&after, Look::Thought),
            (600.0, &mut cells),
            None,
        );
        assert_eq!(cells.len(), 5, "its tone is part of what's kept");
        lay(&after, &mut cells, &mut fonts, &mut layout_cx);
        assert_eq!(cells.len(), 0);
        let mut cells = lay(&after, &mut Cells::new(), &mut fonts, &mut layout_cx).into_cells();
        lay_out_coloured(
            &mut layout_cx,
            &mut fonts,
            m,
            (&after, Look::Words),
            (500.0, &mut cells),
            None,
        );
        assert_eq!(cells.len(), 5);
    }

    #[test]
    fn code_laid_out_plain_is_laid_out_again_once_its_colours_come() {
        use crate::chat::colours::Colours;
        let mut fonts = crate::chat::fixtures::fonts();
        let mut layout_cx = parley::LayoutContext::new();
        let m = Measures::of(text_size(&fonts));
        let text = "```rust\nfn main() {}\n```";
        let mut colours = Colours::default();
        let words = (text, Look::Words);
        let mut cells = Cells::new();
        let plain = lay_out_coloured(
            &mut layout_cx,
            &mut fonts,
            m,
            words,
            (600.0, &mut cells),
            Some(&mut colours),
        );
        assert!(plain.waiting);
        let mut cells = plain.into_cells();
        let asked = colours.take_wanted();
        let span = clackatoo_protocol::Span {
            start: 0,
            end: 2,
            kind: clackatoo_protocol::Kind::Keyword,
        };
        colours.asked(1, asked.into_iter().flatten().collect());
        assert!(colours.answered(1, vec![vec![span]]));
        let coloured = lay_out_coloured(
            &mut layout_cx,
            &mut fonts,
            m,
            words,
            (600.0, &mut cells),
            Some(&mut colours),
        );
        assert!(!coloured.waiting);
        assert_eq!(
            cells.len(),
            1,
            "the plain layout isn't taken for the coloured"
        );
    }

    #[test]
    fn a_key_lays_the_message_out_once_and_drawing_it_again_lays_out_nothing() {
        let mut fonts = crate::chat::fixtures::fonts();
        let node = crate::chat::fixtures::long_chat(3);
        let mut view = ChatView::new(String::new(), node);
        view.paste(&"a message long enough to wrap, ".repeat(10));
        view.settle(&mut fonts);
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        let laid = view.editor.generation();
        // Drawn again, as the dots or the agent's words make it: the
        // message isn't laid out again.
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert_eq!(view.editor.generation(), laid);
        // A key lays it out as it's typed, and drawing it doesn't again.
        view.paste("x");
        view.settle(&mut fonts);
        let typed_in = view.editor.generation();
        assert_ne!(typed_in, laid);
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert_eq!(view.editor.generation(), typed_in);
        // Another width does lay it out again.
        view.paint(&mut fonts, [0.0; 2], [600.0, 1000.0], true);
        assert_ne!(view.editor.generation(), typed_in);
        // And another font size, at the same width.
        let width = view.editor_width.expect("laid out to a width");
        let laid = view.editor.generation();
        let options = FontOptions {
            size: 15.0,
            ..FontOptions::default()
        };
        let mut bigger = Fonts::load(&options, 1.0).expect("a font");
        view.style_editor(&bigger);
        view.size_editor(width);
        view.editor.layout(&mut bigger.font_cx, &mut view.layout_cx);
        assert_ne!(view.editor.generation(), laid, "laid out in the new size");
    }

    #[test]
    fn the_boxs_labels_are_kept_while_drawn_and_let_go_once_not() {
        let mut fonts = crate::chat::fixtures::fonts();
        let mut node = clackatoo_protocol::ChatNode {
            state: ChatState::Ready,
            ..clackatoo_protocol::ChatNode::default()
        };
        node.settings.push(clackatoo_protocol::Setting {
            id: "mode".into(),
            name: "Mode".into(),
            kind: clackatoo_protocol::SettingKind::Mode,
            value: "auto".into(),
            choices: vec![clackatoo_protocol::SettingChoice {
                value: "auto".into(),
                name: "Auto".into(),
                description: None,
            }],
        });
        let mut view = ChatView::new(String::new(), node);
        let kept = |view: &ChatView| {
            let mut kept: Vec<String> = (view.cache.labels.now.keys())
                .map(|(text, _)| text.clone())
                .collect();
            kept.sort();
            kept
        };
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert_eq!(kept(&view), ["+", "↑", "◇  Auto  ▾"]);
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert_eq!(kept(&view), ["+", "↑", "◇  Auto  ▾"]);
        // The mode's gone: its label goes with the frame after.
        view.node.settings.clear();
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert_eq!(kept(&view), ["+", "↑", "◇  Permissions  ▾"]);
        crate::chat::fixtures::paint(&mut view, &mut fonts);
        assert!(
            !(view.cache.labels.last.keys()).any(|(text, _)| text.contains("Auto")),
            "let go"
        );
    }

    /// `markdown` laid out as the agent's words, 600 pixels wide, and
    /// what drawing it at the window's top left draws.
    pub(super) fn laid_and_drawn(markdown: &str) -> (Laid, TextPaint) {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let m = Measures::of(16.0);
        let mut layout_cx = parley::LayoutContext::new();
        let laid = lay_out(&mut layout_cx, &mut fonts, m, markdown, Look::Words, 600.0);
        let mut paint = TextPaint::default();
        let mut canvas = Canvas {
            paint: &mut paint,
            fonts: &mut fonts,
            clip: [0.0, 0.0, 2000.0, 2000.0],
            em: m.em,
            over: false,
        };
        draw_laid(&mut canvas, m, &laid, [100.0, 0.0], 600.0);
        (laid, paint)
    }

    #[test]
    fn prose_code_and_quotes_start_at_one_left_edge() {
        let (laid, paint) = laid_and_drawn("Some words.\n\n```rust\nlet x = 1;\n```\n\n> quoted");
        let m = Measures::of(16.0);
        let [words, code, quote] = [&laid.blocks[0], &laid.blocks[1], &laid.blocks[2]];
        assert!(words.left.abs() < f32::EPSILON);
        // Code's background starts at the column's left, as cards do, and
        // its text inside it.
        let background = (paint.fills.iter())
            .find(|fill| fill.paint == Paint::Code)
            .unwrap();
        assert!((background.rect[0] - 100.0).abs() < f32::EPSILON);
        assert!((code.left - code_inset(m)).abs() < f32::EPSILON);
        // A quote's bar is there too, with its text just after it.
        let bar = (paint.fills.iter())
            .find(|fill| fill.paint == Paint::RaisedEdge)
            .unwrap();
        assert!((bar.rect[0] - 100.0).abs() < f32::EPSILON);
        assert!((quote.left - quote_inset(m)).abs() < f32::EPSILON);
        assert!(quote.left > bar.rect[2]);
        // What's after code is as far from its background as from text.
        let gap = quote.top - (code.top + code.layout.height() + m.em(0.4));
        assert!((gap - m.em(0.7)).abs() < 0.01, "{gap}");
    }

    #[test]
    fn a_long_answer_draws_and_notes_only_what_is_in_view() {
        use crate::chat::fixtures::{paint as draw, talk};

        let answer = (0..400)
            .map(|n| format!("Paragraph {n} of a long answer."))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut chat = ChatView::new(String::new(), talk(&[("show me", &answer)]));
        draw(&mut chat, &mut fonts);
        chat.scroll_lines(40.0);
        let painted = draw(&mut chat, &mut fonts);
        let answers: Vec<_> = (chat.shown.iter())
            .filter(|shown| shown.key.piece == ITEMS)
            .collect();
        assert!(
            !answers.is_empty() && answers.len() < 100,
            "{}",
            answers.len()
        );
        let height = chat.view_height;
        for shown in answers {
            assert!(shown.at[1] + shown.size[1] >= 0.0 && shown.at[1] <= height);
        }
        // A paragraph's few dozen glyphs for each in view, not 400 of them.
        assert!(painted.glyphs.len() < 100 * 40, "{}", painted.glyphs.len());
    }

    #[test]
    fn code_is_drawn_in_its_languages_colours_once_they_come() {
        use crate::chat::fixtures::{paint as draw, talk};
        use clackatoo_protocol::{Kind, Span};

        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let answer = "Here:\n\n```rust\nfn main() {}\n```\n\nand still coming:\n\n```rust\nlet";
        let mut chat = ChatView::new(String::new(), talk(&[("show me", answer)]));
        let keyword = Ink::Color(crate::colors::syntax(Kind::Keyword).fg);
        let coloured = |paint: &TextPaint| {
            paint
                .glyphs
                .iter()
                .filter(|glyph| glyph.ink == keyword)
                .count()
        };
        let painted = draw(&mut chat, &mut fonts);
        assert_eq!(coloured(&painted), 0, "plain until the colours come");
        // Only the block that's whole is asked about.
        let asks = chat.take_colour_asks();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].len(), 1);
        assert_eq!(asks[0][0].text, "fn main() {}");
        chat.colours_asked(Some(4), asks[0].clone());
        let spans = vec![vec![Span {
            start: 0,
            end: 2,
            kind: Kind::Keyword,
        }]];
        assert!(chat.coloured(4, spans));
        let painted = draw(&mut chat, &mut fonts);
        assert_eq!(coloured(&painted), 2, "`fn`, in the keywords' colour");
        assert!(chat.take_colour_asks().is_empty(), "nothing more to ask");
        // In the thinking, it's left quiet.
        let mut node = talk(&[("show me", "")]);
        node.turns[0].items = vec![Item::Thought("```rust\nfn x() {}\n```".into())];
        let mut chat = ChatView::new(String::new(), node);
        chat.toggle_fold(0);
        draw(&mut chat, &mut fonts);
        assert!(
            chat.shown.iter().any(|shown| shown.code),
            "the thought's code is shown"
        );
        assert!(chat.take_colour_asks().is_empty());
    }

    #[test]
    fn only_the_daemons_own_question_is_shown_as_it_is() {
        let question = |title: &str, own| clackatoo_protocol::Question {
            id: 1,
            title: title.to_owned(),
            choices: Vec::new(),
            own,
        };
        assert_eq!(
            asked(&question("run `make` in a terminal", false), true),
            "Allow the agent to run `make` in a terminal?"
        );
        let own = "Keep waiting for Codex, start it again and ask again, or stop?";
        assert_eq!(asked(&question(own, true), true), own);
        // Worded as the daemon's, an agent's is still the agent's.
        assert_eq!(
            asked(&question(own, false), true),
            format!("Allow the agent to {own}?")
        );
        // From a daemon that quoted with single backticks, all as it is.
        let old = "send `x` to chat 1 (trusted) `y` to chat 2";
        assert_eq!(
            asked(&question(old, false), false),
            format!("Allow the agent to {}?", escape(old))
        );
        // Only the code it quotes is Markdown: no stars make bold.
        assert_eq!(
            asked(&question("send ``a `b` **c**`` to *chat* 2", false), true),
            "Allow the agent to send ``a `b` **c**`` to \\*chat\\* 2?"
        );
    }

    #[test]
    fn a_questions_quoted_code_is_drawn_as_code() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut node = clackatoo_protocol::ChatNode {
            state: ChatState::Answering,
            ..clackatoo_protocol::ChatNode::default()
        };
        node.question = Some(clackatoo_protocol::Question {
            id: 1,
            title: "send `` look at the `parser` `` to a new chat".into(),
            choices: Vec::new(),
            own: false,
        });
        let mut chat = ChatView::new(String::new(), node);
        let paint = crate::chat::fixtures::paint(&mut chat, &mut fonts);
        // The code has its background, and no backtick is drawn.
        assert!(paint.fills.iter().any(|fill| fill.paint == Paint::Code));
        let question = chat
            .lay_out_question(&mut fonts, Measures::of(16.0), 600.0)
            .unwrap();
        let text: String = (question.title.blocks.iter())
            .map(|block| block.text.as_str())
            .collect();
        assert_eq!(
            text,
            "Allow the agent to send look at the `parser` to a new chat?"
        );
        // From a daemon before protocol 84, the title is shown as it is.
        chat.set_quoted(false);
        let paint = crate::chat::fixtures::paint(&mut chat, &mut fonts);
        assert!(!paint.fills.iter().any(|fill| fill.paint == Paint::Code));
    }

    #[test]
    fn a_turn_where_the_chat_moved_is_a_line_across_it_with_what_didnt_carry_over() {
        let turn = Turn {
            previews: Vec::new(),
            prompt: String::new(),
            sent: 1,
            items: Vec::new(),
            end: Some(clackatoo_protocol::TurnEnd {
                at: 1,
                outcome: Outcome::Done,
            }),
            files: None,
            from: None,
            switch: Some(clackatoo_protocol::Switch {
                from: "Claude Code".into(),
                to: "Codex".into(),
                model: Some("6 Luna".into()),
                resumed: false,
                notes: vec!["Mode Auto doesn't carry over: Codex uses Auto review".into()],
            }),
        };
        assert_eq!(
            turn_note(&turn),
            Some((
                "── Switched to Codex · 6 Luna · context carried over ──\n\n\
                 Mode Auto doesn't carry over: Codex uses Auto review"
                    .to_owned(),
                Tone::Muted
            ))
        );
        // No bubble, nor work to fold: just the line.
        let m = Measures::of(16.0);
        assert!(guess(m, 400.0, &turn, false, false) >= 4.0 * m.em * LINE);
    }

    #[test]
    fn characters_the_interface_font_lacks_come_from_the_fonts_behind_it() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        // Noto Sans has no ▾. Without it here, there's nothing to check.
        fonts.load_ui(Some("Noto Sans"), 11.0, 1.0);
        if fonts.ui_family.as_deref() != Some("Noto Sans") {
            return;
        }
        let mut layout_cx = parley::LayoutContext::new();
        let laid = lay_out(
            &mut layout_cx,
            &mut fonts,
            Measures::of(16.0),
            "Auto ▾ ✓ ✗ ○",
            Look::Plain(Tone::Plain),
            400.0,
        );
        let layout = &laid.blocks[0].layout;
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    // Glyph 0 is a font's box for what it hasn't got.
                    assert!(run.glyphs().all(|glyph| glyph.id != 0));
                }
            }
        }
    }

    #[test]
    fn a_long_chat_lays_out_only_what_is_near_the_view_and_not_folded_work() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut chat = ChatView::new("chat".into(), crate::chat::fixtures::long_chat(500));
        chat.paint(&mut fonts, [0.0; 2], [900.0, 1000.0], true);
        let laid: Vec<usize> = (chat.cache.turns.iter().enumerate())
            .filter(|(_, slot)| slot.laid.is_some())
            .map(|(index, _)| index)
            .collect();
        assert!(!laid.is_empty() && laid.len() < 10, "{laid:?}");
        assert!(laid.contains(&499));
        // A folded turn's work isn't laid out: its message and answer are.
        let folded = laid[0];
        let turn = &chat.node.turns[folded];
        let answer = fold::answer_start(turn);
        let pieces = &chat.cache.turns[folded].laid.as_ref().unwrap().pieces;
        assert!(pieces.contains_key(&PROMPT));
        assert!(pieces.contains_key(&(ITEMS + answer)));
        assert!((ITEMS..ITEMS + answer).all(|piece| !pieces.contains_key(&piece)));
        assert!(
            chat.cache.turns[folded]
                .laid
                .as_ref()
                .unwrap()
                .cards
                .is_empty()
        );
        // At the top, the first turns are laid out, and the last aren't
        // laid out again.
        chat.scroll_lines(f32::INFINITY);
        chat.paint(&mut fonts, [0.0; 2], [900.0, 1000.0], true);
        assert!(chat.cache.turns[0].laid.is_some());
        assert!(chat.cache.turns[250].laid.is_none());
    }

    #[test]
    fn turns_far_from_the_view_are_let_go_of_and_bounds_kept_up() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let mut chat = ChatView::new("chat".into(), crate::chat::fixtures::long_chat(300));
        let size = [900.0, 1000.0];
        chat.paint(&mut fonts, [0.0; 2], size, true);
        assert!(chat.cache.turns[299].laid.is_some());
        // Read up to the top a half view at a time: what's left behind is
        // let go of, and only what's near is kept.
        while chat.scroll_half(true) {
            chat.paint(&mut fonts, [0.0; 2], size, true);
        }
        assert!(chat.cache.turns[0].laid.is_some());
        assert!(chat.cache.turns[299].laid.is_none());
        assert!(chat.cache.laid.len() < 20, "{}", chat.cache.laid.len());
        assert!(chat.cache.turns[299].height.is_some());
        // The bounds kept up are those worked out afresh.
        let m = Measures::of(chat.cache.size);
        let kept = chat.cache.bounds.clone();
        chat.cache.bounded = 0;
        chat.update_bounds(m);
        assert_eq!(kept, chat.cache.bounds);
    }

    #[test]
    fn an_open_card_notes_only_its_lines_in_view() {
        use clackatoo_protocol::{Item, Tool, ToolContent, ToolKind, ToolStatus};

        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let output = (0..900).fold(String::new(), |output, line| {
            output + &format!("line {line}\n")
        });
        let mut node = crate::chat::fixtures::talk(&[("run it", "Done.")]);
        node.turns[0].items.insert(
            0,
            Item::Tool(Tool {
                id: "run".into(),
                title: "make".into(),
                status: ToolStatus::Done,
                kind: ToolKind::Execute,
                content: vec![ToolContent::Output(output)],
                locations: Vec::new(),
            }),
        );
        let mut chat = ChatView::new("chat".into(), node);
        chat.open_fold(0);
        chat.open_card("run");
        chat.paint(&mut fonts, [0.0; 2], [900.0, 1000.0], true);
        let parts = (chat.shown.iter())
            .filter(|shown| shown.key.piece == ITEMS)
            .count();
        assert!(parts > 0 && parts < 60, "{parts}");
    }

    #[test]
    fn previews_draw_from_a_fresh_snapshot_and_reuse_pixels_between_frames() {
        use crate::chat::fixtures::{fonts, paint, talk};
        let mut fonts = fonts();
        let preview = clackatoo_protocol::ChatPreview {
            width: 2,
            height: 1,
            rgba: vec![255; 8].into(),
        };
        let mut node = talk(&[("[Image]", "A screenshot.")]);
        node.turns[0].previews.push(preview.clone());
        let mut chat = ChatView::new("chat".into(), node);
        let first = paint(&mut chat, &mut fonts);
        assert_eq!(first.images.len(), 1);
        let image = first.images[0].image.clone();
        let second = paint(&mut chat, &mut fonts);
        assert!(std::sync::Arc::ptr_eq(&image, &second.images[0].image));
        chat.paste_image(clackatoo_protocol::ChatImage {
            data: "cG5n".into(),
            width: 2,
            height: 1,
            preview: Some(preview),
        });
        chat.settle(&mut fonts);
        let draft = paint(&mut chat, &mut fonts);
        assert_eq!(draft.images.len(), 2);
        assert!(chat.hits.iter().any(|(hit, _)| *hit == Hit::RemoveImage(0)));
        chat.remove_image(0);
        assert_eq!(paint(&mut chat, &mut fonts).images.len(), 1);
    }

    #[test]
    #[ignore = "a measurement: run with --profile quick --ignored --nocapture"]
    fn frame_time_with_chat_previews() {
        let mut fonts = crate::chat::fixtures::fonts();
        let node = crate::chat::fixtures::talk(&[("[Image]", "A screenshot."); 16]);
        for with_images in [false, true] {
            let mut node = node.clone();
            if with_images {
                for turn in &mut node.turns {
                    turn.previews.push(clackatoo_protocol::ChatPreview {
                        width: 256,
                        height: 160,
                        rgba: vec![127; 256 * 160 * 4].into(),
                    });
                }
            }
            let mut chat = ChatView::new("chat".into(), node);
            crate::chat::fixtures::paint(&mut chat, &mut fonts);
            let start = std::time::Instant::now();
            for _ in 0..1000 {
                std::hint::black_box(crate::chat::fixtures::paint(&mut chat, &mut fonts));
            }
            println!(
                "previews={with_images}: {:?} per cached frame",
                start.elapsed() / 1000
            );
        }
    }

    /// What drawing a long chat costs a frame: the first, which lays it
    /// out, then each one after at the end, in the middle, and while the
    /// agent streams its answer. Prints the times; it asserts nothing.
    #[test]
    #[ignore = "a measurement: run with --release --ignored --nocapture"]
    fn frame_time_on_a_long_chat() {
        use std::time::Instant;

        use clackatoo_protocol::{ChatPatch, Patch, ViewUpdate};

        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        let node = crate::chat::fixtures::long_chat(500);
        let mut chat = ChatView::new("chat".into(), node);
        let size = [900.0, 1000.0];
        let frames = 200;
        let time = |chat: &mut ChatView, fonts: &mut Fonts, what: &str, frames: u32| {
            let start = Instant::now();
            for _ in 0..frames {
                std::hint::black_box(chat.paint(fonts, [0.0; 2], size, true));
            }
            let each = start.elapsed() / frames;
            println!("{what}: {each:?} a frame");
        };
        time(&mut chat, &mut fonts, "first frame", 1);
        // Again, with the fonts' caches warm, as opening a second chat is.
        let mut again = ChatView::new("chat".into(), chat.node.clone());
        time(&mut again, &mut fonts, "first frame, fonts warm", 1);
        time(&mut chat, &mut fonts, "at the end", frames);
        chat.scroll_lines(f32::INFINITY);
        chat.scroll_half(false);
        for _ in 0..200 {
            chat.scroll_half(false);
            chat.paint(&mut fonts, [0.0; 2], size, true);
        }
        time(&mut chat, &mut fonts, "in the middle", frames);
        chat.scroll_lines(f32::NEG_INFINITY);
        let last = u32::try_from(chat.node.turns.len() - 1).unwrap();
        let item = u32::try_from(chat.node.turns.last().unwrap().items.len() - 1).unwrap();
        let start = Instant::now();
        for _ in 0..frames {
            chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Append {
                turn: last,
                item,
                text: " More words, streamed.".into(),
            })]));
            std::hint::black_box(chat.paint(&mut fonts, [0.0; 2], size, true));
        }
        println!("streaming: {:?} a frame", start.elapsed() / frames);

        // A chat of 10,000 turns: a frame costs what's near the view.
        let pairs: Vec<(String, String)> = (0..10_000)
            .map(|turn| {
                (
                    format!("question {turn}"),
                    format!("Answer {turn}, with **some** words."),
                )
            })
            .collect();
        let pairs: Vec<(&str, &str)> = (pairs.iter())
            .map(|(prompt, answer)| (prompt.as_str(), answer.as_str()))
            .collect();
        let mut huge = ChatView::new("chat".into(), crate::chat::fixtures::talk(&pairs));
        time(&mut huge, &mut fonts, "10,000 turns, first frame", 1);
        time(&mut huge, &mut fonts, "10,000 turns, at the end", frames);
    }
}
