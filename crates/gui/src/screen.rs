//! The window's copy of a terminal's view, kept current by the daemon's
//! updates.

use std::collections::{BTreeMap, HashMap};

use clackatoo_protocol::{
    BlockMarks, GridState, LinkId, LinkTarget, Node, NodeId, Patch, Size, Style, View, ViewUpdate,
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
    /// For each row, the number of the shell's block whose prompt starts
    /// on it, if one does.
    pub(crate) prompts: Vec<Option<u32>>,
    /// How the shell's blocks ended, as the daemon said, for the rows and
    /// the history lines fetched. Numbers name the same block for as long
    /// as the terminal lives, so what was learnt stays true.
    pub(crate) exits: Exits,
}

/// How blocks of a shell ended, by number: those whose command finished
/// and whose shell said how.
#[derive(Debug, Default)]
pub(crate) struct Exits(BTreeMap<u32, i32>);

impl Exits {
    /// The most kept: as many as the history lines a window keeps could
    /// show the prompts of. The oldest go first.
    const MAX: usize = 4096;

    /// How block `block` ended, if that's known.
    pub(crate) fn get(&self, block: u32) -> Option<i32> {
        self.0.get(&block).copied()
    }

    /// Takes in how blocks ended.
    pub(crate) fn learn(&mut self, exits: &[(u32, i32)]) {
        self.0.extend(exits.iter().copied());
        while self.0.len() > Self::MAX {
            self.0.pop_first();
        }
    }
}

impl Screen {
    /// The screen `view` shows, if it's a grid.
    pub(crate) fn new(view: &View) -> Option<Self> {
        let Node::Grid(grid) = &view.root else {
            return None;
        };
        let width = usize::from(grid.size.cols());
        let mut screen = Self {
            title: view.title.clone(),
            cwd: view.cwd.clone(),
            size: grid.size,
            state: grid.state,
            rows: grid.lines.iter().map(|line| line.to_row(width)).collect(),
            links: grid.links.iter().cloned().collect(),
            prompts: vec![None; grid.lines.len()],
            exits: Exits::default(),
        };
        screen.mark(&grid.marks);
        Some(screen)
    }

    /// A screen of `size` showing `lines` of text, for the window's own
    /// messages, such as before any terminal is shown.
    pub(crate) fn message(lines: &[impl AsRef<str>], size: Size) -> Self {
        let width = usize::from(size.cols());
        let rows: Vec<Row> = (0..usize::from(size.rows()))
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
            prompts: vec![None; rows.len()],
            rows,
            links: HashMap::new(),
            exits: Exits::default(),
        }
    }

    /// Takes in where blocks' prompts start, and how blocks ended.
    fn mark(&mut self, marks: &BlockMarks) {
        for &(line, block) in &marks.prompts {
            if let Some(prompt) = self.prompts.get_mut(usize::from(line)) {
                *prompt = Some(block);
            }
        }
        self.exits.learn(&marks.exits);
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
                    marks,
                } => {
                    self.state = *state;
                    for (index, line) in lines {
                        if let Some(row) = self.rows.get_mut(usize::from(*index)) {
                            *row = line.to_row(width);
                        }
                        // What it starts, if anything, is in the marks.
                        if let Some(prompt) = self.prompts.get_mut(usize::from(*index)) {
                            *prompt = None;
                        }
                    }
                    self.mark(marks);
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
