//! Reading a chat in normal mode: a cursor in the conversation's text,
//! moved as in Vim, visual mode to select, `y` to copy, and `z` to open
//! and fold turns' work.
//!
//! The text is laid out in proportional fonts, so the cursor isn't on a
//! grid of cells, as the cursor of terminals and editors is (see
//! [`crate::motion`]): it's a place in a block of text, and moving up and
//! down goes by the lines the text was laid out in, keeping as near the
//! same distance across as it can. Moving off the end of a block goes on
//! to the next, as the conversation shows them. Keys move it at once, from
//! what was laid out last; what they copy waits for the fonts, as the turns
//! it takes in may need laying out.
//!
//! | Keys | What they do |
//! |---|---|
//! | `h` `j` `k` `l`, arrows | Move a character, or a line |
//! | `w` `b` `e` | Move by words |
//! | `0` `^` `$` | To the start of the line, its first word, its end |
//! | `{` `}` | To the block before, or after |
//! | `gg` `G` | To the start of the chat, or its end |
//! | Ctrl+D Ctrl+U, Ctrl+E Ctrl+Y | Scroll half a view, or a line |
//! | `v` `V` | Select characters, or lines; `o` goes to the other end |
//! | `y` | Copy what's selected, or the block of code, message or card under the cursor |
//! | `za` `zo` `zc`, Enter | Open or fold the turn's work; `zR` and `zM` all of them |
//! | `u`, Ctrl+R | Undo what the turn changed in files, or redo it; with nothing there, the last turn's, or the first's undone |
//! | `i` `a` `I` `A` `o`, Enter | Type a message |

use clackatoo_protocol::{ChatAction, ChatNode, Item};
use winit::keyboard::{Key, NamedKey};

use super::card::Card;
use super::paint::{AGENTS, FOLD, How, ITEMS, PROMPT, Picked, Position, TextKey, control};
use super::{ChatView, Input};
use crate::input::KeyPress;
use crate::pane::{Copy, Selection};

/// The first key of a two-key command, waiting for the second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prefix {
    /// `g`, and `gg` goes to the start.
    G,
    /// `z`, for opening and folding.
    Z,
}

/// Which way to move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    Back,
    On,
}

impl ChatView {
    /// What visual mode selects, while it does.
    pub(crate) fn visual(&self) -> Option<Selection> {
        match self.picked {
            Some(Picked {
                how: How::Visual(kind),
                ..
            }) => Some(kind),
            _ => None,
        }
    }

    /// Whether a two-key command is half typed, such as `z` waiting for
    /// `a`, which the next key is for.
    pub(crate) fn pending(&self) -> bool {
        self.read_prefix.is_some()
    }

    /// A key in normal or visual mode. Returns whether the chat must be
    /// redrawn, and whether the key goes to insert mode, to type a
    /// message.
    pub(crate) fn read_key(&mut self, press: &KeyPress<'_>) -> (bool, bool) {
        // What the window said of the chat lasts until the next key.
        let unsaid = self.unsay();
        let (redraw, insert) = self.read(press);
        (redraw || unsaid, insert)
    }

