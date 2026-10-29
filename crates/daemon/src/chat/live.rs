//! A chat whose agent goes on through an upgrade (CLACKA-188): its
//! adapter's process isn't stopped, but handed to the next build with its
//! pipes, so the chat goes on as it was. A turn being answered stays
//! open, its tools running, and nothing tells the agent anything: no
//! continuation, no notice, as nothing was lost. What the agent asked
//! the user, which it still waits for, is still asked.
//!
//! [`HostedChat::hand_over`] keeps what any chat keeps; this keeps the
//! rest, for a chat whose agent is still there to go on with it.

use std::time::{Duration, Instant};

use clackatoo_protocol::{ChatState, Choice, ChoiceKind, Outcome, Question};

use super::upgrade::{keep_image, restore_image};
use super::{Asker, HostedChat, Link, Prompt, RawDiff};
use crate::handover::{self, LiveAsker, LiveChat, LiveChoice, LiveLink};

impl HostedChat {
    /// Whether the chat can go on with its agent through an upgrade as it
    /// is: it isn't loading its session, nor passing over what's left of a
    /// load, nor moving to another agent. Otherwise its agent is stopped
    /// as before an upgrade could keep it.
    pub(crate) fn can_go_on(&self) -> bool {
        !self.told_again() && self.carry.is_none()
    }

    /// Hands the chat over to the next build, whose agent goes on with it
    /// (see [`can_go_on`](Self::can_go_on)): a turn being answered stays
    /// open, its tools running, and what the chat waits on goes with it.
    pub(crate) fn hand_over_live(mut self) -> (handover::Chat, LiveChat) {
        let now = Instant::now();
        let left = |until: Instant| millis(until.saturating_duration_since(now));
        let live = LiveChat {
            chat: self.id.0,
            link: match self.link {
                Link::Closed => LiveLink::Closed,
                Link::Opening => LiveLink::Opening,
                Link::Open => LiveLink::Open,
                Link::Closing => LiveLink::Closing,
            },
            queued: self.queued.take().map(|prompt| handover::ResumePrompt {
                role: self.role.take(),
                text: prompt.text,
                images: prompt.images.iter().map(keep_image).collect(),
            }),
            stop_requested: self.stop_requested,
            slow: self.slow,
            heard: self.heard.map(|heard| heard as u64),
            woke_by_task: self.woke_by_task.take(),
            woken_stopping: self.woken_stopping,
            wake_held: self.wake_held.map(left),
            own_tools: self.own_tools.drain(..).collect(),
            diffs: (self.diffs.drain(..))
                .map(|(tool, diffs)| {
                    let diffs = (diffs.into_iter())
                        .map(|diff| handover::LiveDiff {
                            path: diff.path,
                            old: diff.old,
                            new: diff.new,
                        })
                        .collect();
                    (tool, diffs)
                })
                .collect(),
            asking: (self.asking.drain(..))
                .filter_map(|(asker, question)| hand_over_question(asker, question))
                .collect(),
            next_question: self.next_question,
            native: (self.user_queue.native).map(|(id, turn, item)| (id, turn as u64, item as u64)),
            restart: self.user_queue.restart,
            queue_paused: self.node.delivery.paused,
        };
        (self.into_handover(), live)
    }

    /// Goes on as the last build handed the chat over with its agent,
    /// `live`, which goes on too: once the rest of it is resumed, its
    /// queue included. A turn it was answering goes on.
    pub(crate) fn resume_live(&mut self, live: LiveChat) {
        let now = Instant::now();
        self.link = match live.link {
            LiveLink::Closed => Link::Closed,
            LiveLink::Opening => Link::Opening,
            LiveLink::Open => Link::Open,
            LiveLink::Closing => Link::Closing,
        };
        if let Some(queued) = live.queued {
            self.role = queued.role;
            self.queued = Some(Prompt {
                text: queued.text,
                images: queued.images.into_iter().map(restore_image).collect(),
            });
        }
        self.stop_requested = live.stop_requested;
        self.slow = live.slow;
        self.heard = (live.heard).map(|heard| usize::try_from(heard).unwrap_or(usize::MAX));
        self.woke_by_task = live.woke_by_task;
        self.woken_stopping = live.woken_stopping;
        self.wake_held = (live.wake_held).map(|left| now + Duration::from_millis(left));
        self.own_tools = live.own_tools.into();
        self.diffs = (live.diffs.into_iter())
            .map(|(tool, diffs)| {
                let diffs = (diffs.into_iter())
                    .map(|diff| RawDiff {
                        path: diff.path,
                        old: diff.old,
                        new: diff.new,
                    })
                    .collect();
                (tool, diffs)
            })
            .collect();
        self.asking = (live.asking.into_iter())
            .filter_map(resume_question)
            .take(super::MAX_QUESTIONS)
            .collect();
        self.next_question = (live.next_question).max(
            (self.asking.iter())
                .map(|(_, question)| question.id + 1)
                .max()
                .unwrap_or(0),
        );
        self.show_question();
        self.user_queue.native = (live.native).map(|(id, turn, item)| {
            let index = |at: u64| usize::try_from(at).unwrap_or(usize::MAX);
            (id, index(turn), index(item))
        });
        self.user_queue.restart = live.restart;
        self.pause_queue(live.queue_paused);
        if self.answering() {
            self.set_state(ChatState::Answering);
        } else if matches!(self.link, Link::Opening | Link::Closing) && self.queued.is_some() {
            self.set_state(ChatState::Starting);
        }
    }

