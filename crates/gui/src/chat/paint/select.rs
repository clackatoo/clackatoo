//! The conversation's text, as something to point at, select and copy.
//!
//! Each block of a message, and each line a card shows, is laid out on
//! its own, so a place in the text is a block, by its turn, its piece and
//! its part, and a byte in that block's text. Places sort in the order
//! they're shown, so a selection runs from one to another, across blocks,
//! messages and turns. A selection made with the mouse runs from where it
//! started to the mouse; one made in visual mode takes in the character
//! under the cursor, or whole lines.
//!
//! What's copied is the text as it's shown, without its Markdown, and
//! what's folded away isn't: a selection over a folded turn takes its
//! message and its answer.

use parley::{Affinity, Cursor, Layout, Selection as Range};
use unicode_segmentation::UnicodeSegmentation;

use super::cards::CardLaid;
use clackatoo_protocol::Item;

use super::{
    AGENTS, Canvas, FOLD, Laid, Measures, PROMPT, Piece, agents, control, fold, prompt_origin,
    rect_at,
};
use crate::chat::card::Card;
use crate::chat::markdown::{self, BlockKind};
use crate::chat::{Brush, ChatView, Hit, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Ink, Paint};
use crate::vim::Selection;

/// One block of the conversation's text: a block of a message, the line
/// a turn's work folds under, or a line of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::chat) struct TextKey {
    pub(in crate::chat) turn: usize,
    /// Which piece of the turn: [`PROMPT`], [`FOLD`], or an item's.
    pub(in crate::chat) piece: usize,
    /// Which block of the piece, or part of a card.
    pub(in crate::chat) part: usize,
}

/// A place in the conversation's text: a block, and a byte in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::chat) struct Position {
    pub(in crate::chat) text: TextKey,
    pub(in crate::chat) byte: usize,
}

/// A block of text drawn in the last frame, and where, from the pane's
/// top left, for the mouse.
#[derive(Clone, Copy, Debug)]
pub(in crate::chat) struct Shown {
    pub(in crate::chat) key: TextKey,
    /// Its layout's top left.
    pub(in crate::chat) at: [f32; 2],
    pub(in crate::chat) size: [f32; 2],
    /// It's a block of code.
    pub(in crate::chat) code: bool,
}

/// Text selected in the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::chat) struct Picked {
    /// Where it started.
    pub(in crate::chat) anchor: Position,
    /// Where it's got to: the mouse, or the cursor.
    pub(in crate::chat) head: Position,
    pub(in crate::chat) how: How,
}

/// How text was selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::chat) enum How {
    /// With the mouse: up to the mouse.
    Mouse,
    /// In visual mode: up to the cursor, its character included, or the
    /// lines from one end to the other.
    Visual(Selection),
}

/// A block of text, laid out, and where it goes from the column's left
/// and its turn's top.
pub(in crate::chat) struct TextAt<'a> {
    pub(in crate::chat) layout: &'a Layout<Brush>,
    pub(in crate::chat) text: &'a str,
    pub(in crate::chat) at: [f32; 2],
    pub(in crate::chat) kind: Option<&'a BlockKind>,
}

