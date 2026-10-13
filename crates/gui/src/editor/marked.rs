//! The lines `:g` marks, kept track of as its command changes the text.
//!
//! As in Vim, `:g` marks every line that matches first, and then does its
//! command on each line still marked, in turn. A line deleted meanwhile,
//! or joined onto the one before, is no longer marked; the others move
//! with the text, wherever the command edits it.
//!
//! Each mark is where its line starts. Edits move them all, so with tens
//! of thousands of lines, each edit can't step through every mark: what
//! an edit adds to the marks after a place is kept in a Fenwick tree, a
//! table of sums that adds to every mark from one on, or reads one mark's
//! sum, in a few steps.

use clackatoo_text::{Edit, Rope};

/// The lines marked, by where each starts, in order.
#[derive(Debug)]
pub(crate) struct Marked {
    /// Where each started when marked.
    starts: Vec<usize>,
    /// The Fenwick tree of how far each has moved since: entry `i` holds
    /// the sum of the moves added from some marks on, for the marks its
    /// range covers.
    moved: Vec<isize>,
    /// Whether each line has gone.
    gone: Vec<bool>,
}

impl Marked {
    /// The lines starting at `starts`, which are in order.
    pub(crate) fn new(starts: Vec<usize>) -> Self {
        let count = starts.len();
        Self {
            starts,
            moved: vec![0; count + 1],
            gone: vec![false; count],
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.starts.len()
    }

    /// Where line `index` starts now, unless it's gone.
    pub(crate) fn get(&self, index: usize) -> Option<usize> {
        (!self.gone[index]).then(|| self.at(index))
    }

    /// Where mark `index` is now, whether its line is gone or not.
    fn at(&self, index: usize) -> usize {
        let mut sum = 0;
        let mut node = index + 1;
        while node > 0 {
            sum += self.moved[node];
            node &= node - 1;
        }
        self.starts[index].saturating_add_signed(sum)
    }

    /// Moves marks `from` on by `by`.
    fn shift(&mut self, from: usize, by: isize) {
        if by == 0 {
            return;
        }
        let mut node = from + 1;
        while node < self.moved.len() {
            self.moved[node] += by;
            node += node & node.wrapping_neg();
        }
    }

    /// The first mark at or after byte `byte`.
    fn first_from(&self, byte: usize) -> usize {
        let (mut low, mut high) = (0, self.starts.len());
        while low < high {
            let middle = low + (high - low) / 2;
            if self.at(middle) < byte {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    /// Moves the marks through `edit`, made to `before`, the text, and
    /// forgets the lines it takes away: whole lines deleted, and a line
    /// whose break before it is taken from partway along the line before,
    /// joining them, as `J` does. Text put in where a line starts goes into
    /// the line, unless it ends with a line break, making lines of its own
    /// before it.
    pub(crate) fn edited(&mut self, edit: &Edit, before: &Rope) {
        let base = edit.base_len();
        let changes: Vec<(usize, &str, &str)> = edit.changes().collect();
        // Last first, so each change's places are as they were before it.
        for &(at, removed, inserted) in changes.iter().rev() {
            let end = at + removed.len();
            let by = inserted.len().cast_signed() - removed.len().cast_signed();
            let starts_line = at == 0 || before.byte(at - 1) == b'\n';
            let takes_break = removed.ends_with('\n') && !inserted.ends_with('\n');
            let at_goes = starts_line
                && !removed.is_empty()
                && (takes_break || (end == base && !inserted.ends_with('\n')));
            let end_goes = takes_break && !starts_line;
            let mut index = self.first_from(at);
            // Those that go wait where the change was, keeping order.
            while index < self.len() {
                let mark = self.at(index);
                if mark > end || (mark == end && (!end_goes || removed.is_empty())) {
                    break;
                }
                if (mark > at && mark < end) || (mark == at && at_goes) || (mark == end && end_goes)
                {
                    self.gone[index] = true;
                    let back = mark.cast_signed() - at.cast_signed();
                    self.shift(index, -back);
                    self.shift(index + 1, back);
                }
                index += 1;
            }
            // A line at `at` that stays stays before what's put in, unless
            // that's lines of its own.
            let mut after = index;
            if removed.is_empty() && !inserted.ends_with('\n') {
                while after < self.len() && self.at(after) == at {
                    after += 1;
                }
            }
            self.shift(after, by);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of `text` marked if `marked` says so of them, as `:g`
    /// marks them.
    fn marks(text: &str, marked: &[bool]) -> Marked {
        let mut starts = Vec::new();
        let mut at = 0;
        for (line, &mark) in text.split_inclusive('\n').zip(marked) {
            if mark {
                starts.push(at);
            }
            at += line.len();
        }
        Marked::new(starts)
    }

    fn places(marked: &Marked) -> Vec<Option<usize>> {
        (0..marked.len()).map(|index| marked.get(index)).collect()
    }

    #[test]
    fn deleted_and_joined_lines_go_and_the_rest_move() {
        let mut text = Rope::from_str("a\nb\nc\nd");
        let mut marked = marks(&text.to_string(), &[true, true, true, true]);
        let mut edit = |marked: &mut Marked, edit: Edit| {
            marked.edited(&edit, &text);
            edit.apply(&mut text).unwrap();
        };
        // `dd` on "b".
        edit(&mut marked, Edit::replace(7, 2, "b\n", ""));
        assert_eq!(places(&marked), [Some(0), None, Some(2), Some(4)]);
        // `J` on "a", now joining "c".
        edit(&mut marked, Edit::replace(5, 1, "\n", " "));
        assert_eq!(places(&marked), [Some(0), None, None, Some(4)]);
        // `dd` on the last line, "d", takes the break before it.
        edit(&mut marked, Edit::replace(5, 3, "\nd", ""));
        assert_eq!(places(&marked), [Some(0), None, None, None]);
    }

    #[test]
    fn text_put_at_a_lines_start_goes_into_it_unless_its_lines() {
        let before = Rope::from_str("a\nb\n");
        let mut marked = marks(&before.to_string(), &[false, true]);
        // `I` typed on "b".
        marked.edited(&Edit::replace(4, 2, "", "x"), &before);
        assert_eq!(places(&marked), [Some(2)]);
        // A line copied above it with `:t`.
        let before = Rope::from_str("a\nxb\n");
        marked.edited(&Edit::replace(5, 2, "", "a\n"), &before);
        assert_eq!(places(&marked), [Some(4)]);
        // Part of its start replaced, as `:s/^x/y/` does.
        let before = Rope::from_str("a\na\nxb\n");
        marked.edited(&Edit::replace(7, 4, "x", "yy"), &before);
        assert_eq!(places(&marked), [Some(4)]);
        // Its start to partway into the next line taken away: one of the
        // two lines is left, as the first.
        let before = Rope::from_str("a\nbc\nd\n");
        let mut marked = marks(&before.to_string(), &[true, true, true]);
        marked.edited(&Edit::replace(7, 0, "a\nb", ""), &before);
        assert_eq!(places(&marked), [Some(0), None, Some(2)]);
        // A line's break replaced with another joins nothing.
        let before = Rope::from_str("a\nb\n");
        let mut marked = marks(&before.to_string(), &[true, true]);
        marked.edited(&Edit::replace(4, 1, "\n", "\n"), &before);
        assert_eq!(places(&marked), [Some(0), Some(2)]);
    }

    #[test]
    fn moving_lines_to_the_top_keeps_the_rest_in_order() {
        // `:g/^/m0`, a line at a time.
        let mut text = String::from("1\n2\n3\n4\n");
        let mut marked = marks(&text, &[true, true, true, true]);
        for index in 0..marked.len() {
            let at = marked.get(index).unwrap();
            let line_end = at + text[at..].find('\n').unwrap() + 1;
            let line = text[at..line_end].to_owned();
            let mut edit = Edit::new();
            edit.insert(&line).retain(at).delete(&line);
            edit.retain(text.len() - line_end);
            marked.edited(&edit, &Rope::from_str(&text));
            text = edit.apply_to(&text).unwrap();
        }
        assert_eq!(text, "4\n3\n2\n1\n");
    }
}
