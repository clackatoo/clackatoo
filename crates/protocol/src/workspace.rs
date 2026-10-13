//! Workspaces, their tabs, and how each tab's panes are laid out: kept by
//! the daemon, and shown by clients.
//!
//! A workspace is a piece of distinct work, such as a project. It holds
//! tabs, and each tab a [`Layout`]: a tree of splits whose leaves are
//! panes. A split shares its space between its parts by their shares.
//!
//! Which workspace and tab a client shows, and which pane has its focus,
//! are the client's own. The daemon keeps only a hint of where each
//! workspace was last used, for a window opening on it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{
    BoardId, BufferId, FilesId, NewAgent, NewTerminal, OpenBoard, OpenFile, OpenFiles, TerminalId,
};

/// Identifies a workspace within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(pub u32);

/// Identifies a tab within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TabId(pub u32);

/// Identifies a pane within one daemon. Never reused while it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PaneId(pub u32);

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Every workspace a daemon has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspaces {
    /// The workspaces, in the order they were made.
    pub list: Vec<Workspace>,
    /// The one any client last switched to: where a new window starts.
    pub active: Option<WorkspaceId>,
}

/// A piece of distinct work, such as a project: its tabs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's ID.
    pub id: WorkspaceId,
    /// Its name, unique among this daemon's workspaces.
    pub name: String,
    /// Its tabs, in order. Never empty: a workspace goes with its last tab.
    pub tabs: Vec<Tab>,
    /// The tab any client last switched to.
    pub active_tab: TabId,
}

/// One tab of a workspace: its panes, laid out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tab {
    /// The tab's ID.
    pub id: TabId,
    /// Its panes.
    pub layout: Layout,
    /// The pane any client last focused.
    pub focused: PaneId,
    /// Its name, if given one. Clients name a tab without one after its
    /// focused pane.
    pub name: Option<String>,
}

/// How a tab's space is shared between its panes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    /// The space is one pane.
    Pane(Pane),
    /// The space is shared between `parts`, side by side along `axis`.
    /// There are always at least two, and none is a split along the same
    /// axis: that would be the same parts in this split.
    Split {
        /// Which way the parts are side by side.
        axis: Axis,
        /// The parts, left to right or top to bottom.
        parts: Vec<Layout>,
        /// Each part's share of the space: the part gets its share over
        /// their sum. One for each part, and none 0.
        shares: Vec<u32>,
    },
}

/// One rectangle of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    /// The pane's ID.
    pub id: PaneId,
    /// What it shows.
    pub content: Content,
}

/// What a pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Content {
    /// A terminal. A terminal is in one pane at most.
    Terminal(TerminalId),
    /// A buffer: a file being edited. Several panes can show one.
    Buffer(BufferId),
    /// A file explorer: a tree of a project's files. One pane shows each.
    Files(FilesId),
    /// A board agents and the user coordinate on. Several panes can show
    /// one.
    Board(BoardId),
}

impl From<TerminalId> for Content {
    fn from(terminal: TerminalId) -> Self {
        Self::Terminal(terminal)
    }
}

impl From<BufferId> for Content {
    fn from(buffer: BufferId) -> Self {
        Self::Buffer(buffer)
    }
}

impl From<FilesId> for Content {
    fn from(files: FilesId) -> Self {
        Self::Files(files)
    }
}

impl From<BoardId> for Content {
    fn from(board: BoardId) -> Self {
        Self::Board(board)
    }
}

impl Content {
    /// The terminal it is, if it's one.
    #[must_use]
    pub fn terminal(self) -> Option<TerminalId> {
        match self {
            Self::Terminal(terminal) => Some(terminal),
            Self::Buffer(_) | Self::Files(_) | Self::Board(_) => None,
        }
    }

    /// The buffer it is, if it's one.
    #[must_use]
    pub fn buffer(self) -> Option<BufferId> {
        match self {
            Self::Buffer(buffer) => Some(buffer),
            Self::Terminal(_) | Self::Files(_) | Self::Board(_) => None,
        }
    }

    /// The file explorer it is, if it's one.
    #[must_use]
    pub fn files(self) -> Option<FilesId> {
        match self {
            Self::Files(files) => Some(files),
            Self::Terminal(_) | Self::Buffer(_) | Self::Board(_) => None,
        }
    }

    /// The board it is, if it's one.
    #[must_use]
    pub fn board(self) -> Option<BoardId> {
        match self {
            Self::Board(board) => Some(board),
            Self::Terminal(_) | Self::Buffer(_) | Self::Files(_) => None,
        }
    }
}

