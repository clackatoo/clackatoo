//! Inlay hints: what a language server would have written inline between
//! a file's text, as `: u32` after a name bound without a type, or
//! `count:` before an argument (`inlayhints`).
//!
//! A hint is never part of the text. The Vim engine's columns are the
//! line's own cells, as laid out, with no hint in them: so motions, text
//! objects, searches, selections and yanks see only the text, and the
//! cursor never lands on a hint. Hints are a view over those columns, as
//! soft wrap is (see [`wrap`](super::wrap)): a hint takes cells on screen
//! just before the character at its byte, or after the line's end, and
//! drawing, wrapping, the mouse and the view's scrolling count them.
//!
//! The window keeps a buffer's hints with its copy, as bytes, moved
//! through every edit as highlights are. Each editor asks for those of
//! the part of the file it shows, and a screenful either side, after a
//! moment's rest, once the text changes or the view leaves what it asked
//! about; an answer replaces the hints of its range alone, so two panes
//! showing two parts of one file each keep theirs. A hint an edit touches
//! goes, as it may be wrong now, until the answer to the question the edit
//! prompts brings it back.
//!
//! While the cursor types in insert mode, its own line shows none, so the
//! text being typed doesn't shift about.

use std::time::{Duration, Instant};

use clackatoo_protocol::{Edit, InlayHint, InlayHintKind, Query};
use clackatoo_term::{RowBuilder, Style};

use super::{BufferCopy, Editor};

/// How long an editor waits, once the text has changed or the view has
/// moved out of what it asked about, before asking for hints again: so
/// typing and scrolling ask once they pause, not on every key.
pub(crate) const HINTS_WAIT: Duration = Duration::from_millis(250);

/// The most hints a copy keeps, whatever parts of the file editors have
/// shown: past it, those furthest from the last answer's range go.
const MOST_KEPT: usize = 10_000;

/// An inlay hint as the window keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hint {
    /// The byte it's drawn before, in the copy's text.
    pub(crate) at: usize,
    pub(crate) label: Box<str>,
    pub(crate) kind: InlayHintKind,
    pub(crate) pad_left: bool,
    pub(crate) pad_right: bool,
    /// The cells it takes on screen, its padding's too.
    pub(crate) cells: usize,
}

impl Hint {
    /// `hint` as the window keeps it, measured, unless it takes no cells.
    fn of(hint: InlayHint) -> Option<Self> {
        let mut measure = RowBuilder::new(hint.label.chars().count() * 2);
        measure.push(&hint.label, &Style::DEFAULT, true);
        let label = measure.filled();
        (label > 0).then(|| Self {
            at: hint.at,
            cells: label + usize::from(hint.pad_left) + usize::from(hint.pad_right),
            label: hint.label.into(),
            kind: hint.kind,
            pad_left: hint.pad_left,
            pad_right: hint.pad_right,
        })
    }

    /// Draws it at the end of `builder`, as much as fits: its padding
    /// blank, its label muted, a parameter's name a little more so than a
    /// type.
    pub(crate) fn draw(&self, builder: &mut RowBuilder) {
        if self.pad_left {
            builder.push(" ", &Style::DEFAULT, true);
        }
        builder.push(&self.label, &crate::colors::inlay_hint(self.kind), true);
        if self.pad_right {
            builder.push(" ", &Style::DEFAULT, true);
        }
    }
}

/// A buffer's inlay hints, in the order of the text.
#[derive(Clone, Debug, Default)]
pub(crate) struct Hints {
    list: Vec<Hint>,
    /// A number that changes whenever any of them does, so what's worked
    /// out from them can be kept until it does (see
    /// [`Kept::rows`](super::kept::Kept::rows)).
    generation: u64,
    /// How many times some were let go, as too many were kept.
    trims: u64,
}

impl Hints {
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The number that changes whenever any of them does.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Every one, in order.
    pub(crate) fn all(&self) -> &[Hint] {
        &self.list
    }

