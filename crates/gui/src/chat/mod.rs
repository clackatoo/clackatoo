//! A chat with an agent, in a pane: the conversation, and the box a
//! message is typed in.
//!
//! The daemon keeps the chat. The window keeps a copy, current with the
//! daemon's patches, lays it out at the pane's width, and draws it with
//! the window's own fills and fonts: messages the user sent are bubbles on
//! the right, and the agent's words, read as Markdown, run the full width
//! of a column down the middle.
//!
//! Editing the message needs the fonts, to lay it out, and keys arrive
//! without them, so what's typed, pasted or clicked waits in a queue until
//! the window [settles](ChatView::settle) it, which it does before sending
//! anything to the daemon and before drawing.

mod card;
#[cfg(test)]
pub(crate) mod fixtures;
mod markdown;
mod paint;
mod picker;
mod read;

use clackatoo_protocol::{
    ChatAction, ChatNode, ChoiceKind, Setting, SettingKind, TurnFilesState, ViewUpdate,
};
use parley::{LayoutContext, PlainEditor};
use winit::keyboard::{Key, NamedKey};

use crate::fonts::Fonts;
use crate::input::{KeyPress, Modifiers};
use crate::pane::Copy;
use paint::{How, Picked, Position, Shown, TextKey};
use picker::{Row, Spot};

pub(crate) use paint::ChatPaint;

/// How a piece of the chat's text is drawn, carried through layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Brush {
    pub(crate) tone: Tone,
    /// It's code, on a background of its own.
    pub(crate) code: bool,
}

/// The colour of a piece of the chat's text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Tone {
    #[default]
    Plain,
    Muted,
    Accent,
    Warning,
    /// On something in the accent colour.
    OnAccent,
    /// Added to a file.
    Added,
    /// Removed from a file.
    Removed,
}

/// Something done to the chat that waits for the fonts.
#[derive(Debug)]
enum Input {
    Key {
        key: Key,
        text: Option<String>,
        modifiers: Modifiers,
    },
    Paste(String),
    /// A mouse button went down at a point in the pane.
    Press {
        at: [f32; 2],
        clicks: u8,
        shift: bool,
    },
    /// The mouse moved to a point, with a button held.
    Drag([f32; 2]),
    /// The button was let go.
    Release,
    /// The input method is composing this text, with its cursor over this
    /// stretch of it, or has stopped.
    Compose(Option<(String, Option<(usize, usize)>)>),
    /// Copies the conversation's text from one place to another.
    CopyRange(Position, Position, Copy),
    /// Copies this to the clipboard.
    CopyText(String),
    /// Asks this of the daemon.
    Act(ChatAction),
}

/// What settling the chat's queue came to.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    /// It must be redrawn.
    pub(crate) redraw: bool,
    /// Asks for the daemon.
    pub(crate) actions: Vec<ChatAction>,
    /// Text to put on the clipboard, or the primary selection, oldest
    /// first.
    pub(crate) copied: Vec<(String, Copy)>,
}

/// Something in the chat that does something when clicked.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Send,
    Stop,
    /// One of the answers to the agent's question, by its ID.
    Choice(String),
    /// The message being typed.
    Editor,
    /// A button that opens the menu for these settings, by their IDs.
    Chip(Vec<String>),
    /// One of a setting's choices, in its menu.
    Pick {
        setting: String,
        value: String,
    },
    /// The button that shows or hides a model family's versions, by its
    /// name.
    Expand(String),
    /// A step's share of a slider, to click or drag to.
    Step {
        setting: String,
        value: String,
    },
    /// The button beside a slider that leaves its setting to the agent.
    Reset {
        setting: String,
        value: String,
    },
    /// A tool's card, which opens and folds, by the turn it's in and the
    /// tool's ID.
    Card(usize, String),
    /// The button that undoes what a turn changed in files, by the turn.
    Undo(usize),
    /// The button that redoes it, once undone.
    Redo(usize),
    /// The line a turn's work folds under, which opens and folds it, by
    /// the turn.
    Fold(usize),
    /// The button that copies a block of code.
    CopyCode(TextKey),
}

/// What the mouse pointer looks like over a chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pointer {
    /// Over nothing that takes a click.
    Arrow,
    /// Over something to click: a button, a chip, a menu's choice.
    Hand,
    /// Over text to type in or select.
    Text,
}

/// A menu open over the box, to pick the agent's settings.
#[derive(Clone, Debug, PartialEq)]
struct Menu {
    /// The settings it picks from, by their IDs: a section each.
    settings: Vec<String>,
    /// Where the keyboard or the mouse is.
    highlighted: Option<Spot>,
    /// The model family whose versions are shown, by its name.
    expanded: Option<String>,
    /// A slider's setting and the value it was last slid to, until the
    /// agent says it's set, for it to stay where it was put meanwhile.
    held: Option<(String, String)>,
}

impl Menu {
    /// What `setting` is, or is about to be.
    fn value<'a>(&'a self, setting: &'a Setting) -> &'a str {
        match &self.held {
            Some((id, value)) if *id == setting.id => value,
            _ => &setting.value,
        }
    }
}

