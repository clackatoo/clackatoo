//! The window's copy of a terminal's view, kept current by the daemon's
//! updates.

use std::collections::HashMap;

use clackatoo_protocol::{
    GridState, LinkId, LinkTarget, Node, NodeId, Patch, Size, Style, View, ViewUpdate,
};
use clackatoo_term::{Row, RowBuilder};

/// What a terminal shows, as rows of cells ready to draw.
#[derive(Debug)]
pub(crate) struct Screen {
    pub(crate) title: String,
    /// The working directory the shell last reported.
    pub(crate) cwd: Option<String>,
    pub(crate) size: Size,
    pub(crate) state: GridState,
    pub(crate) rows: Vec<Row>,
    /// What the links the rows, and the history lines fetched, are part
    /// of point to, as the daemon's messages said. Its IDs are never given
    /// to other links, so what was learnt stays true.
    pub(crate) links: HashMap<LinkId, LinkTarget>,
}

impl Screen {
    /// The screen `view` shows, if it's a grid.
    pub(crate) fn new(view: &View) -> Option<Self> {
        let Node::Grid(grid) = &view.root else {
            return None;
        };
        let width = usize::from(grid.size.cols());
        Some(Self {
            title: view.title.clone(),
            cwd: view.cwd.clone(),
            size: grid.size,
            state: grid.state,
            rows: grid.lines.iter().map(|line| line.to_row(width)).collect(),
            links: grid.links.iter().cloned().collect(),
        })
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
            cwd: None,
            size,
            state: GridState::default(),
            rows,
            links: HashMap::new(),
        }
    }

    /// Applies an update from the daemon. Lines past the bottom are
    /// ignored, so a confused daemon can't crash the window.
    pub(crate) fn apply(&mut self, update: &ViewUpdate) {
        let patches = match update {
            ViewUpdate::Snapshot(view) => {
                if let Some(screen) = Self::new(view) {
                    *self = screen;
                }
                return;
            }
            ViewUpdate::Patch(patches) => patches,
        };
        let width = usize::from(self.size.cols());
        for patch in patches {
            match patch {
                Patch::Title(title) => title.clone_into(&mut self.title),
                Patch::Cwd(cwd) => self.cwd.clone_from(cwd),
                Patch::Grid {
                    node: NodeId::ROOT,
                    state,
                    lines,
                    links,
                } => {
                    self.state = *state;
                    for (index, line) in lines {
                        if let Some(row) = self.rows.get_mut(usize::from(*index)) {
                            *row = line.to_row(width);
                        }
                    }
                    // An ID always names the same link, so one known
                    // already needn't be copied again.
                    for (id, link) in links {
                        self.links.entry(*id).or_insert_with(|| link.clone());
                    }
                }
                Patch::Grid { .. } | Patch::Chat(_) => {}
            }
        }
    }
}
