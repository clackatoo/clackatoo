//! Where a chat's agent session could be forked: the last message the
//! agent gave in each turn, by the ID the adapter gave it, and the session
//! it came in (CLACKA-198).
//!
//! Both adapters fork a session at a message the client names
//! (`session/fork` with `_meta.jetbrains.air.fork`), keeping the
//! conversation up to and with that message. To start a branch of the
//! conversation before turn `k`, the session is forked at the last
//! message of the turn before: of the agent's words or thinking, as
//! Claude Code's adapter stamps an ID on those only (never on tool calls),
//! and Codex's on every item. The user's own message has no ID a client
//! sees as it goes, so it can't be named.
//!
//! Points are noted as the agent answers and as a session is told again
//! (`session/load` replays carry the IDs too), never for what a subagent
//! of the agent's own says, and kept by the turn's uid, so they stay with
//! their turns.

use std::collections::BTreeMap;

use agent_client_protocol_schema::v1::MessageId;
use clackatoo_protocol::Outcome;

use super::{HostedChat, TurnUid};

/// The most bytes of a message ID kept: longer ones aren't noted.
const MOST_ID: usize = 256;

/// A message a session can be forked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ForkPoint {
    /// The session it came in.
    pub(crate) session: String,
    /// The adapter's ID for the message.
    pub(crate) message: String,
}

/// Each turn's fork point, if it has one, by the turn's uid.
#[derive(Debug, Default)]
pub(super) struct ForkPoints(BTreeMap<TurnUid, ForkPoint>);

impl ForkPoints {
    /// Forgets turn `uid`'s, as the turn is let go.
    pub(super) fn forget(&mut self, uid: TurnUid) {
        self.0.remove(&uid);
    }
}

impl HostedChat {
    /// Notes that the agent's own words or thinking, a piece of its
    /// message `id`, went on the turn being answered: the turn's fork
    /// point, unless a later message of the turn's is.
    pub(super) fn note_message(&mut self, id: Option<&MessageId>) {
        let Some(id) = id.map(|id| &*id.0) else {
            return;
        };
        if id.is_empty() || id.len() > MOST_ID || id.contains(char::is_control) {
            return;
        }
        // Codex keeps no mark of a turn stopped, so one told again ends as
        // if it had finished, though it may stop between a tool's call and
        // its result: a fork there could keep that dangling. Its turns told
        // again are forked before by a digest instead.
        if self.agent == "codex" && self.told_again() {
            return;
        }
        let (Some(turn), Some(session)) = (self.answering_turn(), self.session.as_deref()) else {
            return;
        };
        let Some(uid) = self.uids.of(turn) else {
            return;
        };
        let point = self.forks.0.entry(uid).or_insert_with(|| ForkPoint {
            session: String::new(),
            message: String::new(),
        });
        if point.message != id || point.session != session {
            point.message.clear();
            point.message.push_str(id);
            point.session.clear();
            point.session.push_str(session);
        }
    }

    /// Where to fork the chat's session for a branch of the conversation
    /// before turn `at`: the nearest turn before it whose last message
    /// came in the session the chat has now, and that ended as the agent
    /// meant it to, with its index. A point just before the agent compacts
    /// its context forks the whole conversation uncompacted, which costs
    /// more context than the branch left had; that's kept, as the turns
    /// are what was asked for. A turn stopped or failed may end
    /// between a tool's call and its result, which a fork there would keep
    /// dangling; a turn where the chat moved to another agent has no
    /// message of its own. `None` if there's no such turn, or no session.
    pub(crate) fn fork_point_before(&self, at: usize) -> Option<(usize, &ForkPoint)> {
        let session = self.session.as_deref()?;
        let turns = self.node.turns.get(..at)?;
        (turns.iter().enumerate().rev()).find_map(|(index, turn)| {
            let ended = turn.end.as_ref()?;
            if turn.switch.is_some() || ended.outcome != Outcome::Done {
                return None;
            }
            let point = self.forks.0.get(&self.uids.of(index)?)?;
            (point.session == session).then_some((index, point))
        })
    }

    /// Where its session can be forked, for the next build: nothing if
    /// nowhere is known.
    pub(crate) fn hand_over_forks(&self) -> Option<super::handover::ChatForks> {
        (!self.forks.0.is_empty()).then(|| super::handover::ChatForks {
            chat: self.id.0,
            points: (self.forks.0.iter())
                .map(|(uid, point)| (uid.0, point.session.clone(), point.message.clone()))
                .collect(),
        })
    }

