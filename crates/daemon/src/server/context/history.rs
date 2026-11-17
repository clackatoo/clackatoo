//! Giving a conversation its attachments back when an agent tells it
//! again. A message that went with context reaches the agent as its words
//! and the snapshots quoted after them, and that's all the agent tells
//! back: so each such prompt is looked up in the store (on a worker), and
//! the turn is given its own words and its snapshots again.
//!
//! A chat is looked through when it's added and after its agent tells it
//! turns again, never on the loop's every turn.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clackatoo_protocol::{ChatState, TerminalId};

use super::{Daemon, quoted_snapshot, store};
use crate::chat::TurnUid;
use crate::work::{Asked, Outcome, Priority, Question};

/// The most lookups that wait on workers at once, and how far back in a
/// chat prompts are looked up.
const MOST_ASKED: usize = 8;
const MOST_TURNS: usize = 128;

/// How long a lookup is waited for. It's nobody's keystroke, so this is
/// long enough for workers busy with an index.
const WAIT: Duration = Duration::from_secs(10);

/// The chats to look through, and the lookups under way.
#[derive(Debug, Default)]
pub(super) struct History {
    /// Chats with turns not looked through since they were added or told
    /// again.
    unlooked: BTreeSet<TerminalId>,
    /// Each chat's prompts already looked up, by turn and length, so one
    /// the store doesn't know isn't asked about again.
    looked: BTreeMap<TerminalId, BTreeSet<(u32, usize)>>,
    asked: Asked<Lookup, Result<Option<store::Message>, store::Problem>>,
}

#[cfg(test)]
impl History {
    /// Whether no chat waits to be looked through and no lookup is out.
    pub(super) fn idle(&self) -> bool {
        self.unlooked.is_empty() && self.asked.len() == 0
    }

    /// Whether anything is remembered of the chat `chat`.
    pub(super) fn remembers(&self, chat: TerminalId) -> bool {
        self.unlooked.contains(&chat) || self.looked.contains_key(&chat)
    }
}

/// A prompt being looked up.
#[derive(Debug)]
struct Lookup {
    chat: TerminalId,
    uid: u32,
    index: usize,
    /// The prompt as the agent told it, which the turn must still have
    /// when the answer comes.
    original: Arc<str>,
}

impl Daemon {
    /// The chat `chat` was added, or its agent told it turns again: its
    /// prompts are looked up once it's ready.
    pub(in crate::server) fn context_history_changed(&mut self, chat: TerminalId) {
        self.contexts.history.unlooked.insert(chat);
    }

    /// The chat `chat` closed.
    pub(super) fn forget_context_history(&mut self, chat: TerminalId) {
        let history = &mut self.contexts.history;
        history.unlooked.remove(&chat);
        history.looked.remove(&chat);
        history
            .asked
            .withdraw(&self.work.scheduler, |lookup| lookup.chat == chat);
    }

    /// When the loop must next look, for a lookup's deadline.
    pub(super) fn next_context_history_due(&self) -> Option<Instant> {
        self.contexts.history.asked.next_due()
    }

    /// Gives turns the attachments their lookups found, and looks through
    /// the chats that are waiting to be.
    pub(super) fn take_context_history(&mut self) {
        let settled = self
            .contexts
            .history
            .asked
            .take(&self.work.scheduler, Instant::now());
        // A store that can't be read fails every lookup alike: said once.
        let mut unread = None;
        for (lookup, outcome) in settled {
            let found = match outcome {
                Outcome::Answered(Ok(found)) => found,
                Outcome::Answered(Err(problem)) => {
                    unread.get_or_insert(problem);
                    None
                }
                Outcome::TimedOut => {
                    // Asked again when the chat is next looked through.
                    self.forget_looked(&lookup);
                    None
                }
                Outcome::Lost => None,
            };
            if let Some(record) = found {
                self.restore_context(&lookup, record);
            }
        }
        if let Some(problem) = unread {
            log::warn!("chats' context couldn't be looked up: {problem}");
        }
        let waiting: Vec<TerminalId> = self.contexts.history.unlooked.iter().copied().collect();
        for chat in waiting {
            self.look_through(chat);
        }
    }

    /// `lookup` got no answer in time: its prompt counts as not looked up,
    /// and its chat is looked through again.
    fn forget_looked(&mut self, lookup: &Lookup) {
        if let Some(looked) = self.contexts.history.looked.get_mut(&lookup.chat) {
            looked.remove(&(lookup.uid, lookup.original.len()));
        }
        self.contexts.history.unlooked.insert(lookup.chat);
    }

    /// Gives the turn `lookup` was for the words and snapshots of
    /// `record`, if it still has the prompt that was looked up.
    fn restore_context(&mut self, lookup: &Lookup, mut record: store::Message) {
        for snapshot in &mut record.context {
            snapshot.grant = self.context_grant(lookup.chat, &snapshot.id);
        }
        if let Some(chat) = self.chat(lookup.chat) {
            chat.restore_context_record(
                lookup.index,
                TurnUid(lookup.uid),
                &lookup.original,
                record.text,
                record.context,
            );
        }
        self.collect_damage(lookup.chat);
    }

    /// Looks up the prompts of `chat` that quote a snapshot and have none,
    /// newest first, as many as may wait at once. The chat stays to be
    /// looked through until a pass finds nothing to ask: a pass waits for
    /// the chat to be ready, and for its own lookups to come back.
    fn look_through(&mut self, chat: TerminalId) {
        let history = &self.contexts.history;
        if history.asked.waiting().any(|lookup| lookup.chat == chat) {
            return;
        }
        let Some(own) = self.chat_ref(chat) else {
            self.forget_context_history(chat);
            return;
        };
        if own.node().state != ChatState::Ready {
            return;
        }
        let free = MOST_ASKED.saturating_sub(history.asked.len());
        if free == 0 {
            return;
        }
        let looked = history.looked.get(&chat);
        let mut lookups = Vec::new();
        for (index, turn) in own.node().turns.iter().enumerate().rev().take(MOST_TURNS) {
            if !turn.context.is_empty() || quoted_snapshot(&turn.prompt).is_none() {
                continue;
            }
            let Some(uid) = own.turn_uid(index) else {
                continue;
            };
            if looked.is_some_and(|looked| looked.contains(&(uid.0, turn.prompt.len()))) {
                continue;
            }
            lookups.push(Lookup {
                chat,
                uid: uid.0,
                index,
                original: Arc::from(turn.prompt.as_str()),
            });
            if lookups.len() == free {
                break;
            }
        }
        if lookups.is_empty() {
            self.contexts.history.unlooked.remove(&chat);
            return;
        }
        let root = own.project().to_owned();
        let path = self.context_store_path();
        for lookup in lookups {
            let key = (lookup.uid, lookup.original.len());
            let (path, root, prompt) = (path.clone(), root.clone(), Arc::clone(&lookup.original));
            let asked = self.contexts.history.asked.ask(
                &mut self.work.scheduler,
                Question {
                    name: "context-history",
                    scope: None,
                    wait: WAIT,
                    priority: Priority::Background,
                },
                lookup,
                move |_| store::lookup(&path, &root, &prompt),
            );
            if let Err(why) = asked {
                // The workers have no room. Rather than look through the
                // chat on every turn of the loop until they do, leave it
                // until it's next told again.
                log::debug!("chat {}'s context wasn't looked up: {why}", chat.0);
                self.contexts.history.unlooked.remove(&chat);
                return;
            }
            let looked = &mut self.contexts.history.looked;
            looked.entry(chat).or_default().insert(key);
        }
    }
}