    /// A key in normal or visual mode, as [`read_key`](Self::read_key)
    /// says.
    fn read(&mut self, press: &KeyPress<'_>) -> (bool, bool) {
        let prefix = self.read_prefix.take();
        let modifiers = press.modifiers;
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        let visual = self.visual();
        if modifiers.ctrl {
            let redraw = match (character.map(|ch| ch.to_ascii_lowercase()), modifiers.shift) {
                // Ctrl+Shift+C copies, as in a terminal.
                (Some('c'), true) => {
                    self.copy_picked(Copy::Clipboard);
                    true
                }
                (Some('r'), false) => {
                    self.undo_key(true);
                    true
                }
                (Some('e'), _) => self.scroll_keeping_cursor(|chat| chat.scroll_lines(-1.0)),
                (Some('y'), _) => self.scroll_keeping_cursor(|chat| chat.scroll_lines(1.0)),
                (Some('d'), _) => self.scroll_keeping_cursor(|chat| chat.scroll_half(false)),
                (Some('u'), _) => self.scroll_keeping_cursor(|chat| chat.scroll_half(true)),
                _ => false,
            };
            return (redraw, false);
        }
        if prefix == Some(Prefix::Z) {
            return (self.fold_key(character), false);
        }
        match (character, press.key) {
            (Some('i' | 'a' | 'I' | 'A'), _) => {
                self.leave_visual();
                return (true, true);
            }
            (Some('o'), _) if visual.is_none() => return (true, true),
            (Some('o'), _) => {
                if let Some(picked) = &mut self.picked {
                    std::mem::swap(&mut picked.anchor, &mut picked.head);
                    self.cursor = Some(picked.head);
                    self.goal = None;
                    self.reveal = true;
                }
            }
            (_, Key::Named(NamedKey::Enter)) => {
                let Some(cursor) = self.cursor.filter(|cursor| control(cursor.text.piece)) else {
                    self.leave_visual();
                    return (true, true);
                };
                let TextKey { turn, piece, part } = cursor.text;
                match self.agent_on(turn, part).filter(|_| piece == AGENTS) {
                    Some(chat) => self.queued.push(Input::Act(ChatAction::Open { chat })),
                    None if piece == AGENTS => self.toggle_agents(turn),
                    None => self.toggle_fold(turn),
                }
            }
            (_, Key::Named(NamedKey::Escape)) => self.picked = None,
            (Some('v'), _) => self.visual_key(visual, Selection::Characters),
            (Some('V'), _) => self.visual_key(visual, Selection::Lines),
            (Some('y'), _) => self.yank(),
            (Some('u'), _) => self.undo_key(false),
            (Some('z'), _) => self.read_prefix = Some(Prefix::Z),
            (Some('g'), _) if prefix == Some(Prefix::G) => self.go_to_end(Way::Back),
            (Some('g'), _) => self.read_prefix = Some(Prefix::G),
            (Some('G'), _) => self.go_to_end(Way::On),
            (Some('j'), _) | (_, Key::Named(NamedKey::ArrowDown)) => self.vertical(Way::On),
            (Some('k'), _) | (_, Key::Named(NamedKey::ArrowUp)) => self.vertical(Way::Back),
            (Some('h'), _) | (_, Key::Named(NamedKey::ArrowLeft)) => self.across(Way::Back),
            (Some('l'), _) | (_, Key::Named(NamedKey::ArrowRight)) => self.across(Way::On),
            (Some('w'), _) => self.word(Way::On, false),
            (Some('e'), _) => self.word(Way::On, true),
            (Some('b'), _) => self.word(Way::Back, false),
            (Some(edge @ ('0' | '^' | '$')), _) => self.line_edge(edge),
            (Some('{'), _) => self.block(Way::Back),
            (Some('}'), _) => self.block(Way::On),
            (_, Key::Named(NamedKey::PageDown)) => {
                return (
                    self.scroll_keeping_cursor(|chat| chat.scroll_half(false)),
                    false,
                );
            }
            (_, Key::Named(NamedKey::PageUp)) => {
                return (
                    self.scroll_keeping_cursor(|chat| chat.scroll_half(true)),
                    false,
                );
            }
            _ => return (false, false),
        }
        // Visual mode's selection follows the cursor.
        if let (Some(picked), Some(cursor)) = (&mut self.picked, self.cursor)
            && matches!(picked.how, How::Visual(_))
        {
            picked.head = cursor;
        }
        (true, false)
    }

