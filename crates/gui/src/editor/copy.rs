//! The window's copy of a buffer: edited as you type, without waiting for
//! the daemon, and kept in step with it.
//!
//! The copy is the daemon's text at the version it last heard of, with the
//! window's own changes the daemon hasn't answered yet made on top. One
//! change is on its way at a time; the next goes once the daemon answers.
//! An edit by someone else arrives made to the daemon's text, so it's
//! brought past the changes still waiting before it's made to the copy,
//! and they're brought past it, as the daemon brought them past it (see
//! [`clackatoo_text`]).
//!
//! It keeps the buffer's highlights too, moving them along with each edit
//! until the parse that follows it sends new ones.

use std::collections::VecDeque;

use clackatoo_protocol::{BufferId, BufferUpdate, Change, ClientMessage, Edit};
use clackatoo_text::{Highlights, Rope, Span, compose, transform};

/// A change waiting for the daemon's answer, or to be sent.
#[derive(Debug)]
enum Waiting {
    Edit { edit: Edit, step: bool },
    Undo,
    Redo,
}

/// What an update from the daemon did to the copy, for the panes showing
/// it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// Nothing they need to know.
    Nothing,
    /// The text is new: a snapshot replaced it.
    Replaced,
    /// Someone else's edit was made to it.
    Edited(Edit),
    /// The window's undo or redo was made: this edit.
    Reversed(Edit),
    /// The window's undo or redo found nothing to do: an undo if `undo`.
    NothingToReverse { undo: bool },
}

/// A window's copy of a buffer.
#[derive(Debug)]
pub(crate) struct BufferCopy {
    id: BufferId,
    path: String,
    text: Rope,
    /// The daemon's version the copy is made from.
    version: u64,
    modified: bool,
    /// Whether a snapshot has arrived.
    loaded: bool,
    /// The window's changes not yet answered, oldest first.
    waiting: VecDeque<Waiting>,
    /// The number the first of them was sent with, if it's on its way.
    sent: Option<u64>,
    next_seq: u64,
    outbox: Vec<ClientMessage>,
    /// The edits made here since last taken, for other panes showing the
    /// buffer to follow.
    made: Vec<Edit>,
    /// What each piece of the text is, as its grammar reads it.
    highlights: Highlights,
}

impl BufferCopy {
    pub(crate) fn new(id: BufferId) -> Self {
        Self {
            id,
            path: String::new(),
            text: Rope::new(),
            version: 0,
            modified: false,
            loaded: false,
            waiting: VecDeque::new(),
            sent: None,
            next_seq: 0,
            outbox: Vec::new(),
            made: Vec::new(),
            highlights: Highlights::new(),
        }
    }

    pub(crate) fn text(&self) -> &Rope {
        &self.text
    }

    pub(crate) fn highlights(&self) -> &Highlights {
        &self.highlights
    }

    /// The file's name, for titles.
    pub(crate) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub(crate) fn modified(&self) -> bool {
        self.modified
    }

    pub(crate) fn loaded(&self) -> bool {
        self.loaded
    }

    /// Makes `edit` to the copy and sends it. It starts a new undo step if
    /// `step`, or joins the last. Returns whether it could be made: an edit
    /// that doesn't fit the text is a mistake, and dropped.
    pub(crate) fn edit(&mut self, edit: Edit, step: bool) -> bool {
        if edit.is_noop() {
            return true;
        }
        if let Err(err) = edit.apply(&mut self.text) {
            log::error!("buffer {}: an edit didn't fit: {err}", self.id);
            return false;
        }
        self.highlights.map(&edit);
        self.made.push(edit.clone());
        // An edit joining the last step joins the last edit, if that's
        // still to be sent.
        let unsent = usize::from(self.sent.is_some());
        if !step
            && self.waiting.len() > unsent
            && let Some(Waiting::Edit { edit: last, .. }) = self.waiting.back_mut()
            && let Ok(both) = compose(last, &edit)
        {
            *last = both;
        } else {
            self.waiting.push_back(Waiting::Edit { edit, step });
        }
        self.send_next();
        true
    }