    /// The chat's agent was to go on through the upgrade, and couldn't be
    /// taken back: as if it had stopped, a turn being answered ends, cut
    /// off by the upgrade, and what it ran in the background is lost.
    ///
    /// It goes on as a chat whose agent the upgrade stopped does: from
    /// `live`, what it was doing is taken back, and a turn under way is
    /// handed back ended, to be continued as any other, with what goes on
    /// with it, if it can be (see [`HostedChat::prepare_upgrade`]).
    pub(crate) fn live_lost(&mut self, live: LiveChat) -> Option<handover::ChatContinuation> {
        self.resume_live(live);
        let continuation = self.prepare_upgrade();
        if self.answering() {
            self.end_turn(Outcome::Failed(super::upgrade::CUT_OFF.to_owned()));
        }
        self.link = Link::Closed;
        self.queued = None;
        // Nothing of its tools went on either: no call waits for an answer.
        self.keep_tool_questions(&[]);
        self.set_state(ChatState::Ready);
        continuation
    }

    /// Answers every form its agent asked the user to fill in as
    /// cancelled, as the agent goes on through an upgrade, which can't
    /// carry a form: what to send the agent. The agent may ask again.
    ///
    /// The user is told, and so is the agent, by a notice from clackatoo
    /// once its turn ends, so it doesn't take the cancel for the user's.
    pub(crate) fn cancel_forms_for_upgrade(&mut self) -> Vec<super::Action> {
        let mut actions = Vec::new();
        self.cancel_forms(&mut actions);
        if !actions.is_empty() {
            self.notify(
                "The daemon upgraded and cancelled the agent's form; it can ask again.".to_owned(),
            );
            let text = "[daemon upgraded] Clackatoo's daemon upgraded, which cancelled the form \
                you had asked the user to fill in: the user didn't cancel it. Ask again if you \
                still need the answer."
                .to_owned();
            (self.relations.inbox).push_back(super::Message::upgrade_notice(self.id, text));
        }
        actions
    }

    /// Takes back the questions clackatoo's tools asked whose calls no
    /// longer wait, all but those numbered `calls`: their connections
    /// didn't go on through the upgrade.
    pub(crate) fn keep_tool_questions(&mut self, calls: &[u64]) {
        let before = self.asking.len();
        (self.asking).retain(|(asker, _)| match asker {
            Asker::Tool(call) => calls.contains(call),
            _ => true,
        });
        if self.asking.len() != before {
            self.show_question();
        }
    }
}

/// `millis` as a whole number of milliseconds, as the handover keeps it.
fn millis(left: Duration) -> u64 {
    u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
}

/// The question `question`, `asker`'s, for the next build. A form the
/// agent asked the user to fill in can't be written down: it's answered
/// as cancelled before the agent is handed over (see
/// [`HostedChat::cancel_forms_for_upgrade`]), so none is left here.
fn hand_over_question(asker: Asker, question: Question) -> Option<handover::LiveQuestion> {
    let asker = match asker {
        Asker::Agent(request) => LiveAsker::Agent(request.to_string()),
        Asker::Tool(call) => LiveAsker::Tool(call),
        Asker::Install => LiveAsker::Install,
        Asker::Suggest => LiveAsker::Suggest,
        Asker::Slow => LiveAsker::Slow,
        Asker::Elicit { .. } => {
            log::warn!("a form the agent asked for was left to hand over, and goes");
            return None;
        }
    };
    Some(handover::LiveQuestion {
        asker,
        id: question.id,
        title: question.title,
        choices: (question.choices.into_iter())
            .map(|choice| {
                let kind = match choice.kind {
                    ChoiceKind::AllowOnce => LiveChoice::AllowOnce,
                    ChoiceKind::AllowAlways => LiveChoice::AllowAlways,
                    ChoiceKind::RejectOnce => LiveChoice::RejectOnce,
                    ChoiceKind::RejectAlways => LiveChoice::RejectAlways,
                    ChoiceKind::Other => LiveChoice::Other,
                };
                (choice.id, choice.name, kind)
            })
            .collect(),
        own: question.own,
        plan: question.plan,
    })
}

/// A question the last build handed over, and who asked it: `None` if
/// the agent's request ID isn't JSON, which no build writes.
fn resume_question(live: handover::LiveQuestion) -> Option<(Asker, Question)> {
    let asker = match live.asker {
        LiveAsker::Agent(request) => Asker::Agent(serde_json::from_str(&request).ok()?),
        LiveAsker::Tool(call) => Asker::Tool(call),
        LiveAsker::Install => Asker::Install,
        LiveAsker::Suggest => Asker::Suggest,
        LiveAsker::Slow => Asker::Slow,
    };
    let choices = (live.choices.into_iter())
        .map(|(id, name, kind)| Choice {
            id,
            name,
            kind: match kind {
                LiveChoice::AllowOnce => ChoiceKind::AllowOnce,
                LiveChoice::AllowAlways => ChoiceKind::AllowAlways,
                LiveChoice::RejectOnce => ChoiceKind::RejectOnce,
                LiveChoice::RejectAlways => ChoiceKind::RejectAlways,
                LiveChoice::Other => ChoiceKind::Other,
            },
        })
        .collect();
    let question = Question {
        id: live.id,
        title: live.title,
        choices,
        own: live.own,
        form: None,
        plan: live.plan,
        tool: None,
    };
    Some((asker, question))
}
