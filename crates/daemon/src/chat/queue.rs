//! User messages belong to the daemon, independently of attached windows.
use std::collections::VecDeque;

use clackatoo_protocol::{ChatImage, ChatPatch, ChatTarget, QueuedMessage};

use super::{HostedChat, Prompt};

const MAX_MESSAGES: usize = 8;
const MAX_TEXT: usize = 64 * 1024;
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Default)]
pub(super) struct Queue {
    pub messages: VecDeque<Pending>,
    pub next: u64,
    pub restart: Option<u64>,
    /// The steering message being delivered, the turn it shows in, by its
    /// uid, and the item it's shown as there.
    pub native: Option<(u64, super::TurnUid, usize)>,
}

#[derive(Clone, Debug)]
pub(crate) struct Pending {
    pub id: u64,
    pub prompt: Prompt,
    pub target: Option<ChatTarget>,
}

impl Pending {
    fn bytes(&self) -> usize {
        self.prompt.text.len()
            + self
                .prompt
                .images
                .iter()
                .map(|i| i.data.len())
                .sum::<usize>()
    }
}

/// Validate before accepting the message or changing a provider.
pub(crate) fn validate(text: &str, images: &[ChatImage]) -> Result<(), String> {
    if text.trim().is_empty() && images.is_empty() {
        return Err("Write a message or attach an image first.".into());
    }
    if text.len() > MAX_TEXT {
        return Err("The message exceeds 64 KiB.".into());
    }
    if images.len() > ChatImage::MAX_COUNT
        || images.iter().any(|i| !i.valid())
        || images.iter().map(|i| i.data.len()).sum::<usize>() > ChatImage::MAX_DATA
    {
        return Err("Images are too large or invalid.".into());
    }
    Ok(())
}

impl HostedChat {
    pub(crate) fn fail_queue(&mut self, text: String) {
        self.user_queue.restart = None;
        self.pause_queue(true);
        self.queue_notice(text);
    }

    pub(crate) fn queue_notice(&mut self, text: String) {
        self.set_notice(Some(text));
    }

    pub(crate) fn enqueue_user(
        &mut self,
        text: String,
        images: Vec<ChatImage>,
        target: Option<ChatTarget>,
    ) -> Result<u64, String> {
        validate(&text, &images)?;
        if target
            .as_ref()
            .is_some_and(|t| t.agent.len() > 256 || t.model.as_ref().is_some_and(|m| m.len() > 512))
        {
            return Err("The queued provider or model name is too long.".into());
        }
        let id = self
            .user_queue
            .next
            .checked_add(1)
            .ok_or("Message IDs exhausted.")?;
        let pending = Pending {
            id,
            prompt: Prompt { text, images },
            target,
        };
        let bytes = self
            .user_queue
            .messages
            .iter()
            .map(Pending::bytes)
            .sum::<usize>();
        if self.user_queue.messages.len() >= MAX_MESSAGES || bytes + pending.bytes() > MAX_BYTES {
            return Err(
                "The queue is full (8 messages or 32 MiB). Send or remove a message first.".into(),
            );
        }
        self.user_queue.next = id;
        self.user_queue.messages.push_back(pending);
        self.show_queue();
        Ok(id)
    }

    pub(crate) fn queued_user(&self, id: u64) -> Option<&Pending> {
        self.user_queue.messages.iter().find(|p| p.id == id)
    }

    pub(crate) fn next_user(&self) -> Option<u64> {
        if self.answering() || self.user_queue.native.is_some() {
            return None;
        }
        self.user_queue.restart.or_else(|| {
            (!self.node.delivery.paused)
                .then(|| self.user_queue.messages.front().map(|p| p.id))
                .flatten()
        })
    }

    pub(crate) fn has_user_queue(&self) -> bool {
        !self.user_queue.messages.is_empty()
    }

    pub(crate) fn resume_dispatched(&mut self, id: u64) {
        if self.user_queue.restart == Some(id) {
            self.pause_queue(false);
        }
    }

    pub(crate) fn take_queued_user(&mut self, id: u64) -> Result<Pending, String> {
        if self.node.delivery.sending == Some(id) {
            return Err("This message is being delivered.".into());
        }
        if self.user_queue.restart == Some(id) {
            self.user_queue.restart = None;
        }
        let at = self
            .user_queue
            .messages
            .iter()
            .position(|p| p.id == id)
            .ok_or("That queued message was already sent or removed.")?;
        let pending = self.user_queue.messages.remove(at).expect("position found");
        self.show_queue();
        Ok(pending)
    }

    pub(crate) fn pause_queue(&mut self, paused: bool) {
        if self.node.delivery.paused != paused {
            self.node.delivery.paused = paused;
            self.show_queue();
        }
    }

    pub(crate) fn set_native_steering(&mut self, supported: bool) {
        self.node.delivery.native_steering = supported;
        self.show_queue();
    }

    pub(crate) fn steer_restart(&mut self, id: u64) -> Vec<super::Action> {
        let mut actions = Vec::new();
        self.stop(&mut actions);
        self.user_queue.restart = Some(id);
        self.show_queue();
        actions
    }