    /// Asks the daemon to undo the user's last step, or redo the last
    /// undone.
    pub(crate) fn reverse(&mut self, undo: bool) {
        self.waiting
            .push_back(if undo { Waiting::Undo } else { Waiting::Redo });
        self.send_next();
    }

    /// Takes in what the daemon says about the buffer.
    pub(crate) fn update(&mut self, update: BufferUpdate) -> Heard {
        match update {
            BufferUpdate::Snapshot(snapshot) => {
                self.path = snapshot.path;
                self.text = Rope::from_str(&snapshot.text);
                self.version = snapshot.version;
                self.modified = snapshot.modified;
                self.loaded = true;
                self.waiting.clear();
                self.sent = None;
                self.highlights.clear();
                Heard::Replaced
            }
            BufferUpdate::Acked { version, seq } => {
                if self.sent == Some(seq) {
                    self.waiting.pop_front();
                    self.sent = None;
                    self.version = version;
                    self.send_next();
                }
                Heard::Nothing
            }
            BufferUpdate::Edited {
                version,
                edit,
                answers,
                ..
            } => {
                let reversal = answers.is_some() && answers == self.sent;
                if reversal {
                    self.waiting.pop_front();
                    self.sent = None;
                }
                let Some(edit) = self.bring_past_waiting(edit) else {
                    return Heard::Nothing;
                };
                if let Err(err) = edit.apply(&mut self.text) {
                    log::error!("buffer {}: the daemon's edit didn't fit: {err}", self.id);
                    return Heard::Nothing;
                }
                self.highlights.map(&edit);
                self.version = version;
                self.send_next();
                if reversal {
                    Heard::Reversed(edit)
                } else {
                    Heard::Edited(edit)
                }
            }
            BufferUpdate::NoChange(seq) => {
                if self.sent != Some(seq) {
                    return Heard::Nothing;
                }
                let undo = matches!(self.waiting.pop_front(), Some(Waiting::Undo));
                self.sent = None;
                self.send_next();
                Heard::NothingToReverse { undo }
            }
            BufferUpdate::Modified(modified) => {
                self.modified = modified;
                Heard::Nothing
            }
            BufferUpdate::Highlights {
                version,
                start,
                end,
                spans,
            } => {
                self.recolour(version, start, end, spans);
                Heard::Nothing
            }
        }
    }

    /// Takes highlights for bytes `start` to `end` of the daemon's text at
    /// `version`, moving them past the window's edits waiting.
    fn recolour(&mut self, version: u64, start: usize, end: usize, spans: Vec<Span>) {
        if version != self.version {
            // Not for the text the copy is made from, which the daemon
            // never sends.
            log::debug!("buffer {}: highlights for version {version}", self.id);
            return;
        }
        let waiting = (self.waiting.iter()).filter_map(|waiting| match waiting {
            Waiting::Edit { edit, .. } => Some(edit),
            Waiting::Undo | Waiting::Redo => None,
        });
        (self.highlights).replace_since(start..end, spans, waiting);
    }

    /// The connection was lost. A new one sends a snapshot, and changes
    /// not answered are lost with the old one.
    pub(crate) fn reconnected(&mut self) {
        self.waiting.clear();
        self.sent = None;
    }

    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        std::mem::take(&mut self.outbox)
    }

    /// The edits made here since last asked, oldest first.
    pub(crate) fn take_made(&mut self) -> Vec<Edit> {
        std::mem::take(&mut self.made)
    }

    /// `edit`, made to the daemon's text, brought past the window's edits
    /// waiting, which are brought past it in turn.
    fn bring_past_waiting(&mut self, mut edit: Edit) -> Option<Edit> {
        for waiting in &mut self.waiting {
            let Waiting::Edit { edit: mine, .. } = waiting else {
                continue;
            };
            match transform(mine, &edit) {
                Ok((mine_after, edit_after)) => {
                    *mine = mine_after;
                    edit = edit_after;
                }
                Err(err) => {
                    log::error!("buffer {}: the daemon's edit didn't fit: {err}", self.id);
                    return None;
                }
            }
        }
        Some(edit)
    }

    /// Sends the first change waiting, if none is on its way.
    fn send_next(&mut self) {
        if self.sent.is_some() || !self.loaded {
            return;
        }
        let Some(first) = self.waiting.front() else {
            return;
        };
        let change = match first {
            Waiting::Edit { edit, step } => Change::Edit {
                edit: edit.clone(),
                base: self.version,
                step: *step,
            },
            Waiting::Undo => Change::Undo,
            Waiting::Redo => Change::Redo,
        };
        let seq = self.next_seq;
        self.next_seq += 1;
        self.sent = Some(seq);
        self.outbox.push(ClientMessage::Edit {
            buffer: self.id,
            seq,
            change,
        });
    }
}

