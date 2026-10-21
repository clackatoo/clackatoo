//! The Vim keys every pane shares: keys as the grammar reads them, and the
//! modes the window and its panes are in.

mod stroke;

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
