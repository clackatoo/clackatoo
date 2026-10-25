use super::{ChatView, read::Prefix};
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

    fn describe_conversation(
        &self,
        press: &crate::input::KeyPress<'_>,
        prefix: Option<Prefix>,
    ) -> Hint {
        let character = match press.key {
            winit::keyboard::Key::Character(text) => text.chars().next(),
            _ => None,
        };
        if press.modifiers.ctrl {
            return self.describe_read_control(
                character.map(|ch| ch.to_ascii_lowercase()),
                press.modifiers.shift,
            );
        }
        if prefix == Some(Prefix::Z) {
            let table = self.describe_table_scroll_key(character);
            if table != Hint::None {
                return table;
            }
            return self.describe_fold_bound(
                character,
                self.cursor.map(|cursor| cursor.text.turn),
                self.cursor
                    .is_some_and(|cursor| cursor.text.piece == super::paint::AGENTS),
            );
        }
        self.describe_read_character(press, character, prefix, self.visual())
    }

    #[allow(clippy::too_many_lines, reason = "context-specific key descriptions")]
    pub(crate) fn shortcut_context(&self, mode: Mode) -> Context {
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
            let mut context = Context::new("Agent conversation", &[]);
            for key in bindings::presses() {
                let kept = key.kept();
                let press = kept.press();
                let hint = self.describe_conversation(&press, self.read_prefix);
                if hint == Hint::Prefix {
                    let prefix = match press.key {
                        winit::keyboard::Key::Character(text) if text == "g" => Prefix::G,
                        _ => Prefix::Z,
                    };
                    for next in bindings::presses().filter(|key| {
                        !key.ctrl && !key.alt && matches!(key.base, crate::keymap::Base::Char(_))
                    }) {
                        let tail = next.kept();
                        let hint = self.describe_conversation(&tail.press(), Some(prefix));
                        if hint != self.describe_conversation(&tail.press(), None) {
                            bindings::add(
                                &mut context,
                                format!(
                                    "{}{}",
                                    crate::shortcuts::sequence(&[key]),
                                    crate::shortcuts::sequence(&[next])
                                ),
                                hint,
                            );
                        }
                    }
                } else {
                    bindings::add(&mut context, crate::shortcuts::sequence(&[key]), hint);
                }
            }
            bindings::normal(&mut context, "Clear selection and pending command");
            context
        };
        if self.read_prefix.is_none()
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
