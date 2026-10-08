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

use clackatoo_protocol::{ChatAction, ChatNode, ChoiceKind, ViewUpdate};
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
        if self.node.question.is_some() {
            self.answer_key(key, text, modifiers, settled);
            return;
        }
        let (shift, ctrl) = (modifiers.shift, modifiers.ctrl);
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
        let hit = (self.hits.iter())
            .find(|(_, [left, top, width, height])| {
                at[0] >= *left && at[0] < left + width && at[1] >= *top && at[1] < top + height
            })
            .map(|(hit, _)| hit.clone());
        match hit {
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
}
