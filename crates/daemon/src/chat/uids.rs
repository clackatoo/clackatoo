//! Turn uids: a number for each turn of a chat that stays with that turn,
//! as its index on the conversation shown might not (CLACKA-198).
//!
//! Everything the daemon keeps about one turn, to find it again later,
//! keys it by its uid: what it changed in files (the edit log, the hunks
//! its edits made, a review of it), the activities it started, the
//! message it began with, kept to send again, and so on. Clients, the
//! agent's tools and the wire still name turns by their index on the
//! conversation shown; the daemon maps between the two where they meet it.
//!
//! A new turn takes the chat's next uid, so uids only grow along a
//! conversation, and are never used twice in a chat. A chat whose
//! conversation has never been cut back has `uid == index` for every
//! turn, so what's handed over keyed by turn means the same either way
//! for it, and only a chat that has been cut back hands over its uids.
//!
//! The daemon's own unit tests number a chat's turns from 1000, not 0, and
//! so do its integration tests (through [`number_turns_from`]), so
//! anything keyed by a turn's index where it should be keyed by its uid
//! fails them, without a conversation being cut back. Such a daemon hands
//! over every chat's uids, as none is its index.
//!
//! What counts turns rather than names one — how many the agent's session
//! has seen (`known`), or how many another chat has heard the answers to —
//! stays a count on the conversation shown.

use crate::handover;

/// A turn of a chat, by a number that stays with it (see the module).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TurnUid(pub(crate) u32);