    /// `u`, or with `redo` Ctrl+R: undoes what the turn the cursor's in
    /// changed in files, or makes it again, as Vim's undo and redo do, if
    /// that turn's in view and has something to undo or redo; or else the
    /// last turn that has, or the first to redo. The bar says which, or
    /// that none has.
    fn undo_key(&mut self, redo: bool) {
        // The cursor's turn only if it's in view: what's undone is never
        // out of sight.
        let here = (self.cursor.map(|cursor| cursor.text.turn)).filter(|&turn| {
            (self.shown.iter()).any(|shown| shown.key.turn == turn)
                && (self.node.turns.get(turn)).is_some_and(|turn| super::undoable(turn, redo))
        });
        let Some(turn) = here.or_else(|| self.turn_to_undo(redo)) else {
            let undo = if redo { "redo" } else { "undo" };
            self.say(format!("no turn has files to {undo}"));
            return;
        };
        // It rewrites files: the bar says which turn's.
        let files = match self.node.turns[turn].files.map_or(0, |files| files.count) {
            1 => "1 file".to_owned(),
            count => format!("{count} files"),
        };
        let doing = if redo { "Redoing" } else { "Undoing" };
        let number = clackatoo_protocol::turn_number(&self.node.turns, turn).unwrap_or(turn + 1);
        self.say(format!("{doing} turn {number} ({files})"));
        let turn = u32::try_from(turn).unwrap_or(u32::MAX);
        self.queued.push(Input::Act(if redo {
            ChatAction::Redo { turn, force: false }
        } else {
            ChatAction::Undo { turn, force: false }
        }));
    }

    /// `z` and then `character`: opens or folds the work of the turn the
    /// cursor's in, or every turn's.
    fn fold_key(&mut self, character: Option<char>) -> bool {
        let turn = self.cursor.map(|cursor| cursor.text.turn);
        match (character, turn) {
            (Some('R'), _) => self.set_all_folds(true),
            (Some('M'), _) => self.set_all_folds(false),
            (Some('a'), Some(turn)) => self.toggle_fold(turn),
            (Some('o'), Some(turn)) => self.set_fold(turn, true),
            (Some('c'), Some(turn)) => self.set_fold(turn, false),
            _ => return false,
        }
        // What's folded away can't keep the cursor: it goes to the line
        // its work folds under.
        if let Some(cursor) = self.cursor
            && cursor.text.piece >= ITEMS
            && let Some(turn) = self.node.turns.get(cursor.text.turn)
        {
            let shape = self.shape(cursor.text.turn, turn);
            if !shape.open && cursor.text.piece - ITEMS < shape.answer {
                self.cursor = Some(Position {
                    text: TextKey {
                        piece: FOLD,
                        part: 0,
                        ..cursor.text
                    },
                    byte: 0,
                });
                self.picked = None;
            }
        }
        self.reveal = true;
        true
    }

    /// `v` or `V`: starts selecting `kind` from the cursor, or changes to
    /// it, or if visual mode selects it already, stops.
    fn visual_key(&mut self, visual: Option<Selection>, kind: Selection) {
        if visual == Some(kind) {
            self.picked = None;
            return;
        }
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let anchor = (self.picked)
            .filter(|_| visual.is_some())
            .map_or(cursor, |picked| picked.anchor);
        self.picked = Some(Picked {
            anchor,
            head: cursor,
            how: How::Visual(kind),
        });
    }

    /// Leaves visual mode, if it's in it, selecting nothing.
    fn leave_visual(&mut self) {
        if self.visual().is_some() {
            self.picked = None;
        }
    }

    /// `y`: copies what's selected, to the clipboard, or else what the
    /// cursor's in: a block of code, or a message, an answer or a card
    /// whole.
    fn yank(&mut self) {
        if self.picked_range().is_some() {
            self.copy_picked(Copy::Clipboard);
            return;
        }
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        if let Some(text) = self.text(cursor.text)
            && text.code()
        {
            let code = text.text.to_owned();
            self.queued.push(Input::CopyText(code));
            return;
        }
        if let Some(whole) = self.piece_text(cursor.text) {
            self.queued.push(Input::CopyText(whole));
        }
    }

    /// Copies what's selected to `to`, once the fonts are at hand, and
    /// stops selecting.
    pub(super) fn copy_picked(&mut self, to: Copy) {
        if let Some((start, end)) = self.picked_range() {
            self.queued.push(Input::CopyRange(start, end, to));
        }
        self.picked = None;
    }

