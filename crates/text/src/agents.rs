//! Agent hunks: the runs of lines agents have changed in a text, which
//! agent changed each, in which turn of its chat, and whether the user has
//! reviewed the change yet.
//!
//! They're made as an agent's edits land, not found by comparing texts:
//! each edit's changes are widened to whole lines and compared line by
//! line just there, and what they make is put together with any hunk they
//! overlap, so a hunk's base is what its lines were before any agent
//! changed them. Every other edit, the user's or one undoing an agent's,
//! only moves them, as diagnostics are moved; a hunk whose lines are its
//! base again, as undoing the change makes them, goes.
//!
//! The daemon keeps them, and sends them to every window showing the
//! text, which moves them along with each edit, as the daemon does, until
//! it sends new ones.

use std::ops::Range;

use ropey::Rope;

use crate::edit::{Bias, Edit};
use crate::hunks::{diff, line_starts};

/// One run of lines an agent changed: bytes `start` to `end` of the text,
/// whole lines when made, were `base` before any agent changed them.
/// Lines an agent deleted have no text, and are at the start of the line
/// after them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AgentHunk {
    /// Its number, which no other hunk of the daemon's ever has.
    pub id: u64,
    /// The first byte of its first line.
    pub start: usize,
    /// The byte after its last line's end, or `start` for lines deleted.
    pub end: usize,
    /// What its lines were before the agent changed them.
    pub base: String,
    /// The chat whose agent changed them last, by its terminal's number.
    pub chat: u32,
    /// The turn of the chat it was in, counting from 0.
    pub turn: u32,
    /// Whether the user has kept the change, having reviewed it. A kept
    /// hunk isn't coloured, but still says who made its lines.
    pub reviewed: bool,
}

impl AgentHunk {
    /// The lines of `text` it covers, which are none for lines deleted.
    #[must_use]
    pub fn lines(&self, text: &Rope) -> Range<usize> {
        let len = text.len_bytes();
        let first = text.byte_to_line(self.start.min(len));
        if self.end <= self.start {
            return first..first;
        }
        first..text.byte_to_line((self.end - 1).min(len)) + 1
    }

    /// The line it's marked on in `text`: its first, or for lines
    /// deleted, the line above where they were, or the first line if they
    /// were at the top.
    #[must_use]
    pub fn line(&self, text: &Rope) -> usize {
        let line = text.byte_to_line(self.start.min(text.len_bytes()));
        if self.start == self.end {
            line.saturating_sub(1)
        } else {
            line
        }
    }

    /// The bytes of `text`'s lines it covers, whole, though an edit since
    /// it was made has moved its ends into a line.
    #[must_use]
    pub fn whole_lines(&self, text: &Rope) -> Range<usize> {
        let lines = self.lines(text);
        text.line_to_byte(lines.start)..text.line_to_byte(lines.end)
    }

    /// Whether lines were deleted here, and nothing put in their place.
    #[must_use]
    pub fn deleted(&self) -> bool {
        self.start == self.end
    }
}

/// Who made an agent's edit: the chat's terminal number, and the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Maker {
    /// The chat, by its terminal's number.
    pub chat: u32,
    /// The turn, counting from 0.
    pub turn: u32,
}

/// What putting an agent's edit in did: the hunks it made, and those it
/// took into them, which are gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Added {
    /// The new hunks' numbers.
    pub made: Vec<u64>,
    /// The numbers of the hunks gone.
    pub gone: Vec<u64>,
}

/// A text's agent hunks, in order. They never overlap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentHunks {
    list: Vec<AgentHunk>,
}

/// A run of lines that differ, in the text before an edit and after it.
#[derive(Clone, Debug)]
struct Changed {
    old: Range<usize>,
    new: Range<usize>,
}

impl AgentHunks {
    /// No hunks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// These hunks, put in order.
    #[must_use]
    pub fn of(mut list: Vec<AgentHunk>) -> Self {
        list.sort_by_key(|hunk| hunk.start);
        Self { list }
    }

    /// Every one, in order.
    #[must_use]
    pub fn all(&self) -> &[AgentHunk] {
        &self.list
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Whether any is yet to be reviewed.
    #[must_use]
    pub fn unreviewed(&self) -> bool {
        self.list.iter().any(|hunk| !hunk.reviewed)
    }

    /// The hunk numbered `id`.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&AgentHunk> {
        self.list.iter().find(|hunk| hunk.id == id)
    }

