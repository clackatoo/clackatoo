//! Marks: places in files to go back to, as Vim's are. `m` and a letter
//! sets one at the cursor: `a` to `z` in the file, each file having its
//! own, and `A` to `Z` anywhere, remembering their file. `` ` `` and the
//! letter go back to it, and `'` to its line. The window sets `.` itself,
//! at the last change made to each file.
//!
//! Each file also keeps its last selection, as Vim keeps one for each
//! buffer, which `gv` selects again: `<` and `>` are its first and last
//! characters, which `:'<,'>` acts on. While a selection is being made,
//! it's the last; the one before it is kept too, for `gv` in visual mode
//! to swap with (see [`super::visual`]).
//!
//! A mark moves with the text, as the jump list's places do (see
//! [`FilePlace`]): edits made above it, by anyone, take it down with them,
//! and one on a character put back in other words, as when its case is
//! changed, stays on it. Marks are the window's own, kept until it closes.

use std::borrow::Borrow;

use clackatoo_text::{Bias, Edit, Rope};

use crate::pane::Selection;

/// A place in a file.
///
/// While the window has the file's text, the place is a byte of it, which
/// edits move, and the line and column follow. Once no pane of the window
/// shows the file, the window hears of no edits to it, and the place keeps
/// its line and column, which are kept within the text when the file comes
/// back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilePlace {
    /// The file's path, made absolute, as the daemon gives it.
    pub(crate) path: String,
    /// The line, from 0, and the column, in bytes from its start.
    pub(crate) line: u64,
    pub(crate) col: usize,
    /// The byte in the window's copy of the text, while there is one.
    byte: usize,
}

impl FilePlace {
    /// The place at `byte` of `text`, the file at `path`.
    pub(crate) fn at(path: &str, text: &Rope, byte: usize) -> Self {
        let mut place = Self {
            path: path.to_owned(),
            line: 0,
            col: 0,
            byte,
        };
        place.find_line(text);
        place
    }

    /// The file or directory at `from` has moved to `to`: if the place is
    /// in it, or anything in it, it goes with it.
    pub(crate) fn moved(&mut self, from: &str, to: &str) {
        let rest =
            (self.path.strip_prefix(from)).filter(|rest| rest.is_empty() || rest.starts_with('/'));
        if let Some(rest) = rest {
            self.path = format!("{to}{rest}");
        }
    }

    /// The place on `line`, `col` bytes into it, of the file at `path`,
    /// kept within its `text` if the window has it. If not, its byte is
    /// worked out when the file's text comes.
    pub(crate) fn at_line(path: &str, line: u64, col: usize, text: Option<&Rope>) -> Self {
        let mut place = Self {
            path: path.to_owned(),
            line,
            col,
            byte: 0,
        };
        if let Some(text) = text {
            place.find_byte(text);
        }
        place
    }

    /// Moves it through `edits`, made to its file in turn, which leave its
    /// text as `text`. Text put in where it is goes before it, so a line
    /// opened above its line takes it down with its line; on a character
    /// replaced, it stays on what's put in its place.
    pub(crate) fn edited(&mut self, edits: &[impl Borrow<Edit>], text: &Rope) {
        for edit in edits {
            self.follow(edit.borrow(), Bias::After);
        }
        self.find_line(text);
    }

    /// Moves its byte through `edit`, text put in where it is going after
    /// it if `bias` is [`Bias::Before`]. Its line and column are left for
    /// [`find_line`](Self::find_line).
    fn follow(&mut self, edit: &Edit, bias: Bias) {
        self.byte = edit.map_keeping(self.byte, bias);
    }

    /// Works out the line and column from the byte.
    fn find_line(&mut self, text: &Rope) {
        self.byte = self.byte.min(text.len_bytes());
        let line = text.byte_to_line(self.byte);
        self.line = line as u64;
        self.col = self.byte - text.line_to_byte(line);
    }

