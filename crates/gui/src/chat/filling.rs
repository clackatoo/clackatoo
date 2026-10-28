//! A chat's form question: the agent asks the user to fill in a form or
//! open a link (see [`crate::form`] for the rows, and what keys and clicks
//! do to them), shown in place of the box a message is typed in, as its
//! question about a tool is.
//!
//! Its keys come before the chat's in either mode: in normal mode, `j`
//! and `k` move through the form rather than the conversation, and the
//! rest read as ever. Esc is never the form's: in insert mode it goes to
//! normal mode, as from the message box, so a key pressed from habit
//! can't throw away what's been typed. Ctrl+C stops the agent, as ever,
//! which takes the form away.
//!
//! An answer goes ahead of a paste still reading the clipboard, as any
//! answer does, and, for [`ANSWER_PAUSE`](super::ANSWER_PAUSE) after the form
//! comes or the focus arrives, Submit, Open and Decline say to press
//! again rather than answer, as a key meant for what was there before
//! mustn't.

use clackatoo_protocol::{ChatAction, FormReply};

use super::{ChatView, Input};
use crate::form::{Filled, Filling};
use crate::input::KeyPress;

impl ChatView {
    /// Starts filling in the form the question asks, if it's one not
    /// being filled in already; or stops, if it asks none.
    pub(super) fn start_filling(&mut self) {
        let asked = (self.node.question.as_ref())
            .and_then(|question| question.form.as_ref().map(|form| (question.id, form)));
        match asked {
            Some((id, _)) if self.filling.as_ref().is_some_and(|f| f.question == id) => {}
            Some((id, form)) => self.filling = Some(Filling::new(id, form)),
            None => self.filling = None,
        }
    }

    /// Does `press`, in insert mode if `insert`, to the form being filled
    /// in, if one is and the key is the form's: `None` if it isn't, and
    /// otherwise whether the pane is to go to insert mode, to type in a
    /// box.
    pub(super) fn form_key(&mut self, press: &KeyPress<'_>, insert: bool) -> Option<bool> {
        if self.menu.is_some() || self.viewing_modal() {
            return None;
        }
        let filling = self.filling.as_ref()?;
        let typing = insert && filling.in_box();
        let strokes = crate::vim::plain_strokes(press, typing);
        if strokes.is_empty() {
            return None;
        }
        let form = (self.node.question.as_ref()).and_then(|question| question.form.clone())?;
        let mut taken = None;
        for stroke in strokes {
            let filling = self.filling.as_mut()?;
            let filled = filling.key(&form, stroke, insert);
            match self.filled(filled) {
                None if taken.is_none() => return None,
                None => {}
                Some(to_insert) => taken = Some(taken.unwrap_or(false) | to_insert),
            }
        }
        taken
    }

    /// The row of the form clicked, `row`, is pressed. Returns whether
    /// the pane is to go to insert mode, to type in it.
    pub(super) fn form_clicked(&mut self, row: usize) -> bool {
        let Some(form) = (self.node.question.as_ref()).and_then(|q| q.form.clone()) else {
            return false;
        };
        let Some(filling) = self.filling.as_mut() else {
            return false;
        };
        let filled = filling.click(&form, row);
        self.filled(filled).unwrap_or(false)
    }

    /// Does what a key or click did to the form: `None` if it wasn't the
    /// form's, and otherwise whether the pane is to go to insert mode.
    fn filled(&mut self, filled: Filled) -> Option<bool> {
        let question = self.filling.as_ref()?.question;
        match filled {
            Filled::Unread => return None,
            Filled::Nothing | Filled::Changed => {}
            Filled::Typing => return Some(true),
            Filled::Unfit(why) => {
                self.say(format!("Can't submit yet: {why}"));
            }
            Filled::Reply(_) | Filled::Open(_) if self.just_asked() => {
                self.say("a question just came: press it again to answer".to_owned());
            }
            Filled::Reply(reply) => {
                self.queue_control(Input::Act(ChatAction::Fill { question, reply }));
            }
            Filled::Open(url) => {
                // Checked here too: the daemon that sent it may not be
                // this machine's, and only a plain web link is opened.
                let opens = matches!(
                    crate::links::target_of_uri(&url),
                    Some(crate::links::Target::Url(_))
                ) && clackatoo_protocol::url_host(&url).is_some();
                if !opens {
                    self.say("That link isn't one clackatoo opens".to_owned());
                    return Some(false);
                }
                self.opened.push(url);
                let reply = FormReply::Accept(Vec::new());
                self.queue_control(Input::Act(ChatAction::Fill { question, reply }));
            }
        }
        Some(false)
    }

