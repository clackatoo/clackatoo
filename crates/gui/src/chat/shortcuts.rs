use super::{ChatView, read::ChatKeys};
use crate::shortcuts::{
    Context,
    bindings::{self, Hint},
};
use crate::vim::Mode;

impl ChatView {
    pub(crate) fn leaves_composer(&self, press: &crate::input::KeyPress<'_>) -> bool {
        crate::vim::enters_normal_mode(press)
            || (*press.key == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
                && self.node.question.is_none())
    }

    pub(crate) fn exclusive_shortcuts(&self) -> Option<Context> {
        if self.viewing_modal() {
            return Some(bindings::from_presses(
                if self.usage_modal {
                    "Usage limits"
                } else {
                    "Image viewer"
                },
                |press| self.describe_modal_key(press.key, crate::vim::enters_normal_mode(press)),
            ));
        }
        self.menu.as_ref().map(|_| {
            let mut context = bindings::from_presses(
                if self.searching_models() {
                    "Model picker"
                } else {
                    "Agent settings"
                },
                |press| {
                    let search = self.describe_model_search_key(press.key, press.modifiers);
                    if search == Hint::None {
                        self.describe_menu_choice_key(press.key)
                    } else {
                        search
                    }
                },
            );
            bindings::normal(&mut context, "Close popup");
            if self.searching_models() {
                bindings::paste(&mut context, "Paste into model search");
            }
            context
        })
    }

    pub(crate) fn shortcut_context(&self, mode: Mode) -> Context {
        if let Some(context) = self.prompt_shortcuts() {
            return context;
        }
        let mut context = if mode == Mode::Insert {
            if self.node.question.is_some() {
                let mut context = bindings::from_presses("Agent question", |press| {
                    if matches!(self.pick_of(press), Some((_, super::Pick::Refuse))) {
                        Hint::Action("Decline request")
                    } else {
                        Hint::None
                    }
                });
                bindings::normal(&mut context, "Enter normal mode");
                context
            } else {
                let mut context = bindings::from_presses("Agent composer", |press| {
                    if self.leaves_composer(press) {
                        Hint::Action("Enter normal mode")
                    } else {
                        self.describe_composer_key(press.key, press.modifiers)
                    }
                });
                bindings::normal(&mut context, "Enter normal mode");
                bindings::paste(&mut context, "Paste text or attach an image");
                context
            }
        } else {
            // Walked from how the chat reads keys, as every pane's are.
            let mut context =
                crate::vim::shortcuts::context("Agent conversation", &self.keys, mode, &ChatKeys);
            bindings::normal(&mut context, "Clear selection and pending command");
            context
        };
        if !self.keys.pending()
            && let Some(question) = &self.node.question
        {
            for (index, choice) in question.choices.iter().take(9).enumerate() {
                context.rows.push((
                    (index + 1).to_string(),
                    if mode == Mode::Insert {
                        choice.name.clone()
                    } else {
                        format!("Select {}; press again or Enter to confirm", choice.name)
                    },
                ));
            }
            if self.armed.is_some() {
                context.add(&[("Enter", "Confirm the selected answer")]);
            }
        }
        context
    }
}