    /// The whole of the piece text `key` is in, as it's copied: a message
    /// as it was typed, the agent's words as the Markdown they came as, a
    /// command and its output, a change as a diff, or for the line a
    /// turn's work folds under, the turn.
    pub(super) fn piece_text(&self, key: TextKey) -> Option<String> {
        let turn = self.node.turns.get(key.turn)?;
        match key.piece {
            PROMPT => Some(turn.prompt.clone()),
            AGENTS => Some(self.agents_text(key.turn)),
            FOLD => {
                let alone = ChatNode {
                    turns: vec![turn.clone()],
                    ..ChatNode::default()
                };
                Some(alone.text().trim_end().to_owned())
            }
            piece => match turn.items.get(piece - ITEMS)? {
                Item::Words(text) | Item::Thought(text) => Some(text.clone()),
                Item::Tool(tool) => {
                    Some(Card::of(tool).map_or_else(|| tool.title.clone(), |card| card.text()))
                }
            },
        }
    }

    /// The cursor, or if there's none yet, one at the start of the last
    /// line in view.
    fn cursor_or_default(&mut self) -> Option<Position> {
        if self.cursor.is_none() {
            self.cursor = self.default_cursor();
        }
        self.cursor
    }

    /// Where the cursor starts: the last line drawn fully in view.
    fn default_cursor(&self) -> Option<Position> {
        let bottom = self.view_height;
        let shown = (self.shown.iter().rev())
            .find(|shown| shown.at[1] + shown.size[1] <= bottom)
            .or_else(|| self.shown.first())?;
        let text = self.text(shown.key)?;
        let lines = text.layout.len();
        let line = (0..lines)
            .rev()
            .find(|&line| {
                super::paint::line_middle(text.layout, line)
                    .is_some_and(|(middle, _)| shown.at[1] + middle < bottom)
            })
            .unwrap_or(0);
        let byte = super::paint::line_middle(text.layout, line).map_or(0, |(_, range)| range.start);
        Some(Position {
            text: shown.key,
            byte,
        })
    }

    /// Puts the cursor at `position`, and scrolls to it.
    fn go(&mut self, position: Position) {
        self.cursor = Some(position);
        self.reveal = true;
    }

    /// The block of text after `key`, or before, as the conversation shows
    /// them. A turn not laid out gives the start of it, which is laid out
    /// as the view goes to it.
    fn next_text(&self, key: TextKey, way: Way) -> Option<TextKey> {
        let mut turn = key.turn;
        loop {
            let texts = self.turn_texts(turn);
            let found = match way {
                Way::On => texts.into_iter().find(|text| *text > key),
                Way::Back => texts.into_iter().rev().find(|text| *text < key),
            };
            if found.is_some() {
                return found;
            }
            turn = match way {
                Way::On => Some(turn + 1).filter(|&next| next < self.node.turns.len())?,
                Way::Back => turn.checked_sub(1)?,
            };
            if (self.cache.turns.get(turn)).is_none_or(|slot| !slot.laid_out()) {
                return Some(TextKey {
                    turn,
                    piece: PROMPT,
                    part: 0,
                });
            }
        }
    }

