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

mod markdown;
mod paint;

use clackatoo_protocol::{ChatAction, ChatNode, ChoiceKind, Setting, SettingKind, ViewUpdate};
use parley::{LayoutContext, PlainEditor};
use winit::keyboard::{Key, NamedKey};

use crate::fonts::Fonts;
use crate::input::{KeyPress, Modifiers};

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
    /// The input method is composing this text, with its cursor over this
    /// stretch of it, or has stopped.
    Compose(Option<(String, Option<(usize, usize)>)>),
}

/// What settling the chat's queue came to.
#[derive(Debug, Default)]
pub(crate) struct Settled {
    /// It must be redrawn.
    pub(crate) redraw: bool,
    /// Asks for the daemon.
    pub(crate) actions: Vec<ChatAction>,
    /// Text to put on the clipboard.
    pub(crate) copied: Option<String>,
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
    /// The choice the keyboard or the mouse is on: its setting and value.
    highlighted: Option<(String, String)>,
}

/// The window's side of a chat.
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
    /// The settings menu, while it's open.
    menu: Option<Menu>,
    /// Where the chip that opened the menu was drawn, for the menu to open
    /// from, while drawing.
    menu_anchor: Option<[f32; 4]>,
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
            menu: None,
            menu_anchor: None,
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
            }
            ViewUpdate::Patch(patches) => {
                for patch in patches {
                    match patch {
                        clackatoo_protocol::Patch::Title(title) => {
                            title.clone_into(&mut self.title);
                        }
                        clackatoo_protocol::Patch::Chat(patch) => self.node.apply(patch.clone()),
                        clackatoo_protocol::Patch::Grid { .. } => {}
                    }
                }
            }
        }
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
        if self.selecting {
            self.queued.push(Input::Drag(at));
        }
    }

    pub(crate) fn release(&mut self) {
        self.selecting = false;
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
            None => Pointer::Arrow,
        }
    }

    /// The mouse moved to `at` with no button held: over a menu, its
    /// choice there lights up. Returns whether the chat must be redrawn.
    pub(crate) fn hover(&mut self, at: [f32; 2]) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        let under = (self.hits.iter())
            .find(|(_, rect)| contains(*rect, at))
            .and_then(|(hit, _)| match hit {
                Hit::Pick { setting, value } => Some((setting.clone(), value.clone())),
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
            settled.redraw = true;
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
                Input::Drag(at) => {
                    let [x, y] = self.in_editor(at);
                    self.drive(fonts, |driver| driver.extend_selection_to_point(x, y));
                }
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
            (_, Some("c" | "C")) if ctrl => match self.editor.selected_text() {
                Some(selected) if !selected.is_empty() => {
                    settled.copied = Some(selected.to_owned());
                }
                _ if self.busy() => settled.actions.push(ChatAction::Stop),
                _ => {}
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
        if let Some(choice) = picked {
            settled.actions.push(ChatAction::Answer(choice));
        }
    }

    /// A key while the settings menu is open: Up and Down move through
    /// its choices, Enter picks one, and Esc closes it, as does any other
    /// key, which then does what it would have. Returns whether it took
    /// the key.
    fn menu_key(&mut self, key: &Key, settled: &mut Settled) -> bool {
        let Some(menu) = self.menu.clone() else {
            return false;
        };
        let choices = self.menu_choices(&menu);
        let at = (menu.highlighted.as_ref())
            .and_then(|highlighted| choices.iter().position(|choice| choice == highlighted));
        match key {
            Key::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                if choices.is_empty() {
                    return true;
                }
                let last = choices.len() - 1;
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
                    menu.highlighted = Some(choices[next].clone());
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let Some((setting, value)) = menu.highlighted {
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

    /// Every choice in `menu`, in order: each setting's, by its ID and the
    /// choice's value.
    fn menu_choices(&self, menu: &Menu) -> Vec<(String, String)> {
        (menu.settings.iter())
            .filter_map(|id| self.node.settings.iter().find(|setting| setting.id == *id))
            .flat_map(|setting| {
                (setting.choices.iter()).map(|choice| (setting.id.clone(), choice.value.clone()))
            })
            .collect()
    }

    /// Opens the menu for `settings`, on the choice of the first, or closes
    /// it if it's open for them.
    fn toggle_menu(&mut self, settings: Vec<String>) {
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.settings == settings)
        {
            self.menu = None;
            return;
        }
        let highlighted = (settings.first())
            .and_then(|id| self.node.settings.iter().find(|setting| setting.id == *id))
            .map(|setting| (setting.id.clone(), setting.value.clone()));
        self.menu = Some(Menu {
            settings,
            highlighted,
        });
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
        if !matches!(hit, Some(Hit::Pick { .. } | Hit::Chip(_))) {
            self.menu = None;
        }
        match hit {
            Some(Hit::Chip(settings)) => self.toggle_menu(settings),
            Some(Hit::Pick { setting, value }) => {
                self.menu = None;
                settled.actions.push(ChatAction::Set { setting, value });
            }
            Some(Hit::Send) => self.send(settled),
            Some(Hit::Stop) => settled.actions.push(ChatAction::Stop),
            Some(Hit::Choice(id)) => settled.actions.push(ChatAction::Answer(Some(id))),
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
            None => {}
        }
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

    /// Scrolls to the start, or the end.
    pub(crate) fn scroll_to_end(&mut self, end: bool) -> bool {
        self.scroll_by(if end {
            f32::INFINITY
        } else {
            f32::NEG_INFINITY
        })
    }

    fn scroll_by(&mut self, pixels: f32) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + pixels).clamp(0.0, self.max_scroll);
        self.follow = self.scroll >= self.max_scroll - 1.0;
        (self.scroll - old).abs() > f32::EPSILON
    }

    /// What the bottom bar says about the chat: how far up the view is.
    pub(crate) fn label(&self) -> Option<String> {
        (!self.follow && self.max_scroll > 0.0).then(|| "↑ scrolled".to_owned())
    }
}

#[cfg(test)]
impl ChatView {
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
                ChatAction::Answer(Some("yes".into())),
                ChatAction::Answer(Some("no".into())),
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
}
