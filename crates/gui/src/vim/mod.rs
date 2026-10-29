//! One Vim key grammar for every pane with a cursor: what keys make, not
//! what they do.
//!
//! A pane turns each key pressed into [`Stroke`]s, keeps them in [`Keys`]
//! until they make a [`Command`], and does it. What a command does is the
//! pane's, and what a motion does is [`crate::motion`]'s. Each pane reads
//! the keys with a [`Dialect`] of its own: the commands of its own it
//! adds, and which commands it takes. Here too are the modes the window
//! and its panes are in.

mod command;
pub(crate) mod confirm;
mod grammar;
pub(crate) mod list;
pub(crate) mod search;
pub(crate) mod shortcuts;
mod stroke;

pub(crate) use command::{
    Action, Command, Entry, Finds, Operator, Paging, Recase, Scroll, Target, ViewAt, is_register,
    page_rows,
};
pub(crate) use grammar::{Dialect, Keys, Parsed, Read, Reader, only_count_and_register, parse};
pub(crate) use stroke::{
    Stroke, enters_normal_mode, is_ctrl_backslash, plain_strokes, prompt_strokes, strokes,
};

/// What a search that went past the end and round to the top says, in
/// terminals and editors alike, as Vim does: the match may be a long way
/// back.
pub(crate) const WRAPPED_TO_TOP: &str = "search wrapped to top";

/// What a backward search that went round to the bottom says.
pub(crate) const WRAPPED_TO_BOTTOM: &str = "search wrapped to bottom";

/// What the desk asks of the focused pane's keys, whatever kind of pane it
/// is, to know which mode its bindings are looked up in and whether the
/// next key is the pane's.
pub(crate) trait KeyState {
    /// The mode it reads keys in: normal for a pane with no other.
    fn mode(&self) -> Mode;

    /// Whether something is typed in a prompt of its own, such as a
    /// search, which takes every key.
    fn prompting(&self) -> bool;

    /// Whether a command is half typed, which the next key is for.
    fn pending(&self) -> bool;

    /// The count typed, if that's all that's waiting: a key the pane
    /// doesn't take after it is the window's, as `:` is.
    fn counting(&self) -> Option<usize>;

    /// Forgets the count typed, as a binding of the window's took the key
    /// after it.
    fn forget_count(&mut self);
}

/// A pane of a list, such as an explorer, a board or a review (see
/// [`list`]): no insert mode, and its keys half typed kept as [`Keys`].
pub(crate) trait ListKeys {
    fn keys(&self) -> &Keys;
    fn keys_mut(&mut self) -> &mut Keys;
    /// Whether something's typed in a prompt of its own, as a search.
    fn prompting(&self) -> bool;
}

impl<T: ListKeys> KeyState for T {
    fn mode(&self) -> Mode {
        Mode::Normal
    }

    fn prompting(&self) -> bool {
        ListKeys::prompting(self)
    }

    fn pending(&self) -> bool {
        self.keys().pending()
    }

    fn counting(&self) -> Option<usize> {
        self.keys().counting()
    }

    fn forget_count(&mut self) {
        self.keys_mut().clear();
    }
}

/// Whether keys go to the program or drive clackatoo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Insert,
    Normal,
    /// Selecting, from where `v` or `V` was pressed to the cursor.
    Visual(Selection),
}

/// What a visual selection covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    /// Characters from one end to the other.
    Characters,
    /// Whole lines.
    Lines,
    /// A block of columns, from one corner to the other, as Ctrl+V selects
    /// (see [`crate::motion::Block`]).
    Block,
}

mod describe;

pub(crate) const RECORD_STOP: Stroke = Stroke::char('q');
pub(crate) fn stops_recording(stroke: Stroke) -> bool {
    stroke == RECORD_STOP
}
