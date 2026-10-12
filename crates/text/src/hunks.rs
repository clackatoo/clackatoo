//! Hunks: the runs of lines a text has added, changed or deleted since a
//! base, such as the file's text in git's index.
//!
//! The daemon compares each buffer with its base, off the event loop, and
//! sends the hunks it finds to every window showing the buffer. It and
//! the windows move them along with each edit, as diagnostics are moved,
//! until the next comparison sends new ones. Each hunk keeps the base's
//! text for its lines, so a window can show what was there, or put it
//! back, without asking.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};
use ropey::Rope;

use crate::edit::{Bias, Edit};

/// One hunk: bytes `start` to `end` of the text, which are whole lines,
/// were `base` in the base. Lines added have no base; lines deleted have
/// no text, and are where they were, at the start of the line after them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Hunk {
    /// The first byte of its first line.
    pub start: usize,
    /// The byte after its last line's end, or `start` if it's lines
    /// deleted.
    pub end: usize,
    /// What its lines were in the base, each with its line end.
    pub base: String,
}

impl Hunk {
    /// What kind of hunk it is, or `None` if it's become nothing: its
    /// lines were added, and have all been deleted since.
    #[must_use]
    pub fn kind(&self) -> Option<HunkKind> {
        match (self.start == self.end, self.base.is_empty()) {
            (false, true) => Some(HunkKind::Added),
            (false, false) => Some(HunkKind::Changed),
            (true, false) => Some(HunkKind::Deleted),
            (true, true) => None,
        }
    }

    /// The line it's on in `text`, the text it's for: its first, or for
    /// lines deleted, the line above where they were, as gitsigns marks
    /// them, or the first line if they were at the top.
    #[must_use]
    pub fn line(&self, text: &Rope) -> usize {
        let line = text.byte_to_line(self.start.min(text.len_bytes()));
        if self.start == self.end {
            line.saturating_sub(1)
        } else {
            line
        }
    }

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
}

impl Hunk {
    /// The bytes of `text`'s lines it covers, whole: its own, unless an
    /// edit since it was found, such as one joining lines, has moved its
    /// ends into a line. For lines deleted, none, at the start of the line
    /// after them.
    #[must_use]
    pub fn whole_lines(&self, text: &Rope) -> Range<usize> {
        let lines = self.lines(text);
        text.line_to_byte(lines.start)..text.line_to_byte(lines.end)
    }
}

/// What a hunk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkKind {
    /// Lines that aren't in the base.
    Added,
    /// Lines that were others in the base.
    Changed,
    /// Lines of the base that are gone.
    Deleted,
}

/// What the sign beside a line says about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sign {
    /// It was added.
    Added,
    /// It's changed.
    Changed,
    /// Lines were deleted after it.
    Deleted,
    /// Lines were deleted before it, at the top of the text.
    DeletedAbove,
}

/// A text's hunks, in order. They never overlap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunks {
    list: Vec<Hunk>,
}

impl Hunks {
    /// No hunks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// These hunks, put in order.
    #[must_use]
    pub fn of(mut list: Vec<Hunk>) -> Self {
        list.sort_by_key(|hunk| hunk.start);
        Self { list }
    }

    /// The hunks between `base` and `text`: what `text` has added,
    /// changed and deleted since it was `base`, line by line.
    #[must_use]
    pub fn between(base: &str, text: &str) -> Self {
        Self::of_differences(base, text, &diff(base, text))
    }

    /// The hunks `differences` between `base` and `text` make, one for
    /// each, in the same order, as [`diff`] found them.
    #[must_use]
    pub fn of_differences(base: &str, text: &str, differences: &[Difference]) -> Self {
        let (base_lines, text_lines) = (line_starts(base), line_starts(text));
        let list = (differences.iter())
            .map(|difference| Hunk {
                start: text_lines[difference.after.start],
                end: text_lines[difference.after.end],
                base: base[base_lines[difference.before.start]..base_lines[difference.before.end]]
                    .to_owned(),
            })
            .collect();
        Self { list }
    }

