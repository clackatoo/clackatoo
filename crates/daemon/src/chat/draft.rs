//! The message being typed in a chat's box and not sent yet: kept by the
//! daemon, so a window closed and opened again, another window, or the
//! phone starts with it, and handed over through an upgrade (CLACKA-192).
//!
//! One rule, the same in every client: a client says what's typed
//! (`ChatAction::Draft`, with a number of its own, `by`) as typing pauses,
//! and takes a draft that comes, empty ones too, only if its box hasn't
//! changed since the last draft it took or said, and never its own (the
//! patch carries `by` back). So what's typed somewhere is never replaced,
//! and a box nobody has touched follows: sending in one window empties
//! the box in another.
//!
//! Every draft a client says comes back to it as a patch, the same text
//! or not, so it knows where its own falls among the others': the
//! journal sends each viewer drafts in the order they were said, and one
//! another client said that comes before its own came back was said
//! before its own, and is passed over.
//!
//! Drafts are numbered (`rev`), and only the latest one's text is kept in
//! the chat's journal: an earlier one there is emptied and numbered 0,
//! keeping its `by` for its client to know it came back, for the others
//! to pass over, so typing never pushes out what viewers still need. A
//! chat sends its viewers a draft at most every [`DRAFT_PACE`]; one said
//! sooner is kept at once, for a snapshot, and sent once the pace allows,
//! drafts said meanwhile by the same client going as one. The agent never
//! hears of any of it.

use std::time::{Duration, Instant};

use clackatoo_protocol::{ChatPatch, DRAFT_LONGEST};

use super::HostedChat;
use crate::handover;

/// How often, at most, a chat sends its viewers what's typed in its box.
pub(crate) const DRAFT_PACE: Duration = Duration::from_millis(500);

/// Where a chat's draft stands, beside its text in `ChatNode::draft`.
#[derive(Debug, Default)]
pub(crate) struct Draft {
    /// The latest draft's number, from 1.
    rev: u64,
    /// The client that set it, by its number; 0 for the daemon.
    by: u64,
    /// When a draft was last sent to viewers.
    sent: Option<Instant>,
    /// A draft waits for the pace to be sent.
    waiting: bool,
}

impl HostedChat {
    /// Keeps `text`, cut to [`DRAFT_LONGEST`] bytes at a character's edge,
    /// as what's typed in the box, as the client numbered `by` says.
    /// Returns when it's to be sent to viewers, if the pace holds it back
    /// (see [`send_draft`](Self::send_draft)).
    pub(crate) fn set_draft(&mut self, mut text: String, by: u64) -> Option<Instant> {
        if text.len() > DRAFT_LONGEST {
            text.truncate(text.floor_char_boundary(DRAFT_LONGEST));
        }
        // Another client's, held back, goes first, so it comes back to it.
        if self.draft.waiting && self.draft.by != by {
            self.push_draft();
        }
        self.node.draft = text;
        self.draft.rev += 1;
        self.draft.by = by;
        let due = (self.draft.sent).map(|sent| sent + DRAFT_PACE);
        match due {
            Some(due) if Instant::now() < due => {
                self.draft.waiting = true;
                Some(due)
            }
            _ => {
                self.push_draft();
                None
            }
        }
    }

    /// Sends viewers the draft the pace held back, if one waits.
    pub(crate) fn send_draft(&mut self) {
        if self.draft.waiting {
            self.push_draft();
        }
    }

    fn push_draft(&mut self) {
        self.draft.waiting = false;
        self.draft.sent = Some(Instant::now());
        self.journal.push(ChatPatch::Draft {
            text: self.node.draft.clone(),
            rev: self.draft.rev,
            by: self.draft.by,
        });
    }

    /// Takes the message `id` out of the queue and into the draft, before
    /// what's typed there, in one step, and sends it at once: every client
    /// whose box hasn't changed takes it, and with none open, the next to
    /// open the chat starts with it. A message with images, or one that
    /// moves the chat to another agent, is refused, as a draft holds text
    /// alone.
    pub(crate) fn take_back(&mut self, id: u64) -> Result<(), String> {
        let message = self
            .queued_user(id)
            .ok_or("That queued message was already sent or removed.")?;
        if !message.prompt.images.is_empty() {
            return Err("That message has images, which the box can't take back.".into());
        }
        if message.target.is_some() {
            return Err("That message moves the chat to another agent.".into());
        }
        let message = self.take_queued_user(id)?;
        let text = match self.node.draft.as_str() {
            "" => message.prompt.text,
            typed => format!("{}\n{typed}", message.prompt.text),
        };
        self.set_draft(text, 0);
        self.send_draft();
        Ok(())
    }

    /// What's typed in the box, for the next build, if anything is.
    pub(crate) fn hand_over_draft(&self) -> Option<handover::ChatDraft> {
        (!self.node.draft.is_empty()).then(|| handover::ChatDraft {
            chat: self.id.0,
            text: self.node.draft.clone(),
        })
    }

    /// Takes what was typed in the box from the build before, if it kept
    /// anything for this chat.
    pub(crate) fn resume_draft(&mut self, drafts: &mut Vec<handover::ChatDraft>) {
        if let Some(at) = drafts.iter().position(|draft| draft.chat == self.id.0) {
            let draft = drafts.swap_remove(at);
            self.set_draft(draft.text, 0);
        }
    }
}
