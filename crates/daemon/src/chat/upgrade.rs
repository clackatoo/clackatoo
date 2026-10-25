//! Only planned upgrades continue work automatically. The handover carries
//! explicit intent, so a historical failure never becomes a new instruction.

use clackatoo_protocol::{ChatImage, ChatPreview, ChatState, Item};

use super::{HostedChat, Link, Prompt};
use crate::handover::{ChatContinuation, QueuedImage, ResumePrompt};

const CONTINUE: &str = "Clackatoo resumed this session after a daemon upgrade interrupted \
    the active turn. Continue the user's latest request from where you left off. \
    The upgrade may have interrupted a tool after it already acted: inspect its \
    result or current state before repeating it. Do not assume an interrupted \
    command failed or rerun completed actions. Keep the user's existing scope \
    and permissions; an unanswered approval is not permission. \
    Use read_activities to recover managed operation IDs and recorded outcomes before \
    starting replacement work. Reusing an operation ID inspects the existing job, \
    never starts it twice.";

impl HostedChat {
    pub(crate) fn prepare_upgrade(&mut self) -> Option<ChatContinuation> {
        self.keep_held_for_upgrade();
        if let Some((id, _, _)) = self.user_queue.native {
            self.steered(id, Err("Interrupted by an upgrade".into()));
        }
        self.answering_turn().and_then(|turn| {
            if self.stop_requested || self.skipping() {
                return None;
            }
            Some(ChatContinuation {
                chat: self.id.0,
                turn: turn as u64,
                pending: self.queued.as_ref().map(|prompt| ResumePrompt {
                    role: self.role.clone(),
                    text: prompt.text.clone(),
                    images: prompt.images.iter().map(keep_image).collect(),
                }),
                // An unacknowledged steering delivery may already have
                // acted; keep its queued copy paused for explicit review.
                queue_paused: self.node.delivery.paused,
            })
        })
    }

    /// Reopens the same visible turn, preparing one prompt for the usual
    /// session/connect path. The server calls this once, after restoring
    /// images and queues, and starts connections after restoring all state.
    pub(crate) fn continue_after_upgrade(&mut self, continuation: ChatContinuation) -> bool {
        let Some(index) = self.node.turns.len().checked_sub(1) else {
            return false;
        };
        if continuation.chat != self.id.0
            || continuation.turn != index as u64
            || self.link != Link::Closed
            || self.node.turns[index].end.is_none()
        {
            return false;
        }
        // If the original prompt was sent, only its existing session can
        // safely continue it. Never start an empty conversation with this
        // instruction and guess what work the user meant.
        if continuation.pending.is_none() && self.session.is_none() {
            return false;
        }
        self.role = continuation.pending.as_ref().and_then(|p| p.role.clone());
        let prompt = match continuation.pending {
            Some(pending) => Prompt {
                text: pending.text,
                images: pending.images.into_iter().map(restore_image).collect(),
            },
            None => Prompt {
                text: CONTINUE.to_owned(),
                images: Vec::new(),
            },
        };
        let turn = &mut self.node.turns[index];
        turn.end = None;
        turn.items
            .push(Item::Other("Continuing after a daemon upgrade.".into()));
        self.patch_turn(index);
        self.stop_requested = false;
        self.queued = Some(prompt);
        self.link = Link::Opening;
        self.pause_queue(continuation.queue_paused);
        self.set_state(ChatState::Starting);
        true
    }
}

fn keep_image(image: &ChatImage) -> QueuedImage {
    QueuedImage {
        data: image.data.clone(),
        width: image.width,
        height: image.height,
        preview: image
            .preview
            .as_ref()
            .map(|p| crate::handover::ChatPreview {
                width: p.width,
                height: p.height,
                rgba: p.rgba.to_vec(),
            }),
    }
}

fn restore_image(image: QueuedImage) -> ChatImage {
    ChatImage {
        data: image.data,
        width: image.width,
        height: image.height,
        preview: image.preview.map(|p| ChatPreview {
            original: 0,
            width: p.width,
            height: p.height,
            rgba: p.rgba.into(),
        }),
    }
}
