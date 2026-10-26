//! A buffer only the window has, as a folder's listing being edited is:
//! its copy's changes are answered here, as the daemon answers them, from
//! a text and history of its own, so undo, redo, macros and everything
//! else an editor does work on it as they do on a file's.
//!
//! It's the daemon's side of the protocol for one buffer with one window:
//! the copy sends its changes as ever, one at a time, and the desk hands
//! the answers back as it would the daemon's, so nothing in between knows
//! the difference. A question for a language server is answered that
//! there's nothing to say: no server reads it.

use clackatoo_protocol::{
    Answer, Author, BufferId, BufferSnapshot, BufferUpdate, Change, ClientMessage, Edit,
};
use clackatoo_text::{History, Rope};

/// The daemon's side of a buffer only the window has.
#[derive(Debug)]
pub(crate) struct Scratch {
    path: String,
    text: Rope,
    history: History<Author>,
    /// The text as it began, to say whether it's changed since.
    first: Rope,
    /// Whether two texts say the same, as far as being changed goes.
    same: fn(&Rope, &Rope) -> bool,
    modified: bool,
}

impl Scratch {
    /// A buffer called `path`, holding `text`, changed once `same` says
    /// its text no longer says what it began with.
    pub(crate) fn new(path: &str, text: &str, same: fn(&Rope, &Rope) -> bool) -> Self {
        Self {
            path: path.to_owned(),
            text: Rope::from_str(text),
            history: History::new(0),
            first: Rope::from_str(text),
            same,
            modified: false,
        }
    }

