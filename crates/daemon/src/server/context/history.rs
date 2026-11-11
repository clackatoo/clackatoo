//! Recover attachment metadata when a provider replays an older conversation.
use super::{Daemon, store};
use crate::chat::TurnUid;
use crate::work::{Priority, Step};
use clackatoo_protocol::{ChatState, TerminalId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub(super) struct History {
    pending: BTreeMap<(TerminalId, u32), (u64, Instant)>,
    checked: BTreeSet<(TerminalId, u32, usize)>,
    ready: Arc<Mutex<Vec<Ready>>>,
}
#[derive(Debug)]
struct Ready {
    chat: TerminalId,
    uid: u32,
    index: usize,
    original: String,
    record: Result<Option<store::Message>, String>,
}
impl History {
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.pending.values().map(|(_, due)| *due).min()
    }
}

impl Daemon {
    #[allow(
        clippy::too_many_lines,
        reason = "complete, expire and schedule one bounded replay recovery queue"
    )]
    pub(in crate::server) fn take_context_history(&mut self) {
        let ready = std::mem::take(
            &mut *self
                .contexts
                .history
                .ready
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for ready in ready {
            self.contexts
                .history
                .pending
                .remove(&(ready.chat, ready.uid));
            if let Ok(Some(mut record)) = ready.record {
                for snapshot in &mut record.context {
                    snapshot.grant = self.context_grant(ready.chat, &snapshot.id);
                }
                if let Some(chat) = self.chat(ready.chat) {
                    chat.restore_context_record(
                        ready.index,
                        TurnUid(ready.uid),
                        &ready.original,
                        record.text,
                        record.context,
                    );
                }
                self.collect_damage(ready.chat);
            }
        }
        let expired = self
            .contexts
            .history
            .pending
            .iter()
            .filter(|(_, (_, due))| *due <= Instant::now())
            .map(|(key, (work, _))| (*key, *work))
            .collect::<Vec<_>>();
        for (key, work) in expired {
            let _ = self.work.scheduler.cancel(work);
            self.contexts.history.pending.remove(&key);
        }
        if self.contexts.history.pending.len() >= 8 {
            return;
        }
        let mut targets = Vec::new();
        for (&id, slot) in &self.terminals {
            let Some(chat) = slot.terminal.chat_ref() else {
                continue;
            };
            if chat.node().state != ChatState::Ready {
                continue;
            }
            for (index, turn) in chat.node().turns.iter().enumerate().rev().take(128) {
                if !turn.context.is_empty() || !turn.prompt.contains("> Context ") {
                    continue;
                }
                let Some(uid) = chat.turn_uid(index) else {
                    continue;
                };
                if self.contexts.history.pending.contains_key(&(id, uid.0))
                    || self
                        .contexts
                        .history
                        .checked
                        .contains(&(id, uid.0, turn.prompt.len()))
                {
                    continue;
                }
                targets.push((
                    id,
                    uid.0,
                    index,
                    turn.prompt.clone(),
                    chat.project().to_owned(),
                ));
                if targets.len() + self.contexts.history.pending.len() >= 8 {
                    break;
                }
            }
            if targets.len() + self.contexts.history.pending.len() >= 8 {
                break;
            }
        }
        let path = self.context_store_path();
        for (chat, uid, index, original, root) in targets {
            let path = path.clone();
            let ready = Arc::clone(&self.contexts.history.ready);
            let length = original.len();
            let mut original = Some(original);
            let work = self.work.scheduler.submit(
                format!("context-history:{}:{uid}:{length}", chat.0),
                "Restoring context",
                None,
                Priority::Interactive,
                move |context| {
                    if context.cancelled() {
                        return Ok(Step::Done);
                    }
                    let original = original.take().expect("one context history slice");
                    let record = store::lookup(&path, &root, &original);
                    ready
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(Ready {
                            chat,
                            uid,
                            index,
                            original,
                            record,
                        });
                    Ok(Step::Done)
                },
            );
            if let Ok(work) = work {
                self.contexts
                    .history
                    .pending
                    .insert((chat, uid), (work, Instant::now() + Duration::from_secs(3)));
                self.contexts.history.checked.insert((chat, uid, length));
            }
        }
        if self.contexts.history.checked.len() > 8192 {
            self.contexts.history.checked.clear();
        }
    }
}