/// The window's side of a chat.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent states of what the pane shows"
)]
pub(crate) struct ChatView {
    pub(crate) node: ChatNode,
    pub(crate) title: String,
    /// The message being typed.
    editor: PlainEditor<Brush>,
    /// The size the editor's text was last styled at.
    editor_size: f32,
    layout_cx: LayoutContext<Brush>,
    queued: Vec<Input>,
    /// Laid-out turns, kept while their text and the width stay the same.
    cache: paint::Cache,
    /// How far down the conversation the view is, in pixels from its top.
    scroll: f32,
    /// Whether the view stays at the end as the conversation grows.
    follow: bool,
    /// The most the view can scroll, when last drawn.
    max_scroll: f32,
    /// The view's height, when last drawn.
    view_height: f32,
    /// A line's height, when last drawn.
    line_height: f32,
    /// What was drawn where, last time, from the pane's top left: for
    /// clicks.
    hits: Vec<(Hit, [f32; 4])>,
    /// Where the editor's text was drawn, from the pane's top left.
    editor_origin: [f32; 2],
    /// A drag that started in the editor selects as it goes.
    selecting: bool,
    /// A drag that started on a slider moves it as it goes: its setting.
    sliding: Option<String>,
    /// The settings menu, while it's open.
    menu: Option<Menu>,
    /// Where the chip that opened the menu was drawn, for the menu to open
    /// from, while drawing.
    menu_anchor: Option<[f32; 4]>,
    /// The tools whose cards are open, showing all they have, by their
    /// IDs.
    open_cards: std::collections::HashSet<String>,
    /// What the window says about the chat in the bottom bar, until the
    /// next key, such as that there's nothing to undo.
    said: Option<String>,
    /// The turns whose work was opened or folded, by their index, and
    /// which. A turn not here is open while it's going, and folded once
    /// it's done.
    folds: std::collections::HashMap<usize, bool>,
    /// The time, as turns count it, if it's not the clock's, as for tests.
    clock: Option<u64>,
    /// When the next turn still going counts on to the next second, if one
    /// was drawn.
    live: Option<std::time::Instant>,
    /// The text drawn last, top to bottom, and where.
    shown: Vec<Shown>,
    /// Text selected in the conversation.
    picked: Option<Picked>,
    /// A drag that started on the conversation's text selects as it goes.
    picking: bool,
    /// Whether the pane is in normal or visual mode, reading the chat
    /// rather than typing in it.
    pub(crate) reading: bool,
    /// The cursor in the conversation, in normal mode.
    cursor: Option<Position>,
    /// How far across the column `j` and `k` keep the cursor.
    goal: Option<f32>,
    /// The cursor moved: the view goes to it.
    reveal: bool,
    /// The view scrolled: the cursor stays in it.
    keep_cursor: bool,
    /// The first key of a two-key command in normal mode.
    read_prefix: Option<read::Prefix>,
    /// The block of code under the mouse, which shows a button to copy it.
    hovered_code: Option<TextKey>,
    /// The block of code just copied with its button, which says so.
    copied_code: Option<TextKey>,
    /// The column's right edge, from the pane's left, when last drawn.
    column_right: f32,
}

impl std::fmt::Debug for ChatView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChatView")
            .field("node", &self.node)
            .field("title", &self.title)
            .field("typed", &self.editor.raw_text())
            .field("scroll", &self.scroll)
            .finish_non_exhaustive()
    }
}

impl ChatView {
    pub(crate) fn new(title: String, node: ChatNode) -> Self {
        Self {
            node,
            title,
            editor: PlainEditor::new(16.0),
            editor_size: 0.0,
            layout_cx: LayoutContext::new(),
            queued: Vec::new(),
            cache: paint::Cache::default(),
            scroll: 0.0,
            follow: true,
            max_scroll: 0.0,
            view_height: 0.0,
            line_height: 16.0,
            hits: Vec::new(),
            editor_origin: [0.0; 2],
            selecting: false,
            sliding: None,
            menu: None,
            menu_anchor: None,
            open_cards: std::collections::HashSet::new(),
            said: None,
            folds: std::collections::HashMap::new(),
            clock: None,
            live: None,
            shown: Vec::new(),
            picked: None,
            picking: false,
            reading: false,
            cursor: None,
            goal: None,
            reveal: false,
            keep_cursor: false,
            read_prefix: None,
            hovered_code: None,
            copied_code: None,
            column_right: 0.0,
        }
    }

    /// Applies an update from the daemon.
    pub(crate) fn apply(&mut self, update: &ViewUpdate) {
        match update {
            ViewUpdate::Snapshot(view) => {
                if let Some(chat) = view.chat() {
                    chat.clone_into(&mut self.node);
                }
                view.title.clone_into(&mut self.title);
                for slot in &mut self.cache.turns {
                    slot.stale = true;
                }
            }
            ViewUpdate::Patch(patches) => {
                for patch in patches {
                    match patch {
                        clackatoo_protocol::Patch::Title(title) => {
                            title.clone_into(&mut self.title);
                        }
                        clackatoo_protocol::Patch::Chat(patch) => {
                            if let Some(turn) = touches(patch) {
                                self.restyled(turn);
                            }
                            self.node.apply(patch.clone());
                        }
                        clackatoo_protocol::Patch::Grid { .. }
                        | clackatoo_protocol::Patch::Cwd(_) => {}
                    }
                }
            }
        }
        // The agent has set what a slider was slid to.
        if let Some(menu) = &mut self.menu
            && let Some((id, value)) = &menu.held
            && (self.node.settings.iter())
                .any(|setting| setting.id == *id && setting.value == *value)
        {
            menu.held = None;
        }
    }

    /// Turn `index` must be laid out again, as it's changed, or how it's
    /// shown has.
    fn restyled(&mut self, index: usize) {
        if let Some(slot) = self.cache.turns.get_mut(index) {
            slot.stale = true;
        }
    }

    /// When the chat must be drawn again for a turn still going to count
    /// on a second, if it showed one when last drawn.
    pub(crate) fn live(&self) -> Option<std::time::Instant> {
        self.live
    }

    /// Whether the agent is answering, or starting to.
    fn busy(&self) -> bool {
        self.node
            .turns
            .last()
            .is_some_and(|turn| turn.end.is_none())
    }

    // --- Input, queued until the fonts are at hand --------------------------

    pub(crate) fn key(&mut self, press: &KeyPress<'_>) {
        self.said = None;
        self.queued.push(Input::Key {
            key: press.key.clone(),
            text: press.text.map(str::to_owned),
            modifiers: press.modifiers,
        });
    }

    pub(crate) fn paste(&mut self, text: &str) {
        self.queued.push(Input::Paste(text.to_owned()));
    }

    /// A mouse button went down at `at`, from the pane's top left.
    pub(crate) fn press(&mut self, at: [f32; 2], clicks: u8, shift: bool) {
        self.queued.push(Input::Press { at, clicks, shift });
    }

    /// The mouse moved to `at` with a button held.
    pub(crate) fn drag(&mut self, at: [f32; 2]) {
        self.queued.push(Input::Drag(at));
    }

    pub(crate) fn release(&mut self) {
        self.queued.push(Input::Release);
    }

    /// The input method is composing `text`, with its cursor over that
    /// stretch of it, or has stopped, with `None`.
    pub(crate) fn compose(&mut self, composing: Option<(String, Option<(usize, usize)>)>) {
        self.queued.push(Input::Compose(composing));
    }

    /// How the mouse pointer looks at `at`, from the pane's top left, by
    /// what was drawn there last.
    pub(crate) fn pointer(&self, at: [f32; 2]) -> Pointer {
        let hit = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .map(|(hit, _)| hit);
        match hit {
            Some(Hit::Editor) => Pointer::Text,
            Some(_) => Pointer::Hand,
            None if self.over_text(at) => Pointer::Text,
            None => Pointer::Arrow,
        }
    }

