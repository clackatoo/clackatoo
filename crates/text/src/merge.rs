//! The edit between two texts, and merging a change into a text that has
//! changed meanwhile, as an agent's write to a file is merged into edits
//! you haven't saved.
//!
//! A merge never mixes two changes to the same text: if they touch the
//! same line, it says where, rather than guess which goes first.

use std::collections::HashSet;
use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

use crate::edit::{Edit, EditError};
use crate::hunks::line_starts;

/// The most bytes of changed lines, both sides together, that are compared
/// word by word. Beyond it, what's between their common start and end is
/// replaced whole: such a change is a rewrite, not an edit.
const WORDS: usize = 64 * 1024;

/// The biggest text, in bytes, compared line by line. A bigger one is
/// replaced whole, which merges with nothing: comparing it could hold up
/// the daemon, and no one edits such a file by hand.
const LINES: usize = 4 * 1024 * 1024;

impl Edit {
    /// The edit that makes `text` of `base`, touching as little as it can:
    /// the lines that differ, and within them only the words that do.
    /// Texts bigger than 4 MiB are replaced whole.
    ///
    /// Lines are compared by Myers' algorithm, which takes milliseconds
    /// where the histogram one git signs use can take most of a second,
    /// as for a change to every tenth line of a big file.
    #[must_use]
    pub fn between(base: &str, text: &str) -> Self {
        let mut edit = Self::new();
        if base == text {
            edit.retain(base.len());
            return edit;
        }
        if base.len() > LINES || text.len() > LINES {
            edit.delete(base).insert(text);
            return edit;
        }
        let input = InternedInput::new(base, text);
        let mut found = Diff::compute(Algorithm::Myers, &input);
        found.postprocess_lines(&input);
        let (base_lines, text_lines) = (line_starts(base), line_starts(text));
        let mut done = 0;
        for hunk in found.hunks() {
            let old = base_lines[hunk.before.start as usize]..base_lines[hunk.before.end as usize];
            let new = text_lines[hunk.after.start as usize]..text_lines[hunk.after.end as usize];
            edit.retain(old.start - done);
            words(&mut edit, &base[old.clone()], &text[new]);
            done = old.end;
        }
        edit.retain(base.len() - done);
        edit
    }
}

/// What came of merging a change into a text changed meanwhile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Merged {
    /// The change, as it must be to make it to the text now.
    Made(Edit),
    /// The change touches lines changed meanwhile: these, in the base,
    /// in order, each the lines both changes touch together.
    Conflict(Vec<Range<usize>>),
}

/// The change that made `theirs` of `base`, as it must be to make it to
/// `now`, which has changed from `base` too. A change made exactly the
/// same on both sides is made once. Changes to different lines merge.
/// Changes that touch a line both changed, or overlap at all, are a
/// [`Merged::Conflict`]: never interleaved.
///
/// Lines put in whole between two lines touch neither, so a line added
/// beside one the other changed merges, but two put in at the same place
/// conflict.
///
/// # Errors
///
/// None in practice: both changes are edits of `base`, cut between
/// characters.
pub fn merge(base: &str, theirs: &str, now: &str) -> Result<Merged, EditError> {
    let change = Edit::between(base, theirs);
    if base == now {
        return Ok(Merged::Made(change));
    }
    let meanwhile = Edit::between(base, now);
    let change = without(&change, &meanwhile);
    let starts = line_starts(base);
    let spans = |edit: &Edit| -> Vec<Range<usize>> {
        (edit.changes())
            .map(|(at, removed, inserted)| span(base, &starts, at, removed, inserted))
            .collect()
    };
    let (ours, others) = (spans(&change), spans(&meanwhile));
    let mut met: Vec<Range<usize>> = Vec::new();
    for one in &ours {
        for other in others.iter().filter(|other| meet(one, other)) {
            met.push(one.start.min(other.start)..one.end.max(other.end));
        }
    }
    met.sort_by_key(|range| range.start);
    let mut conflicts: Vec<Range<usize>> = Vec::new();
    for both in met {
        match conflicts.last_mut() {
            Some(last) if last.end >= both.start => last.end = last.end.max(both.end),
            _ => conflicts.push(both),
        }
    }
    if !conflicts.is_empty() {
        return Ok(Merged::Conflict(conflicts));
    }
    // Apart, each change is made where it is in the text now, unchanged:
    // anything else would make text neither side wrote, so it's refused.
    match apart(base, &change, &meanwhile, now) {
        Some(made) => Ok(Merged::Made(made)),
        None => Ok(Merged::Conflict(std::iter::once(0..base.len()).collect())),
    }
}