    /// Works out the byte from the line and column, keeping them within
    /// `text` and on a character's start, as when the file's text is new.
    pub(crate) fn find_byte(&mut self, text: &Rope) {
        // The empty line after a final line break isn't one of the file's.
        let len = text.len_bytes();
        let ends_with_break = len > 0 && text.byte(len - 1) == b'\n';
        let last = text.len_lines() - 1 - usize::from(ends_with_break);
        let line = usize::try_from(self.line).map_or(last, |line| line.min(last));
        let start = text.line_to_byte(line);
        let end = if line + 1 < text.len_lines() {
            // Before the line's break.
            text.line_to_byte(line + 1) - 1
        } else {
            text.len_bytes()
        };
        let byte = (start + self.col).min(end);
        self.byte = text.char_to_byte(text.byte_to_char(byte));
        self.find_line(text);
    }
}

/// Whether `mark` is one of a file's own: `a` to `z`, `.`, or `'`, where
/// the last jump from the file left. The others, `A` to `Z`, are the
/// window's, in whichever file they're in.
fn local(mark: char) -> bool {
    mark.is_ascii_lowercase() || matches!(mark, '.' | '\'' | '<' | '>')
}

/// How a selection was made, besides where it is: as `gv` makes it again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) kind: Selection,
    /// The cursor was at its start, not its end.
    pub(crate) backward: bool,
    /// The cursor kept to line ends, as after `$`.
    pub(crate) to_end: bool,
}

/// A file's selection, kept for `gv` and `'<,'>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    /// Its first character, where `<` is...
    start: FilePlace,
    /// ...the byte after its last...
    end: FilePlace,
    /// ...and its last character, where `>` is.
    last: FilePlace,
    pub(crate) shape: Shape,
}

impl Selected {
    /// The selection of `path` from byte `start` to just before byte
    /// `end` of `text`.
    fn new(path: &str, text: &Rope, (start, end): (usize, usize), shape: Shape) -> Self {
        let place = |byte| FilePlace::at(path, text, byte);
        let mut selected = Self {
            start: place(start),
            end: place(end),
            last: place(start),
            shape,
        };
        selected.settle(text);
        selected
    }

    /// The bytes of its first and last characters.
    pub(crate) fn ends(&self) -> (usize, usize) {
        (self.start.byte, self.last.byte)
    }

    /// Puts it at bytes `start` to `end` of `text`, in place.
    fn place(&mut self, text: &Rope, (start, end): (usize, usize), shape: Shape) {
        self.start.byte = start;
        self.end.byte = end;
        self.shape = shape;
        self.settle(text);
    }

    /// Moves it through `edits`, made to its file in turn, which leave its
    /// text as `text`. Text put in where it starts is in it, and where it
    /// ends isn't, as Vim's columns would have it; but once all of it has
    /// been deleted, text put back there is, as an undo puts it back.
    fn edited(&mut self, edits: &[impl Borrow<Edit>], text: &Rope) {
        for edit in edits {
            let edit = edit.borrow();
            let gone = self.start.byte == self.end.byte;
            self.start.follow(edit, Bias::Before);
            let bias = if gone { Bias::After } else { Bias::Before };
            self.end.follow(edit, bias);
            self.end.byte = self.end.byte.max(self.start.byte);
        }
        self.settle(text);
    }

    /// The text of its file is new: it goes to its lines and columns in
    /// it, as marks do.
    fn replaced(&mut self, text: &Rope) {
        let gone = self.start.byte == self.end.byte;
        self.start.find_byte(text);
        self.last.find_byte(text);
        self.end.byte = if gone {
            self.start.byte
        } else {
            next_char(text, self.last.byte)
        };
        self.settle(text);
    }

    /// Works out its lines and columns, and its last character, from its
    /// bytes.
    fn settle(&mut self, text: &Rope) {
        let len = text.len_bytes();
        self.start.byte = self.start.byte.min(len);
        self.end.byte = self.end.byte.clamp(self.start.byte, len);
        self.last.byte = if self.end.byte > self.start.byte {
            text.char_to_byte(text.byte_to_char(self.end.byte) - 1)
                .max(self.start.byte)
        } else {
            self.start.byte
        };
        for place in [&mut self.start, &mut self.end, &mut self.last] {
            place.find_line(text);
        }
    }