    /// Takes away the hunk numbered `id`, as one rejected.
    pub fn remove(&mut self, id: u64) -> Option<AgentHunk> {
        let at = self.list.iter().position(|hunk| hunk.id == id)?;
        Some(self.list.remove(at))
    }

    /// Marks the hunk numbered `id` reviewed, as one kept. Returns whether
    /// there's such a hunk.
    pub fn keep(&mut self, id: u64) -> bool {
        match self.list.iter_mut().find(|hunk| hunk.id == id) {
            Some(hunk) => {
                hunk.reviewed = true;
                true
            }
            None => false,
        }
    }

    /// Lets go of every hunk `keep` says no to, returning their numbers.
    pub fn retain(&mut self, mut keep: impl FnMut(&AgentHunk) -> bool) -> Vec<u64> {
        let mut gone = Vec::new();
        self.list.retain(|hunk| {
            let kept = keep(hunk);
            if !kept {
                gone.push(hunk.id);
            }
            kept
        });
        gone
    }

    /// The hunk at `line` of `text`, the text they're for: the one
    /// covering the line, or lines deleted just after it, or for the first
    /// line, just before it.
    #[must_use]
    pub fn at_line(&self, text: &Rope, line: usize) -> Option<&AgentHunk> {
        if line >= text.len_lines() {
            return None;
        }
        let start = text.line_to_byte(line);
        let next = text.line_to_byte(line + 1);
        // They don't overlap, so only the last to start before the next
        // line can cover this one.
        let before = self.list.partition_point(|hunk| hunk.start < next);
        if let Some(index) = before.checked_sub(1)
            && self.list[index].end > start
        {
            return Some(&self.list[index]);
        }
        if next > start
            && let Some(hunk) = self.list.get(before)
            && hunk.start == next
            && hunk.deleted()
        {
            return Some(hunk);
        }
        match self.list.first() {
            Some(hunk) if line == 0 && hunk.start == 0 && hunk.deleted() => Some(hunk),
            _ => None,
        }
    }

    /// Moves them along with `edit`, made to the text they're for: an edit
    /// that isn't an agent's own change, such as the user's, or one
    /// undoing an agent's. Text put in at either end of one isn't in it.
    /// With `text`, the text the edit made, a hunk the edit touches whose
    /// lines are its base again goes; without, as when the text it made
    /// isn't at hand, none does. Returns the numbers of those gone.
    pub fn map(&mut self, edit: &Edit, text: Option<&Rope>) -> Vec<u64> {
        if self.list.is_empty() || edit.is_noop() {
            return Vec::new();
        }
        let changes: Vec<Range<usize>> = (edit.changes())
            .map(|(at, removed, _)| at..at + removed.len())
            .collect();
        let mut gone = Vec::new();
        self.list.retain_mut(|hunk| {
            let (start, end) = (hunk.start, hunk.end);
            let touched = (changes.iter()).any(|change| change.start <= end && start <= change.end);
            if start == end {
                // Lines put back where they were deleted are in it, to be
                // seen whether they're its base again.
                hunk.start = edit.map(start, Bias::Before);
                hunk.end = edit.map(end, Bias::After);
            } else {
                hunk.start = edit.map(start, Bias::After);
                hunk.end = edit.map(end, Bias::Before).max(hunk.start);
            }
            if touched && text.is_some_and(|text| back_to_base(hunk, text)) {
                gone.push(hunk.id);
                return false;
            }
            if start == end {
                hunk.end = hunk.start;
            }
            true
        });
        gone
    }