    /// The mouse moved to `at` with no button held: over a menu, its
    /// choice or slider there lights up. Returns whether the chat must be
    /// redrawn.
    pub(crate) fn hover(&mut self, at: [f32; 2]) -> bool {
        // A block of code under the mouse shows a button to copy it.
        let code = self.code_at(at).filter(|_| self.menu.is_none());
        let mut redraw = code != self.hovered_code;
        if redraw {
            self.hovered_code = code;
            self.copied_code = None;
        }
        redraw |= self.hover_menu(at);
        redraw
    }

    /// The mouse left the pane: the button that copies a block of code
    /// goes. Returns whether the chat must be redrawn.
    pub(crate) fn unhover(&mut self) -> bool {
        self.copied_code = None;
        self.hovered_code.take().is_some()
    }

    /// The mouse moved to `at` with no button held, over the menu, if
    /// it's open. Returns whether it must be redrawn.
    fn hover_menu(&mut self, at: [f32; 2]) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        // What's drawn last is on top, as the menu is over the cards.
        let under = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .and_then(|(hit, _)| match hit {
                Hit::Pick { setting, value } => Some(Spot::Choice {
                    setting: setting.clone(),
                    value: value.clone(),
                }),
                Hit::Step { setting, .. } | Hit::Reset { setting, .. } => {
                    Some(Spot::Slider(setting.clone()))
                }
                _ => None,
            });
        if under.is_some() && under != menu.highlighted {
            menu.highlighted = under;
            return true;
        }
        false
    }

    /// Whether input waits to be settled.
    pub(crate) fn unsettled(&self) -> bool {
        !self.queued.is_empty()
    }

    /// Carries out what's waiting in the queue, now that the fonts are at
    /// hand.
    pub(crate) fn settle(&mut self, fonts: &mut Fonts) -> Settled {
        let mut settled = Settled::default();
        if self.queued.is_empty() {
            return settled;
        }
        self.style_editor(fonts);
        for input in std::mem::take(&mut self.queued) {
            // A drag that started nothing changes nothing.
            settled.redraw |= !matches!(input, Input::Drag(_))
                || self.selecting
                || self.sliding.is_some()
                || self.picking;
            match input {
                Input::Key {
                    key,
                    text,
                    modifiers,
                } => self.typed(fonts, &key, text.as_deref(), modifiers, &mut settled),
                Input::Paste(text) => {
                    if self.node.question.is_none() {
                        let text = text.replace("\r\n", "\n");
                        self.drive(fonts, |driver| driver.insert_or_replace_selection(&text));
                    }
                }
                Input::Press { at, clicks, shift } => {
                    self.pressed(fonts, at, clicks, shift, &mut settled);
                }
                // A drag does what the press it follows started, if anything.
                Input::Drag(at) if self.sliding.is_some() => self.slide(at[0], &mut settled),
                Input::Drag(at) if self.selecting => {
                    let [x, y] = self.in_editor(at);
                    self.drive(fonts, |driver| driver.extend_selection_to_point(x, y));
                }
                Input::Drag(at) if self.picking => {
                    let head = self.spot_at(at, true);
                    if let (Some(picked), Some(head)) = (self.picked.as_mut(), head) {
                        picked.head = head;
                    }
                }
                Input::Drag(_) => {}
                Input::Release => {
                    self.selecting = false;
                    self.sliding = None;
                    // Text selected with the mouse goes to the primary
                    // selection, as elsewhere on Linux.
                    if std::mem::take(&mut self.picking) {
                        match self.picked_range() {
                            Some((start, end)) => {
                                let text = self.text_between(fonts, start, end);
                                settled.copied.push((text, Copy::Primary));
                            }
                            // A click selects nothing.
                            None => self.picked = None,
                        }
                    }
                }
                Input::CopyRange(start, end, to) => {
                    let text = self.text_between(fonts, start, end);
                    settled.copied.push((text, to));
                }
                Input::CopyText(text) => settled.copied.push((text, Copy::Clipboard)),
                Input::Act(action) => settled.actions.push(action),
                Input::Compose(Some((text, cursor))) => {
                    self.drive(fonts, |driver| driver.set_compose(&text, cursor));
                }
                Input::Compose(None) => self.drive(fonts, |driver| {
                    driver.clear_compose();
                }),
            }
        }
        settled
    }

    /// Edits the message with parley's editor.
    fn drive(
        &mut self,
        fonts: &mut Fonts,
        edit: impl FnOnce(&mut parley::PlainEditorDriver<'_, Brush>),
    ) {
        let mut driver = self.editor.driver(&mut fonts.font_cx, &mut self.layout_cx);
        edit(&mut driver);
    }

    /// A key typed into the chat in insert mode.
    fn typed(
        &mut self,
        fonts: &mut Fonts,
        key: &Key,
        text: Option<&str>,
        modifiers: Modifiers,
        settled: &mut Settled,
    ) {
        if self.menu.is_some() && self.menu_key(key, settled) {
            return;
        }
        if self.node.question.is_some() {
            self.answer_key(key, text, modifiers, settled);
            return;
        }
        let (shift, ctrl) = (modifiers.shift, modifiers.ctrl);
        // Shift+Tab steps through the agent's modes, as in Claude Code.
        if shift && *key == Key::Named(NamedKey::Tab) {
            self.next_mode(settled);
            return;
        }
        let character = match key {
            Key::Character(character) => Some(character.as_str()),
            _ => None,
        };
        match (key, character) {
            (Key::Named(NamedKey::Enter), _) if shift => {
                self.drive(fonts, |driver| driver.insert_or_replace_selection("\n"));
            }
            (Key::Named(NamedKey::Enter), _) => self.send(settled),
            (Key::Named(NamedKey::Backspace), _) => self.drive(fonts, |driver| {
                if ctrl {
                    driver.backdelete_word();
                } else {
                    driver.backdelete();
                }
            }),
            (Key::Named(NamedKey::Delete), _) => self.drive(fonts, |driver| {
                if ctrl {
                    driver.delete_word();
                } else {
                    driver.delete();
                }
            }),
            (Key::Named(named @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)), _) => {
                let left = *named == NamedKey::ArrowLeft;
                self.drive(fonts, |driver| match (left, ctrl, shift) {
                    (true, true, true) => driver.select_word_left(),
                    (true, true, false) => driver.move_word_left(),
                    (true, false, true) => driver.select_left(),
                    (true, false, false) => driver.move_left(),
                    (false, true, true) => driver.select_word_right(),
                    (false, true, false) => driver.move_word_right(),
                    (false, false, true) => driver.select_right(),
                    (false, false, false) => driver.move_right(),
                });
            }
            (Key::Named(named @ (NamedKey::ArrowUp | NamedKey::ArrowDown)), _) => {
                let up = *named == NamedKey::ArrowUp;
                self.drive(fonts, |driver| match (up, shift) {
                    (true, true) => driver.select_up(),
                    (true, false) => driver.move_up(),
                    (false, true) => driver.select_down(),
                    (false, false) => driver.move_down(),
                });
            }
            (Key::Named(NamedKey::Home), _) => self.drive(fonts, |driver| {
                if shift {
                    driver.select_to_line_start();
                } else {
                    driver.move_to_line_start();
                }
            }),
            (Key::Named(NamedKey::End), _) => self.drive(fonts, |driver| {
                if shift {
                    driver.select_to_line_end();
                } else {
                    driver.move_to_line_end();
                }
            }),
            (_, Some("a" | "A")) if ctrl => self.drive(fonts, |driver| {
                driver.select_all();
            }),
            // Ctrl+C copies what's selected, or else stops the agent, as
            // in a terminal.
            // Ctrl+C copies what's selected in the message, or else stops
            // the agent, as in a terminal; with the agent idle, it copies
            // what's selected in the conversation. Text left selected
            // there never keeps Ctrl+C from stopping it.
            (_, Some("c" | "C")) if ctrl => match self.editor.selected_text() {
                Some(selected) if !selected.is_empty() => {
                    settled.copied.push((selected.to_owned(), Copy::Clipboard));
                }
                _ if self.busy() => settled.actions.push(ChatAction::Stop),
                _ => {
                    if let Some((start, end)) = self.picked_range() {
                        let text = self.text_between(fonts, start, end);
                        settled.copied.push((text, Copy::Clipboard));
                        self.picked = None;
                    }
                }
            },
            _ if ctrl || modifiers.alt => {}
            _ => {
                if let Some(text) = text.filter(|text| !text.chars().any(char::is_control)) {
                    self.drive(fonts, |driver| driver.insert_or_replace_selection(text));
                }
            }
        }
    }

    /// Sends the message typed, unless the agent is still answering the
    /// last.
    fn send(&mut self, settled: &mut Settled) {
        let text = self.editor.raw_text().trim().to_owned();
        if text.is_empty() || self.busy() {
            return;
        }
        self.editor.set_text("");
        self.follow = true;
        self.picked = None;
        settled.actions.push(ChatAction::Send(text));
    }

    /// A key while the agent waits for an answer to its question: an
    /// answer's number picks it, and Esc says no.
    fn answer_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        modifiers: Modifiers,
        settled: &mut Settled,
    ) {
        let Some(question) = &self.node.question else {
            return;
        };
        let picked = match key {
            Key::Named(NamedKey::Escape) => Some(self.refusal()),
            Key::Character(character) if modifiers.ctrl && character.eq_ignore_ascii_case("c") => {
                settled.actions.push(ChatAction::Stop);
                None
            }
            _ => text
                .and_then(|text| text.chars().next())
                .and_then(|ch| ch.to_digit(10))
                .and_then(|digit| usize::try_from(digit).ok())
                .and_then(|digit| digit.checked_sub(1))
                .and_then(|index| question.choices.get(index))
                .map(|choice| Some(choice.id.clone())),
        };
        if let Some(option) = picked {
            settled.actions.push(ChatAction::Answer {
                question: question.id,
                option,
            });
        }
    }

    /// A key while the settings menu is open: Up and Down move through
    /// it, Left and Right move a slider or show and hide a model family's
    /// versions, Enter picks a choice, and Esc closes it, as does any other
    /// key, which then does what it would have. Returns whether it took
    /// the key.
    fn menu_key(&mut self, key: &Key, settled: &mut Settled) -> bool {
        let Some(menu) = self.menu.clone() else {
            return false;
        };
        let rows = picker::rows(&self.node.settings, &menu);
        let spots: Vec<Spot> = rows.iter().filter_map(Row::spot).collect();
        let at = (menu.highlighted.as_ref())
            .and_then(|highlighted| spots.iter().position(|spot| spot == highlighted));
        match key {
            Key::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                if spots.is_empty() {
                    return true;
                }
                let last = spots.len() - 1;
                let next = match (at, *direction == NamedKey::ArrowUp) {
                    (None, true) => last,
                    (None, false) => 0,
                    (Some(at), true) => at.checked_sub(1).unwrap_or(last),
                    (Some(at), false) => {
                        if at == last {
                            0
                        } else {
                            at + 1
                        }
                    }
                };
                if let Some(menu) = &mut self.menu {
                    menu.highlighted = Some(spots[next].clone());
                }
                true
            }
            Key::Named(direction @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)) => {
                let row = (rows.iter()).find(|row| row.spot() == menu.highlighted);
                self.sideways(&rows, row, *direction == NamedKey::ArrowRight, settled);
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(Spot::Choice { setting, value }) = menu.highlighted {
                    settled.actions.push(ChatAction::Set { setting, value });
                }
                self.menu = None;
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.menu = None;
                true
            }
            _ => {
                self.menu = None;
                false
            }
        }
    }

    /// Left or Right, on `row` of the menu's `rows`: along a slider, or
    /// into and out of a model family's versions.
    fn sideways(&mut self, rows: &[Row], row: Option<&Row>, right: bool, settled: &mut Settled) {
        match row {
            Some(Row::Slider(slider)) => {
                let last = slider.steps.len() - 1;
                let next = match slider.at {
                    Some(at) if right => (at + 1).min(last),
                    Some(at) => at.saturating_sub(1),
                    None => 0,
                };
                let value = slider.steps[next].0.clone();
                self.slide_to(&slider.setting, &value, settled);
            }
            Some(Row::Choice(choice)) => {
                let Some(menu) = &mut self.menu else {
                    return;
                };
                if let Some((family, _)) = &choice.family {
                    menu.expanded = right.then(|| family.clone());
                } else if choice.nested && !right {
                    // Back out to the family's row.
                    menu.highlighted = (rows.iter())
                        .find(|row| {
                            matches!(row, Row::Choice(head) if head.family.as_ref().is_some_and(|(_, open)| *open))
                        })
                        .and_then(Row::spot);
                    menu.expanded = None;
                }
            }
            _ => {}
        }
    }

    /// Moves the slider for `setting` to `value`, and asks the agent to
    /// set it, unless it's there.
    fn slide_to(&mut self, setting: &str, value: &str, settled: &mut Settled) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        menu.highlighted = Some(Spot::Slider(setting.to_owned()));
        let Some(current) = (self.node.settings.iter()).find(|each| each.id == setting) else {
            return;
        };
        if menu.value(current) == value {
            return;
        }
        menu.held = Some((setting.to_owned(), value.to_owned()));
        settled.actions.push(ChatAction::Set {
            setting: setting.to_owned(),
            value: value.to_owned(),
        });
    }

    /// The mouse, dragging a slider, moved to `x`, from the pane's left:
    /// the slider goes to the step whose share of it that's in, or the end
    /// nearest.
    fn slide(&mut self, x: f32, settled: &mut Settled) {
        let Some(setting) = self.sliding.clone() else {
            return;
        };
        let steps: Vec<(&String, [f32; 4])> = (self.hits.iter())
            .filter_map(|(hit, rect)| match hit {
                Hit::Step { setting: of, value } if *of == setting => Some((value, *rect)),
                _ => None,
            })
            .collect();
        let within = (steps.iter()).find(|(_, rect)| x >= rect[0] && x < rect[0] + rect[2]);
        let nearest = match steps.first() {
            Some((_, first)) if x < first[0] => steps.first(),
            _ => steps.last(),
        };
        let Some(value) = within.or(nearest).map(|(value, _)| (*value).clone()) else {
            return;
        };
        self.slide_to(&setting, &value, settled);
    }

    /// Opens the menu for `settings`, on the choice made, with the versions
    /// of a model family shown if one of them is chosen, or closes it if
    /// it's open for them.
    fn toggle_menu(&mut self, settings: Vec<String>) {
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.settings == settings)
        {
            self.menu = None;
            return;
        }
        let expanded = (settings.iter())
            .filter_map(|id| self.setting(id))
            .find_map(picker::pinned_family);
        let mut menu = Menu {
            settings,
            highlighted: None,
            expanded,
            held: None,
        };
        menu.highlighted = (picker::rows(&self.node.settings, &menu).iter())
            .find(|row| matches!(row, Row::Choice(choice) if choice.current))
            .and_then(Row::spot);
        self.menu = Some(menu);
    }

    /// Moves the agent's mode on to the next it offers, round to the first.
    fn next_mode(&mut self, settled: &mut Settled) {
        let Some(mode) =
            (self.node.settings.iter()).find(|setting| setting.kind == SettingKind::Mode)
        else {
            return;
        };
        let at = (mode.choices.iter()).position(|choice| choice.value == mode.value);
        let next = at.map_or(0, |at| (at + 1) % mode.choices.len());
        if let Some(choice) = mode.choices.get(next) {
            settled.actions.push(ChatAction::Set {
                setting: mode.id.clone(),
                value: choice.value.clone(),
            });
        }
    }

    /// The answer that says no: the agent's own, or none.
    fn refusal(&self) -> Option<String> {
        let choices = &self.node.question.as_ref()?.choices;
        (choices.iter())
            .find(|choice| choice.kind == ChoiceKind::RejectOnce)
            .or_else(|| (choices.iter()).find(|choice| choice.kind == ChoiceKind::RejectAlways))
            .map(|choice| choice.id.clone())
    }

    /// A mouse button went down: on a button, it's pressed, and in the
    /// message, the cursor goes there.
    fn pressed(
        &mut self,
        fonts: &mut Fonts,
        at: [f32; 2],
        clicks: u8,
        shift: bool,
        settled: &mut Settled,
    ) {
        // Over everything, a menu takes the click first.
        let hit = (self.hits.iter().rev())
            .find(|(_, rect)| contains(*rect, at))
            .map(|(hit, _)| hit.clone());
        // A click anywhere else closes the menu.
        if !matches!(
            hit,
            Some(
                Hit::Pick { .. }
                    | Hit::Chip(_)
                    | Hit::Expand(_)
                    | Hit::Step { .. }
                    | Hit::Reset { .. }
            )
        ) {
            self.menu = None;
        }
        match hit {
            Some(Hit::Chip(settings)) => self.toggle_menu(settings),
            Some(Hit::Expand(family)) => {
                if let Some(menu) = &mut self.menu {
                    menu.expanded = (menu.expanded.as_ref() != Some(&family)).then_some(family);
                }
            }
            Some(Hit::Step { setting, value }) => {
                self.sliding = Some(setting.clone());
                self.slide_to(&setting, &value, settled);
            }
            Some(Hit::Reset { setting, value }) => self.slide_to(&setting, &value, settled),
            Some(Hit::Pick { setting, value }) => {
                self.menu = None;
                settled.actions.push(ChatAction::Set { setting, value });
            }
            Some(Hit::Card(turn, tool)) => {
                self.restyled(turn);
                if !self.open_cards.remove(&tool) {
                    self.open_cards.insert(tool);
                }
            }
            Some(Hit::Undo(turn)) => settled.actions.push(ChatAction::Undo {
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
                force: false,
            }),
            Some(Hit::Redo(turn)) => settled.actions.push(ChatAction::Redo {
                turn: u32::try_from(turn).unwrap_or(u32::MAX),
                force: false,
            }),
            Some(Hit::Fold(turn)) => self.toggle_fold(turn),
            Some(Hit::CopyCode(key)) => {
                if let Some(text) = self.text(key) {
                    settled.copied.push((text.text.to_owned(), Copy::Clipboard));
                    self.copied_code = Some(key);
                }
            }
            Some(Hit::Send) => self.send(settled),
            Some(Hit::Stop) => settled.actions.push(ChatAction::Stop),
            Some(Hit::Choice(id)) => {
                // The question the button was drawn for, which may have
                // been answered since.
                if let Some(question) = &self.node.question {
                    settled.actions.push(ChatAction::Answer {
                        question: question.id,
                        option: Some(id),
                    });
                }
            }
            Some(Hit::Editor) => {
                let [x, y] = self.in_editor(at);
                self.selecting = true;
                self.drive(fonts, |driver| match (clicks, shift) {
                    (_, true) => driver.shift_click_extension(x, y),
                    (2, _) => driver.select_word_at_point(x, y),
                    (3.., _) => driver.select_hard_line_at_point(x, y),
                    _ => driver.move_to_point(x, y),
                });
            }
            None => self.press_text(at, clicks, shift),
        }
    }

    /// A mouse button went down at `at`, on no button: on the
    /// conversation's text, it starts selecting, a character, word or line
    /// at a time as `clicks` says, or with `shift`, goes on with the
    /// selection there is; and the cursor goes there. Anywhere else, what
    /// was selected isn't.
    fn press_text(&mut self, at: [f32; 2], clicks: u8, shift: bool) {
        let Some(spot) = self.spot_at(at, false) else {
            self.picked = None;
            return;
        };
        let (anchor, head) = match (clicks, self.picked) {
            (_, Some(picked)) if shift => (picked.anchor, spot),
            (2, _) => self.word_at(at, false).unwrap_or((spot, spot)),
            (3.., _) => self.word_at(at, true).unwrap_or((spot, spot)),
            _ => (spot, spot),
        };
        self.picked = Some(Picked {
            anchor,
            head,
            how: How::Mouse,
        });
        self.picking = true;
        self.cursor = Some(spot);
        self.goal = None;
    }

    /// The setting with `id`, if the agent offers it.
    fn setting(&self, id: &str) -> Option<&Setting> {
        self.node.settings.iter().find(|setting| setting.id == id)
    }

    /// `at`, from the pane's top left, from the editor's text's instead.
    fn in_editor(&self, at: [f32; 2]) -> [f32; 2] {
        [at[0] - self.editor_origin[0], at[1] - self.editor_origin[1]]
    }

    /// Styles the editor's text for the fonts, if they've changed.
    fn style_editor(&mut self, fonts: &Fonts) {
        let size = paint::text_size(fonts);
        if size.to_bits() == self.editor_size.to_bits() {
            return;
        }
        self.editor_size = size;
        let styles = self.editor.edit_styles();
        for property in paint::body_style(fonts, size) {
            styles.insert(property);
        }
    }

    // --- Scrolling ------------------------------------------------------------

    /// Scrolls `lines` lines up, or down if negative. Returns whether the
    /// view moved.
    pub(crate) fn scroll_lines(&mut self, lines: f32) -> bool {
        self.scroll_by(-lines * self.line_height)
    }

    /// Scrolls half a view up, or down.
    pub(crate) fn scroll_half(&mut self, up: bool) -> bool {
        let half = self.view_height / 2.0;
        self.scroll_by(if up { -half } else { half })
    }

    fn scroll_by(&mut self, pixels: f32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + pixels).clamp(0.0, self.max_scroll);
        self.follow = self.scroll >= self.max_scroll - 1.0;
        (self.scroll - old).abs() > f32::EPSILON
    }

    /// What the bottom bar says about the chat: what the window last said
    /// of it, or how far up the view is.
    pub(crate) fn label(&self) -> Option<String> {
        self.said
            .clone()
            .or_else(|| (!self.follow && self.max_scroll > 0.0).then(|| "↑ scrolled".to_owned()))
    }

    /// Says `text` in the bottom bar, until the next key in the chat.
    /// Returns that the pane must be drawn again.
    pub(crate) fn say(&mut self, text: String) -> bool {
        self.said = Some(text);
        true
    }

    /// Stops saying what [`say`](Self::say) said. Returns whether it said
    /// anything, so the pane must be drawn again.
    pub(crate) fn unsay(&mut self) -> bool {
        self.said.take().is_some()
    }

    /// The turn `u` undoes, or with `redo`, the one Ctrl+R redoes, as Vim
    /// steps through its undo: the last whose changes to files are made,
    /// or the first of those undone. `None` if there's none.
    pub(crate) fn turn_to_undo(&self, redo: bool) -> Option<usize> {
        let mut turns = self.node.turns.iter().enumerate();
        let found = if redo {
            turns.find(|(_, turn)| undoable(turn, redo))
        } else {
            turns.rfind(|(_, turn)| undoable(turn, redo))
        };
        found.map(|(at, _)| at)
    }
}