    /// `j` or `k`: a line down or up, as near the same distance across as
    /// the text allows.
    fn vertical(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            if let Some(key) = self.next_text(cursor.text, way) {
                self.go(Position { text: key, byte: 0 });
            }
            return;
        };
        let goal = self
            .goal
            .unwrap_or_else(|| text.at[0] + super::paint::x_of(text.layout, cursor.byte));
        let line = super::paint::line_of(text.layout, cursor.byte);
        let within = match way {
            Way::On => Some(line + 1).filter(|&next| next < text.layout.len()),
            Way::Back => line.checked_sub(1),
        };
        let (key, line) = if let Some(line) = within {
            (cursor.text, Some(line))
        } else {
            let Some(key) = self.next_text(cursor.text, way) else {
                return;
            };
            let line = self.text(key).map(|text| match way {
                Way::On => 0,
                Way::Back => text.layout.len().saturating_sub(1),
            });
            (key, line)
        };
        // On a character, as in Vim: not past the line's last.
        let byte = match (self.text(key), line) {
            (Some(text), Some(line)) => {
                super::paint::line_middle(text.layout, line).map_or(0, |(middle, range)| {
                    let x = goal - text.at[0];
                    let byte = parley::Cursor::from_point(text.layout, x, middle).index();
                    byte.min(last_char(text.text, range))
                })
            }
            _ => 0,
        };
        self.go(Position { text: key, byte });
        self.goal = Some(goal);
    }

    /// `h` or `l`: a character back or on, within the line.
    fn across(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((_, range)) = super::paint::line_middle(text.layout, line) else {
            return;
        };
        let byte = super::paint::within(text.text, cursor.byte);
        let byte = match way {
            // By what's drawn as one character, as an emoji of several.
            Way::Back if byte > range.start => {
                super::paint::before(text.text, byte).max(range.start)
            }
            Way::On if byte < last_char(text.text, range.clone()) => {
                super::paint::after(text.text, byte)
            }
            _ => byte,
        };
        self.go(Position { byte, ..cursor });
        self.goal = None;
    }

    /// `0`, `^` or `$`: the start of the line, its first character that
    /// isn't blank, or its last character.
    fn line_edge(&mut self, edge: char) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let line = super::paint::line_of(text.layout, cursor.byte);
        let Some((_, range)) = super::paint::line_middle(text.layout, line) else {
            return;
        };
        let byte = match edge {
            '0' => range.start,
            '^' => {
                let line = &text.text[range.start..range.end.min(text.text.len())];
                range.start + line.len() - line.trim_start().len()
            }
            _ => last_char(text.text, range),
        };
        self.go(Position { byte, ..cursor });
        self.goal = None;
    }

    /// `w`, `e` or `b`: the start of the next word, the end of this one or
    /// the next, or the start of this one or the one before, going on to
    /// the next block, or back to the one before, at either end.
    fn word(&mut self, way: Way, end: bool) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let Some(text) = self.text(cursor.text) else {
            return;
        };
        let byte = super::paint::within(text.text, cursor.byte);
        let found = match (way, end) {
            (Way::On, false) => next_word_start(text.text, byte),
            (Way::On, true) => word_end(text.text, byte),
            (Way::Back, _) => word_start_before(text.text, byte),
        };
        if let Some(byte) = found {
            self.go(Position { byte, ..cursor });
        } else if let Some(key) = self.next_text(cursor.text, way) {
            let byte = match (way, self.text(key)) {
                (Way::Back, Some(text)) => {
                    word_start_before(text.text, text.text.len()).unwrap_or(0)
                }
                (Way::On, Some(text)) if end => word_end(text.text, 0).unwrap_or(0),
                _ => 0,
            };
            self.go(Position { text: key, byte });
        }
        self.goal = None;
    }

    /// `{` or `}`: the start of the block before, or after.
    fn block(&mut self, way: Way) {
        let Some(cursor) = self.cursor_or_default() else {
            return;
        };
        let key = match way {
            Way::Back if cursor.byte > 0 => Some(cursor.text),
            _ => self.next_text(cursor.text, way),
        };
        if let Some(key) = key {
            self.go(Position { text: key, byte: 0 });
        }
        self.goal = None;
    }

    /// `gg` or `G`: the start of the chat, or the start of its last line.
    fn go_to_end(&mut self, way: Way) {
        let Some(last) = self.node.turns.len().checked_sub(1) else {
            return;
        };
        let turn = if way == Way::Back { 0 } else { last };
        let texts = self.turn_texts(turn);
        let key = match way {
            Way::Back => texts.first(),
            Way::On => texts.last(),
        };
        let position = match key {
            Some(&key) => {
                let byte = match (way, self.text(key)) {
                    (Way::On, Some(text)) => {
                        let line = text.layout.len().saturating_sub(1);
                        super::paint::line_middle(text.layout, line)
                            .map_or(0, |(_, range)| range.start)
                    }
                    _ => 0,
                };
                Position { text: key, byte }
            }
            // Not laid out: its start, or past its end, until it is.
            None if way == Way::Back => Position {
                text: TextKey {
                    turn,
                    piece: PROMPT,
                    part: 0,
                },
                byte: 0,
            },
            None => Position {
                text: TextKey {
                    turn,
                    piece: usize::MAX,
                    part: 0,
                },
                byte: usize::MAX,
            },
        };
        self.go(position);
        self.goal = None;
        if way == Way::On {
            self.follow = true;
        }
    }

    /// Scrolls as `scroll` does, and if the cursor goes out of view, it
    /// comes back into it, at the edge it went past, once the chat's
    /// drawn. Returns whether the view moved.
    fn scroll_keeping_cursor(&mut self, scroll: impl FnOnce(&mut Self) -> bool) -> bool {
        let moved = scroll(self);
        self.keep_cursor = moved;
        moved
    }

    /// Puts the cursor on text its turn shows, now it's laid out, if it
    /// was put at the start or the end of the turn before it was: on the
    /// first text, or the start of the last line of the last.
    pub(super) fn cursor_onto_text(&mut self) {
        let Some(cursor) = self.cursor else {
            return;
        };
        if self.text(cursor.text).is_some() {
            return;
        }
        let texts = self.turn_texts(cursor.text.turn);
        let back = texts.iter().rev().find(|&&key| key <= cursor.text).copied();
        let Some(key) = back.or_else(|| texts.first().copied()) else {
            return;
        };
        let byte = match (back, self.text(key)) {
            (Some(_), Some(text)) => {
                let line = text.layout.len().saturating_sub(1);
                super::paint::line_middle(text.layout, line).map_or(0, |(_, range)| range.start)
            }
            _ => 0,
        };
        let position = Position { text: key, byte };
        self.cursor = Some(position);
        if let Some(picked) = &mut self.picked
            && matches!(picked.how, How::Visual(_))
            && picked.head == cursor
        {
            picked.head = position;
        }
    }

    /// Brings the cursor back into view, if scrolling took it out: to the
    /// first line in view, or the last.
    pub(super) fn cursor_into_view(&mut self) {
        if !std::mem::take(&mut self.keep_cursor) {
            return;
        }
        let Some(cursor) = self.cursor else {
            return;
        };
        let height = self.view_height;
        let in_view = |shown: &&super::paint::Shown| {
            !control(shown.key.piece) && shown.at[1] >= 0.0 && shown.at[1] + shown.size[1] <= height
        };
        if self
            .shown
            .iter()
            .filter(in_view)
            .any(|shown| shown.key == cursor.text)
        {
            return;
        }
        let above = (self.shown.first()).is_some_and(|first| cursor.text < first.key);
        let shown = if above {
            self.shown.iter().find(in_view)
        } else {
            self.shown.iter().rev().find(in_view)
        };
        if let Some(shown) = shown {
            let key = shown.key;
            let byte = match (above, self.text(key)) {
                (false, Some(text)) => {
                    let line = text.layout.len().saturating_sub(1);
                    super::paint::line_middle(text.layout, line).map_or(0, |(_, range)| range.start)
                }
                _ => 0,
            };
            self.cursor = Some(Position { text: key, byte });
            if let Some(picked) = &mut self.picked
                && matches!(picked.how, How::Visual(_))
            {
                picked.head = Position { text: key, byte };
            }
        }
    }
}