    /// Puts in an agent's edit, `edit`, which made `new` of `old`, by
    /// `maker`: the lines it changed become hunks, numbered from
    /// `next_id`, taking in those they overlap. Lines it leaves as they
    /// were before any agent changed them aren't hunks, so an agent
    /// undoing its own change takes its hunk away.
    ///
    /// `fits` says whether a hunk's lines in `old` are still what the
    /// agent made them. One that doesn't, as someone has typed in it, is
    /// taken in as it is, as if kept: its base is lost, so that rejecting
    /// the hunk made can never take away what was typed.
    pub fn add(
        &mut self,
        edit: &Edit,
        (old, new): (&str, &str),
        (maker, fits): (Maker, &dyn Fn(&AgentHunk) -> bool),
        next_id: &mut u64,
    ) -> Added {
        let changed = changed_lines(edit, old, new);
        let mut added = Added::default();
        if changed.is_empty() {
            return added;
        }
        let shift = |place: usize, after: bool| shift(&changed, place, after);
        let old_list = std::mem::take(&mut self.list);
        let mut list = Vec::with_capacity(old_list.len() + changed.len());
        // Where the last run of lines ended, after the edit: lines put in
        // there went into it, whether or not it's still a hunk.
        let mut floor = 0;
        let mut hunks = old_list.into_iter().peekable();
        let mut pending = changed.iter().peekable();
        loop {
            // The next cluster: whatever starts first, and everything that
            // overlaps what's gathered so far.
            let first = match (hunks.peek(), pending.peek()) {
                (None, None) => break,
                (Some(hunk), Some(change)) if change.old.start < hunk.start => change.old.clone(),
                (Some(hunk), _) => hunk.start..hunk.end,
                (None, Some(change)) => change.old.clone(),
            };
            let (mut from, mut to) = (first.start, first.end);
            let mut gathered = Vec::new();
            let mut any_change = false;
            loop {
                if let Some(hunk) = hunks.next_if(|hunk| overlaps(hunk.start..hunk.end, from..to)) {
                    from = from.min(hunk.start);
                    to = to.max(hunk.end);
                    gathered.push(hunk);
                } else if let Some(change) =
                    pending.next_if(|change| overlaps(change.old.clone(), from..to))
                {
                    from = from.min(change.old.start);
                    to = to.max(change.old.end);
                    any_change = true;
                } else {
                    break;
                }
            }
            if !any_change {
                // Untouched: moved with the changes before it.
                for mut hunk in gathered {
                    hunk.start = shift(hunk.start, false).max(floor);
                    hunk.end = shift(hunk.end, false).max(hunk.start);
                    floor = hunk.end;
                    list.push(hunk);
                }
                continue;
            }
            let base = base_of(&gathered, old, from..to, fits);
            added.gone.extend(gathered.iter().map(|hunk| hunk.id));
            let (start, end) = (shift(from, false).max(floor), shift(to, true));
            let end = end.max(start);
            floor = end;
            if new.get(start..end) == Some(base.as_str()) {
                continue;
            }
            let id = *next_id;
            *next_id += 1;
            added.made.push(id);
            list.push(AgentHunk {
                id,
                start,
                end,
                base,
                chat: maker.chat,
                turn: maker.turn,
                reviewed: false,
            });
        }
        self.list = list;
        added
    }
}

/// Where `place`, in the text before the edit that made `changed`, is
/// after it: past lines put in just there if `after`, else before them.
fn shift(changed: &[Changed], place: usize, after: bool) -> usize {
    let mut moved = place;
    for change in changed {
        let before = change.old.end < place
            || (change.old.end == place && (after || !change.old.is_empty()));
        if before {
            moved = change.new.end + (place - change.old.end);
        } else if change.old.start < place {
            // Inside a change, which the places asked about never are: to
            // an end of it.
            return if after {
                change.new.end
            } else {
                change.new.start
            };
        } else {
            break;
        }
    }
    moved
}

/// What bytes `run` of `old` were before any agent changed them, where
/// `hunks` are the hunks in it: their bases where they are, but for kept
/// hunks, whose lines the user took as they were, and those that no
/// longer fit, as `fits` says, whose lines someone changed since, and
/// `old` elsewhere.
fn base_of(
    hunks: &[AgentHunk],
    old: &str,
    run: Range<usize>,
    fits: &dyn Fn(&AgentHunk) -> bool,
) -> String {
    let mut base = String::new();
    let mut at = run.start;
    for hunk in hunks {
        base.push_str(&old[at..hunk.start.max(at)]);
        if hunk.reviewed || !fits(hunk) {
            base.push_str(&old[hunk.start.max(at)..hunk.end.max(at)]);
        } else {
            base.push_str(&hunk.base);
        }
        at = at.max(hunk.end);
    }
    base.push_str(&old[at..run.end.max(at)]);
    base
}

