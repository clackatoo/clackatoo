//! Window-local locations shared by jumps and marks. Positions belong to
//! their content, not to screen coordinates; pane-specific code validates
//! them before restoration.
use crate::editor::FilePlace;
use clackatoo_protocol::{BoardId, MachineId, PaneId, ReviewId, TerminalId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderPlace {
    pub(crate) dir: String,
    pub(crate) entry: Option<String>,
}
impl FolderPlace {
    pub(crate) fn moved(&mut self, from: &str, to: &str) {
        for path in std::iter::once(&mut self.dir).chain(self.entry.iter_mut()) {
            if let Some(rest) = path
                .strip_prefix(from)
                .filter(|rest| rest.is_empty() || rest.starts_with('/'))
            {
                *path = format!("{to}{rest}");
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LinePlace {
    pub(crate) line: u64,
    pub(crate) col: usize,
    pub(crate) generation: u64,
    pub(crate) alternate: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChatPlace {
    pub(crate) turn: u64,
    pub(crate) sent: u64,
    pub(crate) generation: u64,
    pub(crate) piece: usize,
    pub(crate) part: usize,
    pub(crate) byte: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ItemPlace {
    Explorer {
        root: String,
        path: Option<String>,
    },
    Board {
        board: BoardId,
        entry: u32,
    },
    Review {
        review: ReviewId,
        at: crate::review::Place,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Place {
    File(FilePlace),
    Folder {
        pane: PaneId,
        machine: Option<MachineId>,
        at: FolderPlace,
    },
    Line {
        pane: PaneId,
        terminal: TerminalId,
        at: LinePlace,
    },
    Chat {
        pane: PaneId,
        terminal: TerminalId,
        at: ChatPlace,
    },
    Item {
        pane: PaneId,
        at: ItemPlace,
    },
    Pane(PaneId),
}
impl Place {
    pub(crate) fn file(&self) -> Option<&FilePlace> {
        if let Self::File(file) = self {
            Some(file)
        } else {
            None
        }
    }
    pub(crate) fn file_mut(&mut self) -> Option<&mut FilePlace> {
        if let Self::File(file) = self {
            Some(file)
        } else {
            None
        }
    }
    pub(crate) fn pane(&self) -> Option<PaneId> {
        match self {
            Self::File(_) => None,
            Self::Folder { pane, .. }
            | Self::Line { pane, .. }
            | Self::Chat { pane, .. }
            | Self::Item { pane, .. }
            | Self::Pane(pane) => Some(*pane),
        }
    }
    pub(crate) fn same_scope(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::File(a), Self::File(b)) => a.file() == b.file(),
            (Self::File(_), _) | (_, Self::File(_)) => false,
            _ => self.pane() == other.pane(),
        }
    }
    pub(crate) fn same_line(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::File(a), Self::File(b)) => a.file() == b.file() && a.line == b.line,
            (
                Self::Line {
                    pane: a,
                    terminal: ta,
                    at: x,
                },
                Self::Line {
                    pane: b,
                    terminal: tb,
                    at: y,
                },
            ) => {
                a == b
                    && ta == tb
                    && x.line == y.line
                    && x.generation == y.generation
                    && x.alternate == y.alternate
            }
            _ => self == other,
        }
    }
    pub(crate) fn moved(&mut self, machine: Option<MachineId>, from: &str, to: &str) {
        match self {
            Self::File(file) => file.moved(machine, from, to),
            Self::Folder {
                machine: on, at, ..
            } if *on == machine => at.moved(from, to),
            _ => {}
        }
    }
}
