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
//! one on a character put back in other words, as when its case is
//! changed, stays on it, and one on lines `:m` moves goes with them. Marks
//! are the window's own, kept until it closes.
//!
//! The last selection's `<` and `>` are kept as Vim keeps them instead: a
//! line and a column each, which edits within the line leave as they are,
//! and an undo puts back as they were (see [`Selected`]), so `gv` selects
//! what Vim's would.
//!
//! A file is its path on the machine it's on: the daemon's own, or another
//! machine whose workspaces `:connect` brought in, where the same path may
//! well be another file. Marks and places are kept by both.

use std::borrow::Borrow;

use clackatoo_protocol::MachineId;
use clackatoo_text::{Bias, Edit, Moved, Rope};

use crate::vim::Selection;

/// A file, as places are kept in it: its path, on the machine it's on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct File<'a> {
    /// The machine, if it's another than the daemon's.
    pub(crate) machine: Option<MachineId>,
    /// Its path there, made absolute, as the daemon gives it.
    pub(crate) path: &'a str,
}

#[cfg(test)]
impl<'a> File<'a> {
    /// The file at `path` on the daemon's own machine.
    pub(crate) fn here(path: &'a str) -> Self {
        Self {
            machine: None,
            path,
        }
    }
}

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
    /// The machine the file is on, if it's another than the daemon's.
    pub(crate) machine: Option<MachineId>,
    /// The line, from 0, and the column, in bytes from its start.
    pub(crate) line: u64,
    pub(crate) col: usize,
    /// The byte in the window's copy of the text, while there is one.
    byte: usize,
}

impl FilePlace {
    /// The place at `byte` of `text`, of `file`.
    pub(crate) fn at(file: File<'_>, text: &Rope, byte: usize) -> Self {
        let mut place = Self {
            path: file.path.to_owned(),
            machine: file.machine,
            line: 0,
            col: 0,
            byte,
        };
        place.find_line(text);
        place
    }