    /// Those drawn between bytes `start` and `end` of the text, both
    /// counted: a line's, from its start to its end, where one after its
    /// last character goes.
    #[cfg(test)]
    pub(crate) fn within(&self, start: usize, end: usize) -> &[Hint] {
        &self.list[self.range(start, end)]
    }

    /// Where those [`within`](Self::within) bytes `start` and `end` are
    /// among [`all`](Self::all).
    pub(crate) fn range(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        let first = self.list.partition_point(|hint| hint.at < start);
        first..first + self.list[first..].partition_point(|hint| hint.at <= end)
    }

    /// Forgets them all, as when the text is replaced, or the server
    /// stops.
    pub(crate) fn clear(&mut self) {
        if !self.list.is_empty() {
            self.list = Vec::new();
            self.generation += 1;
        }
    }

    /// Makes `hints`, in the order of the text, those between bytes
    /// `start` and `end`, both counted, and no others there. Returns
    /// whether that changed anything. If too many are kept, those furthest
    /// from the range go.
    pub(crate) fn replace(&mut self, start: usize, end: usize, hints: Vec<InlayHint>) -> bool {
        let hints: Vec<Hint> = (hints.into_iter())
            .filter(|hint| (start..=end).contains(&hint.at))
            .filter_map(Hint::of)
            .collect();
        let first = self.list.partition_point(|hint| hint.at < start);
        let after = first + self.list[first..].partition_point(|hint| hint.at <= end);
        if self.list[first..after] == hints[..] {
            return false;
        }
        let count = hints.len();
        self.list.splice(first..after, hints);
        self.generation += 1;
        if self.list.len() > MOST_KEPT {
            // From either end, whichever is further from the range, until
            // few enough are left; never those just put in.
            let (mut from, mut to) = (0, self.list.len());
            for _ in MOST_KEPT..self.list.len() {
                let before = (from < first).then(|| start - self.list[from].at);
                let beyond = (to > first + count).then(|| self.list[to - 1].at - end);
                match (before, beyond) {
                    (Some(before), beyond) if before >= beyond.unwrap_or(0) => from += 1,
                    (_, Some(_)) => to -= 1,
                    // An answer bigger than the most kept, as the daemon
                    // never sends, is kept whole.
                    _ => break,
                }
            }
            // Another pane's may have gone: editors ask again.
            if from > 0 || to < self.list.len() {
                self.trims += 1;
            }
            self.list.truncate(to);
            self.list.drain(..from);
        }
        true
    }

    /// How many times hints have been let go to keep few enough, which
    /// an editor that asked for those of another part of the file sees,
    /// and asks again, in case they were its.
    pub(crate) fn trims(&self) -> u64 {
        self.trims
    }

    /// Moves them along with `edit`, made to the text they're for. One a
    /// change touches, at or beside its byte, goes, as it may be wrong
    /// now. The hints and the changes are walked together once, so an
    /// edit of thousands of changes, as `:%s` makes, costs no more than
    /// that.
    pub(crate) fn map(&mut self, edit: &Edit) {
        if self.list.is_empty() {
            return;
        }
        let Some((first, _, _)) = edit.changes().next() else {
            return;
        };
        // Those before the first change don't move.
        let from = self.list.partition_point(|hint| hint.at < first);
        if from == self.list.len() {
            return;
        }
        self.generation += 1;
        let mut untouched = edit.untouched();
        let mut kept = from;
        for index in from..self.list.len() {
            let Some(at) = untouched.place(self.list[index].at) else {
                continue;
            };
            self.list[index].at = at;
            // Those passed over since are let go once all are moved.
            if kept != index {
                self.list.swap(kept, index);
            }
            kept += 1;
        }
        self.list.truncate(kept);
    }
}

/// What an editor last saw of what it asks hints for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    /// The first and last lines it shows.
    lines: (u64, u64),
    revision: u64,
    /// How many times the daemon has said what the server does, which it
    /// says again when the server says its hints may be out of date.
    told: u64,
    /// How many times the copy has let hints go, as too many were kept.
    trims: u64,
}