/// Whether `a` and `b` overlap, as runs of lines: they share a byte, or
/// one is lines deleted at a place in the other, or at either end of it.
fn overlaps(a: Range<usize>, b: Range<usize>) -> bool {
    if a.is_empty() || b.is_empty() {
        a.start <= b.end && b.start <= a.end
    } else {
        a.start < b.end && b.start < a.end
    }
}

/// Whether `hunk`'s lines in `text` are its base again.
fn back_to_base(hunk: &AgentHunk, text: &Rope) -> bool {
    if hunk.deleted() {
        return hunk.base.is_empty();
    }
    let lines = hunk.whole_lines(text);
    lines.len() == hunk.base.len() && text.byte_slice(lines) == hunk.base.as_str()
}

/// The runs of whole lines `edit`, which made `new` of `old`, changed, in
/// both texts, in order: each change widened to the lines it's on, those
/// that then meet put together, and compared line by line.
fn changed_lines(edit: &Edit, old: &str, new: &str) -> Vec<Changed> {
    // Each window of lines changed, in the text before and after.
    let mut windows: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut moved: isize = 0;
    for (at, removed, inserted) in edit.changes() {
        let new_at = at.saturating_add_signed(moved);
        let old_window = line_start(old, at)..line_end(old, at + removed.len());
        let new_window = line_start(new, new_at)..line_end(new, new_at + inserted.len());
        moved += isize::try_from(inserted.len()).unwrap_or(isize::MAX)
            - isize::try_from(removed.len()).unwrap_or(isize::MAX);
        match windows.last_mut() {
            Some((last_old, last_new)) if old_window.start < last_old.end => {
                last_old.end = last_old.end.max(old_window.end);
                last_new.end = last_new.end.max(new_window.end);
            }
            _ => windows.push((old_window, new_window)),
        }
    }
    let mut changed = Vec::new();
    for (old_window, new_window) in windows {
        let (before, after) = (&old[old_window.clone()], &new[new_window.clone()]);
        let (old_lines, new_lines) = (line_starts(before), line_starts(after));
        for difference in diff(before, after) {
            changed.push(Changed {
                old: old_window.start + old_lines[difference.before.start]
                    ..old_window.start + old_lines[difference.before.end],
                new: new_window.start + new_lines[difference.after.start]
                    ..new_window.start + new_lines[difference.after.end],
            });
        }
    }
    changed
}

/// Where the line `at` is on starts, in `text`.
fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |end| end + 1)
}

