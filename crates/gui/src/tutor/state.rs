//! What the tutor remembers between windows, in a small file under
//! `$XDG_STATE_HOME`: the step each lesson was left at, the lessons
//! finished, and the lines of what's new already noticed and read.
//!
//! ```text
//! step text-objects 2
//! done basics
//! noticed 3f2a9c0e1b7d4a56
//! read 3f2a9c0e1b7d4a56
//! ```

use std::collections::{BTreeMap, BTreeSet};

/// What the tutor remembers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct State {
    /// The step each lesson not finished was left at, from 0.
    pub(crate) steps: BTreeMap<String, usize>,
    /// The lessons finished.
    pub(crate) done: BTreeSet<String>,
    /// The lines of what's new the window has said there are, by key.
    pub(crate) noticed: BTreeSet<String>,
    /// The lines of what's new listed by `:Tutor new`, by key.
    pub(crate) read: BTreeSet<String>,
}

impl State {
    /// Reads the file's text. A line it doesn't know is passed over, so an
    /// older build reads a newer one's file.
    pub(crate) fn parse(text: &str) -> Self {
        let mut state = Self::default();
        for line in text.lines() {
            let mut words = line.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some("step"), Some(lesson), Some(step)) => {
                    if let Ok(step) = step.parse() {
                        state.steps.insert(lesson.to_owned(), step);
                    }
                }
                (Some("done"), Some(lesson), None) => {
                    state.done.insert(lesson.to_owned());
                }
                (Some("noticed"), Some(key), None) => {
                    state.noticed.insert(key.to_owned());
                }
                (Some("read"), Some(key), None) => {
                    state.read.insert(key.to_owned());
                }
                _ => {}
            }
        }
        state
    }

    /// Takes in what `ours` remembers, over what another window wrote: the
    /// step each lesson `ours` has a step for was left at is its, and
    /// everything else either remembers is kept.
    pub(crate) fn merge(&mut self, ours: &State) {
        self.steps.extend(
            ours.steps
                .iter()
                .map(|(lesson, &step)| (lesson.clone(), step)),
        );
        self.done.extend(ours.done.iter().cloned());
        self.noticed.extend(ours.noticed.iter().cloned());
        self.read.extend(ours.read.iter().cloned());
    }

    /// The file's text.
    pub(crate) fn write(&self) -> String {
        use std::fmt::Write as _;
        let mut text = String::from(
            "# clackatoo's tutor: where each lesson was left, and what's new that's been seen.\n",
        );
        let steps = self.steps.iter();
        let lines = (steps.map(|(lesson, step)| format!("step {lesson} {step}")))
            .chain(self.done.iter().map(|lesson| format!("done {lesson}")))
            .chain(self.noticed.iter().map(|key| format!("noticed {key}")))
            .chain(self.read.iter().map(|key| format!("read {key}")));
        for line in lines {
            writeln!(text, "{line}").expect("a String takes all that's written");
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_is_written_and_read_back_and_unknown_lines_are_passed_over() {
        let mut state = State::default();
        state.steps.insert("text-objects".into(), 2);
        state.done.insert("basics".into());
        state.noticed.insert("00ff".into());
        state.read.insert("00ff".into());
        let text = state.write();
        assert_eq!(State::parse(&text), state);
        let newer = format!("{text}favourite basics\nstep wrap many\n");
        assert_eq!(State::parse(&newer), state);
    }
}
