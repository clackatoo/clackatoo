//! The message being typed in a chat's box and not sent yet: kept by the
//! daemon, so a window closed and opened again, another window, or the
//! phone starts with it, and handed over through an upgrade (CLACKA-192).
//!
//! Clients say what's typed as typing pauses (`ChatAction::Draft`), and
//! the chat keeps the last they said, text only, whichever client said
//! it. A client starts with it only if nothing's typed there yet, so a
//! draft never replaces what someone is typing. The agent never hears of
//! it.

use clackatoo_protocol::{ChatPatch, DRAFT_LONGEST};

use super::HostedChat;
use crate::handover;

impl HostedChat {
    /// Keeps `text` as what's typed in the box, cut to [`DRAFT_LONGEST`]
    /// bytes at a character's edge.
    pub(super) fn set_draft(&mut self, mut text: String) {
        if text.len() > DRAFT_LONGEST {
            let end = text.floor_char_boundary(DRAFT_LONGEST);
            text.truncate(end);
        }
        if self.node.draft != text {
            self.node.draft.clone_from(&text);
            self.journal.push(ChatPatch::Draft(text));
        }
    }

    /// A client sent `text`: if it's what was typed in the box, it's
    /// gone from the box, whichever client sent it, so none starts with
    /// it again, as a phone that doesn't say what's typed would leave it.
    pub(crate) fn sent_from_box(&mut self, text: &str) {
        if !self.node.draft.is_empty() && self.node.draft.trim() == text.trim() {
            self.set_draft(String::new());
        }
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
            self.set_draft(draft.text);
        }
    }
}