impl TurnUid {
    /// The uid of turn `index` of a chat that has never been cut back, as
    /// a record from before uids, or from a test, names it.
    pub(crate) fn of_index(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

impl std::fmt::Display for TurnUid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// The uid a chat's first turn gets, if a test said (see
/// [`number_turns_from`]).
static FIRST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The uid a chat's first turn gets: 0, as its index, except in tests
/// (see the module).
fn first() -> u32 {
    if cfg!(test) {
        return 1000;
    }
    FIRST.load(std::sync::atomic::Ordering::Relaxed)
}

/// Numbers every chat's first turn `from` from now on, for tests: so a
/// turn's uid is never its index.
pub(crate) fn number_turns_from(from: u32) {
    FIRST.store(from, std::sync::atomic::Ordering::Relaxed);
}

/// The uids of a chat's turns on the conversation shown, in step with its
/// turns, and the uid the next turn gets.
#[derive(Debug)]
pub(crate) struct Uids {
    /// One per turn shown, strictly growing.
    shown: Vec<TurnUid>,
    next: u32,
}

impl Default for Uids {
    fn default() -> Self {
        Self {
            shown: Vec::new(),
            next: first(),
        }
    }
}

impl Uids {
    /// A new turn's uid, at the end of the conversation shown.
    pub(super) fn push(&mut self) -> TurnUid {
        let uid = TurnUid(self.next);
        self.next = self.next.saturating_add(1);
        self.shown.push(uid);
        uid
    }

    /// Puts back `uid`, a turn's kept off the conversation shown, at its
    /// end, as a branch is shown again.
    pub(super) fn push_kept(&mut self, uid: TurnUid) {
        debug_assert!(self.shown.last().is_none_or(|last| *last < uid));
        debug_assert!(uid.0 < self.next);
        self.shown.push(uid);
    }

    /// Cuts the conversation shown back to its first `len` turns: the uids
    /// of those after are never given again.
    pub(super) fn truncate(&mut self, len: usize) {
        self.shown.truncate(len);
    }

    /// The uid of turn `index` of the conversation shown, if it has one.
    pub(crate) fn of(&self, index: usize) -> Option<TurnUid> {
        self.shown.get(index).copied()
    }

    /// Where turn `uid` is on the conversation shown, if it's there.
    pub(crate) fn index(&self, uid: TurnUid) -> Option<usize> {
        // Uids only grow along the conversation, and a chat never cut
        // back has each where its number says: try that first.
        let at = usize::try_from(uid.0).ok()?;
        if self.shown.get(at) == Some(&uid) {
            return Some(at);
        }
        self.shown.binary_search(&uid).ok()
    }

    /// The uids of turns `range` of the conversation shown, as far as it
    /// has them.
    pub(crate) fn within(&self, range: std::ops::Range<usize>) -> &[TurnUid] {
        let end = range.end.min(self.shown.len());
        self.shown
            .get(range.start.min(end)..end)
            .unwrap_or_default()
    }

    /// The uid of the last turn shown.
    pub(crate) fn last(&self) -> Option<TurnUid> {
        self.shown.last().copied()
    }

    /// The uid the next turn gets.
    pub(crate) fn next(&self) -> TurnUid {
        TurnUid(self.next)
    }

    /// Whether every turn's uid is its index, and the next turn's is the
    /// count: so for a chat never cut back.
    fn plain(&self) -> bool {
        usize::try_from(self.next).ok() == Some(self.shown.len())
            && (self.shown.iter().enumerate()).all(|(at, uid)| TurnUid::of_index(at) == *uid)
    }

    /// What's handed over of them for chat `chat`: nothing for a chat
    /// never cut back, whose uids the next build works out again.
    pub(super) fn hand_over(&self, chat: u32) -> Option<handover::ChatUids> {
        (!self.plain()).then(|| handover::ChatUids {
            chat,
            next: self.next,
            uids: self.shown.iter().map(|uid| uid.0).collect(),
        })
    }

    /// The uids of a chat with `turns` turns, as the last build handed
    /// them over in `record`, or else as for a chat never cut back. A
    /// record that doesn't fit the turns isn't believed.
    pub(super) fn resume(turns: usize, record: Option<&handover::ChatUids>) -> Self {
        Self::resume_checked(turns, record).0
    }

    /// As [`resume`](Self::resume), and whether a record given fitted.
    fn resume_checked(turns: usize, record: Option<&handover::ChatUids>) -> (Self, bool) {
        let plain = || Self::numbered_from(first(), turns);
        let Some(record) = record else {
            return (plain(), true);
        };
        let fits = record.uids.len() == turns
            && record.uids.windows(2).all(|pair| pair[0] < pair[1])
            && record.uids.last().is_none_or(|&last| last < record.next);
        if !fits {
            log::warn!(
                "chat {}'s turn numbers didn't fit its turns: numbered again",
                record.chat
            );
            return (plain(), false);
        }
        let uids = Self {
            shown: record.uids.iter().copied().map(TurnUid).collect(),
            next: record.next,
        };
        (uids, true)
    }

    /// `turns` turns numbered from `from`.
    fn numbered_from(from: u32, turns: usize) -> Self {
        let count = u32::try_from(turns).unwrap_or(u32::MAX);
        Self {
            shown: (0..count)
                .map(|at| TurnUid(from.saturating_add(at)))
                .collect(),
            next: from.saturating_add(count),
        }
    }

    /// The uid turn `index` of the conversation shown has, or will have if
    /// it comes next, as turns told again after a cold start do: so what a
    /// saved file names by index can be kept by uid before they're told.
    pub(crate) fn of_or_to_come(&self, index: usize) -> Option<TurnUid> {
        if let Some(uid) = self.of(index) {
            return Some(uid);
        }
        let ahead = u32::try_from(index.checked_sub(self.shown.len())?).ok()?;
        Some(TurnUid(self.next.checked_add(ahead)?))
    }

    /// Leaves out `skip` uids before the next turn's, so turns from here
    /// on have uids other than their indexes, as after a cut: for tests
    /// that check what's kept by turn follows the uid.
    #[cfg(test)]
    pub(crate) fn skip(&mut self, skip: u32) {
        self.next += skip;
    }
}

impl super::HostedChat {
    /// The uid of turn `index` of the conversation shown, if there is one.
    pub(crate) fn turn_uid(&self, index: usize) -> Option<TurnUid> {
        self.uids.of(index)
    }

    /// Where turn `uid` is on the conversation shown, if it's there.
    pub(crate) fn turn_index(&self, uid: TurnUid) -> Option<usize> {
        self.uids.index(uid)
    }

    /// The uids of turns `range` of the conversation shown, as far as it
    /// has them.
    pub(crate) fn turn_uids(&self, range: std::ops::Range<usize>) -> &[TurnUid] {
        self.uids.within(range)
    }

    /// The uid of the last turn, which the agent works in.
    pub(crate) fn last_uid(&self) -> Option<TurnUid> {
        self.uids.last()
    }

    /// The uid turn `index` has, or will have if it comes next, so many
    /// turns after the last (see [`Uids::of_or_to_come`]).
    pub(crate) fn turn_uid_to_come(&self, index: usize) -> Option<TurnUid> {
        self.uids.of_or_to_come(index)
    }

    /// Where turn `uid` is on the conversation shown, or will be if it's
    /// one to come, the inverse of [`turn_uid_to_come`](Self::turn_uid_to_come):
    /// `None` for a turn on another branch, or let go.
    pub(crate) fn turn_index_to_come(&self, uid: TurnUid) -> Option<usize> {
        if let Some(index) = self.uids.index(uid) {
            return Some(index);
        }
        let ahead = uid.0.checked_sub(self.uids.next().0)?;
        Some(self.node.turns.len() + usize::try_from(ahead).ok()?)
    }

    /// The uid the next turn gets.
    pub(crate) fn next_uid(&self) -> TurnUid {
        self.uids.next()
    }

    /// What's handed over of its turns' uids: nothing if each is its
    /// index.
    pub(crate) fn hand_over_uids(&self) -> Option<handover::ChatUids> {
        self.uids.hand_over(self.id.0)
    }

    /// Takes its turns' uids from what the build before handed over, if
    /// it kept any for this chat. Before anything else handed over keyed
    /// by turn is taken in. Returns false if what was handed over didn't
    /// fit, and the turns were numbered again: records kept by the old
    /// uids are to be forgotten.
    pub(crate) fn resume_uids(&mut self, records: &mut Vec<handover::ChatUids>) -> bool {
        let Some(at) = records.iter().position(|record| record.chat == self.id.0) else {
            return true;
        };
        let record = records.swap_remove(at);
        let (uids, fitted) = Uids::resume_checked(self.node.turns.len(), Some(&record));
        self.uids = uids;
        fitted
    }

    /// Leaves out `skip` uids before the next turn's (see [`Uids::skip`]).
    #[cfg(test)]
    pub(crate) fn skip_uids(&mut self, skip: u32) {
        self.uids.skip(skip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_never_cut_back_numbers_each_turn_by_its_index_and_hands_nothing_over() {
        let mut uids = Uids::numbered_from(0, 0);
        for at in 0..3 {
            assert_eq!(uids.push(), TurnUid::of_index(at));
        }
        assert_eq!(uids.index(TurnUid(1)), Some(1));
        assert_eq!(uids.of(2), Some(TurnUid(2)));
        assert_eq!(uids.of(3), None);
        assert_eq!(uids.within(1..9), &[TurnUid(1), TurnUid(2)]);
        assert!(uids.hand_over(4).is_none());
        let back = Uids::numbered_from(0, 3);
        assert_eq!((back.shown, back.next), (uids.shown, uids.next));
    }

    #[test]
    fn uids_other_than_indexes_are_found_and_handed_over() {
        let mut uids = Uids::numbered_from(0, 0);
        uids.push();
        uids.skip(5);
        assert_eq!(uids.push(), TurnUid(6));
        assert_eq!(uids.push(), TurnUid(7));
        assert_eq!(uids.index(TurnUid(7)), Some(2));
        assert_eq!(uids.index(TurnUid(2)), None);
        assert_eq!(uids.index(TurnUid(1)), None);
        let record = uids.hand_over(4).unwrap();
        assert_eq!((record.next, &record.uids[..]), (8, &[0, 6, 7][..]));
        let back = Uids::resume(3, Some(&record));
        assert_eq!(back.shown, uids.shown);
        assert_eq!(back.next, 8);
        // A record that doesn't fit the turns handed over isn't believed.
        let (back, fitted) = Uids::resume_checked(2, Some(&record));
        assert!(!fitted);
        assert_eq!(back.shown, vec![TurnUid(first()), TurnUid(first() + 1)]);
        assert_eq!(Uids::default().push(), TurnUid(first()));
        assert_eq!(Uids::resume(1, None).of(0), Some(TurnUid(first())));
        let bad = handover::ChatUids {
            chat: 4,
            next: 3,
            uids: vec![0, 6, 7],
        };
        assert_eq!(Uids::resume(3, Some(&bad)).next, first() + 3);
    }
}