    /// What a copy starts from.
    pub(crate) fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            path: self.path.clone(),
            text: self.text.to_string(),
            version: self.history.version(),
            modified: self.modified,
            applied: None,
            landed: None,
            daemon: 0,
        }
    }

    /// The text's version: how many edits have been made to it.
    fn version(&self) -> u64 {
        self.history.version()
    }

    /// Makes `change`, as the user's, as the daemon makes a window's
    /// change: an edit made to version `base` is brought up to date first.
    /// Returns how the copy is told what it made, or why it couldn't be.
    fn make(&mut self, change: Change, seq: u64) -> Result<BufferUpdate, String> {
        let text = |err: clackatoo_text::HistoryError| err.to_string();
        let made = match change {
            Change::Edit { edit, base, step } => self.edit(edit, base, step, None),
            Change::Step { edit, base, state } => self.edit(edit, base, true, Some(state)),
            Change::Undo | Change::Redo => {
                let reversed = if change == Change::Undo {
                    self.history.undo(&Author::User)
                } else {
                    self.history.redo(&Author::User)
                };
                match reversed.map_err(text)? {
                    Some(edit) => {
                        edit.apply(&mut self.text).map_err(|err| err.to_string())?;
                        BufferUpdate::Edited {
                            version: self.history.version(),
                            edit,
                            author: Author::User,
                            answers: Some(seq),
                        }
                    }
                    None => BufferUpdate::NoChange(seq),
                }
            }
            // Only its own copy edits it, so places are in the text now.
            Change::Empty { state, .. } => {
                let len = self.text.len_bytes();
                (self.history.empty_step(&Author::User, len, Some(state))).map_err(text)?;
                BufferUpdate::NoChange(seq)
            }
            Change::Reverse { redo, visual, .. } => {
                let reversed = self.history.reverse(&Author::User, !redo, visual, false);
                match reversed.map_err(text)? {
                    Some(reversal) => {
                        (reversal.edit.apply(&mut self.text)).map_err(|err| err.to_string())?;
                        BufferUpdate::Reversed {
                            version: self.history.version(),
                            seq,
                            edit: reversal.edit,
                            state: reversal.state,
                        }
                    }
                    None => BufferUpdate::NoChange(seq),
                }
            }
        };
        Ok(match made {
            BufferUpdate::Acked { version, .. } => BufferUpdate::Acked { version, seq },
            made => made,
        })
    }

    /// Makes `edit`, made to version `base`, starting a step that keeps
    /// what `step` says, or joining the last.
    fn edit(
        &mut self,
        edit: Edit,
        base: u64,
        step: bool,
        state: Option<clackatoo_text::StepState>,
    ) -> BufferUpdate {
        let made = (self.history.rebase(edit, base))
            .map_err(|err| err.to_string())
            .and_then(|edit| {
                edit.apply(&mut self.text).map_err(|err| err.to_string())?;
                (self.history.record_with(edit, Author::User, step, state))
                    .map_err(|err| err.to_string())
            });
        match made {
            Ok(version) => BufferUpdate::Acked { version, seq: 0 },
            Err(err) => {
                // Never so, as only its own copy edits it; but if it were,
                // the copy starts afresh from the text.
                log::error!("a scratch buffer's change didn't fit: {err}");
                BufferUpdate::Snapshot(self.snapshot())
            }
        }
    }

    /// Answers `message`, which a copy of `buffer` would send the daemon,
    /// as the daemon would: what the copy is to be told, in turn.
    pub(crate) fn answer(&mut self, buffer: BufferId, message: ClientMessage) -> Vec<BufferUpdate> {
        let version = self.version();
        let nothing = |id| BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Nothing,
        };
        let mut answers = match message {
            ClientMessage::Edit { seq, change, .. } => match self.make(change, seq) {
                Ok(answer) => vec![answer],
                Err(err) => {
                    // Never so, as only its own copy edits it; but if it
                    // were, the copy starts afresh from the text.
                    log::error!("scratch buffer {buffer}: a change didn't fit: {err}");
                    vec![BufferUpdate::Snapshot(self.snapshot())]
                }
            },
            ClientMessage::Query { id, .. }
            | ClientMessage::Rename { id, .. }
            | ClientMessage::Stage { id, .. } => vec![nothing(id)],
            other => {
                log::debug!("scratch buffer {buffer}: {other:?} isn't for a buffer");
                Vec::new()
            }
        };
        let modified = !(self.same)(&self.text, &self.first);
        if modified != self.modified {
            self.modified = modified;
            answers.push(BufferUpdate::Modified(modified));
        }
        answers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::BufferCopy;

    /// Hands everything `copy` has to hear back to it, as the desk does.
    fn answer_all(copy: &mut BufferCopy) {
        loop {
            let answers = copy.take_answers();
            if answers.is_empty() {
                break;
            }
            for answer in answers {
                copy.update(answer);
            }
        }
    }

    #[test]
    fn a_scratch_copy_answers_itself_as_the_daemon_would() {
        let mut copy = BufferCopy::scratch(BufferId(u32::MAX), "/p", "a\nb\n");
        assert!(copy.loaded() && copy.is_scratch() && !copy.modified());
        copy.edit(Edit::replace(4, 0, "a", "x"), true);
        copy.edit(Edit::replace(4, 1, "", "y"), false);
        assert!(copy.take_outbox().is_empty(), "nothing goes to the daemon");
        answer_all(&mut copy);
        assert_eq!(copy.text().to_string(), "xy\nb\n");
        assert!(copy.modified());
        // Undone as one step, and the copy's told it was its own undo.
        copy.reverse(true, None);
        let answers = copy.take_answers();
        assert!(matches!(
            answers.as_slice(),
            [
                BufferUpdate::Reversed { state: None, .. },
                BufferUpdate::Modified(false)
            ]
        ));
        for answer in answers {
            copy.update(answer);
        }
        assert_eq!(copy.text().to_string(), "a\nb\n");
        assert!(!copy.modified());
        copy.reverse(true, None);
        let heard: Vec<_> = (copy.take_answers().into_iter())
            .map(|answer| copy.update(answer))
            .collect();
        assert_eq!(
            heard,
            [crate::editor::Heard::NothingToReverse { undo: true }]
        );
        copy.reverse(false, None);
        answer_all(&mut copy);
        assert_eq!(copy.text().to_string(), "xy\nb\n");
        // A question for a language server: there's nothing to say.
        let id = copy.ask(clackatoo_protocol::Query::Hover, 0);
        let heard: Vec<_> = (copy.take_answers().into_iter())
            .map(|answer| copy.update(answer))
            .collect();
        assert_eq!(
            heard,
            [crate::editor::Heard::Answer {
                id,
                answer: Answer::Nothing
            }]
        );
        // A lost connection loses nothing of it.
        copy.edit(Edit::replace(5, 5, "", "z"), true);
        copy.reconnected();
        answer_all(&mut copy);
        assert_eq!(copy.text().to_string(), "xy\nb\nz");
    }
}