    /// Every one, in order.
    #[must_use]
    pub fn all(&self) -> &[Hunk] {
        &self.list
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The hunk at `line` of `text`, the text they're for, and the sign
    /// beside it: the hunk covering the line, or lines deleted just after
    /// it, or for the first line, just before it.
    #[must_use]
    pub fn at_line(&self, text: &Rope, line: usize) -> Option<(&Hunk, Sign)> {
        (self.find(text, line)).map(|(index, sign)| (&self.list[index], sign))
    }

    /// Which hunk is at `line` of `text`, as [`at_line`](Self::at_line)
    /// finds it, by its place in [`all`](Self::all).
    #[must_use]
    pub fn find(&self, text: &Rope, line: usize) -> Option<(usize, Sign)> {
        if line >= text.len_lines() {
            return None;
        }
        let start = text.line_to_byte(line);
        let next = text.line_to_byte(line + 1);
        // Hunks don't overlap, and each covers whole lines, so only the
        // last to start before the next line can cover this one.
        let before = self.list.partition_point(|hunk| hunk.start < next);
        if let Some(index) = before.checked_sub(1)
            && self.list[index].end > start
        {
            match self.list[index].kind() {
                Some(HunkKind::Added) => return Some((index, Sign::Added)),
                Some(HunkKind::Changed) => return Some((index, Sign::Changed)),
                _ => {}
            }
        }
        // The line after the last line end has nothing after it.
        if next > start
            && let Some(hunk) = self.list.get(before)
            && hunk.start == next
            && hunk.kind() == Some(HunkKind::Deleted)
        {
            return Some((before, Sign::Deleted));
        }
        match self.list.first() {
            Some(hunk)
                if line == 0 && hunk.start == 0 && hunk.kind() == Some(HunkKind::Deleted) =>
            {
                Some((0, Sign::DeletedAbove))
            }
            _ => None,
        }
    }

    /// The sign beside `line` of `text`, the text they're for, if any.
    #[must_use]
    pub fn sign(&self, text: &Rope, line: usize) -> Option<Sign> {
        self.at_line(text, line).map(|(_, sign)| sign)
    }

    /// Takes away the hunk at `index` in [`all`](Self::all), as one put
    /// back as its base is about to be.
    pub fn remove(&mut self, index: usize) -> Hunk {
        self.list.remove(index)
    }

    /// Moves them along with `edit`, made to the text they're for. Text
    /// put in at either end of one isn't in it, but for at its start,
    /// which is in its first line. Lines added that are all deleted go.
    pub fn map(&mut self, edit: &Edit) {
        for hunk in &mut self.list {
            hunk.start = edit.map(hunk.start, Bias::Before);
            hunk.end = edit.map(hunk.end, Bias::Before).max(hunk.start);
        }
        self.list.retain(|hunk| hunk.kind().is_some());
    }
}

/// A run of lines that differ: lines `before` of the base became lines
/// `after` of the text, counting from 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    /// The base's lines.
    pub before: Range<usize>,
    /// The text's lines.
    pub after: Range<usize>,
}

/// How `text` differs from `base`, line by line, in order: by the
/// histogram algorithm, as git's `--histogram` and Helix compare, with
/// each run put where a person reading it would put it.
#[must_use]
pub fn diff(base: &str, text: &str) -> Vec<Difference> {
    let input = InternedInput::new(base, text);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    (diff.hunks())
        .map(|hunk| Difference {
            before: hunk.before.start as usize..hunk.before.end as usize,
            after: hunk.after.start as usize..hunk.after.end as usize,
        })
        .collect()
}