/// `change`, an edit of `base` none of whose changes meet those of
/// `meanwhile`, made to `now`, which `meanwhile` made of `base`: each
/// change as it is, moved past those made before it. Where both change
/// at one place, lines put in whole go before a change within the line
/// after them. `None` if a change doesn't find its text where it goes.
fn apart(base: &str, change: &Edit, meanwhile: &Edit, now: &str) -> Option<Edit> {
    let between_lines = |at: usize, removed: &str, inserted: &str| {
        removed.is_empty()
            && inserted.ends_with('\n')
            && (at == 0 || base.as_bytes()[at - 1] == b'\n')
    };
    let mut others = meanwhile.changes().peekable();
    let mut made = Edit::new();
    // How far the text now has moved from `base`, and how much of it the
    // edit has passed.
    let (mut moved, mut done) = (0_isize, 0);
    for (at, removed, inserted) in change.changes() {
        let ours_between = between_lines(at, removed, inserted);
        while let Some(&(other_at, other_removed, other_inserted)) = others.peek() {
            let before = other_at < at
                || (other_at == at
                    && !ours_between
                    && between_lines(other_at, other_removed, other_inserted));
            if !before {
                break;
            }
            moved += other_inserted.len().cast_signed() - other_removed.len().cast_signed();
            others.next();
        }
        let place = at.checked_add_signed(moved)?;
        if now.get(place..place + removed.len())? != removed {
            return None;
        }
        made.retain(place.checked_sub(done)?)
            .delete(removed)
            .insert(inserted);
        done = place + removed.len();
    }
    made.retain(now.len().checked_sub(done)?);
    Some(made)
}

/// The bytes of `base` a change touches, `removed` at `at` replaced by
/// `inserted`: the whole lines it's on, or for whole lines put in between
/// two, the empty place between them. `starts` are where `base`'s lines
/// start.
fn span(base: &str, starts: &[usize], at: usize, removed: &str, inserted: &str) -> Range<usize> {
    let line_start = |place: usize| starts[starts.partition_point(|&start| start <= place) - 1];
    let between_lines = removed.is_empty()
        && inserted.ends_with('\n')
        && (at == 0 || base.as_bytes()[at - 1] == b'\n');
    if between_lines {
        return at..at;
    }
    let start = line_start(at);
    // The end of the line the change's last byte is on, or its place's.
    let last = if removed.is_empty() {
        at
    } else {
        at + removed.len() - 1
    };
    let last = last.min(base.len());
    let end = (base.as_bytes()[last..].iter())
        .position(|&byte| byte == b'\n')
        .map_or(base.len(), |newline| last + newline + 1);
    start..end.max(start)
}

/// Whether two changes' spans meet: share a byte, or are the same place
/// between lines, or one is a place inside the other's lines.
fn meet(a: &Range<usize>, b: &Range<usize>) -> bool {
    let inside = |place: usize, lines: &Range<usize>| lines.start < place && place < lines.end;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => a.start == b.start,
        (true, false) => inside(a.start, b),
        (false, true) => inside(b.start, a),
        (false, false) => a.start < b.end && b.start < a.end,
    }
}

/// `edit` without the changes `made` makes too, exactly, both being edits
/// of one text.
fn without(edit: &Edit, made: &Edit) -> Edit {
    let made: HashSet<_> = made.changes().collect();
    let mut left = Edit::new();
    let mut done = 0;
    for change @ (at, removed, inserted) in edit.changes() {
        if made.contains(&change) {
            continue;
        }
        left.retain(at - done).delete(removed).insert(inserted);
        done = at + removed.len();
    }
    left.retain(edit.base_len() - done);
    left
}