#[cfg(test)]
pub(super) mod tests {
    use clackatoo_protocol::{Author, BufferSnapshot};
    use clackatoo_text::History;

    use super::*;

    /// The daemon's side, as the daemon does it.
    pub(in crate::editor) struct Daemon {
        pub(in crate::editor) text: Rope,
        history: History<Author>,
    }

    impl Daemon {
        pub(in crate::editor) fn new(text: &str) -> Self {
            Self {
                text: Rope::from_str(text),
                history: History::new(0),
            }
        }

        pub(in crate::editor) fn snapshot(&self) -> BufferUpdate {
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/a".into(),
                text: self.text.to_string(),
                version: self.history.version(),
                modified: false,
            })
        }

        /// Makes the change a window sent, answering it as the daemon
        /// does: to the window, and to everyone else.
        pub(in crate::editor) fn take(
            &mut self,
            message: ClientMessage,
        ) -> (BufferUpdate, Option<BufferUpdate>) {
            let ClientMessage::Edit { seq, change, .. } = message else {
                panic!("an edit");
            };
            match change {
                Change::Edit { edit, base, step } => {
                    let edit = self.history.rebase(edit, base).unwrap();
                    edit.apply(&mut self.text).unwrap();
                    let version = self
                        .history
                        .record(edit.clone(), Author::User, step)
                        .unwrap();
                    let others = BufferUpdate::Edited {
                        version,
                        edit,
                        author: Author::User,
                        answers: None,
                    };
                    (BufferUpdate::Acked { version, seq }, Some(others))
                }
                Change::Undo | Change::Redo => {
                    let edit = if change == Change::Undo {
                        self.history.undo(&Author::User).unwrap()
                    } else {
                        self.history.redo(&Author::User).unwrap()
                    };
                    let Some(edit) = edit else {
                        return (BufferUpdate::NoChange(seq), None);
                    };
                    edit.apply(&mut self.text).unwrap();
                    let version = self.history.version();
                    let answer = BufferUpdate::Edited {
                        version,
                        edit: edit.clone(),
                        author: Author::User,
                        answers: Some(seq),
                    };
                    let others = BufferUpdate::Edited {
                        version,
                        edit,
                        author: Author::User,
                        answers: None,
                    };
                    (answer, Some(others))
                }
            }
        }

        /// An agent's edit, made in the daemon.
        pub(in crate::editor) fn agent(&mut self, at: usize, text: &str) -> BufferUpdate {
            let edit = Edit::replace(self.text.len_bytes(), at, "", text);
            edit.apply(&mut self.text).unwrap();
            let author = Author::Agent(clackatoo_protocol::TerminalId(9));
            let version = self.history.record(edit.clone(), author, true).unwrap();
            BufferUpdate::Edited {
                version,
                edit,
                author,
                answers: None,
            }
        }
    }

    fn typed(copy: &mut BufferCopy, at: usize, text: &str, step: bool) {
        let edit = Edit::replace(copy.text.len_bytes(), at, "", text);
        assert!(copy.edit(edit, step));
    }

    #[test]
    fn typing_while_an_agent_edits_ends_with_the_same_text_everywhere() {
        let mut daemon = Daemon::new("fn main() {}\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());

        // You type twice; the first is sent, the second waits.
        typed(&mut copy, 11, "x", true);
        typed(&mut copy, 12, "y", false);
        let sent = copy.take_outbox();
        assert_eq!(sent.len(), 1, "one change on its way at a time");
        // Meanwhile an agent edits, before the daemon gets yours.
        let agents = daemon.agent(0, "pub ");
        let (ack, _) = daemon.take(sent.into_iter().next().unwrap());
        // The window hears the agent's edit first, then its own answered.
        assert!(matches!(copy.update(agents), Heard::Edited(_)));
        assert_eq!(copy.text.to_string(), "pub fn main() {xy}\n");
        copy.update(ack);
        for message in copy.take_outbox() {
            daemon.take(message);
        }
        assert_eq!(daemon.text.to_string(), "pub fn main() {xy}\n");
        assert_eq!(copy.text.to_string(), daemon.text.to_string());
    }

    #[test]
    fn highlights_move_with_edits_and_land_past_those_not_answered() {
        use clackatoo_text::{Kind, Span};
        let span = |start, end, kind| Span { start, end, kind };
        let mut daemon = Daemon::new("fn a\n");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        copy.update(BufferUpdate::Highlights {
            version: 0,
            start: 0,
            end: 5,
            spans: vec![span(0, 2, Kind::Keyword), span(3, 4, Kind::Function)],
        });
        // Typed at the start, before the daemon has it: the spans move.
        typed(&mut copy, 0, "pub ", true);
        assert_eq!(
            copy.highlights().spans(),
            [span(4, 6, Kind::Keyword), span(7, 8, Kind::Function)]
        );
        // Answered; then more is typed, and the daemon's parse of the text
        // it has comes while that's still on its way.
        let (ack, _) = daemon.take(copy.take_outbox().remove(0));
        copy.update(ack);
        typed(&mut copy, 0, "x", true);
        copy.update(BufferUpdate::Highlights {
            version: 1,
            start: 0,
            end: 9,
            spans: vec![span(0, 3, Kind::Keyword), span(4, 6, Kind::Keyword)],
        });
        assert_eq!(
            copy.highlights().spans(),
            [span(1, 4, Kind::Keyword), span(5, 7, Kind::Keyword)],
            "what the parse found replaces what was there, past the \"x\""
        );
        // Highlights for a version the copy isn't made from are dropped.
        copy.update(BufferUpdate::Highlights {
            version: 0,
            start: 0,
            end: 9,
            spans: Vec::new(),
        });
        assert_eq!(copy.highlights().spans().len(), 2);
    }

    #[test]
    fn undo_comes_back_from_the_daemon_and_waits_its_turn() {
        let mut daemon = Daemon::new("a");
        let mut copy = BufferCopy::new(BufferId(1));
        copy.update(daemon.snapshot());
        typed(&mut copy, 1, "b", true);
        copy.reverse(true);
        // The undo waits for the edit to be answered.
        let mut messages = copy.take_outbox();
        assert_eq!(messages.len(), 1);
        let (ack, _) = daemon.take(messages.remove(0));
        copy.update(ack);
        let undo = copy.take_outbox().remove(0);
        // Typing while the undo is on its way.
        typed(&mut copy, 2, "c", true);
        let (answer, _) = daemon.take(undo);
        assert!(matches!(copy.update(answer), Heard::Reversed(_)));
        assert_eq!(copy.text.to_string(), "ac");
        for message in copy.take_outbox() {
            let (ack, _) = daemon.take(message);
            copy.update(ack);
        }
        assert_eq!(daemon.text.to_string(), "ac");
        copy.reverse(true);
        copy.reverse(true);
        for _ in 0..2 {
            let message = copy.take_outbox().remove(0);
            let (answer, _) = daemon.take(message);
            copy.update(answer);
        }
        assert_eq!(copy.text.to_string(), "a");
        copy.reverse(true);
        let (answer, _) = daemon.take(copy.take_outbox().remove(0));
        assert_eq!(copy.update(answer), Heard::NothingToReverse { undo: true });
    }
}