/// The start of the last character of the line of `text` in `range`,
/// before its line break.
fn last_char(text: &str, range: std::ops::Range<usize>) -> usize {
    let end = range.end.min(text.len());
    let line = text[range.start.min(end)..end].trim_end_matches(['\n', '\r']);
    line.char_indices()
        .next_back()
        .map_or(range.start, |(at, _)| range.start + at)
}

/// What sort of character `ch` is, for words: blank, a word's, or other
/// punctuation, as Vim's `w` sees them.
fn class(ch: char) -> u8 {
    if ch.is_whitespace() {
        0
    } else if ch.is_alphanumeric() || ch == '_' {
        1
    } else {
        2
    }
}

/// Where the next word after the one at `byte` in `text` starts, if one
/// does.
fn next_word_start(text: &str, byte: usize) -> Option<usize> {
    let mut chars = text[byte..].char_indices().map(|(at, ch)| (byte + at, ch));
    let (_, first) = chars.next()?;
    let mut was = class(first);
    for (at, ch) in chars {
        let is = class(ch);
        if is != 0 && is != was {
            return Some(at);
        }
        was = is;
    }
    None
}

/// Where the word at `byte` in `text` ends, or if `byte` is at its end
/// already, the next one: its last character.
fn word_end(text: &str, byte: usize) -> Option<usize> {
    let chars: Vec<(usize, char)> = text[byte..]
        .char_indices()
        .map(|(at, ch)| (byte + at, ch))
        .collect();
    let mut index = 1;
    while index < chars.len() && class(chars[index].1) == 0 {
        index += 1;
    }
    let class_here = class(chars.get(index)?.1);
    while index + 1 < chars.len() && class(chars[index + 1].1) == class_here {
        index += 1;
    }
    Some(chars[index].0)
}

