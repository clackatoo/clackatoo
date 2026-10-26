//! What a window keeps with an undo step, as Vim's undo header does: where
//! the cursor was as the step began, and the file's last selection then.

use crate::edit::{Bias, Edit};

/// A place a window keeps with an undo step: a line, and a column on it.
///
/// The line is kept as a byte offset on it, where it started when the
/// place was kept, and edits made since move that as any place in the text
/// moves, so the place stays on its line as lines are put in or taken away
/// above it. The window takes the line as the one that byte is on. The
/// column is bytes into the line, as Vim's are, and may be past its end:
/// edits leave it as it is, as Vim's marks keep their columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StepPlace {
    /// A byte on the place's line: where it started.
    pub line: u64,
    /// How many bytes into the line it is.
    pub col: u32,
}

/// How a selection is made, as Vim's visual modes make it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StepSelection {
    /// By characters, as `v` selects.
    Characters,
    /// By whole lines, as `V` selects.
    Lines,
    /// A block of columns, as Ctrl+V selects.
    Block,
}

/// A file's last selection, as `gv` makes it again, kept with an undo step,
/// as Vim's `uh_visual` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StepVisual {
    /// Its first end, where `'<` is...
    pub start: StepPlace,
    /// ...and its last, where `'>` is.
    pub end: StepPlace,
    /// How it was made.
    pub kind: StepSelection,
    /// The cursor was at its start, not its end.
    pub backward: bool,
    /// The cursor kept to lines' ends, as after `$`.
    pub to_end: bool,
}

/// What a window keeps with an undo step it starts, as Vim's undo header
/// keeps it, for undo and redo to give back: where its cursor was as the
/// step began (`uh_cursor`), and the file's last selection (`uh_visual`).
///
/// The history keeps the places in it on their lines through edits made
/// since, others' too, as it keeps the step's edit up to date. Undoing or
/// redoing the step gives back its cursor, which stays with the step, and
/// its selection, which is swapped for the window's as it undoes, as
/// Vim's is: a redo then gives back the selection the undo was made with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct StepState {
    /// Where the cursor was as the step began.
    pub cursor: StepPlace,
    /// Whether that's the start of what an operator, or a command on what
    /// was selected, took, which Vim's step holds all of however little of
    /// it changed: undoing the step puts the cursor there however far the
    /// change went.
    pub taken: bool,
    /// The file's last selection as the step began, if it had one.
    pub visual: Option<StepVisual>,
}

impl StepPlace {
    /// Moves it through `edit`, made to the text it's in.
    pub fn map(&mut self, edit: &Edit) {
        let line = usize::try_from(self.line).unwrap_or(usize::MAX);
        self.line = edit.map(line, Bias::After) as u64;
    }

    /// The place in a text `len` bytes long, its line's byte no further
    /// than the text goes, as a place read from elsewhere is kept.
    #[must_use]
    pub fn within(self, len: usize) -> Self {
        Self {
            line: self.line.min(len as u64),
            ..self
        }
    }
}

impl StepVisual {
    /// Moves its ends through `edit`, made to the text it's in.
    pub fn map(&mut self, edit: &Edit) {
        self.start.map(edit);
        self.end.map(edit);
    }

    /// It, in a text `len` bytes long (see [`StepPlace::within`]).
    #[must_use]
    pub fn within(self, len: usize) -> Self {
        Self {
            start: self.start.within(len),
            end: self.end.within(len),
            ..self
        }
    }
}

impl StepState {
    /// Moves its places through `edit`, made to the text they're in.
    pub fn map(&mut self, edit: &Edit) {
        self.cursor.map(edit);
        if let Some(visual) = &mut self.visual {
            visual.map(edit);
        }
    }

    /// It, in a text `len` bytes long (see [`StepPlace::within`]).
    #[must_use]
    pub fn within(self, len: usize) -> Self {
        Self {
            cursor: self.cursor.within(len),
            visual: self.visual.map(|visual| visual.within(len)),
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_keeps_to_its_line_and_column_through_edits() {
        // Line 2 of "one\ntwo\nthree\n", 2 bytes in.
        let mut place = StepPlace { line: 4, col: 2 };
        // A line put in above takes it down with its line.
        place.map(&Edit::replace(14, 0, "", "zero\n"));
        assert_eq!(place, StepPlace { line: 9, col: 2 });
        // Text typed at its line's start leaves it on the line, at its
        // column, as Vim's marks keep theirs.
        place.map(&Edit::replace(19, 9, "", "> "));
        assert_eq!(place, StepPlace { line: 11, col: 2 });
        // Its line joined onto the one before: its byte is on that line.
        place.map(&Edit::replace(21, 8, "\n", " "));
        assert_eq!(place.line, 11);
        assert_eq!(place.within(3), StepPlace { line: 3, col: 2 });
    }
}