    /// Pastes `text` into the box of the form picked, if one is: whether
    /// it did.
    pub(super) fn paste_into_form(&mut self, text: &str) -> bool {
        self.filling
            .as_mut()
            .is_some_and(|filling| filling.paste(text))
    }

    /// The links a form asked to open, for the window to open.
    pub(crate) fn take_opened(&mut self) -> Vec<String> {
        std::mem::take(&mut self.opened)
    }

    /// Scrolls the plan the question shows `lines` down (up if negative),
    /// if it shows one: whether it did.
    pub(crate) fn scroll_proposal(&mut self, lines: f32) -> bool {
        if self.proposal_rect.is_none() {
            return false;
        }
        let before = self.proposal_scroll;
        self.proposal_scroll = (self.proposal_scroll + lines * 20.0).max(0.0);
        (self.proposal_scroll - before).abs() > f32::EPSILON
    }

    /// Whether the plan the question shows was drawn under `pixel`.
    pub(crate) fn over_proposal(&self, pixel: [f32; 2]) -> bool {
        self.proposal_rect.is_some_and(|[x, y, width, height]| {
            (x..x + width).contains(&pixel[0]) && (y..y + height).contains(&pixel[1])
        })
    }

    /// `j` or `k` in normal mode while the question shows a plan: scroll
    /// it. Returns whether it took the key.
    pub(super) fn proposal_key(&mut self, press: &KeyPress<'_>) -> bool {
        use winit::keyboard::Key;
        if self.proposal_rect.is_none() || press.modifiers.ctrl || press.modifiers.alt {
            return false;
        }
        match press.key {
            Key::Character(text) if text == "j" => {
                self.scroll_proposal(1.0);
                true
            }
            Key::Character(text) if text == "k" => {
                self.scroll_proposal(-1.0);
                true
            }
            _ => false,
        }
    }

    /// Whether a click went into a box of the form since this was last
    /// asked, so the pane is to go to insert mode.
    pub(crate) fn take_wants_insert(&mut self) -> bool {
        std::mem::take(&mut self.wants_insert)
    }

    /// The form `form`, as someone looking at it reads it, line by line,
    /// onto `text`, for looking at the window without a desktop.
    pub(super) fn form_text(&self, form: &clackatoo_protocol::Form, text: &mut String) {
        use crate::form::Row;
        use clackatoo_protocol::FieldInput;
        use std::fmt::Write as _;
        let _ = writeln!(text, "{}", form.message);
        if let clackatoo_protocol::FormAsk::Url(url) = &form.ask {
            let _ = writeln!(text, "{url}");
        }
        let Some(filling) = &self.filling else {
            return;
        };
        let fields = form.fields();
        for row in filling.rows() {
            let line = match *row {
                Row::Option { field, option } => match &fields[field].input {
                    FieldInput::One { options, .. } | FieldInput::Many { options, .. } => {
                        let mark = if filling.picked(field, option) {
                            "[x]"
                        } else {
                            "[ ]"
                        };
                        format!("{mark} {}", options[option].title)
                    }
                    _ => continue,
                },
                Row::Toggle(field) => {
                    let mark = if filling.on(field) { "[x]" } else { "[ ]" };
                    format!("{mark} {}", fields[field].name())
                }
                Row::Text(field) => format!("{}: {}", fields[field].name(), filling.typed(field)),
                Row::Submit => "Submit".to_owned(),
                Row::Open => "Open the link".to_owned(),
                Row::Decline => "Decline".to_owned(),
            };
            let _ = writeln!(text, "{line}");
        }
    }

    /// What the bar says to a key that does nothing while a form waits.
    pub(super) fn form_hint() -> String {
        "a form waits: j/k move, Space picks, Enter presses Submit".to_owned()
    }
}