/// What an editor asks of the hints of the part of the file it shows.
#[derive(Debug, Default)]
pub(super) struct HintWatch {
    seen: Option<Seen>,
    /// When to ask, once things have stayed as seen that long.
    due: Option<Instant>,
    /// What it asked about last: the lines, from the first to the last,
    /// and the text and what the server said, as seen then.
    asked: Option<(u64, u64, Seen)>,
}

impl HintWatch {
    /// Whether what it asked about last still holds for `seen`: the same
    /// text, the server not having said its hints may be out of date, and
    /// the lines shown among those asked about.
    fn holds(&self, seen: Seen) -> bool {
        self.asked.is_some_and(|(first, last, asked)| {
            asked.revision == seen.revision
                && asked.told == seen.told
                && asked.trims == seen.trims
                && first <= seen.lines.0
                && seen.lines.1 <= last
        })
    }
}

impl Editor {
    /// Watches the lines shown and the text, as of `now`, asking for the
    /// hints of those shown, and a screenful either side, once they've
    /// stayed out of what was asked last for [`HINTS_WAIT`]: if the editor
    /// `may_ask` (it's shown, and the daemon reads the question), the
    /// option's on, the server says, and it's not a lesson. Returns
    /// whether it asked.
    pub(crate) fn watch_hints(
        &mut self,
        now: Instant,
        may_ask: bool,
        copy: &mut BufferCopy,
    ) -> bool {
        let (Some(first), Some(last)) = (self.shown.first(), self.shown.last()) else {
            return false;
        };
        if !may_ask || !self.inlay_hints || self.lesson || !copy.features().inlay_hints {
            self.hints = HintWatch::default();
            return false;
        }
        let seen = Seen {
            lines: (first.line, last.line),
            revision: copy.revision(),
            told: copy.features_told(),
            trims: copy.hints().trims(),
        };
        if self.hints.holds(seen) {
            self.hints.due = None;
            self.hints.seen = Some(seen);
            return false;
        }
        if self.hints.seen != Some(seen) || self.hints.due.is_none() {
            self.hints.seen = Some(seen);
            self.hints.due = Some(now + HINTS_WAIT);
            return false;
        }
        if self.hints.due.is_some_and(|due| due > now) {
            return false;
        }
        self.hints.due = None;
        let text = self.text(copy);
        let margin = self.height.max(1) as u64;
        let (from, to) = (
            first.line.saturating_sub(margin),
            (last.line + margin).min(text.last_line()),
        );
        let (start, end) = (text.line_start(from), text.line_end(to));
        drop(text);
        copy.ask(Query::InlayHints { end }, start);
        self.hints.asked = Some((from, to, seen));
        true
    }

