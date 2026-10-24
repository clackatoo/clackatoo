//! Accept and dispatch user messages through the existing chat lifecycle.
use clackatoo_protocol::{ChatAction, Error, TerminalId};

use super::Daemon;

impl Daemon {
    pub(super) fn submit_chat(
        &mut self,
        chat: TerminalId,
        text: String,
        images: Vec<clackatoo_protocol::ChatImage>,
        target: Option<clackatoo_protocol::ChatTarget>,
        steer: bool,
    ) -> Result<(), Error> {
        if let Some(target) = &target {
            self.config
                .agents
                .find(&target.agent)
                .map_err(Error::Other)?;
        }
        let state = self.chat(chat).ok_or(Error::NotAChat(chat))?;
        let idle = !state.answering() && state.node().delivery.sending.is_none();
        if steer && state.node().delivery.sending.is_some() {
            return Err(Error::Other(
                "A steering message is already being delivered.".into(),
            ));
        }
        if idle && !state.has_user_queue() {
            state.pause_queue(false);
        }
        let id = state
            .enqueue_user(text, images, target)
            .map_err(Error::Other)?;
        self.watch_chat_queue(chat);
        // Once accepted, the daemon owns the draft. A dispatch failure must
        // pause it here, not tell the client to restore a duplicate copy.
        let result = if steer && !idle {
            self.steer_queued(chat, id)
        } else if idle {
            self.dispatch_queued(chat, id)
        } else {
            Ok(())
        };
        if let Err(error) = result
            && let Some(state) = self.chat(chat)
        {
            state.fail_queue(format!("Message queued but not sent: {error}"));
        }
        self.collect_damage(chat);
        Ok(())
    }

    pub(in crate::server) fn dispatch_queued(
        &mut self,
        chat: TerminalId,
        id: u64,
    ) -> Result<(), Error> {
        let state = self.chat(chat).ok_or(Error::NotAChat(chat))?;
        if state.answering() || state.node().delivery.sending.is_some() {
            return Err(Error::Other(
                "The agent is still working. Use Steer now to redirect it.".into(),
            ));
        }
        let target = state
            .queued_user(id)
            .ok_or_else(|| Error::Other("That message was already sent or removed.".into()))?
            .target
            .clone();
        if let Some(target) = target {
            self.config
                .agents
                .find(&target.agent)
                .map_err(Error::Other)?;
            self.switch_chat(chat, &target.agent, target.model, target.budget as usize);
            if self
                .chat_ref(chat)
                .is_none_or(|c| c.agent() != target.agent)
            {
                return Err(Error::Other(
                    "The queued provider handoff was refused.".into(),
                ));
            }
        }
        let state = self.chat(chat).ok_or(Error::NotAChat(chat))?;
        state.resume_dispatched(id);
        let pending = state.take_queued_user(id).map_err(Error::Other)?;
        let actions = state.act(ChatAction::SendImages {
            text: pending.prompt.text,
            images: pending.prompt.images,
        });
        self.act(chat, actions);
        self.collect_damage(chat);
        Ok(())
    }

    pub(super) fn send_queued(
        &mut self,
        chat: TerminalId,
        id: u64,
        steer: bool,
    ) -> Result<(), Error> {
        let state = self.chat(chat).ok_or(Error::NotAChat(chat))?;
        if state.queued_user(id).is_none() || state.node().delivery.sending.is_some() {
            return Err(Error::Other(
                "That message is gone or a steering request is still pending.".into(),
            ));
        }
        let paused = state.node().delivery.paused;
        state.pause_queue(false);
        let result = if steer
            && self
                .chat_ref(chat)
                .is_some_and(crate::chat::HostedChat::answering)
        {
            self.steer_queued(chat, id)
        } else {
            self.dispatch_queued(chat, id)
        };
        if let Err(error) = result {
            if let Some(state) = self.chat(chat) {
                state.pause_queue(paused);
            }
            self.collect_damage(chat);
            return Err(error);
        }
        self.watch_chat_queue(chat);
        self.collect_damage(chat);
        Ok(())
    }
}