    pub(crate) fn begin_steering(&mut self, id: u64) -> Result<String, String> {
        if self.user_queue.native.is_some() {
            return Err("A steering message is already being delivered.".into());
        }
        let text = self
            .queued_user(id)
            .ok_or("The queued message is gone.")?
            .prompt
            .text
            .clone();
        let turn = self.answering_turn().ok_or("The turn has finished.")?;
        let item = self.node.turns[turn].items.len();
        let value = super::Item::Other(format!(
            "**You · Steering (sending)**\n\n{}",
            text.lines()
                .map(|l| format!("> {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
        self.node.turns[turn].items.push(value.clone());
        self.patch_item(turn, item, value);
        let uid = self.turn_uid(turn).unwrap_or_default();
        self.user_queue.native = Some((id, uid, item));
        self.node.delivery.sending = Some(id);
        self.show_queue();
        Ok(text)
    }

    pub(crate) fn steered(&mut self, id: u64, result: Result<String, String>) {
        let Some((pending, turn, item)) = self.user_queue.native else {
            return;
        };
        if pending != id {
            return;
        }
        self.user_queue.native = None;
        self.node.delivery.sending = None;
        let label = match result {
            Ok(outcome) if outcome == "injected" => {
                let _ = self.take_queued_user(id);
                "You · Steering"
            }
            Ok(outcome) if outcome == "promptRequired" => {
                // The adapter did not consume this message; the old prompt's
                // completion still owns the turn. Wait for it before sending.
                if !self.node.delivery.paused {
                    self.user_queue.restart = Some(id);
                }
                "Steering queued for the next turn"
            }
            _ => {
                self.pause_queue(true);
                self.queue_notice("Steering was not confirmed. The message remains paused; check the conversation before retrying.".into());
                "Steering not confirmed"
            }
        };
        let Some(turn) = self.turn_index(turn) else {
            self.show_queue();
            return;
        };
        if let Some(super::Item::Other(text)) = self
            .node
            .turns
            .get_mut(turn)
            .and_then(|t| t.items.get_mut(item))
        {
            *text = text.replacen("You · Steering (sending)", label, 1);
            let value = super::Item::Other(text.clone());
            self.patch_item(turn, item, value);
        }
        self.show_queue();
    }

    pub(crate) fn queue_disconnected(&mut self) {
        if let Some((id, _, _)) = self.user_queue.native {
            self.steered(id, Err("Disconnected".into()));
        }
        self.user_queue.restart = None;
        self.pause_queue(true);
        self.set_native_steering(false);
    }

    fn show_queue(&mut self) {
        self.node.delivery.queued = self
            .user_queue
            .messages
            .iter()
            .map(|p| QueuedMessage {
                id: p.id,
                text: p.prompt.text.clone(),
                images: u8::try_from(p.prompt.images.len()).unwrap_or(u8::MAX),
                target: p.target.clone(),
            })
            .collect();
        self.journal
            .push(ChatPatch::Delivery(self.node.delivery.clone()));
    }
}

impl HostedChat {
    pub(crate) fn hand_over_queue(&self) -> crate::handover::ChatQueue {
        use crate::handover::{
            ChatPreview, ChatQueue, QueuedChatMessage, QueuedImage, QueuedTarget,
        };
        ChatQueue {
            chat: self.id.0,
            next: self.user_queue.next,
            messages: self
                .user_queue
                .messages
                .iter()
                .map(|p| QueuedChatMessage {
                    id: p.id,
                    text: p.prompt.text.clone(),
                    target: p.target.as_ref().map(|t| QueuedTarget {
                        agent: t.agent.clone(),
                        model: t.model.clone(),
                        budget: t.budget,
                    }),
                    images: p
                        .prompt
                        .images
                        .iter()
                        .map(|i| QueuedImage {
                            data: i.data.clone(),
                            width: i.width,
                            height: i.height,
                            preview: i.preview.as_ref().map(|p| ChatPreview {
                                width: p.width,
                                height: p.height,
                                rgba: p.rgba.to_vec(),
                            }),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    pub(crate) fn resume_queue(&mut self, records: &mut [crate::handover::ChatQueue]) {
        let Some(record) = records.iter_mut().find(|r| r.chat == self.id.0) else {
            return;
        };
        for p in std::mem::take(&mut record.messages) {
            let images = p
                .images
                .into_iter()
                .map(|i| ChatImage {
                    data: i.data,
                    width: i.width,
                    height: i.height,
                    preview: i.preview.map(|p| clackatoo_protocol::ChatPreview {
                        width: p.width,
                        height: p.height,
                        rgba: p.rgba.into(),
                        original: 0,
                    }),
                })
                .collect();
            let target = p.target.map(|t| ChatTarget {
                agent: t.agent,
                model: t.model,
                budget: t.budget,
            });
            if self.enqueue_user(p.text, images, target).is_ok() {
                self.user_queue
                    .messages
                    .back_mut()
                    .expect("just inserted")
                    .id = p.id;
            }
        }
        self.user_queue.next = record.next.max(
            self.user_queue
                .messages
                .iter()
                .map(|p| p.id)
                .max()
                .unwrap_or(0),
        );
        // Messages handed over wait, shown paused, for the user to send
        // or drop: any may have been delivered as the upgrade came. An
        // empty queue stays paused too, so one paused by a failure keeps
        // that; a message the user sends next unpauses it (CLACKA-185).
        self.node.delivery.paused = true;
        self.show_queue();
    }
}
