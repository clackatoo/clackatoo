//! How lines should be indented, as a language's grammar says: for `=`,
//! and to put right the indent a new line was given at once.
//!
//! Only the daemon has the syntax tree the rules are read from, so a window
//! asks it, and moves what it's told along with edits made since, as it
//! moves pieces of code.

use crate::edit::{Edit, Op};

/// How one line should be indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineIndent {
    /// The byte the line starts at.
    pub line: usize,
    /// How many columns its indent should reach, or `None` if the rules
    /// say to keep it as it is, as they do inside a string.
    pub width: Option<u32>,
}

impl LineIndent {
    /// Moves it along with `edit`, made to the text it was found in, as
    /// [`line_start_through`] moves the start of a line. Returns whether
    /// the line is still there.
    #[must_use]
    pub fn map(&mut self, edit: &Edit) -> bool {
        match line_start_through(edit, self.line) {
            Some(line) => {
                self.line = line;
                true
            }
            None => false,
        }
    }
}

/// Where the line starting at byte `at` starts after `edit`, so it stays
/// the line with the same text, or `None` if it's gone.
///
/// Text put in at its start, as typing there does, goes on the line, after
/// the place; but where that text ends a line, as a line put above it
/// does, the place goes past that line. The line's gone if text taken out
/// holds its start, or starts at it and holds its line break, as `dd`
/// takes it. If only the line break before it is taken out, as `J` on the
/// line before does, the place goes to where that was, which only the
/// text can say is now in the middle of a line: the caller is to check.
#[must_use]
pub fn line_start_through(edit: &Edit, at: usize) -> Option<usize> {
    let (mut old, mut new) = (0, 0);
    // Where it goes, once the edit has reached it.
    let mut reached = None;
    for op in edit.ops() {
        if old == at && reached.is_none() {
            reached = Some(new);
        }
        match op {
            Op::Retain(len) => {
                if let Some(place) = reached {
                    return Some(place);
                }
                if at < old + len {
                    return Some(new + (at - old));
                }
                old += len;
                new += len;
            }
            Op::Insert(text) => {
                if reached.is_some()
                    && let Some(end) = text.rfind('\n')
                {
                    reached = Some(new + end + 1);
                }
                new += text.len();
            }
            Op::Delete(text) => {
                let end = old + text.len();
                let holds = old < at && at < end;
                let takes = old == at && text.contains('\n');
                if holds || takes {
                    return None;
                }
                old = end;
            }
        }
    }
    reached.or(Some(new))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where the start of the line at `at` of "ab\ncd\nef" goes when `text`
    /// is put in at byte `put`.
    fn after_insert(at: usize, put: usize, text: &str) -> Option<usize> {
        let edit = Edit::replace(8, put, "", text);
        line_start_through(&edit, at)
    }

    /// ...and when `removed`, at byte `from`, is taken out.
    fn after_delete(at: usize, from: usize, removed: &str) -> Option<usize> {
        let edit = Edit::replace(8, from, removed, "");
        line_start_through(&edit, at)
    }

    #[test]
    fn a_line_start_keeps_to_its_line() {
        // Typed at its start: the place stays, before what's typed.
        assert_eq!(after_insert(3, 3, "xy"), Some(3));
        // A line put above it, or text ending one: past it.
        assert_eq!(after_insert(3, 3, "new\n"), Some(7));
        assert_eq!(after_insert(3, 3, "a\nb"), Some(5));
        // Text put in before or after it moves it, or doesn't.
        assert_eq!(after_insert(3, 1, "xy\n"), Some(6));
        assert_eq!(after_insert(3, 4, "\n"), Some(3));
        // The first line, and the end of the text.
        assert_eq!(after_insert(0, 0, "top\n"), Some(4));
        assert_eq!(after_insert(8, 8, "z"), Some(8));
        // Text taken out before it, or the line above it whole, or its
        // first characters: it's still there.
        assert_eq!(after_delete(3, 0, "a"), Some(2));
        assert_eq!(after_delete(3, 0, "ab\n"), Some(0));
        assert_eq!(after_delete(3, 3, "c"), Some(3));
        // Its first characters taken out, and others put in their place.
        let replaced = Edit::replace(8, 3, "cd", "x\ny");
        assert_eq!(line_start_through(&replaced, 3), Some(5));
    }

    #[test]
    fn a_line_taken_out_or_joined_to_the_one_before_is_gone() {
        // `dd`: from its start, with its line break.
        assert_eq!(after_delete(3, 3, "cd\n"), None);
        // `J` on the line before: where the line break was, in the middle
        // of a line, as the text says.
        assert_eq!(after_delete(3, 2, "\n"), Some(2));
        // Text taken out across its start.
        assert_eq!(after_delete(3, 1, "b\nc"), None);
        // The line after the one taken out is still there.
        assert_eq!(after_delete(6, 3, "cd\n"), Some(3));
        let mut indent = LineIndent {
            line: 6,
            width: Some(4),
        };
        assert!(indent.map(&Edit::replace(8, 3, "cd", "x\ny")));
        assert_eq!(indent.line, 7);
        assert!(indent.map(&Edit::replace(9, 3, "x\ny\n", "")));
        // The lines above it taken out whole: it's still there.
        assert_eq!(indent.line, 3);
        // Taken out itself, with the line break after it.
        assert!(!indent.map(&Edit::replace(6, 3, "ef\n", "")));
    }
}