/// Whether what `turn` changed in files can be undone, or with `redo`,
/// made again.
fn undoable(turn: &clackatoo_protocol::Turn, redo: bool) -> bool {
    turn.end.is_some()
        && turn.files.is_some_and(|files| match files.state {
            TurnFilesState::Made => !redo,
            TurnFilesState::Undone => redo,
            TurnFilesState::Partly => true,
            TurnFilesState::TooOld => false,
        })
}

#[cfg(test)]
impl ChatView {
    /// Stops the clock at `now`, in milliseconds since the Unix epoch, for
    /// what turns still going say to stay the same.
    pub(crate) fn set_clock(&mut self, now: u64) {
        self.clock = Some(now);
    }

    /// Opens turn `turn`'s work, as clicking the line it folds under does.
    pub(crate) fn open_fold(&mut self, turn: usize) {
        self.set_fold(turn, true);
    }

    /// Shows the button that copies the first block of code drawn, as
    /// the mouse over it does.
    pub(crate) fn hover_first_code(&mut self) {
        self.hovered_code = self
            .shown
            .iter()
            .find(|shown| shown.code)
            .map(|shown| shown.key);
    }

    /// Opens the card of the tool with `id`, as clicking it does.
    pub(crate) fn open_card(&mut self, id: &str) {
        self.open_cards.insert(id.to_owned());
    }

