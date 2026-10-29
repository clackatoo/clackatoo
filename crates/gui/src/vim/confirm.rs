//! A question the bar asks of whatever pane has the focus, answered with
//! one key, as Vim's `confirm()` and its "(y/n)" prompts are: each answer
//! is a key, and any other key, Esc among them, gives up, doing nothing.
//! The explorer asks before writing over or deleting a file, and a chat
//! before rewinding its conversation (CLACKA-198), through this one type.

use super::Stroke;
use crate::shortcuts::bindings::Hint;

/// A question, and what each of its answers' keys does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Confirm<T> {
    question: String,
    choices: Vec<(char, &'static str, T)>,
}

impl<T: Clone> Confirm<T> {
    /// Asks `question`, as the bar shows it whole: it says which keys
    /// answer it.
    pub(crate) fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            choices: Vec::new(),
        }
    }

    /// The same, with `key` (in either case) answering it with `then`,
    /// which `help` describes in Ctrl+?.
    #[must_use]
    pub(crate) fn choice(mut self, key: char, help: &'static str, then: T) -> Self {
        self.choices.push((key.to_ascii_lowercase(), help, then));
        self
    }

    /// What the bar shows.
    pub(crate) fn question(&self) -> &str {
        &self.question
    }

    /// What `stroke` answers: one of the choices, or `None`, giving up,
    /// for any other key.
    pub(crate) fn answer(&self, stroke: Stroke) -> Option<T> {
        let Stroke::Char { ch, ctrl: false } = stroke else {
            return None;
        };
        let ch = ch.to_ascii_lowercase();
        (self.choices.iter())
            .find(|(key, ..)| *key == ch)
            .map(|(.., then)| then.clone())
    }

    /// What Ctrl+? says `stroke` does.
    pub(crate) fn hint(&self, stroke: Stroke) -> Hint {
        let Stroke::Char { ch, ctrl: false } = stroke else {
            return Hint::Text("Cancel");
        };
        let ch = ch.to_ascii_lowercase();
        (self.choices.iter())
            .find(|(key, ..)| *key == ch)
            .map_or(Hint::Text("Cancel"), |(_, help, _)| Hint::Action(help))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_answers_and_any_other_gives_up() {
        let asked = Confirm::new("undo? (y/n)")
            .choice('y', "Undo", 1)
            .choice('n', "Keep", 2);
        assert_eq!(asked.answer(Stroke::char('y')), Some(1));
        assert_eq!(asked.answer(Stroke::char('Y')), Some(1));
        assert_eq!(asked.answer(Stroke::char('n')), Some(2));
        assert_eq!(asked.answer(Stroke::char('q')), None);
        assert_eq!(asked.answer(Stroke::Escape), None);
        assert_eq!(asked.answer(Stroke::ctrl('y')), None);
        assert_eq!(asked.question(), "undo? (y/n)");
        assert!(matches!(
            asked.hint(Stroke::char('n')),
            Hint::Action("Keep")
        ));
        assert!(matches!(asked.hint(Stroke::Escape), Hint::Text("Cancel")));
    }
}
