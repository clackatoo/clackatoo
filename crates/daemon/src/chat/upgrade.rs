//! Planned upgrades continue work automatically, and so does an agent whose
//! process stopped by itself as it answered, once per turn. The handover
//! carries explicit intent, so a historical failure never becomes a new
//! instruction.

use std::fmt::Write as _;

use clackatoo_protocol::{
    ChatImage, ChatPreview, ChatState, Item, Outcome, TerminalId, ToolStatus, code_span,
};

use super::{Asker, HostedChat, Link, Message, Prompt};
use crate::handover::{self, ChatContinuation, QueuedImage, ResumePrompt};

const CONTINUE: &str = "Clackatoo resumed this session after the daemon restarted for an \
    upgrade, which interrupted the active turn. Continue the user's latest request from where you left off. \
    The upgrade may have interrupted a tool after it already acted: inspect its \
    result or current state before repeating it. Do not assume an interrupted \
    command failed or rerun completed actions. Keep the user's existing scope \
    and permissions; an unanswered approval is not permission. \
    Use read_activities to recover managed operation IDs and recorded outcomes before \
    starting replacement work. Reusing an operation ID never starts it twice: a kept \
    one is inspected, and one let go long ago is refused.";

/// What the agent is told as its turn goes on after its process stopped
/// by itself.
const CONTINUE_AFTER_STOP: &str = "Clackatoo started your agent's process again: it stopped \
    unexpectedly during this turn, perhaps killed by the system for using too much memory. \
    Continue the work of this turn from where you left off. A tool may have been interrupted \
    after it already acted: inspect its result or the current state before repeating it, and \
    don't assume an interrupted command failed. If a heavy build or test was running, it may be \
    what used the memory: if you still need it, run it again lighter, with fewer jobs or \
    threads. Keep the user's existing scope and permissions; an unanswered approval is not \
    permission.";

/// What a turn shows where it went on after the agent's process stopped,
/// by which it's known to have once.
const WENT_ON_AFTER_STOP: &str = "Continuing after the agent's process stopped.";

impl HostedChat {
    /// The agent's process stopped by itself, having started, as when the
    /// system kills it for memory. A turn under way goes on, in the same
    /// session and visible turn, once the agent has started again, as
    /// after an upgrade that stopped it: once a turn, so one that brings it
    /// down again ends, as any turn does as its agent stops. Not one the
    /// user asked to stop, nor one telling its session again. Returns
    /// whether it goes on, for the server to start the agent; otherwise
    /// it's as [`Self::disconnected`].
    pub(crate) fn agent_died(&mut self) -> bool {
        let went_on = |turn: &clackatoo_protocol::Turn| {
            (turn.items.iter())
                .any(|item| matches!(item, Item::Other(said) if said == WENT_ON_AFTER_STOP))
        };
        let goes_on = self.answering_turn().filter(|&index| {
            !self.stop_requested
                && !self.told_again()
                && matches!(self.link, Link::Open | Link::Opening)
                && (self.session.is_some() || self.queued.is_some())
                && !went_on(&self.node.turns[index])
        });
        let Some(index) = goes_on else {
            self.disconnected(None);
            return false;
        };
        let lost = self.going_task_names();
        // Steering it may have had and acted on is kept for review, as
        // through an upgrade.
        let paused = self.node.delivery.paused || self.user_queue.native.is_some();
        self.setting_asked.clear();
        self.put_back.clear();
        self.tasks_lost();
        self.elicitations_lost();
        self.queue_disconnected();
        self.pause_queue(paused);
        self.clear_slow();
        // A message it never had goes as it was; otherwise it's told to go on.
        let prompt = self.queued.take().unwrap_or_else(|| {
            let mut text = CONTINUE_AFTER_STOP.to_owned();
            if !lost.is_empty() {
                let names: Vec<String> = lost.iter().map(|name| code_span(name)).collect();
                let _ = write!(
                    text,
                    "\n\nWhat you ran in the background stopped with your process: {}. Start \
                     again whatever you still need.",
                    names.join(", ")
                );
            }
            Prompt {
                resolved_context: false,
                context: Vec::new(),
                text,
                images: Vec::new(),
            }
        });
        // What it was running, and what it asked, went with its process.
        self.set_retrying(None);
        let turn = &mut self.node.turns[index];
        for item in &mut turn.items {
            if let Some(tool) = item.tool_mut()
                && tool.status == ToolStatus::Running
            {
                tool.status = ToolStatus::Failed;
            }
        }
        turn.items.push(Item::Other(WENT_ON_AFTER_STOP.into()));
        self.patch_turn(index);
        (self.asking).retain(|(asker, _)| !matches!(asker, Asker::Agent(_)));
        self.show_question();
        self.queued = Some(prompt);
        self.link = Link::Opening;
        self.set_state(ChatState::Starting);
        true
    }

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
        let pending_context = self.node.turns[index].context.clone();
        let mut prompt = match continuation.pending {
            Some(pending) => Prompt {
                resolved_context: !pending_context.is_empty(),
                context: pending_context,
                text: pending.text,
                images: pending.images.into_iter().map(restore_image).collect(),
            },
            None => Prompt {
                resolved_context: false,
                context: Vec::new(),
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
        // A turn the user asked to stop isn't told of after, nor what it
        // ran; and a chat they stopped stays held for them (see
        // `held_by_stop`), what waits waiting on. A load passed over is
        // taken for a stop too, as the build before took it.
        let stopping = self.answering() && (self.skipping() || self.woken_stopping());
        if self.stop_requested || stopping {
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
    /// (see `Daemon::pass_upgrade_notice`). A chat whose user had stopped
    /// it holds it until they write there. The daemon may have restarted as the
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
            image: None,
        }),
    }
}
