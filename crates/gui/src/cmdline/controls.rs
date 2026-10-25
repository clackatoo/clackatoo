//! Controls shared by editable prompts and their help.
use crate::input::KeyPress;
use crate::shortcuts::{
    Context,
    bindings::{self, Hint},
};
use crate::vim::Stroke;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Control {
    Accept,
    Cancel,
    Other,
}
impl Control {
    pub(crate) fn read(stroke: Stroke) -> (Self, Hint) {
        match stroke {
            Stroke::Enter => (Self::Accept, Hint::Action("Accept the text")),
            Stroke::Escape => (Self::Cancel, Hint::Action("Cancel prompt")),
            _ => (Self::Other, Hint::None),
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Register {
    Word,
    Command,
    Search,
    Named(char),
    None,
}
impl Register {
    pub(crate) fn read(press: &KeyPress<'_>) -> (Self, Hint) {
        use winit::keyboard::Key;
        let Key::Character(text) = press.key else {
            return (Self::None, Hint::None);
        };
        let Some(name) = text.chars().next() else {
            return (Self::None, Hint::None);
        };
        match (name, press.modifiers.ctrl) {
            ('w', true) => (Self::Word, Hint::Action("Insert word under editor cursor")),
            (_, true) => (Self::None, Hint::None),
            (':', false) => (Self::Command, Hint::Action("Insert last command")),
            ('/', false) => (Self::Search, Hint::Action("Insert last search")),
            (name, false) => (
                Self::Named(name),
                if crate::vim::is_register(name) {
                    Hint::Text("Insert register contents")
                } else {
                    Hint::None
                },
            ),
        }
    }
}

pub(crate) fn register_help(title: &str) -> Context {
    let mut context = bindings::from_presses(title, |press| Register::read(press).1);
    bindings::normal(&mut context, "Cancel prompt");
    context
}

pub(crate) fn history(key: &winit::keyboard::Key) -> Option<bool> {
    use winit::keyboard::{Key, NamedKey};
    match key {
        Key::Named(NamedKey::ArrowUp) => Some(true),
        Key::Named(NamedKey::ArrowDown) => Some(false),
        _ => None,
    }
}
