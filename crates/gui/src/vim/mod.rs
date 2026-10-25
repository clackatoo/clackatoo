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
mod grammar;
pub(crate) mod shortcuts;
mod stroke;

pub(crate) use command::{
    Action, Command, Entry, Finds, Operator, Paging, Recase, Scroll, Target, ViewAt, is_register,
    page_rows,
};
#[cfg(test)]
pub(crate) use grammar::parse;
pub(crate) use grammar::{Dialect, Keys, Parsed, Read, Reader};
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

/// A count and a key half typed, as a pane of a list keeps them for keys
/// of its own: the `5` of `5j`, the `g` of `gg`.
#[derive(Debug, Default)]
pub(crate) struct HalfTyped {
    pub(crate) count: Option<usize>,
    pub(crate) key: Option<char>,
}

impl HalfTyped {
    /// Whether anything's half typed.
    pub(crate) fn pending(&self) -> bool {
        self.count.is_some() || self.key.is_some()
    }
}

/// A pane of a list, such as an explorer, a board or a review: no insert
/// mode, and its keys half typed kept as a [`HalfTyped`].
pub(crate) trait ListPane {
    fn half_typed(&self) -> &HalfTyped;
    fn half_typed_mut(&mut self) -> &mut HalfTyped;
    /// Whether something's typed in a prompt of its own.
    fn prompting(&self) -> bool;
}

impl<T: ListPane> KeyState for T {
    fn mode(&self) -> Mode {
        Mode::Normal
    }

    fn prompting(&self) -> bool {
        ListPane::prompting(self)
    }

    fn pending(&self) -> bool {
        self.half_typed().pending()
    }

    fn counting(&self) -> Option<usize> {
        let typed = self.half_typed();
        typed.count.filter(|_| typed.key.is_none())
    }

    fn forget_count(&mut self) {
        self.half_typed_mut().count = None;
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