/// Which way a split's parts are side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    /// Left to right, with upright dividers.
    LeftToRight,
    /// Top to bottom, with flat dividers.
    TopToBottom,
}

/// Where a new pane goes beside the one split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// To its right.
    Right,
    /// To its left.
    Left,
    /// Below it.
    Below,
    /// Above it.
    Above,
}

impl Side {
    /// The axis along which the two panes are side by side.
    #[must_use]
    pub fn axis(self) -> Axis {
        match self {
            Self::Right | Self::Left => Axis::LeftToRight,
            Self::Below | Self::Above => Axis::TopToBottom,
        }
    }

    /// Whether the new pane comes first along that axis.
    #[must_use]
    pub fn before(self) -> bool {
        matches!(self, Self::Left | Self::Above)
    }
}

/// What a new pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneTerminal {
    /// A new one. With no working directory given, it starts in the one
    /// the pane beside it, or the workspace's focused pane, is in.
    New(NewTerminal),
    /// One already running and in no pane, such as one started by a script.
    Existing(TerminalId),
    /// A new chat with an agent.
    Agent(NewAgent),
    /// A file to edit.
    File(OpenFile),
    /// A file explorer.
    Files(OpenFiles),
    /// A board.
    Board(OpenBoard),
}

impl Workspaces {
    /// The workspace with `id`.
    #[must_use]
    pub fn get(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.list.iter().find(|workspace| workspace.id == id)
    }

    /// Where the pane with `id` is: its workspace, its tab and itself.
    #[must_use]
    pub fn find_pane(&self, id: PaneId) -> Option<(&Workspace, &Tab, Pane)> {
        self.list.iter().find_map(|workspace| {
            workspace.tabs.iter().find_map(|tab| {
                tab.layout
                    .panes()
                    .into_iter()
                    .find(|pane| pane.id == id)
                    .map(|pane| (workspace, tab, pane))
            })
        })
    }

    /// The pane showing `terminal`, if one does.
    #[must_use]
    pub fn pane_showing(&self, terminal: TerminalId) -> Option<PaneId> {
        self.list
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .find(|pane| pane.content == Content::Terminal(terminal))
            .map(|pane| pane.id)
    }
}

impl Workspaces {
    /// The workspace holding the tab with `id`, and the tab.
    #[must_use]
    pub fn find_tab(&self, id: TabId) -> Option<(&Workspace, &Tab)> {
        self.list
            .iter()
            .find_map(|workspace| workspace.tab(id).map(|tab| (workspace, tab)))
    }
}

impl Tab {
    /// The terminals its panes show.
    #[must_use]
    pub fn terminals(&self) -> Vec<TerminalId> {
        (self.layout.panes().into_iter())
            .filter_map(|pane| pane.content.terminal())
            .collect()
    }
}

impl Workspace {
    /// The tab with `id`.
    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The terminals its panes show, in every tab.
    #[must_use]
    pub fn terminals(&self) -> Vec<TerminalId> {
        self.tabs
            .iter()
            .flat_map(|tab| tab.layout.panes())
            .filter_map(|pane| pane.content.terminal())
            .collect()
    }
}

impl Layout {
    /// Its panes, left to right and top to bottom.
    #[must_use]
    pub fn panes(&self) -> Vec<Pane> {
        let mut panes = Vec::new();
        self.collect_panes(&mut panes);
        panes
    }

    fn collect_panes(&self, panes: &mut Vec<Pane>) {
        match self {
            Self::Pane(pane) => panes.push(*pane),
            Self::Split { parts, .. } => {
                for part in parts {
                    part.collect_panes(panes);
                }
            }
        }
    }

    /// Whether it has the pane with `id`.
    #[must_use]
    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Self::Pane(pane) => pane.id == id,
            Self::Split { parts, .. } => parts.iter().any(|part| part.contains(id)),
        }
    }

    /// The layout at `path`: the index of a part in this split, then in
    /// that part, and so on. The empty path is this layout.
    #[must_use]
    pub fn at(&self, path: &[u16]) -> Option<&Self> {
        match path.split_first() {
            None => Some(self),
            Some((&index, rest)) => match self {
                Self::Split { parts, .. } => parts.get(usize::from(index))?.at(rest),
                Self::Pane(_) => None,
            },
        }
    }

    /// As [`at`](Self::at), for changing it.
    pub fn at_mut(&mut self, path: &[u16]) -> Option<&mut Self> {
        match path.split_first() {
            None => Some(self),
            Some((&index, rest)) => match self {
                Self::Split { parts, .. } => parts.get_mut(usize::from(index))?.at_mut(rest),
                Self::Pane(_) => None,
            },
        }
    }
}