/// `byte` in `text`, moved back to the start of the character it's in,
/// and kept inside it, as text that's been replaced may need.
pub(in crate::chat) fn within(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// Where what's after the character `byte` is in starts, in `text`: the
/// whole of what's drawn as one, as an accented letter or an emoji made
/// of several.
pub(in crate::chat) fn after(text: &str, byte: usize) -> usize {
    let byte = within(text, byte);
    byte + text[byte..].graphemes(true).next().map_or(0, str::len)
}

/// Where the character before the one at `byte` starts, in `text`, as
/// [`after`] counts characters.
pub(in crate::chat) fn before(text: &str, byte: usize) -> usize {
    let byte = within(text, byte);
    (text[..byte].grapheme_indices(true).next_back()).map_or(byte, |(at, _)| at)
}

/// The blocks of `markdown`'s text, as they're copied: a line apart in a
/// list, a table's cells a tab apart and its rows a line, and a blank
/// line apart otherwise.
pub(in crate::chat) fn blocks_text(markdown: &str) -> String {
    let mut text = String::new();
    let mut before: Option<BlockKind> = None;
    for block in markdown::blocks(markdown) {
        text.push_str(&joint(before.as_ref(), &block.kind));
        text.push_str(&block.text);
        before = Some(block.kind);
    }
    text
}

/// What goes before a block copied, `after`, after the block before it,
/// `before`, if there's one in the same piece: a line break between a
/// list's items, a blank line between other blocks, and in a table, a tab
/// for each column and a line break for each row between them, so empty
/// cells, which are no blocks, still have their places.
fn joint(before: Option<&BlockKind>, after: &BlockKind) -> String {
    let tabs = |count: usize| "\t".repeat(count);
    match (before.and_then(BlockKind::cell), after.cell()) {
        (Some(before), Some(after)) if before.table == after.table => {
            if before.row == after.row {
                tabs(after.column.saturating_sub(before.column))
            } else {
                "\n".repeat(after.row - before.row) + &tabs(after.column)
            }
        }
        (_, Some(after)) if before.is_none() => tabs(after.column),
        (_, Some(after)) => "\n\n".to_owned() + &tabs(after.column),
        _ => match before {
            None => String::new(),
            Some(BlockKind::Item(_)) if matches!(after, BlockKind::Item(_)) => "\n".to_owned(),
            Some(_) => "\n\n".to_owned(),
        },
    }
}

/// Where the line of `text` that `byte` is in starts, and where it ends,
/// before its line break.
pub(in crate::chat) fn hard_line(text: &str, byte: usize) -> (usize, usize) {
    let byte = within(text, byte);
    let start = text[..byte].rfind('\n').map_or(0, |at| at + 1);
    let end = text[byte..].find('\n').map_or(text.len(), |at| byte + at);
    (start, end)
}

/// The line of `layout` that `byte` is on.
pub(in crate::chat) fn line_of(layout: &Layout<Brush>, byte: usize) -> usize {
    let count = layout.len();
    (0..count)
        .find(|&index| (layout.get(index)).is_some_and(|line| line.text_range().end > byte))
        .unwrap_or(count.saturating_sub(1))
}

/// Line `line` of `layout`'s middle, from its top, and the bytes it holds.
pub(in crate::chat) fn line_middle(
    layout: &Layout<Brush>,
    line: usize,
) -> Option<(f32, std::ops::Range<usize>)> {
    let line = layout.get(line)?;
    let metrics = line.metrics();
    Some((
        f32::midpoint(metrics.block_min_coord, metrics.block_max_coord),
        line.text_range(),
    ))
}

/// How far across `layout` `byte` is.
#[allow(
    clippy::cast_possible_truncation,
    reason = "pixels in a pane, far inside f32's range"
)]
pub(in crate::chat) fn x_of(layout: &Layout<Brush>, byte: usize) -> f32 {
    Cursor::from_byte_index(layout, byte, Affinity::Downstream)
        .geometry(layout, 1.0)
        .x0 as f32
}

