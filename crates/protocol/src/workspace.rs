//! Workspaces, their tabs, and how each tab's panes are laid out: kept by
//! the daemon, and shown by clients.
//!
//! A workspace is a piece of distinct work, such as a project. It holds
//! tabs, and each tab a [`Layout`]: a tree of splits whose leaves are
//! panes. A split shares its space equally between its parts.
//!
//! Which workspace and tab a client shows, and which pane has its focus,
//! are the client's own. The daemon keeps only a hint of where each
//! workspace was last used, for a window opening on it.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{NewTerminal, TerminalId};

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
}

/// How a tab's space is shared between its panes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Layout {
    /// The space is one pane.
    Pane(Pane),
    /// The space is shared equally between `parts`, side by side along
    /// `axis`. There are always at least two, and none is a split along
    /// the same axis: that would be the same parts in this split.
    Split {
        /// Which way the parts are side by side.
        axis: Axis,
        /// The parts, left to right or top to bottom.
        parts: Vec<Layout>,
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

/// The terminal a new pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneTerminal {
    /// A new one. With no working directory given, it starts in the one
    /// the pane beside it, or the workspace's focused pane, is in.
    New(NewTerminal),
    /// One already running and in no pane, such as one started by a script.
    Existing(TerminalId),
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
            .map(|pane| match pane.content {
                Content::Terminal(terminal) => terminal,
            })
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
}