/// Adds to `edit` the change of `old`, a run of whole lines, into `new`:
/// what's between their common start and end, word by word.
fn words(edit: &mut Edit, old: &str, new: &str) {
    let start = common_start(old, new);
    let end = common_end(&old[start..], &new[start..]);
    let (old_middle, new_middle) = (&old[start..old.len() - end], &new[start..new.len() - end]);
    edit.retain(start);
    if old_middle.is_empty() || new_middle.is_empty() || old_middle.len() + new_middle.len() > WORDS
    {
        edit.delete(old_middle).insert(new_middle);
    } else {
        let (old_words, new_words) = (split(old_middle), split(new_middle));
        let mut input = InternedInput::default();
        input.update_before(old_words.iter().copied());
        input.update_after(new_words.iter().copied());
        let found = Diff::compute(Algorithm::Myers, &input);
        let mut done = 0;
        for hunk in found.hunks() {
            let (before, after) = (
                hunk.before.start as usize..hunk.before.end as usize,
                hunk.after.start as usize..hunk.after.end as usize,
            );
            edit.retain(
                old_words[done..before.start]
                    .iter()
                    .map(|word| word.len())
                    .sum(),
            );
            edit.delete(&old_words[before.clone()].concat())
                .insert(&new_words[after].concat());
            done = before.end;
        }
        edit.retain(old_words[done..].iter().map(|word| word.len()).sum());
    }
    edit.retain(end);
}

/// How many bytes `a` and `b` start with in common, up to a character.
fn common_start(a: &str, b: &str) -> usize {
    let mut len = (a.bytes().zip(b.bytes()))
        .take_while(|(a, b)| a == b)
        .count();
    while !(a.is_char_boundary(len) && b.is_char_boundary(len)) {
        len -= 1;
    }
    len
}

/// How many bytes `a` and `b` end with in common, up to a character.
fn common_end(a: &str, b: &str) -> usize {
    let mut len = (a.bytes().rev().zip(b.bytes().rev()))
        .take_while(|(a, b)| a == b)
        .count();
    while !(a.is_char_boundary(a.len() - len) && b.is_char_boundary(b.len() - len)) {
        len -= 1;
    }
    len
}