/// Where the word before `byte` in `text` starts, or the one `byte` is
/// in, if it's not at its start.
fn word_start_before(text: &str, byte: usize) -> Option<usize> {
    let chars: Vec<(usize, char)> = text[..byte].char_indices().collect();
    let mut index = chars.len().checked_sub(1)?;
    while class(chars[index].1) == 0 {
        index = index.checked_sub(1)?;
    }
    let class_here = class(chars[index].1);
    while index > 0 && class(chars[index - 1].1) == class_here {
        index -= 1;
    }
    Some(chars[index].0)
}

#[cfg(test)]
mod tests {
    use winit::keyboard::NamedKey;

    use super::{next_word_start, word_end, word_start_before};
    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, paint, read, read_named, talk};
    use crate::chat::paint::{FOLD, ITEMS, PROMPT, TextKey};
    use crate::pane::{Copy, Selection};

    fn reading(node: clackatoo_protocol::ChatNode) -> (ChatView, crate::fonts::Fonts) {
        let mut fonts = fonts();
        let mut chat = ChatView::new(String::new(), node);
        chat.reading = true;
        paint(&mut chat, &mut fonts);
        (chat, fonts)
    }

    fn key(turn: usize, piece: usize) -> TextKey {
        TextKey {
            turn,
            piece,
            part: 0,
        }
    }

    #[test]
    fn the_cursor_starts_at_the_end_and_moves_through_the_text() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "first answer"), ("two", "second")]));
        // Nothing's moved it: it's on the last line in view.
        read(&mut chat, "l");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(1, ITEMS)));
        assert_eq!(chat.cursor.unwrap().byte, 1);
        read(&mut chat, "gg");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, PROMPT)));
        // `j` keeps to the same distance across, and the message is on
        // the right: `}` goes to the answer's start.
        read(&mut chat, "}w");
        let cursor = chat.cursor.unwrap();
        assert_eq!((cursor.text, cursor.byte), (key(0, ITEMS), 6));
        read(&mut chat, "$");
        assert_eq!(chat.cursor.unwrap().byte, "first answer".len() - 1);
        read(&mut chat, "jk0");
        assert_eq!(chat.cursor.unwrap().byte, 0);
        paint(&mut chat, &mut fonts);
        // `G` goes to the end.
        read(&mut chat, "G");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(1, ITEMS)));
    }

    #[test]
    fn visual_mode_copies_characters_or_lines_across_messages() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "first answer"), ("two", "second")]));
        read(&mut chat, "ggVj");
        assert_eq!(chat.visual(), Some(Selection::Lines));
        assert_eq!(read(&mut chat, "y"), (true, false));
        assert_eq!(chat.visual(), None);
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("one\n\nfirst answer".to_owned(), Copy::Clipboard)]
        );
        // Characters: the cursor's own included.
        read(&mut chat, "gg}wvly");
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("an".to_owned(), Copy::Clipboard)]
        );
        // `o` goes to the other end, and Esc stops selecting.
        read(&mut chat, "vho");
        read_named(&mut chat, NamedKey::Escape);
        assert_eq!(chat.visual(), None);
    }

    #[test]
    fn enter_and_z_open_and_fold_the_turn_under_the_cursor() {
        let node = crate::chat::fixtures::long_chat(2);
        let (mut chat, mut fonts) = reading(node);
        read(&mut chat, "gg");
        paint(&mut chat, &mut fonts);
        // The block after the message: the line its work folds under.
        read(&mut chat, "}");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, FOLD)));
        let open = |chat: &ChatView| chat.shape(0, &chat.node.turns[0]).open;
        assert!(!open(&chat));
        assert_eq!(read_named(&mut chat, NamedKey::Enter), (true, false));
        assert!(open(&chat));
        paint(&mut chat, &mut fonts);
        // Into the work, and `zc` folds it, the cursor going to its line.
        read(&mut chat, "}");
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, ITEMS)));
        read(&mut chat, "zc");
        assert!(!open(&chat));
        assert_eq!(chat.cursor.map(|cursor| cursor.text), Some(key(0, FOLD)));
        read(&mut chat, "zo");
        assert!(open(&chat));
        read(&mut chat, "zM");
        assert!(!open(&chat));
        read(&mut chat, "za");
        assert!(open(&chat));
        // Enter anywhere else types a message.
        paint(&mut chat, &mut fonts);
        read(&mut chat, "gg");
        assert_eq!(read_named(&mut chat, NamedKey::Enter), (true, true));
    }

    #[test]
    fn y_copies_a_message_or_an_answer_whole() {
        let (mut chat, mut fonts) = reading(talk(&[("one", "**bold** answer")]));
        read(&mut chat, "y");
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("**bold** answer".to_owned(), Copy::Clipboard)]
        );
        read(&mut chat, "ggy");
        assert_eq!(
            chat.settle(&mut fonts).copied,
            [("one".to_owned(), Copy::Clipboard)]
        );
    }

    #[test]
    fn u_undoes_the_turn_under_the_cursor_or_else_the_last() {
        use clackatoo_protocol::{ChatAction, TurnFiles, TurnFilesState};

        let mut node = talk(&[("one", "first"), ("two", "second"), ("three", "third")]);
        for turn in &mut node.turns[..2] {
            turn.files = Some(TurnFiles {
                count: 1,
                state: TurnFilesState::Made,
            });
        }
        let (mut chat, mut fonts) = reading(node);
        let undo = |turn| ChatAction::Undo { turn, force: false };
        // On the last turn, which changed nothing: the last that did.
        read(&mut chat, "u");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(1)]);
        // It rewrites files: the bar says whose.
        assert_eq!(chat.label().as_deref(), Some("Undoing turn 2 (1 file)"));
        read(&mut chat, "ggu");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(0)]);
        // The cursor's turn out of view, as scrolled away from: the last.
        chat.shown.retain(|shown| shown.key.turn != 0);
        read(&mut chat, "u");
        assert_eq!(chat.settle(&mut fonts).actions, [undo(1)]);
        // Nothing to redo: the bar says so, until the next key.
        let ctrl_r = |chat: &mut ChatView| {
            chat.read_key(&crate::input::KeyPress {
                key: &winit::keyboard::Key::Character("r".into()),
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: crate::input::Modifiers {
                    ctrl: true,
                    ..crate::input::Modifiers::default()
                },
            })
        };
        assert_eq!(ctrl_r(&mut chat), (true, false));
        assert_eq!(chat.label().as_deref(), Some("no turn has files to redo"));
        read(&mut chat, "j");
        assert_eq!(chat.label(), None);
    }

    #[test]
    fn words_are_found_as_vim_finds_them() {
        let text = "fn main() {  x }";
        assert_eq!(next_word_start(text, 0), Some(3));
        assert_eq!(next_word_start(text, 3), Some(7));
        assert_eq!(next_word_start(text, 10), Some(13));
        assert_eq!(word_end(text, 0), Some(1));
        assert_eq!(word_end(text, 1), Some(6));
        assert_eq!(word_start_before(text, 7), Some(3));
        assert_eq!(word_start_before(text, 5), Some(3));
        assert_eq!(word_start_before(text, 0), None);
        assert_eq!(next_word_start("end", 0), None);
    }
}
