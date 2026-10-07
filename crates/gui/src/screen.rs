//! The window's copy of a terminal's view, kept current by the daemon's
//! updates.

use clackatoo_protocol::{CursorState, InputModes, Node, NodeId, Patch, Size, View, ViewUpdate};
use clackatoo_term::Row;

/// What a terminal shows, as rows of cells ready to draw.
#[derive(Debug)]
pub(crate) struct Screen {
    pub(crate) title: String,
    pub(crate) size: Size,
    pub(crate) cursor: CursorState,
    pub(crate) modes: InputModes,
    pub(crate) rows: Vec<Row>,
}

impl Screen {
    pub(crate) fn new(view: &View) -> Self {
        let Node::Grid(grid) = &view.root;
        let width = usize::from(grid.size.cols());
        Self {
            title: view.title.clone(),
            size: grid.size,
            cursor: grid.cursor,
            modes: grid.modes,
            rows: grid.lines.iter().map(|line| line.to_row(width)).collect(),
        }
    }

    /// Applies an update from the daemon. Lines past the bottom are
    /// ignored, so a confused daemon can't crash the window.
    pub(crate) fn apply(&mut self, update: &ViewUpdate) {
        let patches = match update {
            ViewUpdate::Snapshot(view) => {
                *self = Self::new(view);
                return;
            }
            ViewUpdate::Patch(patches) => patches,
        };
        let width = usize::from(self.size.cols());
        for patch in patches {
            match patch {
                Patch::Title(title) => title.clone_into(&mut self.title),
                Patch::Grid {
                    node: NodeId::ROOT,
                    cursor,
                    modes,
                    lines,
                } => {
                    self.cursor = *cursor;
                    self.modes = *modes;
                    for (index, line) in lines {
                        if let Some(row) = self.rows.get_mut(usize::from(*index)) {
                            *row = line.to_row(width);
                        }
                    }
                }
                Patch::Grid { .. } => {}
            }
        }
    }
}