impl ChatView {
    /// Text `key`, laid out, if it is.
    pub(in crate::chat) fn text(&self, key: TextKey) -> Option<TextAt<'_>> {
        let slot = self.cache.turns.get(key.turn)?;
        let laid = slot.laid.as_ref()?;
        let m = Measures::of(self.cache.size);
        let span = (laid.spans.iter()).find(|span| match span.piece {
            Piece::Prompt => key.piece == PROMPT,
            Piece::Fold => key.piece == FOLD,
            Piece::Agents => key.piece == AGENTS,
            Piece::Item(piece) | Piece::Card(piece) | Piece::Rule(piece) => key.piece == piece,
            Piece::Handoff
            | Piece::Previews
            | Piece::Note
            | Piece::Footer
            | Piece::Badge
            | Piece::Divider => false,
        })?;
        if key.piece == AGENTS {
            let card = slot.agents.as_ref()?;
            let line = fold::height(m);
            let laid = match key.part {
                0 => &card.header,
                part => &card.rows.get(part - 1)?.1,
            };
            let block = laid.blocks.first()?;
            let left = if key.part == 0 {
                0.0
            } else {
                m.em(agents::INDENT)
            };
            let top = span.top + agents::line_top(m, key.part) + (line - laid.height) / 2.0;
            return Some(TextAt {
                layout: &block.layout,
                text: &block.text,
                at: [left + block.left, top + block.top],
                kind: None,
            });
        }
        if key.piece == FOLD {
            let header = slot.header.as_ref()?;
            let block = header.blocks.first().filter(|_| key.part == 0)?;
            let top = span.top + (fold::height(m) - header.height) / 2.0;
            return Some(TextAt {
                layout: &block.layout,
                text: &block.text,
                at: [block.left, top + block.top],
                kind: None,
            });
        }
        if let Some(card) = laid.cards.get(&key.piece) {
            let (part, [x, y]) = card.part(m, key.part)?;
            let block = part.blocks.first()?;
            return Some(TextAt {
                layout: &block.layout,
                text: &block.text,
                at: [span.left + x + block.left, span.top + y + block.top],
                kind: None,
            });
        }
        let piece = laid.pieces.get(&key.piece)?;
        let block = piece.blocks.get(key.part)?;
        let origin = if key.piece == PROMPT {
            prompt_origin(m, piece, self.cache.width)
        } else {
            [0.0; 2]
        };
        let scroll = (piece.table_of(key.part))
            .map_or(0.0, |table| self.table_scroll(key.turn, key.piece, table));
        Some(TextAt {
            layout: &block.layout,
            text: &block.text,
            at: [
                span.left + origin[0] + block.left - scroll,
                span.top + origin[1] + block.top,
            ],
            kind: Some(&block.kind),
        })
    }

    /// The blocks of text turn `turn` shows, in order, as it was last laid
    /// out.
    pub(in crate::chat) fn turn_texts(&self, turn: usize) -> Vec<TextKey> {
        let Some(slot) = self.cache.turns.get(turn) else {
            return Vec::new();
        };
        let Some(laid) = &slot.laid else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        let mut add = |piece: usize, parts: usize| {
            keys.extend((0..parts).map(|part| TextKey { turn, piece, part }));
        };
        for span in &laid.spans {
            match span.piece {
                Piece::Prompt => add(
                    PROMPT,
                    laid.pieces.get(&PROMPT).map_or(0, |laid| laid.blocks.len()),
                ),
                Piece::Fold => add(FOLD, usize::from(slot.header.is_some())),
                Piece::Agents => add(
                    AGENTS,
                    slot.agents.as_ref().map_or(0, |card| 1 + card.rows.len()),
                ),
                Piece::Item(piece) | Piece::Rule(piece) => add(
                    piece,
                    laid.pieces.get(&piece).map_or(0, |laid| laid.blocks.len()),
                ),
                Piece::Card(piece) => {
                    add(piece, laid.cards.get(&piece).map_or(0, CardLaid::parts));
                }
                Piece::Handoff
                | Piece::Previews
                | Piece::Note
                | Piece::Footer
                | Piece::Badge
                | Piece::Divider => {}
            }
        }
        keys
    }

    /// Where the selection starts, and where it ends, past its last
    /// character.
    pub(in crate::chat) fn picked_range(&self) -> Option<(Position, Position)> {
        self.range_of(self.picked?)
    }

    /// Where `picked` would select from, and to, past its last character,
    /// as [`picked_range`](Self::picked_range) says of the selection: what
    /// an operator takes from one place to another, too.
    pub(in crate::chat) fn range_of(&self, picked: Picked) -> Option<(Position, Position)> {
        let (start, end) = if picked.anchor <= picked.head {
            (picked.anchor, picked.head)
        } else {
            (picked.head, picked.anchor)
        };
        let bounds = match picked.how {
            How::Mouse => (start, end),
            // A chat has no block selection: Ctrl+V does nothing there.
            How::Visual(Selection::Characters | Selection::Block) => {
                let past = self
                    .text(end.text)
                    .map_or(end.byte, |text| after(text.text, end.byte));
                (start, Position { byte: past, ..end })
            }
            How::Visual(Selection::Lines) => {
                let first = self
                    .text(start.text)
                    .map_or(0, |text| hard_line(text.text, start.byte).0);
                // Not laid out, as at the end of a turn far away: to its
                // end.
                let last = self
                    .text(end.text)
                    .map_or(usize::MAX, |text| hard_line(text.text, end.byte).1);
                (
                    Position {
                        byte: first,
                        ..start
                    },
                    Position { byte: last, ..end },
                )
            }
        };
        (bounds.0 != bounds.1).then_some(bounds)
    }

    /// The text from `start` to `end`, as it's shown: blocks a line apart
    /// in a list or a card, and a blank line apart otherwise. The turns at
    /// either end are laid out for it, if they aren't; those between are
    /// copied from the chat itself, as laying out a long chat's worth of
    /// turns would take seconds.
    pub(in crate::chat) fn text_between(
        &mut self,
        fonts: &mut Fonts,
        start: Position,
        end: Position,
    ) -> String {
        for turn in [start.text.turn, end.text.turn] {
            self.lay_out_keeping_view(fonts, turn);
        }
        let mut copied = self.laid_text_between(start, end, start.text.turn);
        if start.text.turn < end.text.turn {
            for turn in start.text.turn + 1..end.text.turn {
                let text = self.turn_text(turn);
                if !text.is_empty() {
                    copied.push_str("\n\n");
                    copied.push_str(&text);
                }
            }
            let last = self.laid_text_between(start, end, end.text.turn);
            if !last.is_empty() {
                copied.push_str("\n\n");
                copied.push_str(&last);
            }
        }
        copied
    }

    /// Lays out turn `turn` if it isn't, as it is, for its text to be
    /// copied or searched, keeping what's in view where it was: a turn
    /// above the view that finds its height moves what's in it down as
    /// far.
    pub(in crate::chat) fn lay_out_keeping_view(&mut self, fonts: &mut Fonts, turn: usize) {
        self.update_bounds(Measures::of(self.cache.size));
        let first = super::in_view(&self.cache.bounds, self.scroll, self.scroll).start;
        let grew = self.lay_out_for_copy(fonts, turn);
        if turn < first && !self.follow {
            self.scroll += grew;
        }
    }

    /// Turn `index` as it's copied whole: its message, and what of the
    /// agent's work shows, the answer, if the work's folded, from the
    /// chat itself rather than its layout.
    fn turn_text(&self, index: usize) -> String {
        let Some(turn) = self.node.turns.get(index) else {
            return String::new();
        };
        let shape = self.shape(index, turn);
        let nesting = super::nested::Nesting::of(turn);
        let items = (turn.items.iter().enumerate())
            .filter(|&(at, _)| at >= shape.answer || shape.open)
            .filter(|(_, item)| !nesting.hidden(item, &self.open_cards))
            .map(|(_, item)| match item.inner() {
                Item::Words(text) | Item::Thought(text) | Item::Other(text) => blocks_text(text),
                Item::Compacted(details) => Item::compacted_label(details),
                Item::Tool(tool) => Card::of(tool).map_or_else(
                    || super::tool_text(tool.status, &tool.title),
                    |card| card.text(),
                ),
                Item::Nested { .. } => String::new(),
            });
        (std::iter::once(turn.prompt.clone()).chain(items))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// The text from `start` to `end` that turn `turn`, laid out, shows.
    fn laid_text_between(&self, start: Position, end: Position, turn: usize) -> String {
        let mut copied = String::new();
        let mut last: Option<(TextKey, Option<BlockKind>)> = None;
        for key in self.turn_texts(turn) {
            if key < start.text || key > end.text || control(key.piece) {
                continue;
            }
            let Some(text) = self.text(key) else {
                continue;
            };
            if let Some((before, kind)) = &last {
                let same = (before.turn, before.piece) == (key.turn, key.piece);
                copied.push_str(&match (kind, text.kind) {
                    _ if !same => "\n\n".to_owned(),
                    (Some(before), Some(after)) => joint(Some(before), after),
                    // A card's lines.
                    _ => "\n".to_owned(),
                });
            }
            let from = if key == start.text {
                within(text.text, start.byte)
            } else {
                0
            };
            let to = if key == end.text {
                within(text.text, end.byte)
            } else {
                text.text.len()
            };
            copied.push_str(&text.text[from..to.max(from)]);
            last = Some((key, text.kind.cloned()));
        }
        copied
    }

    /// Lays out turn `turn` if it isn't, as it is, for its text to be
    /// copied. Returns how much taller that made it than it was taken to
    /// be.
    fn lay_out_for_copy(&mut self, fonts: &mut Fonts, turn: usize) -> f32 {
        let Some(slot) = self.cache.turns.get(turn) else {
            return 0.0;
        };
        if slot.current() || turn >= self.node.turns.len() {
            return 0.0;
        }
        let before = slot.height.unwrap_or(0.0);
        let m = Measures::of(self.cache.size);
        let turns = std::mem::take(&mut self.node.turns);
        self.lay_out_turn(fonts, m, self.cache.width, turn, &turns);
        self.node.turns = turns;
        self.cache.turns[turn].height.unwrap_or(0.0) - before
    }

    /// Scrolls the cursor into view, a line from the edge, if a key moved
    /// it.
    pub(super) fn reveal_cursor(&mut self) {
        if !self.reveal {
            return;
        }
        self.reveal_cell();
        let Some(cursor) = self.cursor else {
            return;
        };
        let Some(&bound) = self.cache.bounds.get(cursor.text.turn) else {
            return;
        };
        let line = self.text(cursor.text).and_then(|text| {
            let line = text.layout.get(line_of(text.layout, cursor.byte))?;
            let metrics = line.metrics();
            let top = bound[0] + text.at[1];
            Some((top + metrics.block_min_coord, top + metrics.block_max_coord))
        });
        let (top, bottom) = line.unwrap_or((bound[0], bound[0] + self.line_height));
        let margin = self.line_height;
        if top - margin < self.scroll {
            self.scroll = top - margin;
        } else if bottom + margin > self.scroll + self.view_height {
            self.scroll = bottom + margin - self.view_height;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        self.follow = self.scroll >= self.max_scroll - 1.0;
    }

    /// The place in the text at `at`, from the pane's top left, as drawn
    /// last: in a block, or with `nearest`, in the block nearest it, as a
    /// drag past the text needs.
    pub(in crate::chat) fn spot_at(&self, at: [f32; 2], nearest: bool) -> Option<Position> {
        let within_view = at[1] >= 0.0 && at[1] < self.view_height;
        if !within_view && !nearest {
            return None;
        }
        let texts = self.shown.iter().filter(|shown| !control(shown.key.piece));
        let distance = |shown: &&Shown| {
            let [left, top] = shown.at;
            let [wide, high] = shown.size;
            let dx = (left - at[0]).max(at[0] - (left + wide)).max(0.0);
            let dy = (top - at[1]).max(at[1] - (top + high)).max(0.0);
            (dy, dx)
        };
        let shown = texts
            .min_by(|a, b| {
                distance(a)
                    .partial_cmp(&distance(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .filter(|shown| nearest || distance(shown) == (0.0, 0.0))?;
        let text = self.text(shown.key)?;
        let byte =
            Cursor::from_point(text.layout, at[0] - shown.at[0], at[1] - shown.at[1]).index();
        Some(Position {
            text: shown.key,
            byte,
        })
    }

    /// Whether `at`, from the pane's top left, is over the conversation's
    /// text, as drawn last.
    pub(in crate::chat) fn over_text(&self, at: [f32; 2]) -> bool {
        at[1] >= 0.0
            && at[1] < self.view_height
            && (self.shown.iter()).any(|shown| {
                !control(shown.key.piece)
                    && crate::chat::contains(
                        [shown.at[0], shown.at[1], shown.size[0], shown.size[1]],
                        at,
                    )
            })
    }

    /// The word, with `lines` the line, at `at`, from the pane's top
    /// left: where it starts and ends.
    pub(in crate::chat) fn word_at(
        &self,
        at: [f32; 2],
        lines: bool,
    ) -> Option<(Position, Position)> {
        let spot = self.spot_at(at, false)?;
        let shown = self.shown.iter().find(|shown| shown.key == spot.text)?;
        let text = self.text(spot.text)?;
        let [x, y] = [at[0] - shown.at[0], at[1] - shown.at[1]];
        let range = if lines {
            Range::hard_line_from_point(text.layout, x, y)
        } else {
            Range::word_from_point(text.layout, x, y)
        };
        let range = range.text_range();
        let spot = |byte| Position {
            text: spot.text,
            byte,
        };
        Some((spot(range.start), spot(range.end)))
    }

    /// The block of code drawn at `at`, from the pane's top left, if any:
    /// its background, out to the column's edge, counts.
    pub(in crate::chat) fn code_at(&self, at: [f32; 2]) -> Option<TextKey> {
        if at[1] < 0.0 || at[1] >= self.view_height {
            return None;
        }
        let m = Measures::of(self.cache.size);
        (self.shown.iter())
            .filter(|shown| shown.code)
            .find(|shown| crate::chat::contains(code_area(m, shown, self.column_right), at))
            .map(|shown| shown.key)
    }

    /// The label on the button that copies the block of code under the
    /// mouse, laid out, if there's one: from the chat's own text, where
    /// [`draw_reading`](Self::draw_reading) puts it back.
    pub(super) fn copy_label(&mut self, fonts: &mut Fonts, m: Measures) -> Option<Laid> {
        self.hovered_code?;
        let label = if self.copied_code == self.hovered_code {
            "Copied"
        } else {
            "Copy"
        };
        Some(self.note(fonts, m, label, Tone::Muted, f32::INFINITY))
    }

    /// Draws what the last search matches, in view, while it's lit up: the
    /// match the cursor's on brighter than the rest.
    fn draw_lit(&self, canvas: &mut Canvas<'_>, pane: [f32; 2]) {
        for lit in self.lit_matches() {
            let paint = if self.found_at(&lit) {
                Paint::Found
            } else {
                Paint::OtherMatch
            };
            let (index, range) = lit;
            let shown = &self.shown[index];
            let Some(text) = self.text(shown.key) else {
                continue;
            };
            let range = Range::new(
                Cursor::from_byte_index(text.layout, range.start, Affinity::Downstream),
                Cursor::from_byte_index(text.layout, range.end, Affinity::Upstream),
            );
            let at = [pane[0] + shown.at[0], pane[1] + shown.at[1]];
            for (bounds, _) in range.geometry(text.layout) {
                canvas.fill(Fill::solid(rect_at(bounds, at), paint));
            }
        }
    }

    /// Draws what's selected, the cursor in normal mode if `caret`, and
    /// over the block of code under the mouse, `label` on the button
    /// that copies it. The pane's top left is at `pane`.
    pub(super) fn draw_reading(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        pane: [f32; 2],
        (caret, label): (bool, Option<Laid>),
    ) {
        // Inside the column, as all its text is but a table's cells
        // scrolled sideways out of it, which the cursor and the selection
        // mustn't be drawn over the margins for.
        let view = canvas.clip;
        let left = pane[0] + self.column_right - self.cache.width;
        canvas.clip = super::intersection(view, [left, view[1], self.cache.width, view[3]]);
        self.draw_lit(canvas, pane);
        if let Some((start, end)) = self.picked_range() {
            for shown in &self.shown {
                if shown.key < start.text || shown.key > end.text || control(shown.key.piece) {
                    continue;
                }
                let Some(text) = self.text(shown.key) else {
                    continue;
                };
                let from = if shown.key == start.text {
                    within(text.text, start.byte)
                } else {
                    0
                };
                let to = if shown.key == end.text {
                    within(text.text, end.byte)
                } else {
                    text.text.len()
                };
                let range = Range::new(
                    Cursor::from_byte_index(text.layout, from, Affinity::Downstream),
                    Cursor::from_byte_index(text.layout, to, Affinity::Upstream),
                );
                let at = [pane[0] + shown.at[0], pane[1] + shown.at[1]];
                for (bounds, _) in range.geometry(text.layout) {
                    canvas.fill(Fill::solid(rect_at(bounds, at), Paint::Selected));
                }
            }
        }
        if caret
            && self.reading
            && let Some(cursor) = self.cursor
            && let Some(shown) = self.shown.iter().find(|shown| shown.key == cursor.text)
            && let Some(text) = self.text(cursor.text)
        {
            let at = [pane[0] + shown.at[0], pane[1] + shown.at[1]];
            let byte = within(text.text, cursor.byte);
            let here = Cursor::from_byte_index(text.layout, byte, Affinity::Downstream);
            let next =
                Cursor::from_byte_index(text.layout, after(text.text, byte), Affinity::Upstream);
            let block = Range::new(here, next).geometry(text.layout);
            let rect = match block.first() {
                Some((bounds, _)) if byte < text.text.len() => rect_at(*bounds, at),
                _ => {
                    let caret = rect_at(here.geometry(text.layout, 1.0), at);
                    [caret[0], caret[1], m.em(0.55), caret[3]]
                }
            };
            // Solid, as an editor's is in normal mode, with the character
            // under it drawn again over it in the colour that shows there.
            let solid = Fill::rounded(rect, Paint::Accent, 2.0).clipped(canvas.clip);
            canvas.paint.over.push(solid);
            canvas.text_over(text.layout, at, rect, Ink::OnColor);
        }
        canvas.clip = view;
        let Some(label) = label else {
            return;
        };
        if let Some(key) = self.hovered_code
            && let Some(shown) = self.shown.iter().find(|shown| shown.key == key)
        {
            let area = code_area(m, shown, self.column_right);
            let size = [label.width + m.em(1.2), label.height + m.em(0.3)];
            let rect = [
                pane[0] + area[0] + area[2] - m.em(0.4) - size[0],
                pane[1] + area[1] + m.em(0.4),
                size[0],
                size[1],
            ];
            canvas.fill(
                Fill::rounded(rect, Paint::Raised, m.em(0.4)).bordered(1.0, Paint::RaisedEdge),
            );
            let text = [
                rect[0] + m.em(0.6),
                rect[1] + (size[1] - label.height) / 2.0,
            ];
            super::draw_laid(canvas, m, &label, text, size[0]);
            let [left, top, wide, high] = super::intersection(canvas.clip, rect);
            if wide > 0.0 && high > 0.0 {
                self.hits.push((
                    Hit::CopyCode(key),
                    [left - pane[0], top - pane[1], wide, high],
                ));
            }
        }
        self.cache
            .notes
            .insert((label.source.clone(), Tone::Muted), label);
    }
}

/// The background of block of code `shown`, from the pane's top left, out
/// to `right`, the column's right edge.
fn code_area(m: Measures, shown: &Shown, right: f32) -> [f32; 4] {
    let left = shown.at[0] - m.em(0.6);
    [
        left,
        shown.at[1] - m.em(0.4),
        (right - left).max(0.0),
        shown.size[1] + m.em(0.8),
    ]
}

#[cfg(test)]
mod tests {
    use winit::keyboard::{Key, KeyLocation};

    use super::super::{ITEMS, PROMPT};
    use super::{Shown, TextKey};
    use crate::chat::fixtures::{fonts, lines, paint, read, talk, yanked};
    use crate::chat::{ChatView, Hit, Pointer};
    use crate::input::{KeyPress, Modifiers};
    use crate::pane::Copy;

    #[test]
    fn a_tables_cells_copy_a_tab_apart_and_its_rows_a_line() {
        assert_eq!(
            super::blocks_text("Look:\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- x\n- y"),
            "Look:\n\na\tb\n1\t2\n\nx\ny"
        );
        // Empty cells keep their places: leading, in the middle, a whole
        // row, and the first cell of all.
        assert_eq!(
            super::blocks_text(
                "| a | b | c |\n|---|---|---|\n|  | 2 | 3 |\n| 4 |  | 6 |\n|  |  |  |\n| 7 | 8 | 9 |"
            ),
            "a\tb\tc\n\t2\t3\n4\t\t6\n\n7\t8\t9"
        );
        assert_eq!(
            super::blocks_text("|  | b |\n|---|---|\n| 1 | 2 |"),
            "\tb\n1\t2"
        );
        assert_eq!(
            super::blocks_text("Look:\n\n|  | b |\n|---|---|"),
            "Look:\n\n\tb"
        );
    }

    /// Where text `key` was drawn.
    fn shown(chat: &ChatView, turn: usize, piece: usize) -> Shown {
        let key = TextKey {
            turn,
            piece,
            part: 0,
        };
        *(chat.shown.iter())
            .find(|shown| shown.key == key)
            .expect("drawn")
    }

    #[test]
    fn dragging_selects_across_messages_and_letting_go_copies_it() {
        let mut fonts = fonts();
        let node = talk(&[("first question", "first answer"), ("second question", "x")]);
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        let from = shown(&chat, 0, ITEMS);
        let to = shown(&chat, 1, PROMPT);
        let middle = |shown: &Shown| shown.at[1] + shown.size[1] / 2.0;
        let start = [from.at[0] + 1.0, middle(&from)];
        assert_eq!(chat.pointer(start), Pointer::Text);
        chat.press(start, 1, false);
        chat.drag([to.at[0] + to.size[0] + 5.0, middle(&to)]);
        chat.release();
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("first answer\n\nsecond question".to_owned(), Copy::Primary)]
        );
        // It's drawn, until a click elsewhere.
        let picked = paint(&mut chat, &mut fonts);
        assert!(chat.picked_range().is_some());
        assert!(!picked.fills.is_empty());
        chat.press([1.0, 1.0], 1, false);
        chat.release();
        assert!(chat.settle(&mut fonts).copied.is_empty());
        assert!(chat.picked.is_none());
    }

    #[test]
    fn a_double_click_selects_a_word_and_ctrl_c_copies_it() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&[("q", "alpha beta gamma")]));
        paint(&mut chat, &mut fonts);
        let answer = shown(&chat, 0, ITEMS);
        let x = answer.at[0] + answer.size[0] / 2.0;
        chat.press([x, answer.at[1] + 4.0], 2, false);
        chat.release();
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("beta".to_owned(), Copy::Primary)]
        );
        // Typing a message, Ctrl+C copies it to the clipboard, the box
        // having nothing selected, rather than stopping the agent.
        chat.key(&KeyPress {
            key: &Key::Character("c".into()),
            text: None,
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        });
        let settled = chat.settle(&mut fonts);
        assert_eq!(settled.copied, [("beta".to_owned(), Copy::Clipboard)]);
        assert!(settled.actions.is_empty());
    }

    /// Ctrl+C, typing a message.
    fn ctrl_c(chat: &mut ChatView) {
        chat.key(&KeyPress {
            key: &Key::Character("c".into()),
            text: None,
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        });
    }

    #[test]
    fn while_the_agent_works_ctrl_c_stops_it_whatever_is_selected() {
        use clackatoo_protocol::{ChatAction, ChatState};

        let mut fonts = fonts();
        let mut node = talk(&[("q", "alpha beta gamma")]);
        node.state = ChatState::Answering;
        node.turns[0].end = None;
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        let answer = shown(&chat, 0, ITEMS);
        chat.press([answer.at[0] + 2.0, answer.at[1] + 4.0], 2, false);
        chat.release();
        chat.settle(&mut fonts);
        assert!(chat.picked_range().is_some());
        ctrl_c(&mut chat);
        let settled = chat.settle(&mut fonts);
        assert_eq!(settled.actions, [ChatAction::Stop]);
        assert!(settled.copied.is_empty());
        // Sending a message lets go of what was selected.
        let mut chat = ChatView::new(String::new(), talk(&[("q", "alpha beta")]));
        paint(&mut chat, &mut fonts);
        chat.press([answer.at[0] + 2.0, answer.at[1] + 4.0], 2, false);
        chat.release();
        chat.paste("more");
        chat.key(&KeyPress {
            key: &Key::Named(winit::keyboard::NamedKey::Enter),
            text: Some("\r"),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
        chat.settle(&mut fonts);
        assert!(chat.picked.is_none());
    }

    #[test]
    fn a_mouse_selection_shows_as_visual_while_y_would_yank_it_and_any_other_key_forgets_it() {
        use crate::vim::Mode;
        let mut fonts = fonts();
        let node = talk(&[("q", "alpha beta")]);
        let snapshot = clackatoo_protocol::ViewUpdate::Snapshot(clackatoo_protocol::View {
            title: "agent".into(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(node)),
        });
        let mut pane =
            crate::chat::ChatPane::of_snapshot(clackatoo_protocol::TerminalId(1), &snapshot);
        pane.enter_normal_mode();
        pane.paint(&mut fonts, [0.0; 2], [900.0, 1000.0], true);
        let select = |pane: &mut crate::chat::ChatPane, fonts: &mut crate::fonts::Fonts| {
            let answer = shown(pane.view(), 0, ITEMS);
            pane.view_mut()
                .press([answer.at[0] + 2.0, answer.at[1] + 4.0], 2, false);
            pane.view_mut().release();
            pane.settle(fonts);
        };
        select(&mut pane, &mut fonts);
        assert_eq!(pane.mode(), Mode::Normal);
        assert_eq!(
            pane.shown_mode(),
            Mode::Visual(crate::vim::Selection::Characters)
        );
        // A register waits with it.
        type_in(&mut pane, "\"");
        type_in(&mut pane, "a");
        assert_eq!(
            pane.shown_mode(),
            Mode::Visual(crate::vim::Selection::Characters)
        );
        type_in(&mut pane, "y");
        assert_eq!(pane.shown_mode(), Mode::Normal);
        pane.settle(&mut fonts);
        assert_eq!(pane.take_yanks().len(), 1);
        // Anything else forgets it: `j` moves, and `y` then waits for a
        // motion.
        select(&mut pane, &mut fonts);
        type_in(&mut pane, "j");
        assert_eq!(pane.shown_mode(), Mode::Normal);
        assert!(pane.view().picked.is_none());
    }

    /// Types `text` in `pane`.
    fn type_in(pane: &mut crate::chat::ChatPane, text: &str) {
        let key = Key::Character(text.into());
        pane.key(&KeyPress {
            key: &key,
            text: Some(text),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
    }

    #[test]
    fn copies_settled_together_all_go() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), talk(&[("q", "alpha beta")]));
        paint(&mut chat, &mut fonts);
        let answer = shown(&chat, 0, ITEMS);
        chat.press([answer.at[0] + 2.0, answer.at[1] + 4.0], 2, false);
        chat.release();
        chat.reading = true;
        // Let go of, the word goes to the primary selection; and `yy`,
        // typed before the chat's settled, yanks the line the cursor's on
        // into the registers. Both go.
        read(&mut chat, "yy");
        let settled = chat.settle(&mut fonts);
        assert_eq!(settled.copied, [("alpha".to_owned(), Copy::Primary)]);
        assert_eq!(settled.yanks.len(), 1);
        assert_eq!(settled.yanks[0].register.text, "alpha beta\n");
        // Selected with the mouse, `y` yanks what's selected, as Vim takes
        // a mouse selection as a visual one, after a register too.
        read(&mut chat, "\"ay");
        let settled = chat.settle(&mut fonts);
        assert_eq!(settled.yanks.len(), 1);
        assert_eq!(settled.yanks[0].register.text, "alpha");
        assert_eq!(
            settled.yanks[0].to,
            crate::pane::CopyTo::Register(Some('a'))
        );
        assert!(chat.picked.is_none(), "yanked, it's no longer selected");
    }

    #[test]
    fn copying_a_long_chat_lays_out_only_the_turns_at_its_ends() {
        let mut fonts = fonts();
        let mut node = crate::chat::fixtures::long_chat(60);
        node.turns[59].end = node.turns[0].end.clone();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        read(&mut chat, "VG");
        assert!(chat.cache.turns[30].laid.is_none());
        read(&mut chat, "y");
        let copied = yanked(&mut chat, &mut fonts).remove(0).0;
        assert!(copied.starts_with("Turn 0: why"), "{copied}");
        assert!(copied.contains("Turn 30: why"), "{copied}");
        assert!(copied.contains("Turn 59: why"), "{copied}");
        // As shown: folded work left out, and the answer without its
        // Markdown.
        assert!(!copied.contains("Compiling"), "{copied}");
        assert!(
            !copied.contains("**perl**") && copied.contains("needs perl"),
            "{copied}"
        );
        // What it didn't lay out, it didn't keep.
        assert!(chat.cache.turns[30].laid.is_none());
    }

    #[test]
    fn a_character_drawn_as_one_is_stepped_over_whole() {
        let mut fonts = fonts();
        // An e and an accent that joins it, then x.
        let mut chat = ChatView::new(String::new(), talk(&[("q", "e\u{301}x")]));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "0l");
        assert_eq!(chat.cursor.unwrap().byte, 3);
        read(&mut chat, "h");
        assert_eq!(chat.cursor.unwrap().byte, 0);
    }

    #[test]
    fn y_copies_the_block_of_code_under_the_cursor_and_so_does_its_button() {
        let mut fonts = fonts();
        let answer = "Run this:\n\n```sh\ncargo test\ncargo build\n```\n\nThen look.";
        let mut chat = ChatView::new(String::new(), talk(&[("how?", answer)]));
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        let code = *(chat.shown.iter())
            .find(|shown| shown.code)
            .expect("code drawn");
        let inside = [code.at[0] + 2.0, code.at[1] + 2.0];
        chat.press(inside, 1, false);
        chat.release();
        chat.settle(&mut fonts);
        read(&mut chat, "yix");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("cargo test\ncargo build\n")]
        );
        // With its fence, as the Markdown writes it.
        read(&mut chat, "yax");
        assert_eq!(
            yanked(&mut chat, &mut fonts),
            [lines("```sh\ncargo test\ncargo build\n```\n")]
        );
        // The mouse over it shows a button, which copies it, and says so.
        assert!(chat.hover(inside));
        paint(&mut chat, &mut fonts);
        let (_, button) = (chat.hits.iter())
            .find(|(hit, _)| matches!(hit, Hit::CopyCode(_)))
            .cloned()
            .expect("a copy button");
        chat.press([button[0] + 2.0, button[1] + 2.0], 1, false);
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("cargo test\ncargo build".to_owned(), Copy::Clipboard)]
        );
        assert_eq!(chat.copied_code, Some(code.key));
        // Away from it, it goes.
        assert!(chat.hover([1.0, 1.0]));
        assert_eq!(chat.hovered_code, None);
    }

    #[test]
    fn a_selection_over_a_folded_turn_leaves_its_work_out() {
        let mut fonts = fonts();
        let mut node = crate::chat::fixtures::long_chat(2);
        node.turns[1].end = node.turns[0].end.clone();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        read(&mut chat, "ggVGy");
        let copied = yanked(&mut chat, &mut fonts).remove(0).0;
        assert!(copied.starts_with("Turn 0: why"), "{copied}");
        assert!(copied.contains("The build fails on CI"), "{copied}");
        assert!(!copied.contains("Compiling"), "{copied}");
        assert!(!copied.contains("Let me build it"), "{copied}");
        assert!(copied.contains("Turn 1: why"), "{copied}");
    }
}