/// `text` cut into words: runs of letters, digits and `_`, runs of spaces,
/// and each other character alone, a line's end among them.
fn split(text: &str) -> Vec<&str> {
    #[derive(PartialEq)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() && c != '\n' {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut words = Vec::new();
    let mut start = 0;
    let mut last = None;
    for (at, c) in text.char_indices() {
        let this = class(c);
        let joins = this != Class::Other && last.as_ref() == Some(&this);
        if !joins && at > start {
            words.push(&text[start..at]);
            start = at;
        }
        last = Some(this);
    }
    if start < text.len() {
        words.push(&text[start..]);
    }
    words
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::edit::Op;

    /// What `edit` takes away and puts in, with where, in the text before.
    fn changes(edit: &Edit) -> Vec<(usize, &str, &str)> {
        edit.changes().collect()
    }

    /// A list of the one range `range`.
    fn one(range: Range<usize>) -> Vec<Range<usize>> {
        std::iter::once(range).collect()
    }

    /// `merge`'s edit, made to `now`.
    fn merged(base: &str, theirs: &str, now: &str) -> String {
        match merge(base, theirs, now).unwrap() {
            Merged::Made(edit) => edit.apply_to(now).unwrap(),
            Merged::Conflict(lines) => panic!("a conflict at {lines:?}"),
        }
    }

    /// The bytes of `base` `merge` says conflict.
    fn conflict(base: &str, theirs: &str, now: &str) -> Vec<Range<usize>> {
        match merge(base, theirs, now).unwrap() {
            Merged::Conflict(lines) => lines,
            Merged::Made(edit) => panic!("merged: {:?}", edit.apply_to(now)),
        }
    }

    #[test]
    fn only_the_words_that_differ_are_changed() {
        let base = "fn main() {\n    let x = 1;\n}\n";
        let text = "fn main() {\n    let count = 1;\n}\n";
        let edit = Edit::between(base, text);
        assert_eq!(edit.apply_to(base).unwrap(), text);
        assert_eq!(changes(&edit), [(20, "x", "count")]);
        // Lines added and taken away go whole.
        let edit = Edit::between("a\nb\nc\n", "a\nc\nd\n");
        assert_eq!(changes(&edit), [(2, "b\n", ""), (6, "", "d\n")]);
        // The same text, or none, changes nothing.
        assert!(Edit::between("same", "same").is_noop());
        assert_eq!(Edit::between("", "new").ops(), [Op::Insert("new".into())]);
    }

    #[test]
    fn a_change_is_cut_between_characters() {
        let edit = Edit::between("é\n", "è\n");
        assert_eq!(changes(&edit), [(0, "é", "è")]);
        let edit = Edit::between("aé", "aè");
        assert_eq!(edit.apply_to("aé").unwrap(), "aè");
    }

    #[test]
    fn a_text_too_big_to_compare_is_replaced_whole() {
        let base = "a\n".repeat(LINES / 2 + 1);
        let text = format!("b\n{}", &base[2..]);
        assert_eq!(changes(&Edit::between(&base, &text)), [(0, &*base, &*text)]);
    }

    #[test]
    fn changes_to_other_lines_merge() {
        let base = "one\ntwo\nthree\n";
        // Theirs changes the last line, and yours the first.
        assert_eq!(
            merged(base, "one\ntwo\nTHREE\n", "one, said I\ntwo\nthree\n"),
            "one, said I\ntwo\nTHREE\n"
        );
        // Lines put in beside a line changed touch neither.
        assert_eq!(
            merged(base, "one\nTWO\nthree\n", "one\nnew\ntwo\nthree\n"),
            "one\nnew\nTWO\nthree\n"
        );
        // Unchanged meanwhile, it's the edit between.
        assert_eq!(
            merge(base, "x\n", base).unwrap(),
            Merged::Made(Edit::between(base, "x\n"))
        );
    }

    #[test]
    fn changes_to_the_same_line_conflict_and_are_never_interleaved() {
        // Both change the same text.
        assert_eq!(conflict("x = 1", "x = 3", "x = 2"), one(0..5));
        let lines = conflict("a\nx = 1\nb\n", "a\nx = 3\nb\n", "a\nx = 2\nb\n");
        assert_eq!(lines, one(2..8));
        // Different words of one line.
        let base = "let x = f(a);\n";
        let lines = conflict(base, "let x = f(b);\n", "let y = f(a);\n");
        assert_eq!(lines, one(0..14));
        // A change inside a line the other took away.
        assert_eq!(conflict("a\nb c\nd\n", "a\nb C\nd\n", "a\nd\n"), one(2..6));
        // Lines put in at the same place, and a change running into
        // another's line.
        assert_eq!(conflict("a\nb\n", "a\nx\nb\n", "a\ny\nb\n"), one(2..2));
        assert_eq!(conflict("a\nb\nc\n", "a\nB\nC\n", "a\nb\nc!\n"), one(4..6));
        // A change to a line only your unsaved text has, merged into the
        // text saved: refused, never a mix of the two.
        let lines = conflict("a\nfoo bar\nc\n", "a\nfoo baz\nc\n", "a\nc\n");
        assert_eq!(lines, one(2..10));
    }

    #[test]
    fn the_same_change_made_by_both_is_made_once() {
        let base = "a\nb\n";
        let both = "a\nB\n";
        assert_eq!(merged(base, both, both), both);
    }

    fn texts() -> impl Strategy<Value = String> {
        // Few letters and many line ends, so texts share lines and words.
        proptest::string::string_regex("[ab é_\n]{0,40}").unwrap()
    }

    proptest! {
        #[test]
        fn the_edit_between_makes_the_text(base in texts(), text in texts()) {
            let edit = Edit::between(&base, &text);
            prop_assert_eq!(edit.apply_to(&base).unwrap(), text);
        }

        #[test]
        fn a_merge_keeps_both_sides_or_says_where_they_meet(
            base in texts(),
            theirs in texts(),
            now in texts(),
        ) {
            match merge(&base, &theirs, &now).unwrap() {
                Merged::Made(edit) => {
                    let merged = edit.apply_to(&now).unwrap();
                    // Nothing changed meanwhile: theirs exactly. Nothing
                    // changed by them: now exactly.
                    if base == now {
                        prop_assert_eq!(&merged, &theirs);
                    }
                    if base == theirs {
                        prop_assert_eq!(&merged, &now);
                    }
                    // Merged the other way, the same text.
                    if let Merged::Made(other) = merge(&base, &now, &theirs).unwrap() {
                        prop_assert_eq!(other.apply_to(&theirs).unwrap(), merged);
                    }
                }
                Merged::Conflict(lines) => {
                    prop_assert!(base != now && base != theirs);
                    prop_assert!(lines.iter().all(|lines| lines.end <= base.len()));
                }
            }
        }
    }
}
