//! The message being typed in a chat's box and not sent yet: kept by the
//! daemon, so a window closed and opened again, a window after an
//! upgrade, another window, or the phone starts with it (CLACKA-192).
//!
//! Clients say what's typed as typing pauses and as they close
//! (`ChatAction::Draft`), and the chat keeps the last said, whichever
//! client said it. It isn't sent to viewers as it changes: typing isn't
//! mirrored from box to box. A client takes it only into a box that's
//! empty and untouched there, as it first shows the chat. Sending a
//! message empties it, and tells viewers what was sent
//! (`ChatPatch::DraftCleared`), for every box holding just that to empty
//! too. Taking a queued message back puts it in the draft, before what
//! the draft held, and answers with the message's text, for the client
//! asking to put before what's typed in its own box. The agent never
//! hears of any of it.

use clackatoo_protocol::{ChatPatch, DRAFT_LONGEST};

use super::HostedChat;
use crate::handover;

impl HostedChat {
    /// What's typed in the box, as the chat keeps it.
    pub(crate) fn draft(&self) -> &str {
        self.node.draft.as_deref().unwrap_or("")
    }

    /// Keeps `text`, cut to [`DRAFT_LONGEST`] bytes at a character's edge,
    /// as what's typed in the box.
    pub(crate) fn set_draft(&mut self, mut text: String) {
        if text.len() > DRAFT_LONGEST {
            text.truncate(text.floor_char_boundary(DRAFT_LONGEST));
        }
        self.node.draft = Some(text);
    }

    /// A client sent `sent` from the box: the draft is empty, and viewers
    /// are told what was sent, for boxes holding just that to empty.
    pub(crate) fn draft_sent(&mut self, sent: String) {
        self.node.draft = Some(String::new());
        self.journal.push(ChatPatch::DraftCleared { sent });
    }

    /// Takes the message `id` out of the queue and into the draft, before
    /// what the draft held, in one step, and returns the message's text,
    /// for the client asking to put before what's typed in its own box;
    /// with none open, the next to open the chat starts with the draft. A message with images, or one that
    /// moves the chat to another agent, is refused, as a draft holds text
    /// alone.
    pub(crate) fn take_back(&mut self, id: u64) -> Result<String, String> {
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
        let text = message.prompt.text;
        let draft = match self.draft() {
            "" => text.clone(),
            typed => format!("{text}\n{typed}"),
        };
        self.set_draft(draft);
        Ok(text)
    }

    /// What's typed in the box, for the next build, if anything is.
    pub(crate) fn hand_over_draft(&self) -> Option<handover::ChatDraft> {
        (!self.draft().is_empty()).then(|| handover::ChatDraft {
            chat: self.id.0,
            text: self.draft().to_owned(),
        })
    }

    /// Takes what was typed in the box from the build before, if it kept
    /// anything for this chat.
    pub(crate) fn resume_draft(&mut self, drafts: &mut Vec<handover::ChatDraft>) {
        if let Some(at) = drafts.iter().position(|draft| draft.chat == self.id.0) {
            let draft = drafts.swap_remove(at);
            self.set_draft(draft.text);
        }
    }
}
