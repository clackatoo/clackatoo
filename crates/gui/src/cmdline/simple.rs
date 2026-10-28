//! The small append-only prompts used by list panes, and what's typed into
//! a picker (see [`crate::picker`]).
use crate::shortcuts::{
    Context,
    bindings::{self, Hint},
};
use crate::vim::Stroke;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Accept,
    Cancel,
    Edited,
    Other,
}

#[derive(Clone, Copy)]
enum Action {
    Accept,
    Cancel,
    Backspace,
    Clear,
    Word,
    Type(char),
    Other,
}

impl Action {
    fn read(stroke: Stroke, empty: bool) -> (Self, Hint) {
        match stroke {
            Stroke::Escape => (Self::Cancel, Hint::Action("Cancel prompt")),
            Stroke::Enter => (Self::Accept, Hint::Action("Accept")),
            Stroke::Backspace if empty => (Self::Cancel, Hint::Action("Cancel empty prompt")),
            Stroke::Backspace => (Self::Backspace, Hint::Action("Delete last character")),
            Stroke::Char {
                ch: 'u',
                ctrl: true,
            } => (Self::Clear, Hint::Action("Clear text")),
            Stroke::Char {
                ch: 'w',
                ctrl: true,
            } => (Self::Word, Hint::Action("Delete last word")),
            Stroke::Char { ch, ctrl: false } => (Self::Type(ch), Hint::Text("Insert text")),
            _ => (Self::Other, Hint::None),
        }
    }
}

pub(crate) fn key(text: &mut String, stroke: Stroke) -> Outcome {
    match Action::read(stroke, text.is_empty()).0 {
        Action::Cancel => return Outcome::Cancel,
        Action::Accept => return Outcome::Accept,
        Action::Other => return Outcome::Other,
        Action::Backspace => {
            text.pop();
        }
        Action::Clear => text.clear(),
        Action::Word => {
            let kept = text.trim_end_matches(|ch: char| !ch.is_alphanumeric());
            let kept = kept.trim_end_matches(char::is_alphanumeric).len();
            text.truncate(kept);
        }
        Action::Type(ch) => text.push(ch),
    }
    Outcome::Edited
}

/// What [`key`] does with `stroke`, as a prompt's help says it, the
/// prompt holding nothing if `empty`.
pub(crate) fn hint(stroke: Stroke, empty: bool) -> Hint {
    Action::read(stroke, empty).1
}

pub(crate) fn help(title: &str, text: &str) -> Context {
    let mut context = bindings::list(title, None, |stroke, _| {
        Action::read(stroke, text.is_empty()).1
    });
    crate::shortcuts::bindings::normal(&mut context, "Cancel prompt");
    crate::shortcuts::bindings::paste(&mut context, "Paste clipboard text");
    context
}
