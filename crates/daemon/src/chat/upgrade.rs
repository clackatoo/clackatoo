//! Only planned upgrades continue work automatically. The handover carries
//! explicit intent, so a historical failure never becomes a new instruction.

use std::fmt::Write as _;

use clackatoo_protocol::{ChatImage, ChatPreview, ChatState, Item, Outcome, TerminalId, code_span};

use super::{HostedChat, Link, Message, Prompt};
use crate::handover::{self, ChatContinuation, QueuedImage, ResumePrompt};

const CONTINUE: &str = "Clackatoo resumed this session after the daemon restarted for an \
    upgrade, which interrupted the active turn. Continue the user's latest request from where you left off. \
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
            // A turn the agent took by itself is nothing the user asked
            // for, so there's no request of theirs to go on with: it's
            // handed over ended, and the agent's own next turn, if it
            // takes one, comes as before.
            if self.woken() {
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
    /// `stopped` are the background tasks the upgrade ended, which the
    /// agent is told of with the prompt that goes on.
    pub(crate) fn continue_after_upgrade(
        &mut self,
        continuation: ChatContinuation,
        stopped: &[String],
    ) -> bool {
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
        let mut prompt = match continuation.pending {
            Some(pending) => Prompt {
                text: pending.text,
                images: pending.images.into_iter().map(restore_image).collect(),
            },
            None => Prompt {
                text: CONTINUE.to_owned(),
                images: Vec::new(),
            },
        };
        if let Some(said) = stopped_tasks(stopped) {
            prompt.text.push_str("\n\n");
            prompt.text.push_str(&said);
        }
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

impl HostedChat {
    /// Where the chat stands, beyond what [`Self::hand_over`] keeps, into
    /// `talk`, for the next build: what its agent runs in the background,
    /// the turns it took by itself, and which of its answers each chat has
    /// had.
    pub(crate) fn hand_over_standing(&self, talk: &mut handover::Talk) {
        talk.tasks.extend(self.hand_over_tasks());
        // A turn the user asked to stop isn't told of after, nor what it ran.
        if self.answering() && (self.stop_requested || self.skipping() || self.woken_stopping()) {
            talk.stopped.push(self.id.0);
        }
        let woke = (self.node.turns.iter().enumerate()).filter_map(|(turn, each)| {
            Some(handover::WokeTurn {
                chat: self.id.0,
                turn: turn as u64,
                why: each.woke.clone()?,
            })
        });
        talk.woke.extend(woke);
        let heard = self.answers_heard.borrow();
        talk.heard
            .extend(heard.iter().map(|(&by, &turns)| handover::Heard {
                chat: self.id.0,
                by: by.0,
                turns: turns as u64,
            }));
    }

    /// Takes back where the chat stood, from `talk`, as the last build
    /// handed it over. Returns the background tasks the upgrade stopped,
    /// to tell the agent.
    pub(crate) fn resume_standing(&mut self, talk: &mut handover::Talk, live: bool) -> Vec<String> {
        let id = self.id.0;
        for woke in (talk.woke.iter()).filter(|woke| woke.chat == id) {
            let at = usize::try_from(woke.turn).unwrap_or(usize::MAX);
            if let Some(turn) = self.node.turns.get_mut(at) {
                turn.woke = Some(woke.why.clone());
            }
        }
        for heard in (talk.heard.iter()).filter(|heard| heard.chat == id) {
            let turns = usize::try_from(heard.turns).unwrap_or(usize::MAX);
            self.heard_by(TerminalId(heard.by), turns.min(self.node.turns.len()));
        }
        match talk.tasks.iter().position(|tasks| tasks.chat == id) {
            Some(at) => self.resume_tasks(talk.tasks.swap_remove(at), live),
            None => Vec::new(),
        }
    }

    /// The turn the agent took by itself that the upgrade cut off, if its
    /// last was one: its number, and why it took it, if it said.
    fn cut_off_by_itself(&self) -> Option<(usize, String)> {
        let index = self.node.turns.len().checked_sub(1)?;
        let turn = &self.node.turns[index];
        let cut = matches!(
            turn.end.as_ref().map(|end| &end.outcome),
            Some(Outcome::Failed(why)) if why == CUT_OFF
        );
        let why = turn.woke.clone().filter(|_| cut)?;
        let number = clackatoo_protocol::turn_number(&self.node.turns, index).unwrap_or(index + 1);
        Some((number, why))
    }

    /// Tells the agent, as a notice from clackatoo that starts a turn of
    /// its own, what the upgrade stopped, if it isn't told as its turn goes
    /// on: the background tasks `stopped`, and the turn it had taken by
    /// itself, if the upgrade cut one off. One notice, once: it starts no
    /// new chain of messages, but its turn counts as one a notice started
    /// (see `Daemon::pass_upgrade_notice`). Nothing is told a chat whose
    /// user had asked it to stop. The daemon may have restarted as the
    /// same build, if the next one failed to start, so it says restarted.
    /// Returns whether there was anything to tell.
    pub(crate) fn tell_upgrade(&mut self, stopped: &[String]) -> bool {
        let cut = self.cut_off_by_itself();
        if stopped.is_empty() && cut.is_none() {
            return false;
        }
        let mut text = "[daemon restarted] Clackatoo's daemon restarted for an upgrade, which \
            ended your agent's process."
            .to_owned();
        if let Some((number, why)) = cut {
            let why = if why.is_empty() {
                String::new()
            } else {
                format!(", after {}", code_span(&why))
            };
            let _ = write!(
                text,
                " It cut off turn {number}, which you had taken by yourself{why}: look at where \
                 it got to before going on."
            );
        }
        if let Some(said) = stopped_tasks(stopped) {
            text.push(' ');
            text.push_str(&said);
        }
        (self.relations.inbox).push_back(Message::upgrade_notice(self.id, text));
        true
    }
}

/// How a turn the upgrade ended says it ended.
pub(crate) const CUT_OFF: &str = "cut off by an upgrade";

/// What the agent is told of the background tasks `stopped`, which the
/// upgrade ended, if there are any.
fn stopped_tasks(stopped: &[String]) -> Option<String> {
    if stopped.is_empty() {
        return None;
    }
    let names: Vec<String> = stopped.iter().map(|name| code_span(name)).collect();
    Some(format!(
        "The upgrade also stopped what you ran in the background, which went with your \
         process: {}. Start again whatever you still need.",
        names.join(", ")
    ))
}

pub(super) fn keep_image(image: &ChatImage) -> QueuedImage {
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

pub(super) fn restore_image(image: QueuedImage) -> ChatImage {
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