    fn moved(&mut self, from: &str, to: &str) {
        for place in [&mut self.start, &mut self.end, &mut self.last] {
            place.moved(from, to);
        }
    }

    fn path(&self) -> &str {
        &self.start.path
    }
}

/// The byte after the character at `byte` of `text`, or its end.
fn next_char(text: &Rope, byte: usize) -> usize {
    let len = text.len_bytes();
    if byte >= len {
        return len;
    }
    text.char_to_byte(text.byte_to_char(byte) + 1)
}

/// A file's last selection, and the one before it.
#[derive(Debug)]
struct Selections {
    last: Selected,
    before: Option<Selected>,
}

/// The marks set, in every file.
#[derive(Debug, Default)]
pub(crate) struct Marks {
    /// Each mark, and where it is. A file's own marks are kept for each
    /// file, by its path; there are few enough that a list does.
    list: Vec<(char, FilePlace)>,
    /// Each file's last selection, where `<` and `>` are, and the one
    /// before it.
    selections: Vec<Selections>,
}

impl Marks {
    /// Sets `mark` at `place`, in place of where it was: in `place`'s file,
    /// for a file's own mark, or anywhere.
    pub(crate) fn set(&mut self, mark: char, place: FilePlace) {
        let local = local(mark);
        self.list
            .retain(|(set, at)| *set != mark || (local && at.path != place.path));
        self.list.push((mark, place));
    }

    /// Where `mark` is, for the file at `path`: its own `a` to `z`, or
    /// `A` to `Z` in any file.
    pub(crate) fn get(&self, mark: char, path: &str) -> Option<&FilePlace> {
        match mark {
            '<' => return self.selected(path).map(|selected| &selected.start),
            '>' => return self.selected(path).map(|selected| &selected.last),
            _ => {}
        }
        let local = local(mark);
        (self.list.iter())
            .find(|(set, at)| *set == mark && (!local || at.path == path))
            .map(|(_, at)| at)
    }

    /// The marks the file at `path` can go to, its own and then the
    /// others, each in order.
    pub(crate) fn of(&self, path: &str) -> Vec<(char, &FilePlace)> {
        let mut marks: Vec<(char, &FilePlace)> = (self.list.iter())
            .filter(|(mark, at)| !local(*mark) || at.path == path)
            .map(|(mark, at)| (*mark, at))
            .collect();
        if let Some(selected) = self.selected(path) {
            marks.extend([('<', &selected.start), ('>', &selected.last)]);
        }
        // As Vim lists them: `'`, `a` to `z`, `.`, then `A` to `Z`.
        marks.sort_by_key(|&(mark, _)| (mark != '\'', !local(mark), mark == '.', mark));
        marks
    }

    /// Moves the marks in the file at `path` through `edits`, made to it in
    /// turn, which leave its text as `text`.
    pub(crate) fn edited(&mut self, path: &str, edits: &[impl Borrow<Edit>], text: &Rope) {
        if edits.is_empty() {
            return;
        }
        for (_, place) in self.list.iter_mut().filter(|(_, at)| at.path == path) {
            place.edited(edits, text);
        }
        self.edited_selections(path, edits, text);
    }

    /// Moves the selections kept for the file at `path` through `edits`,
    /// as [`edited`](Self::edited) does, but no other marks: for a text
    /// that's no file, as a folder's listing is, which keeps them anyway.
    pub(crate) fn edited_selections(
        &mut self,
        path: &str,
        edits: &[impl Borrow<Edit>],
        text: &Rope,
    ) {
        for kept in self
            .selections
            .iter_mut()
            .filter(|kept| kept.last.path() == path)
        {
            kept.last.edited(edits, text);
            if let Some(before) = &mut kept.before {
                before.edited(edits, text);
            }
        }
    }

    /// The last selection in the file at `path`, if it's had one.
    pub(crate) fn selected(&self, path: &str) -> Option<&Selected> {
        (self.selections.iter())
            .find(|kept| kept.last.path() == path)
            .map(|kept| &kept.last)
    }

