//! The window's copy of a terminal's view, kept current by the daemon's
//! updates.

use clackatoo_protocol::{GridState, Node, NodeId, Patch, Size, Style, View, ViewUpdate};
use clackatoo_term::{Row, RowBuilder};

/// What a terminal shows, as rows of cells ready to draw.
#[derive(Debug)]
pub(crate) struct Screen {
    pub(crate) title: String,
    pub(crate) size: Size,
    pub(crate) state: GridState,
    pub(crate) rows: Vec<Row>,
}

impl Screen {
    pub(crate) fn new(view: &View) -> Self {
        let Node::Grid(grid) = &view.root;
        let width = usize::from(grid.size.cols());
        Self {
            title: view.title.clone(),
            size: grid.size,
            state: grid.state,
            rows: grid.lines.iter().map(|line| line.to_row(width)).collect(),
        }
    }

    /// A screen of `size` showing `lines` of text, for the window's own
    /// messages, such as before any terminal is shown.
    pub(crate) fn message(lines: &[impl AsRef<str>], size: Size) -> Self {
        let width = usize::from(size.cols());
        let rows = (0..usize::from(size.rows()))
            .map(|line| {
                let mut builder = RowBuilder::new(width);
                if let Some(text) = lines.get(line) {
                    builder.push(text.as_ref(), &Style::DEFAULT, false);
                }
                builder.finish(false)
            })
            .collect();
        Self {
            title: "clackatoo".into(),
            size,
            state: GridState::default(),
            rows,
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
                    state,
                    lines,
                } => {
                    self.state = *state;
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