    /// Takes where its session can be forked from the build before, if it
    /// kept any for this chat: only for turns it has, shown or not.
    pub(crate) fn resume_forks(&mut self, records: &mut Vec<super::handover::ChatForks>) {
        let Some(at) = records.iter().position(|record| record.chat == self.id.0) else {
            return;
        };
        let record = records.swap_remove(at);
        for (uid, session, message) in record.points {
            let fits = |text: &str| {
                !text.is_empty() && text.len() <= MOST_ID && !text.contains(char::is_control)
            };
            if fits(&session) && fits(&message) {
                (self.forks.0).insert(TurnUid(uid), ForkPoint { session, message });
            }
        }
    }

    /// The fork point of turn `index`, if it has one.
    #[cfg(test)]
    pub(crate) fn fork_point(&self, index: usize) -> Option<&ForkPoint> {
        self.forks.0.get(&self.uids.of(index)?)
    }
}

#[cfg(test)]
mod tests {
    use agent_client_protocol_schema::v1::{
        ContentBlock, ContentChunk, SessionUpdate, StopReason, TextContent,
    };

    use super::super::tests::{chat, connect, send};
    use super::*;

    fn said(text: &str, id: Option<&str>, thinking: bool) -> SessionUpdate {
        let mut chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(text)));
        chunk.message_id = id.map(MessageId::new);
        if thinking {
            SessionUpdate::AgentThoughtChunk(chunk)
        } else {
            SessionUpdate::AgentMessageChunk(chunk)
        }
    }

    fn point(session: &str, message: &str) -> ForkPoint {
        ForkPoint {
            session: session.to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn a_turns_fork_point_is_its_last_message_in_the_session_it_came_in() {
        let mut chat = chat();
        connect(&mut chat, "s1");
        send(&mut chat, "first");
        chat.update(said("hm", Some("think-1"), true));
        chat.update(said("one", Some("msg-1"), false));
        chat.update(said(" more", Some("msg-1"), false));
        chat.update(said("no id", None, false));
        chat.answered(Ok(StopReason::EndTurn));
        assert_eq!(chat.fork_point(0), Some(&point("s1", "msg-1")));
        // Nothing is noted with no turn answering.
        chat.update(said("late", Some("msg-late"), false));
        assert_eq!(chat.fork_point(0), Some(&point("s1", "msg-1")));
        send(&mut chat, "second");
        chat.update(said("two", Some("msg-2"), false));
        chat.update(said("bad", Some("a\nb"), false));
        chat.answered(Ok(StopReason::EndTurn));
        assert_eq!(chat.fork_point(1), Some(&point("s1", "msg-2")));
        // A branch before turn 2 forks at turn 1's last message; one
        // before turn 1, at nothing.
        assert_eq!(chat.fork_point_before(2), Some((1, &point("s1", "msg-2"))));
        assert_eq!(chat.fork_point_before(1), Some((0, &point("s1", "msg-1"))));
        assert_eq!(chat.fork_point_before(0), None);
    }

    #[test]
    fn a_stopped_turn_or_another_sessions_is_passed_over_for_an_earlier_one() {
        let mut chat = chat();
        connect(&mut chat, "s1");
        send(&mut chat, "first");
        chat.update(said("one", Some("msg-1"), false));
        chat.answered(Ok(StopReason::EndTurn));
        send(&mut chat, "second");
        chat.update(said("two", Some("msg-2"), false));
        chat.answered(Ok(StopReason::Cancelled));
        assert_eq!(chat.fork_point(1), Some(&point("s1", "msg-2")));
        assert_eq!(chat.fork_point_before(2), Some((0, &point("s1", "msg-1"))));
        // In another session, it isn't the session's to fork.
        chat.session = Some("s2".to_owned());
        assert_eq!(chat.fork_point_before(2), None);
    }

    #[test]
    fn fork_points_follow_their_turns_uids() {
        let mut chat = chat();
        connect(&mut chat, "s1");
        chat.skip_uids(9);
        send(&mut chat, "first");
        chat.update(said("one", Some("msg-1"), false));
        chat.answered(Ok(StopReason::EndTurn));
        let uid = chat.turn_uid(0).unwrap();
        assert_eq!(chat.forks.0.keys().copied().collect::<Vec<_>>(), [uid]);
        assert_eq!(chat.fork_point_before(1), Some((0, &point("s1", "msg-1"))));
    }
}