    /// Notes the selection in the file at `path`, bytes `start` to just
    /// before `end` of its `text`, as its last. If it's `anew`, as visual
    /// mode has just begun, the last is kept as the one before it.
    pub(crate) fn select(
        &mut self,
        path: &str,
        text: &Rope,
        bytes: (usize, usize),
        shape: Shape,
        anew: bool,
    ) {
        match self
            .selections
            .iter_mut()
            .find(|kept| kept.last.path() == path)
        {
            Some(kept) => {
                if anew {
                    kept.before = Some(kept.last.clone());
                }
                kept.last.place(text, bytes, shape);
            }
            None => self.selections.push(Selections {
                last: Selected::new(path, text, bytes, shape),
                before: None,
            }),
        }
    }

    /// Swaps the last selection in the file at `path` with the one before
    /// it, as `gv` does in visual mode: the one it selects, if there is
    /// one.
    pub(crate) fn swap(&mut self, path: &str) -> Option<Selected> {
        let kept = self
            .selections
            .iter_mut()
            .find(|kept| kept.last.path() == path)?;
        let before = kept.before.take()?;
        kept.before = Some(std::mem::replace(&mut kept.last, before.clone()));
        Some(before)
    }

    /// Forgets the selections kept for `path`, as when a folder's listing
    /// is made afresh.
    pub(crate) fn forget_selections(&mut self, path: &str) {
        self.selections.retain(|kept| kept.last.path() != path);
    }

    /// The file or directory at `from` has moved to `to`: marks in it, or
    /// anything in it, go with it.
    pub(crate) fn moved(&mut self, from: &str, to: &str) {
        for (_, place) in &mut self.list {
            place.moved(from, to);
        }
        for kept in &mut self.selections {
            kept.last.moved(from, to);
            if let Some(before) = &mut kept.before {
                before.moved(from, to);
            }
        }
    }

    /// The text of the file at `path` is new, as when it's opened again:
    /// its marks go to their lines and columns in it.
    pub(crate) fn replaced(&mut self, path: &str, text: &Rope) {
        for (_, place) in self.list.iter_mut().filter(|(_, at)| at.path == path) {
            place.find_byte(text);
        }
        for kept in self
            .selections
            .iter_mut()
            .filter(|kept| kept.last.path() == path)
        {
            kept.last.replaced(text);
            if let Some(before) = &mut kept.before {
                before.replaced(text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_own_marks_are_its_and_the_others_anyones() {
        let text = Rope::from_str("one\ntwo\n");
        let mut marks = Marks::default();
        marks.set('a', FilePlace::at("/x", &text, 1));
        marks.set('a', FilePlace::at("/y", &text, 5));
        marks.set('B', FilePlace::at("/x", &text, 4));
        assert_eq!(marks.get('a', "/x").map(|at| at.col), Some(1));
        assert_eq!(marks.get('a', "/y").map(|at| at.line), Some(1));
        assert_eq!(marks.get('B', "/y").map(|at| at.path.as_str()), Some("/x"));
        // Set again, `B` moves to another file.
        marks.set('B', FilePlace::at("/y", &text, 0));
        assert_eq!(marks.get('B', "/x").map(|at| at.path.as_str()), Some("/y"));
        let names = |path| -> String { marks.of(path).iter().map(|(mark, _)| *mark).collect() };
        assert_eq!(names("/x"), "aB");
        assert_eq!(marks.get('z', "/x"), None);
    }

    #[test]
    fn marks_follow_edits_and_are_kept_within_new_text() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        let mut marks = Marks::default();
        marks.set('a', FilePlace::at("/f", &text, 10));
        let place = |marks: &Marks| {
            let at = marks.get('a', "/f").unwrap();
            (at.line, at.col)
        };
        assert_eq!(place(&marks), (2, 2));
        let edit = Edit::replace(14, 0, "", "zero\n");
        let text = Rope::from_str("zero\none\ntwo\nthree\n");
        marks.edited("/f", std::slice::from_ref(&edit), &text);
        marks.edited("/other", std::slice::from_ref(&edit), &text);
        assert_eq!(place(&marks), (3, 2));
        marks.replaced("/f", &Rope::from_str("zero\nx\n"));
        assert_eq!(place(&marks), (1, 1));
    }
}
