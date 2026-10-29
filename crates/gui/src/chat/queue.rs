//! Sending a draft and controlling the daemon-owned pending messages,
//! and taking one back into the box to edit, as Claude Code's Up does:
//! the daemon moves it out of the queue into the chat's draft in one step
//! (`ChatAction::TakeBack`), before what the draft held, and answers with
//! the message's text, which goes before what's typed in the box asking.
//! Nothing of it lives only in a window: with the window gone, the next to
//! open the chat starts with the draft.
use clackatoo_protocol::{ChatAction, ChatTarget, QueuedMessage};

use super::{ChatView, Settled, handoff};

/// Why `message`, waiting in the queue, can't be taken back into the box,
/// if it can't: the box would lose what it has beside its words.
pub(super) fn kept_back(message: &QueuedMessage) -> Option<&'static str> {
    if message.images > 0 {
        Some("That message has images, which the box can't take back: send or remove it")
    } else if message.target.is_some() {
        Some("That message moves the chat to another agent: send or remove it")
    } else {
        None
    }
}

/// Whether `message` can be taken back into the box (see [`kept_back`]).
pub(super) fn editable(message: &QueuedMessage) -> bool {
    kept_back(message).is_none()
}

impl ChatView {
    pub(super) fn submit_draft(&mut self, steer: bool, settled: &mut Settled) {
        if !self.queue_supported || self.handoff_sent.is_some() {
            return;
        }
        let text = self.editor.raw_text().trim().to_owned();
        if text.is_empty() && self.images.is_empty() {
            return;
        }
        let target = self.handoff.as_ref().map(|p| ChatTarget {
            agent: p.agent.clone(),
            model: p.model.clone(),
            budget: 0,
        });
        let images = std::mem::take(&mut self.images);
        settled.actions.push(ChatAction::Submit {
            text,
            images: images.clone(),
            target,
            steer,
        });
        self.handoff_sent = Some(handoff::Draft {
            text: self.editor.raw_text().to_owned(),
            images,
        });
        self.editor.set_text("");
        self.follow = true;
        self.picked = None;
    }
}

impl super::ChatView {
    pub(crate) fn queue_answer(&mut self, response: clackatoo_protocol::Response) {
        if let clackatoo_protocol::Response::Error(error) = response {
            self.say(error.to_string());
        }
    }

    /// Whether Up, in the box, takes back the newest message waiting: the
    /// box is empty, and one waits, and the daemon keeps drafts.
    pub(super) fn takes_back(&self) -> bool {
        self.queue_supported
            && self.drafts_supported
            && self.memory.taking_back.is_none()
            && self.handoff_sent.is_none()
            && self.editor.raw_text().is_empty()
            && self.images.is_empty()
            && self.node.delivery.queued.last().is_some()
    }

    /// Asks the daemon to move the message `id` out of the queue and into
    /// the draft, telling it first what's typed, so with the window gone
    /// the draft holds both; or says why it can't be.
    pub(super) fn take_back(&mut self, id: u64, settled: &mut Settled) {
        let Some(message) = (self.node.delivery.queued.iter()).find(|message| message.id == id)
        else {
            return;
        };
        if self.node.delivery.sending == Some(id) {
            self.say("That message is being sent".into());
            return;
        }
        if let Some(why) = kept_back(message) {
            self.say(why.into());
            return;
        }
        if !self.drafts_supported {
            self.say("Taking a queued message back needs a newer daemon".into());
            return;
        }
        if self.memory.taking_back.is_some() {
            return;
        }
        let text = message.text.clone();
        if let Some(typed) = self.take_draft(true) {
            settled.actions.push(ChatAction::Draft { text: typed });
        }
        self.memory.taking_back = Some(super::prompts::TakingBack { id, text });
        settled.actions.push(ChatAction::TakeBack { queued: id });
    }

    /// Whether the message `id` is being taken back into the box.
    #[cfg(test)]
    pub(crate) fn taking_back(&self, id: u64) -> bool {
        (self.memory.taking_back.as_ref()).is_some_and(|taking| taking.id == id)
    }
}
