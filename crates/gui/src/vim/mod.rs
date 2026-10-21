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
mod stroke;

pub(crate) use command::{
    Action, Command, Entry, Operator, Recase, Scroll, Target, ViewAt, WRAPPED_TO_BOTTOM,
    WRAPPED_TO_TOP, is_register,
};
#[cfg(test)]
pub(crate) use grammar::parse;
pub(crate) use grammar::{Dialect, Keys, Parsed, Read, Reader};
pub(crate) use stroke::{Stroke, enters_normal_mode, is_ctrl_backslash, strokes};

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