/// Where the line after the one `at` is on starts, in `text`, or its end.
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map_or(text.len(), |end| at + end + 1)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    const AGENT: Maker = Maker { chat: 7, turn: 0 };

    fn spans(hunks: &AgentHunks) -> Vec<(usize, usize, &str)> {
        (hunks.all().iter())
            .map(|hunk| (hunk.start, hunk.end, hunk.base.as_str()))
            .collect()
    }

    /// `hunks`, with an agent's change from `old` to `new` put in.
    fn add(hunks: &mut AgentHunks, old: &str, new: &str, maker: Maker, next: &mut u64) -> Added {
        let edit = Edit::between(old, new);
        hunks.add(&edit, (old, new), (maker, &|_| true), next)
    }

    /// `text` with every hunk not reviewed put back as its base.
    fn rejected(hunks: &AgentHunks, text: &str) -> String {
        let mut out = String::new();
        let mut at = 0;
        for hunk in hunks.all() {
            out.push_str(&text[at..hunk.start]);
            if hunk.reviewed {
                out.push_str(&text[hunk.start..hunk.end]);
            } else {
                out.push_str(&hunk.base);
            }
            at = hunk.end;
        }
        out.push_str(&text[at..]);
        out
    }

    #[test]
    fn an_agents_changes_become_hunks_of_whole_lines() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let old = "a\nb\nc\nd\ne\n";
        // b changed in a word, a line put in after c, e deleted.
        let new = "a\nbee\nc\nnew\nd\n";
        let added = add(&mut hunks, old, new, AGENT, &mut next);
        assert_eq!(added.made, [1, 2, 3]);
        assert_eq!(spans(&hunks), [(2, 6, "b\n"), (8, 12, ""), (14, 14, "e\n")]);
        assert_eq!(rejected(&hunks, new), old);
        let rope = Rope::from_str(new);
        assert_eq!(hunks.at_line(&rope, 1).map(|hunk| hunk.id), Some(1));
        assert_eq!(hunks.at_line(&rope, 3).map(|hunk| hunk.id), Some(2));
        // Lines deleted at the end are marked on the line above.
        assert_eq!(hunks.at_line(&rope, 4).map(|hunk| hunk.id), Some(3));
        assert_eq!(hunks.at_line(&rope, 0), None);
    }

    #[test]
    fn a_change_over_a_hunk_keeps_what_the_lines_were_first() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let old = "a\nb\nc\n";
        let one = "a\nB\nc\n";
        add(&mut hunks, old, one, AGENT, &mut next);
        // The next turn changes the same line and the one after.
        let later = Maker { chat: 7, turn: 1 };
        let two = "a\nBB\nCC\n";
        let added = add(&mut hunks, one, two, later, &mut next);
        assert_eq!(
            added,
            Added {
                made: vec![2, 3],
                gone: vec![1]
            }
        );
        // The line it changed again keeps its first base; the line after
        // it is a hunk of its own.
        assert_eq!(spans(&hunks), [(2, 5, "b\n"), (5, 8, "c\n")]);
        assert_eq!(hunks.all()[0].turn, 1);
        assert_eq!(rejected(&hunks, two), old);
        // And putting the lines back as they were takes the hunks away.
        let added = add(&mut hunks, two, old, later, &mut next);
        assert_eq!(
            added,
            Added {
                made: vec![],
                gone: vec![2, 3]
            }
        );
        assert!(hunks.is_empty());
    }

    #[test]
    fn a_change_over_a_kept_hunk_takes_its_lines_as_they_were_kept() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        add(&mut hunks, "a\nb\n", "a\nB\n", AGENT, &mut next);
        assert!(hunks.keep(1));
        assert!(!hunks.unreviewed());
        add(&mut hunks, "a\nB\n", "a\nBB\n", AGENT, &mut next);
        assert_eq!(spans(&hunks), [(2, 5, "B\n")]);
        assert!(hunks.unreviewed());
    }

    #[test]
    fn hunks_beside_a_change_are_moved_and_kept_apart() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let old = "a\nb\nc\nd\n";
        let one = "a\nb\nc\nD\n";
        add(&mut hunks, old, one, AGENT, &mut next);
        // A line put in at the top, and one changed just above the hunk.
        let two = "top\na\nb\nC\nD\n";
        let added = add(&mut hunks, one, two, Maker { chat: 9, turn: 2 }, &mut next);
        assert_eq!(added.gone, Vec::<u64>::new());
        assert_eq!(spans(&hunks), [(0, 4, ""), (8, 10, "c\n"), (10, 12, "d\n")]);
        let chats: Vec<u32> = hunks.all().iter().map(|hunk| hunk.chat).collect();
        assert_eq!(chats, [9, 9, 7]);
        assert_eq!(rejected(&hunks, two), old);
    }

    #[test]
    fn other_edits_move_hunks_and_clear_those_put_back() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        add(&mut hunks, old, new, AGENT, &mut next);
        // A line typed above it isn't in it, nor one typed below.
        let mut text = Rope::from_str(new);
        let edit = Edit::replace(text.len_bytes(), 2, "", "x\n");
        edit.apply(&mut text).unwrap();
        assert!(hunks.map(&edit, Some(&text)).is_empty());
        let edit = Edit::replace(text.len_bytes(), 6, "", "y\n");
        edit.apply(&mut text).unwrap();
        assert!(hunks.map(&edit, Some(&text)).is_empty());
        assert_eq!(text.to_string(), "a\nx\nB\ny\nc\n");
        assert_eq!(spans(&hunks), [(4, 6, "b\n")]);
        // Typing in it keeps it, the change still the agent's.
        let edit = Edit::replace(text.len_bytes(), 5, "", "!");
        edit.apply(&mut text).unwrap();
        assert!(hunks.map(&edit, Some(&text)).is_empty());
        assert_eq!(spans(&hunks), [(4, 7, "b\n")]);
        // Typing the old line back takes it away.
        let edit = Edit::replace(text.len_bytes(), 4, "B!", "b");
        edit.apply(&mut text).unwrap();
        assert_eq!(hunks.map(&edit, Some(&text)), [1]);
        assert!(hunks.is_empty());
    }

    #[test]
    fn lines_an_agent_deleted_are_put_back_by_undoing() {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let old = "a\nb\nc\n";
        let new = "a\nc\n";
        add(&mut hunks, old, new, AGENT, &mut next);
        assert_eq!(spans(&hunks), [(2, 2, "b\n")]);
        let mut text = Rope::from_str(new);
        let edit = Edit::between(new, old);
        edit.apply(&mut text).unwrap();
        assert_eq!(hunks.map(&edit, Some(&text)), [1]);
        assert!(hunks.is_empty());
        // Other lines typed there leave the lines deleted.
        add(&mut hunks, old, new, AGENT, &mut next);
        let mut text = Rope::from_str(new);
        let edit = Edit::replace(text.len_bytes(), 2, "", "x\n");
        edit.apply(&mut text).unwrap();
        assert!(hunks.map(&edit, Some(&text)).is_empty());
        assert_eq!(spans(&hunks), [(2, 2, "b\n")]);
    }

    #[test]
    fn a_hunk_is_found_by_any_of_its_lines_and_kept_or_taken() {
        let (mut hunks, mut next) = (AgentHunks::new(), 5);
        let new = "a\nx\ny\nc\n";
        add(&mut hunks, "a\nb\nc\n", new, AGENT, &mut next);
        let rope = Rope::from_str(new);
        for line in 1..=2 {
            let hunk = hunks.at_line(&rope, line).expect("a hunk");
            assert_eq!(hunk.lines(&rope), 1..3);
            assert_eq!(hunk.line(&rope), 1);
        }
        assert!(hunks.at_line(&rope, 99).is_none());
        assert!(!hunks.keep(4));
        assert!(hunks.get(5).is_some());
        assert_eq!(hunks.remove(5).map(|hunk| hunk.base), Some("b\n".into()));
        assert!(hunks.is_empty());
    }

    /// What reject-all makes of `text`: every hunk not reviewed put back
    /// as its base, or `None`, refused, if any no longer fits, as one
    /// whose lines someone typed in since, which the daemon's merge would
    /// refuse.
    fn reject_all(
        hunks: &AgentHunks,
        text: &str,
        made: &std::collections::HashMap<u64, String>,
    ) -> Option<String> {
        let fits = |hunk: &AgentHunk| {
            text.get(hunk.start..hunk.end) == made.get(&hunk.id).map(String::as_str)
        };
        hunks
            .all()
            .iter()
            .filter(|hunk| !hunk.reviewed)
            .all(fits)
            .then(|| rejected(hunks, text))
    }

    /// One step of the agent's or the user's.
    #[derive(Clone, Debug)]
    enum Step {
        /// The agent replaces the line the `seed`th points at with `text`,
        /// or deletes it if `text` is empty, or puts `text` in before it if
        /// it starts with `+`.
        Agent { seed: usize, text: String },
        /// The user types a mark of their own at the place the `seed`th
        /// points at.
        User { seed: usize },
    }

    fn steps() -> impl Strategy<Value = Vec<Step>> {
        let agent =
            (any::<usize>(), "\\+?[ab]{0,3}").prop_map(|(seed, text)| Step::Agent { seed, text });
        let user = any::<usize>().prop_map(|seed| Step::User { seed });
        proptest::collection::vec(prop_oneof![agent, user], 1..12)
    }

    /// Where the agent and the user end up from `first`, taking `steps`
    /// in turn, the daemon's way: the hunks, the text, what the agent made
    /// each hunk into, and the user's marks.
    #[allow(clippy::type_complexity)]
    fn play(
        first: String,
        steps: Vec<Step>,
    ) -> (
        AgentHunks,
        String,
        std::collections::HashMap<u64, String>,
        Vec<String>,
    ) {
        let (mut hunks, mut next) = (AgentHunks::new(), 1);
        let mut made: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
        let mut text = first;
        let mut marks = Vec::new();
        for step in steps {
            match step {
                Step::Agent { seed, text: put } => {
                    let starts = line_starts(&text);
                    let line = seed % starts.len().max(1);
                    let start = starts[line];
                    let end = starts.get(line + 1).copied().unwrap_or(text.len());
                    let new = match put.strip_prefix('+') {
                        Some(put) => format!("{}{put}\n{}", &text[..start], &text[start..]),
                        None if put.is_empty() => format!("{}{}", &text[..start], &text[end..]),
                        None => format!("{}{put}\n{}", &text[..start], &text[end..]),
                    };
                    let edit = Edit::between(&text, &new);
                    let old = text.clone();
                    let fits = |hunk: &AgentHunk| {
                        old.get(hunk.start..hunk.end) == made.get(&hunk.id).map(String::as_str)
                    };
                    let added = hunks.add(&edit, (&old, &new), (AGENT, &fits), &mut next);
                    for gone in &added.gone {
                        made.remove(gone);
                    }
                    for id in &added.made {
                        let hunk = hunks.get(*id).unwrap();
                        made.insert(*id, new[hunk.start..hunk.end].to_owned());
                    }
                    text = new;
                }
                Step::User { seed } => {
                    // A letter of its own, which no other typing can split.
                    let mark = char::from(b'A' + u8::try_from(marks.len()).unwrap()).to_string();
                    let at = seed % (text.len() + 1);
                    let edit = Edit::replace(text.len(), at, "", &mark);
                    let mut rope = Rope::from_str(&text);
                    edit.apply(&mut rope).unwrap();
                    for gone in hunks.map(&edit, Some(&rope)) {
                        made.remove(&gone);
                    }
                    text = rope.to_string();
                    marks.push(mark);
                }
            }
        }
        (hunks, text, made, marks)
    }

    fn texts() -> impl Strategy<Value = String> {
        proptest::string::string_regex("([ab]{0,3}\n){0,6}[ab]{0,2}").unwrap()
    }

    proptest! {
        #[test]
        fn rejecting_every_hunk_gives_the_text_before_any_agent(
            first in texts(),
            second in texts(),
            third in texts(),
        ) {
            let (mut hunks, mut next) = (AgentHunks::new(), 1);
            add(&mut hunks, &first, &second, AGENT, &mut next);
            prop_assert_eq!(rejected(&hunks, &second), first.clone());
            add(&mut hunks, &second, &third, Maker { chat: 7, turn: 1 }, &mut next);
            prop_assert_eq!(rejected(&hunks, &third), first);
            for pair in hunks.all().windows(2) {
                prop_assert!(pair[0].end <= pair[1].start);
            }
        }

        #[test]
        fn rejecting_never_takes_away_what_the_user_typed(
            first in "([ab]{0,3}\n){1,6}",
            steps in steps(),
        ) {
            let (hunks, text, made, marks) = play(first, steps);
            // What the user typed and has now is never taken away: a mark
            // the agent deleted itself comes back only while no one has
            // typed in the lines it deleted it in.
            if let Some(after) = reject_all(&hunks, &text, &made) {
                for mark in marks.iter().filter(|mark| text.contains(mark.as_str())) {
                    prop_assert!(after.contains(mark.as_str()), "{mark} lost: {after:?}");
                }
            }
        }

        #[test]
        fn hunks_stay_in_order_and_in_the_text_through_anyones_edits(
            first in texts(),
            second in texts(),
            typed in texts(),
            third in texts(),
        ) {
            let (mut hunks, mut next) = (AgentHunks::new(), 1);
            add(&mut hunks, &first, &second, AGENT, &mut next);
            let mut text = Rope::from_str(&second);
            let edit = Edit::between(&second, &typed);
            edit.apply(&mut text).unwrap();
            hunks.map(&edit, Some(&text));
            add(&mut hunks, &typed, &third, AGENT, &mut next);
            for hunk in hunks.all() {
                prop_assert!(hunk.start <= hunk.end && hunk.end <= third.len());
            }
            for pair in hunks.all().windows(2) {
                prop_assert!(pair[0].end <= pair[1].start);
            }
        }
    }
}