    /// Opens the menu for the setting of `kind`, as clicking its chip does.
    pub(crate) fn open_menu(&mut self, kinds: &[SettingKind]) {
        let ids = (kinds.iter())
            .filter_map(|kind| {
                self.node
                    .settings
                    .iter()
                    .find(|setting| setting.kind == *kind)
            })
            .map(|setting| setting.id.clone())
            .collect();
        self.toggle_menu(ids);
    }
}

/// The turn `patch` changes, if it changes one.
fn touches(patch: &clackatoo_protocol::ChatPatch) -> Option<usize> {
    use clackatoo_protocol::ChatPatch;
    match patch {
        ChatPatch::Turn { index: turn, .. }
        | ChatPatch::Item { turn, .. }
        | ChatPatch::Append { turn, .. }
        | ChatPatch::Output { turn, .. }
        | ChatPatch::Files { turn, .. } => usize::try_from(*turn).ok(),
        ChatPatch::State(_)
        | ChatPatch::Question(_)
        | ChatPatch::Notice(_)
        | ChatPatch::Settings(_)
        | ChatPatch::Usage(_) => None,
    }
}

/// Whether `rect` (left, top, width and height) holds `at`.
fn contains([left, top, width, height]: [f32; 4], at: [f32; 2]) -> bool {
    at[0] >= left && at[0] < left + width && at[1] >= top && at[1] < top + height
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Choice, Question, Turn};

    use super::*;

    fn press(view: &mut ChatView, key: &Key, text: Option<&str>, modifiers: Modifiers) {
        view.key(&KeyPress {
            key,
            text,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers,
        });
    }

    fn typed(view: &mut ChatView, text: &str) {
        for ch in text.chars() {
            let text = ch.to_string();
            press(
                view,
                &Key::Character(text.as_str().into()),
                Some(&text),
                Modifiers::default(),
            );
        }
    }

    fn enter(view: &mut ChatView, shift: bool) {
        press(
            view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers {
                shift,
                ..Modifiers::default()
            },
        );
    }

    fn fonts() -> Fonts {
        Fonts::load(&crate::fonts::FontOptions::default(), 1.0).expect("a font")
    }

    #[test]
    fn enter_sends_what_was_typed_and_shift_enter_starts_a_line() {
        let mut fonts = fonts();
        let mut view = ChatView::new(String::new(), ChatNode::default());
        typed(&mut view, "hello");
        enter(&mut view, true);
        typed(&mut view, "there");
        enter(&mut view, false);
        let settled = view.settle(&mut fonts);
        assert_eq!(settled.actions, [ChatAction::Send("hello\nthere".into())]);
        assert_eq!(view.editor.raw_text(), "");
        // Nothing typed: nothing sent.
        enter(&mut view, false);
        assert_eq!(view.settle(&mut fonts).actions, []);
    }

    #[test]
    fn nothing_is_sent_while_the_agent_answers_and_ctrl_c_stops_it() {
        let mut fonts = fonts();
        let mut node = ChatNode::default();
        node.turns.push(Turn {
            prompt: "hi".into(),
            sent: 0,
            items: Vec::new(),
            end: None,
            files: None,
            from: None,
        });
        let mut view = ChatView::new(String::new(), node);
        typed(&mut view, "more");
        enter(&mut view, false);
        press(
            &mut view,
            &Key::Character("c".into()),
            None,
            Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        );
        assert_eq!(view.settle(&mut fonts).actions, [ChatAction::Stop]);
        assert_eq!(view.editor.raw_text(), "more");
    }

    #[test]
    fn a_question_is_answered_by_number_or_turned_down_with_esc() {
        let mut fonts = fonts();
        let mut node = ChatNode::default();
        let choice = |id: &str, kind| Choice {
            id: id.into(),
            name: id.into(),
            kind,
        };
        node.question = Some(Question {
            id: 4,
            title: "run ls".into(),
            choices: vec![
                choice("yes", ChoiceKind::AllowOnce),
                choice("no", ChoiceKind::RejectOnce),
            ],
        });
        let mut view = ChatView::new(String::new(), node);
        typed(&mut view, "1");
        press(
            &mut view,
            &Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        typed(&mut view, "7");
        assert_eq!(
            view.settle(&mut fonts).actions,
            [
                ChatAction::Answer {
                    question: 4,
                    option: Some("yes".into()),
                },
                ChatAction::Answer {
                    question: 4,
                    option: Some("no".into()),
                },
            ]
        );
        // Typing went to the answer, not the message.
        assert_eq!(view.editor.raw_text(), "");
    }

    fn with_settings() -> ChatView {
        use clackatoo_protocol::SettingChoice;
        let choice = |value: &str| SettingChoice {
            value: value.into(),
            name: value.into(),
            description: None,
        };
        let node = ChatNode {
            settings: vec![
                Setting {
                    id: "mode".into(),
                    name: "Mode".into(),
                    kind: SettingKind::Mode,
                    value: "auto".into(),
                    choices: vec![choice("default"), choice("plan"), choice("auto")],
                },
                Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: "haiku".into(),
                    choices: vec![choice("haiku"), choice("sonnet")],
                },
            ],
            ..ChatNode::default()
        };
        ChatView::new(String::new(), node)
    }

    fn set(setting: &str, value: &str) -> ChatAction {
        ChatAction::Set {
            setting: setting.into(),
            value: value.into(),
        }
    }

    #[test]
    fn shift_tab_steps_through_the_modes() {
        let mut fonts = fonts();
        let mut view = with_settings();
        press(
            &mut view,
            &Key::Named(NamedKey::Tab),
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        );
        // From the last, round to the first.
        assert_eq!(view.settle(&mut fonts).actions, [set("mode", "default")]);
    }

    #[test]
    fn a_menu_is_picked_from_with_the_keyboard() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.open_menu(&[SettingKind::Model]);
        press(
            &mut view,
            &Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut view,
            &Key::Named(NamedKey::Enter),
            Some("\r"),
            Modifiers::default(),
        );
        assert_eq!(view.settle(&mut fonts).actions, [set("model", "sonnet")]);
        assert!(view.menu.is_none());
        // Esc closes it, picking nothing; any other key closes it and
        // types.
        view.open_menu(&[SettingKind::Model]);
        press(
            &mut view,
            &Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        view.open_menu(&[SettingKind::Model]);
        typed(&mut view, "x");
        assert_eq!(view.settle(&mut fonts).actions, []);
        assert_eq!((view.menu.as_ref(), view.editor.raw_text()), (None, "x"));
    }

    #[test]
    fn the_pointer_is_a_hand_over_what_takes_a_click() {
        let mut view = with_settings();
        view.hits = vec![
            (Hit::Editor, [0.0, 0.0, 100.0, 20.0]),
            (Hit::Send, [100.0, 0.0, 20.0, 20.0]),
        ];
        assert_eq!(view.pointer([10.0, 10.0]), Pointer::Text);
        assert_eq!(view.pointer([110.0, 10.0]), Pointer::Hand);
        assert_eq!(view.pointer([10.0, 40.0]), Pointer::Arrow);
    }

    /// A chat with Claude Code's settings, in short: Opus, with a pinned
    /// version, and effort from low to max.
    fn with_claude() -> ChatView {
        use clackatoo_protocol::SettingChoice;
        let choice = |value: &str, name: &str| SettingChoice {
            value: value.into(),
            name: name.into(),
            description: None,
        };
        let node = ChatNode {
            settings: vec![
                Setting {
                    id: "model".into(),
                    name: "Model".into(),
                    kind: SettingKind::Model,
                    value: "opus".into(),
                    choices: vec![
                        choice("opus", "Opus 5.5"),
                        choice("claude-opus-4-8", "Opus 4.8"),
                    ],
                },
                Setting {
                    id: "effort".into(),
                    name: "Effort".into(),
                    kind: SettingKind::Effort,
                    value: "high".into(),
                    choices: vec![
                        choice("default", "Default"),
                        choice("low", "Low"),
                        choice("high", "High"),
                        choice("max", "Max"),
                    ],
                },
            ],
            ..ChatNode::default()
        };
        ChatView::new(String::new(), node)
    }

    fn key(view: &mut ChatView, named: NamedKey) {
        press(view, &Key::Named(named), None, Modifiers::default());
    }

    #[test]
    fn right_shows_a_model_familys_versions_to_pick_from() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        key(&mut view, NamedKey::ArrowRight);
        key(&mut view, NamedKey::ArrowDown);
        key(&mut view, NamedKey::Enter);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [set("model", "claude-opus-4-8")]
        );
    }

    #[test]
    fn left_and_right_slide_the_effort_and_leave_the_menu_open() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Model, SettingKind::Effort]);
        // Up, from the model, round to the slider at the bottom.
        key(&mut view, NamedKey::ArrowUp);
        for _ in 0..2 {
            key(&mut view, NamedKey::ArrowRight);
        }
        key(&mut view, NamedKey::ArrowLeft);
        // Max, then nothing past it, then back to high: the slider stays
        // where it was put while the agent sets it.
        assert_eq!(
            view.settle(&mut fonts).actions,
            [set("effort", "max"), set("effort", "high")]
        );
        assert!(view.menu.is_some());
    }

    #[test]
    fn dragging_a_slider_sets_each_step_it_crosses() {
        let mut fonts = fonts();
        let mut view = with_claude();
        view.open_menu(&[SettingKind::Effort]);
        let step = |value: &str| Hit::Step {
            setting: "effort".into(),
            value: value.into(),
        };
        view.hits = vec![
            (step("low"), [0.0, 0.0, 10.0, 10.0]),
            (step("high"), [10.0, 0.0, 10.0, 10.0]),
            (step("max"), [20.0, 0.0, 10.0, 10.0]),
        ];
        view.press([5.0, 5.0], 1, false);
        view.drag([12.0, 40.0]);
        // Past the end: the last step.
        view.drag([90.0, 5.0]);
        view.release();
        view.drag([5.0, 5.0]);
        assert_eq!(
            view.settle(&mut fonts).actions,
            [
                set("effort", "low"),
                set("effort", "high"),
                set("effort", "max")
            ]
        );
    }

    #[test]
    fn clicking_a_card_opens_it_and_clicking_again_folds_it() {
        let mut fonts = fonts();
        let mut view = with_settings();
        view.hits = vec![(Hit::Card(0, "t".into()), [0.0, 0.0, 100.0, 50.0])];
        assert_eq!(view.pointer([10.0, 10.0]), Pointer::Hand);
        view.press([10.0, 10.0], 1, false);
        view.settle(&mut fonts);
        assert!(view.open_cards.contains("t"));
        view.press([10.0, 10.0], 1, false);
        view.settle(&mut fonts);
        assert!(view.open_cards.is_empty());
    }

    /// A chat whose turns changed files, made or undone, as `states` say.
    fn with_files(states: &[clackatoo_protocol::TurnFilesState]) -> ChatView {
        use clackatoo_protocol::{Outcome, TurnEnd, TurnFiles};
        let mut node = ChatNode::default();
        for &state in states {
            node.turns.push(Turn {
                from: None,
                prompt: "change it".into(),
                sent: 0,
                items: Vec::new(),
                end: Some(TurnEnd {
                    at: 0,
                    outcome: Outcome::Done,
                }),
                files: Some(TurnFiles { count: 2, state }),
            });
        }
        ChatView::new(String::new(), node)
    }

    #[test]
    fn a_turn_that_changed_files_has_a_button_to_undo_it() {
        use clackatoo_protocol::TurnFilesState::{Made, Undone};
        let mut fonts = fonts();
        let mut view = with_files(&[Undone, Made]);
        view.paint(&mut fonts, [0.0, 0.0], [600.0, 800.0], false);
        let button = |view: &ChatView, wanted: &Hit| {
            let (_, [x, y, width, height]) = (view.hits.iter())
                .find(|(hit, _)| hit == wanted)
                .expect("the button is drawn");
            [x + width / 2.0, y + height / 2.0]
        };
        let at = button(&view, &Hit::Undo(1));
        assert_eq!(view.pointer(at), Pointer::Hand);
        view.press(at, 1, false);
        let undo = ChatAction::Undo {
            turn: 1,
            force: false,
        };
        assert_eq!(view.settle(&mut fonts).actions, [undo]);
        let at = button(&view, &Hit::Redo(0));
        view.press(at, 1, false);
        let redo = ChatAction::Redo {
            turn: 0,
            force: false,
        };
        assert_eq!(view.settle(&mut fonts).actions, [redo]);
    }

    #[test]
    fn u_takes_the_last_turn_made_and_ctrl_r_the_first_undone() {
        use clackatoo_protocol::TurnFilesState::{Made, TooOld, Undone};
        let view = with_files(&[TooOld, Made, Made, Undone, Undone]);
        assert_eq!(view.turn_to_undo(false), Some(2));
        assert_eq!(view.turn_to_undo(true), Some(3));
        let view = with_files(&[TooOld]);
        assert_eq!(view.turn_to_undo(false), None);
        assert_eq!(view.turn_to_undo(true), None);
    }

    #[test]
    fn a_menu_over_a_card_lights_up_under_the_mouse() {
        let mut view = with_settings();
        view.open_menu(&[SettingKind::Model]);
        view.hits = vec![
            (Hit::Card(0, "t".into()), [0.0, 0.0, 100.0, 100.0]),
            (
                Hit::Pick {
                    setting: "model".into(),
                    value: "sonnet".into(),
                },
                [10.0, 10.0, 50.0, 20.0],
            ),
        ];
        assert!(view.hover([20.0, 20.0]));
        assert_eq!(
            view.menu.unwrap().highlighted,
            Some(Spot::Choice {
                setting: "model".into(),
                value: "sonnet".into()
            })
        );
    }
}