    /// The file it's in.
    pub(crate) fn file(&self) -> File<'_> {
        File {
            machine: self.machine,
            path: &self.path,
        }
    }

    /// Whether it's in `file`.
    pub(crate) fn is_in(&self, file: File<'_>) -> bool {
        self.file() == file
    }

    /// The file or directory at `from` on `machine`, the daemon's own if
    /// `None`, has moved to `to`: if the place is in it, or anything in
    /// it, it goes with it.
    pub(crate) fn moved(&mut self, machine: Option<MachineId>, from: &str, to: &str) {
        if machine != self.machine {
            return;
        }
        let rest =
            (self.path.strip_prefix(from)).filter(|rest| rest.is_empty() || rest.starts_with('/'));
        if let Some(rest) = rest {
            self.path = format!("{to}{rest}");
        }
    }

    /// The place on `line`, `col` bytes into it, of `file`, kept within
    /// its `text` if the window has it. If not, its byte is worked out
    /// when the file's text comes.
    pub(crate) fn at_line(file: File<'_>, line: u64, col: usize, text: Option<&Rope>) -> Self {
        let mut place = Self {
            path: file.path.to_owned(),
            machine: file.machine,
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
    /// replaced, it stays on what's put in its place; and in text moved,
    /// it goes with it.
    pub(crate) fn edited(&mut self, edits: &[Passing<'_>], text: &Rope) {
        for edit in edits {
            self.follow(edit, Bias::After);
        }
        self.find_line(text);
    }

    /// Moves its byte through `edit`, text put in where it is going after
    /// it if `bias` is [`Bias::Before`]. Its line and column are left for
    /// [`find_line`](Self::find_line).
    fn follow(&mut self, edit: &Passing<'_>, bias: Bias) {
        self.byte = (edit.moved)
            .and_then(|moved| moved.place(self.byte))
            .unwrap_or_else(|| edit.edit.map_keeping(self.byte, bias));
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

/// An edit, as places are moved through it, with the lines it moves whole
/// if it moves any, as `:m` moves them: worked out once for every place.
pub(crate) struct Passing<'a> {
    edit: &'a Edit,
    moved: Option<Moved>,
}

/// `edits`, made in turn, which leave the text as `text`, as places are
/// moved through them.
pub(crate) fn passing<'a>(edits: &'a [impl Borrow<Edit>], text: &Rope) -> Vec<Passing<'a>> {
    let mut passing: Vec<Passing<'a>> = (edits.iter())
        .map(|edit| {
            let edit = edit.borrow();
            Passing {
                edit,
                moved: edit.moved(),
            }
        })
        .collect();
    // Whether what one moves is whole lines is seen in the text it made,
    // worked back to from the last's: only as far as the first that might
    // move any, as few do.
    let Some(first) = passing.iter().position(|edit| edit.moved.is_some()) else {
        return passing;
    };
    let mut after = text.clone();
    for index in (first..passing.len()).rev() {
        let edit = &mut passing[index];
        edit.moved = edit.moved.filter(|moved| moved.is_lines(&after));
        if index > first && edit.edit.invert().apply(&mut after).is_err() {
            // Not the text they made: none of them is taken for a move.
            for edit in &mut passing[first..index] {
                edit.moved = None;
            }
            break;
        }
    }
    passing
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

/// A file's selection, kept for `gv` and `'<,'>`: `'<` and `'>`, its first
/// and last ends, each a line and a column in bytes, as Vim keeps them,
/// which may be past the line's end, as Vim's may.
///
/// Edits move them as Vim's `mark_adjust()` and `mark_col_adjust()` move
/// marks, not as the file's other marks, which keep to their characters:
/// lines put in or taken away above take an end with them; a line taken
/// away leaves it at that line number, its column as it was; a line
/// joined onto the one before takes it along, its column moved with the
/// line's text, less the indent `J` takes away; lines `:m` moves take it
/// with them; and an edit within its line leaves it at its column. An undo
/// or a redo that makes a text again puts back the selection it had then
/// (see [`Selections`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selected {
    /// Its first end, where `<` is...
    start: FilePlace,
    /// ...and its last, where `>` is.
    last: FilePlace,
    /// Where `<` and `>` are, as marks to go to: its ends, or by lines,
    /// the first line's start and the last's end, as Vim's.
    marked: [FilePlace; 2],
    pub(crate) shape: Shape,
}

impl Selected {
    /// The selection of `file` from byte `first` to byte `last` of `text`,
    /// both in it.
    fn new(file: File<'_>, text: &Rope, (first, last): (usize, usize), shape: Shape) -> Self {
        let place = |byte| FilePlace::at(file, text, byte);
        let mut selected = Self {
            start: place(first),
            last: place(last),
            marked: [place(first), place(last)],
            shape,
        };
        selected.settle(text);
        selected
    }

    /// Its first and last ends: each a line and a column in bytes, which
    /// may be past the line's end, or the text's last line.
    pub(crate) fn ends(&self) -> [(u64, usize); 2] {
        [
            (self.start.line, self.start.col),
            (self.last.line, self.last.col),
        ]
    }

    /// Puts it at bytes `first` to `last` of `text`, in place.
    fn place(&mut self, text: &Rope, (first, last): (usize, usize), shape: Shape) {
        self.start.byte = first;
        self.last.byte = last;
        self.start.find_line(text);
        self.last.find_line(text);
        self.shape = shape;
        self.settle(text);
    }

    /// Moves it through `edits`, made to its file in turn, as `texts` says
    /// what each was made to and made, `text` being the last's.
    fn edited(&mut self, edits: &[Passing<'_>], texts: &[(Rope, Rope)]) {
        for (edit, (before, after)) in edits.iter().zip(texts) {
            end_edited(&mut self.start, edit, (before, after));
            end_edited(&mut self.last, edit, (before, after));
        }
        // Lines moved past one end from inside it cross its ends over,
        // which are taken in order, as Vim takes `'<` and `'>`: the
        // cursor's end is the other.
        if (self.start.line, self.start.col) > (self.last.line, self.last.col) {
            std::mem::swap(&mut self.start, &mut self.last);
            self.shape.backward = !self.shape.backward;
        }
    }

    /// Works out its ends' bytes in `text`, as far as they go in it, and
    /// where `<` and `>` are.
    fn settle(&mut self, text: &Rope) {
        for end in [&mut self.start, &mut self.last] {
            end.byte = byte_of(text, end.line, end.col);
        }
        self.marked = [self.start.clone(), self.last.clone()];
        if self.shape.kind == Selection::Lines {
            let [first, last] = &mut self.marked;
            first.col = 0;
            last.col = usize::MAX;
            for end in &mut self.marked {
                end.byte = byte_of(text, end.line, end.col);
                end.col = end.byte - byte_of(text, end.line, 0);
            }
        }
    }

    fn moved(&mut self, machine: Option<MachineId>, from: &str, to: &str) {
        for end in [&mut self.start, &mut self.last]
            .into_iter()
            .chain(&mut self.marked)
        {
            end.moved(machine, from, to);
        }
    }
}

/// The byte `col` bytes into line `line` of `text`, as far as the line,
/// and the text, go, on a character's start.
fn byte_of(text: &Rope, line: u64, col: usize) -> usize {
    let lines = text.len_lines();
    let Some(line) = usize::try_from(line).ok().filter(|&line| line < lines) else {
        return text.len_bytes();
    };
    let start = text.line_to_byte(line);
    let row = text.line(line);
    let len = row.len_bytes() - usize::from(row.bytes().last() == Some(b'\n'));
    text.char_to_byte(text.byte_to_char(start + col.min(len)))
}

/// Moves `end`, an end of a selection, through `edit`, made to `before`
/// and making `after`, as [`Selected`] says.
fn end_edited(end: &mut FilePlace, edit: &Passing<'_>, (before, after): (&Rope, &Rope)) {
    // In lines `:m` moves, with them.
    let line = usize::try_from(end.line)
        .ok()
        .filter(|&line| line < before.len_lines());
    if let Some(line) = line {
        let start = before.line_to_byte(line);
        if let Some(moved) = edit.moved.and_then(|moved| moved.place(start)) {
            end.line = after.byte_to_line(moved.min(after.len_bytes())) as u64;
            return;
        }
    }
    // Each change, the last first, so the lines of those before it are as
    // they were.
    let changes: Vec<_> = edit.edit.changes().collect();
    for &(at, removed, put) in changes.iter().rev() {
        end_changed(end, (at, removed, put), before);
    }
}

/// Moves `end` through one change of an edit to `before`: `removed` taken
/// away at byte `at` and `put` put in its place.
fn end_changed(end: &mut FilePlace, (at, removed, put): (usize, &str, &str), before: &Rope) {
    let len = before.len_bytes();
    let point = |byte: usize| {
        let byte = byte.min(len);
        let line = before.byte_to_line(byte);
        (line as u64, byte - before.line_to_byte(line))
    };
    let ((first, at_col), (last, end_col)) = (point(at), point(at + removed.len()));
    let breaks = put.bytes().filter(|&byte| byte == b'\n').count() as u64;
    // Where what follows the change starts on its line after it.
    let rest = match put.rfind('\n') {
        Some(newline) => put.len() - newline - 1,
        None => at_col + put.len(),
    };
    let (line, col) = (end.line, end.col);
    if line < first {
        return;
    }
    if line > last {
        end.line = line + breaks - (last - first);
        return;
    }
    // Whole lines put in at its line's start, as `p`, `P` and `O` put
    // them, take it down with its line. A line broken there, as Enter
    // breaks it, starts with the break, and leaves it on its line, as
    // Vim's `open_line()` does; empty lines the editor puts start with the
    // break at the end of the line before (see `Editor::put_register`). At
    // the text's very start, before a line that isn't empty, a break alone
    // is taken as a line put above, as `O` puts it there; after an empty
    // first line, as `o` and `p` put one, it's that line's end.
    let first_empty = before.bytes().next().is_none_or(|byte| byte == b'\n');
    let above = !put.starts_with('\n') || (at == 0 && put == "\n" && !first_empty);
    if at_col == 0 && removed.is_empty() && put.ends_with('\n') && above {
        end.line = line + breaks;
        return;
    }
    // Within its line, or on the line a change starts on, from where it
    // is on, or taken away whole from its start, or on lines put back as
    // many as were taken, as recasing them does: as it is, as Vim's.
    let taken = removed.bytes().filter(|&byte| byte == b'\n').count() as u64;
    if line == first || taken == breaks {
        return;
    }
    // Its line joined on: as `J` joins it, the indent taken away and any
    // space put in its place, the column with the text, back to the space
    // if it was in the indent; or as `d` joins it, the column on from
    // where its line now goes on, as Vim's `do_join()` after
    // `op_delete()` puts it, or a line taken away between, left at the
    // first line's, its column on as far.
    let joins = line == last
        && removed
            .strip_prefix('\n')
            .is_some_and(|indent| indent.bytes().all(|byte| byte == b' ' || byte == b'\t'))
        && put.bytes().all(|byte| byte == b' ');
    end.line = first + breaks;
    end.col = if joins {
        if col + put.len() < end_col {
            at_col
        } else {
            col + put.len() + at_col - end_col
        }
    } else {
        col + rest
    };
}

/// How many of a file's texts [`Selections`] remembers the last selection
/// in.
const MOST_SEEN: usize = 100;

/// A file's last selection, and the one before it; and the last selection
/// in each of the file's recent texts, so an undo or a redo that makes one
/// of them again puts back the selection it had, as Vim's undo puts back
/// the one kept with its step (`uh_visual`). Kept by the text itself, not
/// by the steps, whose undo is the daemon's, it can't fall out of step:
/// an undo that makes a text not seen, as after another's edit, moves the
/// selection as any edit does.
#[derive(Debug)]
struct Selections {
    path: String,
    machine: Option<MachineId>,
    last: Option<Selected>,
    before: Option<Selected>,
    seen: std::collections::VecDeque<(Rope, Option<Selected>)>,
}

impl Selections {
    fn is_of(&self, file: File<'_>) -> bool {
        self.machine == file.machine && self.path == file.path
    }

    /// Moves them through `edits`, made in turn to the file, whose text
    /// each made and was made to `texts` says, the last making `text`: an
    /// undo's or a redo's, if `reversed`.
    fn edited(
        &mut self,
        edits: &[Passing<'_>],
        texts: &[(Rope, Rope)],
        text: &Rope,
        reversed: bool,
    ) {
        // As each edit starts, as Vim keeps it with the step it starts; so
        // a redo finds it as its undo started.
        let remembered = texts
            .first()
            .map(|(before, _)| self.remember(before))
            .is_some();
        for selected in [&mut self.last, &mut self.before].into_iter().flatten() {
            selected.edited(edits, texts);
            selected.settle(text);
        }
        // Not as this one started, which may be the same text, as after
        // `:m` swaps two lines alike.
        if reversed {
            let kept = (self.seen.iter().rev().skip(usize::from(remembered)))
                .find(|(seen, _)| seen.len_bytes() == text.len_bytes() && seen == text)
                .and_then(|(_, selected)| selected.clone());
            if let Some(kept) = kept {
                self.last = Some(kept);
            }
        }
    }

    /// Notes the last selection as it is in `text`.
    fn remember(&mut self, text: &Rope) {
        if self.seen.len() >= MOST_SEEN {
            self.seen.pop_front();
        }
        self.seen.push_back((text.clone(), self.last.clone()));
    }
}

/// What each of `edits`, made in turn, was made to and made, the last
/// making `text`, worked back to by undoing them: `text` for each if
/// they didn't make it.
fn texts_of(edits: &[Passing<'_>], text: &Rope) -> Vec<(Rope, Rope)> {
    let mut texts = Vec::with_capacity(edits.len());
    let mut after = text.clone();
    for edit in edits.iter().rev() {
        let mut before = after.clone();
        if edit.edit.invert().apply(&mut before).is_err() {
            return vec![(text.clone(), text.clone()); edits.len()];
        }
        texts.push((before.clone(), after));
        after = before;
    }
    texts.reverse();
    texts
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
    /// for a file's own mark, or anywhere. A buffer with no file yet keeps
    /// only its own: one for anywhere would have nothing to open.
    pub(crate) fn set(&mut self, mark: char, place: FilePlace) {
        let local = local(mark);
        if !local && place.path.is_empty() {
            return;
        }
        self.list
            .retain(|(set, at)| *set != mark || (local && !at.is_in(place.file())));
        self.list.push((mark, place));
    }

    /// Where `mark` is, for `file`: its own `a` to `z`, or `A` to `Z` in
    /// any file.
    pub(crate) fn get(&self, mark: char, file: File<'_>) -> Option<&FilePlace> {
        match mark {
            '<' => return self.selected(file).map(|selected| &selected.marked[0]),
            '>' => return self.selected(file).map(|selected| &selected.marked[1]),
            _ => {}
        }
        let local = local(mark);
        (self.list.iter())
            .find(|(set, at)| *set == mark && (!local || at.is_in(file)))
            .map(|(_, at)| at)
    }

    /// The marks `file` can go to, its own and then the others, each in
    /// order.
    pub(crate) fn of(&self, file: File<'_>) -> Vec<(char, &FilePlace)> {
        let mut marks: Vec<(char, &FilePlace)> = (self.list.iter())
            .filter(|(mark, at)| !local(*mark) || at.is_in(file))
            .map(|(mark, at)| (*mark, at))
            .collect();
        if let Some(selected) = self.selected(file) {
            marks.extend([('<', &selected.marked[0]), ('>', &selected.marked[1])]);
        }
        // As Vim lists them: `'`, `a` to `z`, `.`, then `A` to `Z`.
        marks.sort_by_key(|&(mark, _)| (mark != '\'', !local(mark), mark == '.', mark));
        marks
    }

    /// Moves the marks in `file` through `edits`, made to it in turn,
    /// which leave its text as `text`: an undo's or a redo's, if
    /// `reversed`, which can put back a selection (see [`Selections`]).
    pub(crate) fn edited(
        &mut self,
        file: File<'_>,
        edits: &[impl Borrow<Edit>],
        text: &Rope,
        reversed: bool,
    ) {
        if edits.is_empty() {
            return;
        }
        let edits = passing(edits, text);
        for (_, place) in self.list.iter_mut().filter(|(_, at)| at.is_in(file)) {
            place.edited(&edits, text);
        }
        self.selections_edited(file, &edits, text, reversed);
    }

    /// Moves the selections kept for `file` through `edits`,
    /// as [`edited`](Self::edited) does, but no other marks: for a text
    /// that's no file, as a folder's listing is, which keeps them anyway.
    pub(crate) fn edited_selections(
        &mut self,
        file: File<'_>,
        edits: &[impl Borrow<Edit>],
        text: &Rope,
        reversed: bool,
    ) {
        if edits.is_empty() {
            return;
        }
        self.selections_edited(file, &passing(edits, text), text, reversed);
    }

    fn selections_edited(
        &mut self,
        file: File<'_>,
        edits: &[Passing<'_>],
        text: &Rope,
        reversed: bool,
    ) {
        let Some(kept) = self.selections.iter_mut().find(|kept| kept.is_of(file)) else {
            return;
        };
        let texts = texts_of(edits, text);
        kept.edited(edits, &texts, text, reversed);
    }

    /// The selections kept for `file`, made if there are none yet.
    fn selections_of(&mut self, file: File<'_>) -> &mut Selections {
        let index = if let Some(index) = self.selections.iter().position(|kept| kept.is_of(file)) {
            index
        } else {
            self.selections.push(Selections {
                path: file.path.to_owned(),
                machine: file.machine,
                last: None,
                before: None,
                seen: std::collections::VecDeque::new(),
            });
            self.selections.len() - 1
        };
        &mut self.selections[index]
    }

    /// The last selection in `file`, if it's had one.
    pub(crate) fn selected(&self, file: File<'_>) -> Option<&Selected> {
        (self.selections.iter())
            .find(|kept| kept.is_of(file))
            .and_then(|kept| kept.last.as_ref())
    }

    /// Notes the selection in `file`, from byte `first` to byte `last` of
    /// its `text`, both in it, as its last. If it's `anew`, as visual mode
    /// has just begun, the last is kept as the one before it.
    pub(crate) fn select(
        &mut self,
        file: File<'_>,
        text: &Rope,
        bytes: (usize, usize),
        shape: Shape,
        anew: bool,
    ) {
        let kept = self.selections_of(file);
        match &mut kept.last {
            Some(last) => {
                if anew {
                    kept.before = Some(last.clone());
                }
                last.place(text, bytes, shape);
            }
            None => kept.last = Some(Selected::new(file, text, bytes, shape)),
        }
    }

    /// Swaps the last selection in `file` with the one before it, as `gv`
    /// does in visual mode: the one it selects, if there is one.
    pub(crate) fn swap(&mut self, file: File<'_>) -> Option<Selected> {
        let kept = self.selections.iter_mut().find(|kept| kept.is_of(file))?;
        let before = kept.before.take()?;
        kept.before = kept.last.replace(before.clone());
        Some(before)
    }

    /// Forgets the selections kept for `file`, as when a folder's listing
    /// is made afresh.
    pub(crate) fn forget_selections(&mut self, file: File<'_>) {
        self.selections.retain(|kept| !kept.is_of(file));
    }

    /// The file or directory at `from` on `machine`, the daemon's own if
    /// `None`, has moved to `to`: marks in it, or anything in it, go with
    /// it.
    pub(crate) fn moved(&mut self, machine: Option<MachineId>, from: &str, to: &str) {
        for (_, place) in &mut self.list {
            place.moved(machine, from, to);
        }
        for kept in &mut self.selections {
            let mut place = FilePlace::at_line(
                File {
                    machine: kept.machine,
                    path: &kept.path,
                },
                0,
                0,
                None,
            );
            place.moved(machine, from, to);
            kept.path = place.path;
            for selected in [&mut kept.last, &mut kept.before].into_iter().flatten() {
                selected.moved(machine, from, to);
            }
        }
    }

    /// The text of `file` is new, as when it's opened again: its marks go
    /// to their lines and columns in it, and its selections stay at theirs,
    /// as Vim's do.
    pub(crate) fn replaced(&mut self, file: File<'_>, text: &Rope) {
        for (_, place) in self.list.iter_mut().filter(|(_, at)| at.is_in(file)) {
            place.find_byte(text);
        }
        if let Some(kept) = self.selections.iter_mut().find(|kept| kept.is_of(file)) {
            for selected in [&mut kept.last, &mut kept.before].into_iter().flatten() {
                selected.settle(text);
            }
            kept.seen.clear();
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
        marks.set('a', FilePlace::at(File::here("/x"), &text, 1));
        marks.set('a', FilePlace::at(File::here("/y"), &text, 5));
        marks.set('B', FilePlace::at(File::here("/x"), &text, 4));
        assert_eq!(marks.get('a', File::here("/x")).map(|at| at.col), Some(1));
        assert_eq!(marks.get('a', File::here("/y")).map(|at| at.line), Some(1));
        assert_eq!(
            marks.get('B', File::here("/y")).map(|at| at.path.as_str()),
            Some("/x")
        );
        // Set again, `B` moves to another file.
        marks.set('B', FilePlace::at(File::here("/y"), &text, 0));
        assert_eq!(
            marks.get('B', File::here("/x")).map(|at| at.path.as_str()),
            Some("/y")
        );
        let names = |path| -> String {
            marks
                .of(File::here(path))
                .iter()
                .map(|(mark, _)| *mark)
                .collect()
        };
        assert_eq!(names("/x"), "aB");
        assert_eq!(marks.get('z', File::here("/x")), None);
    }

    #[test]
    fn the_same_path_on_another_machine_is_another_file() {
        let text = Rope::from_str("one\ntwo\n");
        let desk = File {
            machine: Some(MachineId([7; 16])),
            path: "/x",
        };
        let mut marks = Marks::default();
        marks.set('a', FilePlace::at(File::here("/x"), &text, 1));
        marks.set('a', FilePlace::at(desk, &text, 5));
        assert_eq!(marks.get('a', File::here("/x")).map(|at| at.col), Some(1));
        assert_eq!(marks.get('a', desk).map(|at| at.line), Some(1));
        // A move there moves only its own.
        marks.moved(desk.machine, "/x", "/y");
        assert_eq!(marks.get('a', File::here("/x")).map(|at| at.col), Some(1));
        assert!(marks.get('a', desk).is_none());
    }

    #[test]
    fn marks_follow_edits_and_are_kept_within_new_text() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        let mut marks = Marks::default();
        marks.set('a', FilePlace::at(File::here("/f"), &text, 10));
        let place = |marks: &Marks| {
            let at = marks.get('a', File::here("/f")).unwrap();
            (at.line, at.col)
        };
        assert_eq!(place(&marks), (2, 2));
        let edit = Edit::replace(14, 0, "", "zero\n");
        let text = Rope::from_str("zero\none\ntwo\nthree\n");
        marks.edited(File::here("/f"), std::slice::from_ref(&edit), &text, false);
        marks.edited(
            File::here("/other"),
            std::slice::from_ref(&edit),
            &text,
            false,
        );
        assert_eq!(place(&marks), (3, 2));
        marks.replaced(File::here("/f"), &Rope::from_str("zero\nx\n"));
        assert_eq!(place(&marks), (1, 1));
    }

    #[test]
    fn a_blank_line_taken_out_and_put_in_elsewhere_moves_no_marks() {
        // As a formatter might: the blank line after `a` goes after `b`.
        let mut marks = Marks::default();
        let text = Rope::from_str("a\n\nb\nc\n");
        marks.set('a', FilePlace::at(File::here("/f"), &text, 2));
        let mut edit = Edit::new();
        edit.retain(2).delete("\n").retain(2).insert("\n").retain(2);
        let text = Rope::from_str("a\nb\n\nc\n");
        marks.edited(File::here("/f"), std::slice::from_ref(&edit), &text, false);
        let at = marks.get('a', File::here("/f")).unwrap();
        assert_eq!((at.line, at.col), (1, 0), "where the line was");
    }
}