    /// When hints are next to be asked for, if they are.
    pub(super) fn hints_due(&self) -> Option<Instant> {
        self.hints.due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(at: usize, label: &str) -> InlayHint {
        InlayHint {
            at,
            label: label.into(),
            kind: InlayHintKind::Type,
            pad_left: false,
            pad_right: true,
        }
    }

    fn places(hints: &Hints) -> Vec<(usize, &str)> {
        (hints.list.iter())
            .map(|hint| (hint.at, &*hint.label))
            .collect()
    }

    #[test]
    fn a_hint_takes_its_labels_cells_and_its_paddings() {
        let mut hints = Hints::default();
        let wide = InlayHint {
            pad_left: true,
            ..hint(3, ": 字x")
        };
        assert!(hints.replace(0, 10, vec![wide, hint(5, "\u{301}")]));
        // A label of no cells is none.
        assert_eq!(places(&hints), [(3, ": 字x")]);
        assert_eq!(hints.within(0, 10)[0].cells, 1 + 5 + 1);
    }

    #[test]
    fn an_answer_replaces_the_hints_of_its_range_alone_and_says_if_that_changed_any() {
        let mut hints = Hints::default();
        hints.replace(0, 100, vec![hint(5, "a"), hint(50, "b"), hint(100, "c")]);
        // Another pane's range, inside: what's either side stays.
        assert!(hints.replace(40, 60, vec![hint(45, "d"), hint(99, "outside")]));
        assert_eq!(places(&hints), [(5, "a"), (45, "d"), (100, "c")]);
        assert!(!hints.replace(40, 60, vec![hint(45, "d")]), "the same");
        // Both ends counted.
        assert!(hints.replace(5, 100, Vec::new()));
        assert!(hints.is_empty());
        assert_eq!(hints.within(0, 0), []);
    }

    #[test]
    fn too_many_kept_lose_those_furthest_from_the_last_answer() {
        let mut hints = Hints::default();
        let many = |from: usize| {
            (from..from + MOST_KEPT)
                .map(|at| hint(at * 2, "x"))
                .collect()
        };
        // More than are kept, in one answer, as the daemon never sends, are
        // kept whole.
        let mut more: Vec<InlayHint> = many(0);
        more.push(hint(MOST_KEPT * 2, "x"));
        hints.replace(0, usize::MAX / 4, more);
        assert_eq!((hints.list.len(), hints.trims()), (MOST_KEPT + 1, 0));
        hints.replace(0, usize::MAX / 4, many(0));
        hints.replace(30_000, 30_010, vec![hint(30_004, "y")]);
        assert_eq!(hints.list.len(), MOST_KEPT);
        assert_eq!(hints.trims(), 1, "editors are to ask again");
        assert!(hints.within(30_004, 30_004).len() == 1);
        // The furthest, at the start, went.
        assert_eq!(hints.list[0].at, 2);
        assert_eq!(hints.list[MOST_KEPT - 2].at, (MOST_KEPT - 1) * 2);
        // And from the other end, when that's further.
        hints.replace(0, 1, vec![hint(0, "z")]);
        assert_eq!(hints.list.len(), MOST_KEPT);
        assert_eq!(hints.list[0].at, 0);
        assert_eq!(hints.list.last().unwrap().at, (MOST_KEPT - 1) * 2);
    }

    #[test]
    fn hints_move_with_edits_and_one_an_edit_touches_goes() {
        let mut hints = Hints::default();
        hints.replace(
            0,
            100,
            vec![hint(2, "a"), hint(5, "b"), hint(10, "c"), hint(20, "d")],
        );
        // "xy" typed at 5, where b is: it goes; after it, all move.
        let typed = Edit::replace(30, 5, "", "xy");
        hints.map(&typed);
        assert_eq!(places(&hints), [(2, "a"), (12, "c"), (22, "d")]);
        // Bytes 8 to 12 deleted: c, just after them, is touched, and goes;
        // what's after moves back.
        hints.map(&Edit::replace(32, 8, "abcd", ""));
        assert_eq!(places(&hints), [(2, "a"), (18, "d")]);
    }

    #[test]
    fn moving_hints_through_an_edit_of_many_changes_goes_through_it_once() {
        // As `map_untouched` moves a hint, one at a time, for each of a
        // thousand changes.
        let len = 20_000;
        let mut edit = Edit::new();
        for change in 0..1000 {
            edit.retain(if change == 0 { 7 } else { 19 });
            edit.delete("a");
            edit.insert("bb");
        }
        edit.retain(len - edit.base_len());
        let at: Vec<usize> = (0..len).step_by(3).collect();
        let mut hints = Hints::default();
        hints.replace(0, len, at.iter().map(|&at| hint(at, "h")).collect());
        let started = Instant::now();
        hints.map(&edit);
        let took = started.elapsed();
        let expected: Vec<usize> = (at.iter())
            .filter_map(|&at| edit.map_untouched(at, at).map(|(at, _)| at))
            .collect();
        let moved: Vec<usize> = hints.list.iter().map(|hint| hint.at).collect();
        assert_eq!(moved, expected);
        assert!(took < Duration::from_millis(20), "{took:?}");
    }
}