/// The byte each line of `text` starts at, and its length, after them,
/// where a line after the last would start. Lines end at `\n`, which is
/// part of the line, as in [`diff`].
#[must_use]
pub fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut at = 0;
    for line in imara_diff::sources::lines(text) {
        at += line.len();
        starts.push(at);
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(hunks: &Hunks) -> Vec<(usize, usize, &str)> {
        (hunks.all().iter())
            .map(|hunk| (hunk.start, hunk.end, hunk.base.as_str()))
            .collect()
    }

    fn signs(hunks: &Hunks, text: &str) -> Vec<Option<Sign>> {
        let rope = Rope::from_str(text);
        (0..rope.len_lines())
            .map(|line| hunks.sign(&rope, line))
            .collect()
    }

    #[test]
    fn lines_added_changed_and_deleted_are_found() {
        let base = "a\nb\nc\nd\ne\n";
        // b changed, a line added after c, e deleted.
        let text = "a\nB\nc\nnew\nd\n";
        let hunks = Hunks::between(base, text);
        assert_eq!(kinds(&hunks), [(2, 4, "b\n"), (6, 10, ""), (12, 12, "e\n")]);
        let kinds: Vec<_> = hunks.all().iter().map(Hunk::kind).collect();
        assert_eq!(
            kinds,
            [
                Some(HunkKind::Changed),
                Some(HunkKind::Added),
                Some(HunkKind::Deleted)
            ]
        );
        assert_eq!(
            signs(&hunks, text),
            [
                None,
                Some(Sign::Changed),
                None,
                Some(Sign::Added),
                Some(Sign::Deleted),
                None
            ]
        );
        assert!(Hunks::between(base, base).is_empty());
    }

    #[test]
    fn lines_deleted_at_the_top_are_marked_on_the_first() {
        let hunks = Hunks::between("gone\nstays\n", "stays\n");
        assert_eq!(kinds(&hunks), [(0, 0, "gone\n")]);
        let text = Rope::from_str("stays\n");
        assert_eq!(hunks.sign(&text, 0), Some(Sign::DeletedAbove));
        assert_eq!(hunks.all()[0].line(&text), 0);
        // And at the bottom, on the last.
        let hunks = Hunks::between("stays\ngone\n", "stays\n");
        assert_eq!(hunks.sign(&text, 0), Some(Sign::Deleted));
        assert_eq!(hunks.sign(&text, 1), None);
    }

    #[test]
    fn a_last_line_without_its_end_differs_from_one_with() {
        let hunks = Hunks::between("a\nb\n", "a\nb");
        assert_eq!(kinds(&hunks), [(2, 3, "b\n")]);
        assert_eq!(signs(&hunks, "a\nb"), [None, Some(Sign::Changed)]);
        // A file that was empty is all added.
        let hunks = Hunks::between("", "x\ny\n");
        assert_eq!(kinds(&hunks), [(0, 4, "")]);
        assert_eq!(hunks.all()[0].lines(&Rope::from_str("x\ny\n")), 0..2);
    }

    #[test]
    fn they_move_with_edits() {
        let base = "a\nb\nc\nd\n";
        let text = "a\nB\nc\nd\nnew\n";
        let mut hunks = Hunks::between(base, text);
        assert_eq!(kinds(&hunks), [(2, 4, "b\n"), (8, 12, "")]);
        // What's typed at the start of a hunk's first line is in it, as a
        // line put in before lines added is added too.
        let mut edit = Edit::new();
        edit.retain(2)
            .insert("xy")
            .retain(6)
            .insert("put\n")
            .retain(4);
        hunks.map(&edit);
        assert_eq!(kinds(&hunks), [(2, 6, "b\n"), (10, 18, "")]);
        // Deleting the lines added takes them away; deleting the changed
        // line leaves where it was, as lines deleted.
        let mut edit = Edit::new();
        edit.retain(2)
            .delete("xyB\n")
            .retain(4)
            .delete("put\nnew\n");
        hunks.map(&edit);
        assert_eq!(kinds(&hunks), [(2, 2, "b\n")]);
    }

    #[test]
    fn the_hunk_at_a_line_is_found_by_any_of_its_lines() {
        let base = "a\nb\nc\n";
        let text = "a\nx\ny\nz\nc\n";
        let hunks = Hunks::between(base, text);
        let rope = Rope::from_str(text);
        for line in 1..=3 {
            let (hunk, sign) = hunks.at_line(&rope, line).expect("a hunk");
            assert_eq!((hunk.base.as_str(), sign), ("b\n", Sign::Changed));
            assert_eq!(hunk.lines(&rope), 1..4);
            assert_eq!(hunk.line(&rope), 1);
        }
        assert_eq!(hunks.at_line(&rope, 0), None);
        assert_eq!(hunks.at_line(&rope, 4), None);
        assert_eq!(hunks.at_line(&rope, 99), None);
    }

    #[test]
    fn line_starts_end_with_the_length() {
        assert_eq!(line_starts(""), [0]);
        assert_eq!(line_starts("a\nbc"), [0, 2, 4]);
        assert_eq!(line_starts("a\n\n"), [0, 2, 3]);
    }
}
