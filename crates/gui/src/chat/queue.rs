//! Sending a draft and controlling the daemon-owned pending messages.
use clackatoo_protocol::{ChatAction, ChatTarget};

use super::{ChatView, Settled, handoff};

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
}
