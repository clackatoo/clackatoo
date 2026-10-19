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
    BoardId, BufferId, FilesId, Machine, MachineId, NewAgent, NewReview, NewTerminal, OpenBoard,
    OpenFile, OpenFiles, ReviewId, TerminalId, wire,
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
    /// The home directory of the daemon's user, which windows write paths
    /// under from `~`, as a tab named after its terminal's folder. `None`
    /// from a daemon older than protocol 86.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::BLOCK_MARKS }, _>",
        deserialize_with = "wire::since::<{ wire::BLOCK_MARKS }, _, _>"
    )]
    pub home: Option<String>,
    /// What panes the daemon made again from the workspaces it kept, as
    /// it started, still have to show for it: a command a terminal was
    /// running, to offer to put at its new shell's prompt, and where an
    /// editor was. Each goes once a window has done with it: a terminal's
    /// once it's dismissed
    /// ([`ClientMessage::Dismiss`](crate::ClientMessage::Dismiss)),
    /// anything is typed into it, its shell runs a command, or the pane
    /// closes; an editor's once a window
    /// says where it is ([`ClientMessage::Place`](crate::ClientMessage::Place)),
    /// or the pane shows another file. Empty from a daemon older than
    /// protocol 90.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::RESTORED }, _>",
        deserialize_with = "wire::since::<{ wire::RESTORED }, _, _>"
    )]
    pub restored: Vec<Restored>,
    /// Which daemon sent them: its ID and name. `None` from a daemon
    /// older than protocol 91.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::FEDERATION }, _>",
        deserialize_with = "wire::since::<{ wire::FEDERATION }, _, _>"
    )]
    pub machine: Option<Machine>,
    /// What the daemon knows the machines its panes are on to be called,
    /// for those it isn't connected to itself, as a pane another machine
    /// put in one of its workspaces is ([`PaneTerminal::Lent`]): a window
    /// names such a pane's machine by it, as it can't by a connection.
    /// Empty from a daemon older than protocol 94.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::MIXED }, _>",
        deserialize_with = "wire::since::<{ wire::MIXED }, _, _>"
    )]
    pub others: Vec<OtherMachine>,
}

/// A machine a daemon isn't connected to, but whose panes it shows: see
/// [`Workspaces::others`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherMachine {
    /// Its ID.
    pub id: MachineId,
    /// What it said it's called, made safe to show, as
    /// [`Machine::name`] is.
    pub name: String,
}

/// What a pane made again as the daemon started still has to show for
/// it: see [`Workspaces::restored`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Restored {
    /// The pane.
    pub pane: PaneId,
    /// For a terminal: the command line its shell was running when the
    /// daemon last stopped, as the shell said, which windows offer to put
    /// at the new shell's prompt, typed as if by the user, without Enter:
    /// never run by themselves. Always one line, with no control
    /// characters or runs of spaces.
    pub ran: Option<String>,
    /// For an editor: where its cursor and view were.
    pub place: Option<Place>,
}

/// Where an editor's cursor and view are in its file, kept from one run
/// of the daemon to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// The cursor's line, from 0.
    pub line: u64,
    /// The cursor's column, in bytes from the line's start.
    pub column: u64,
    /// The line at the top of the view, from 0.
    pub top: u64,
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
    /// The daemon it lives on, its *home*, by its ID, when that's another
    /// than the daemon that sent it, as for a workspace from another
    /// machine one daemon passes on from another: `None` for the sender's
    /// own, and from a daemon older than protocol 91.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::FEDERATION }, _>",
        deserialize_with = "wire::since::<{ wire::FEDERATION }, _, _>"
    )]
    pub home: Option<MachineId>,
    /// Who it's lent to, if it holds a pane the daemon made for a daemon
    /// connected to it, to show in a workspace of that daemon's own
    /// ([`Request::Lend`](crate::Request::Lend)), or put in one itself
    /// ([`Lent::placed`]): such a workspace has one tab of one pane, and is
    /// shown to daemons alone, never to a window: to that daemon, and
    /// from protocol 94, to any other, which shows its pane where that
    /// daemon's layout names it. `None` for any other, and from a daemon
    /// older than protocol 93.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::LEND }, _>",
        deserialize_with = "wire::since::<{ wire::LEND }, _, _>"
    )]
    pub lent: Option<Lent>,
}

/// Who a workspace holding a pane lent to another daemon is lent to, and
/// how that daemon knows the pane: see [`Workspace::lent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lent {
    /// The machine of the daemon it's lent to.
    pub to: MachineId,
    /// The number that daemon chose for the pane as it asked for it, kept
    /// with the pane from one run to the next of either daemon, by which
    /// it finds the pane again: the pane's own ID is new each run.
    pub key: u64,
    /// The daemon lending it put it in that daemon's layout itself
    /// ([`PaneTerminal::Lent`]), rather than being asked for it
    /// ([`Request::Lend`](crate::Request::Lend)), as that daemon can't
    /// reach this one: it's this daemon's, not that one's, to close or
    /// give back once that daemon's layout no longer names it. `false`
    /// from a daemon older than protocol 94.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::MIXED }, _>",
        deserialize_with = "wire::since::<{ wire::MIXED }, _, _>"
    )]
    pub placed: bool,
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
        #[serde(deserialize_with = "nested_parts")]
        parts: Vec<Layout>,
        /// Each part's share of the space: the part gets its share over
        /// their sum. One for each part, and none 0.
        shares: Vec<u32>,
    },
}

/// How deep splits may be nested in a layout (see [`Layout::depth`]). A
/// layout as deep has panes too small to see, but reading one any deeper,
/// a split at a time, could overflow the stack, which aborts: so one that
/// is is refused when read, and a daemon never makes one.
pub const MAX_NESTING: u32 = 256;

std::thread_local! {
    /// How deep in a layout the one being read on this thread is.
    static NESTING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Reads a split's parts, one level deeper, refusing past
/// [`MAX_NESTING`].
fn nested_parts<'de, D: serde::Deserializer<'de>>(parts: D) -> Result<Vec<Layout>, D::Error> {
    /// Counts the level back out however reading it ends.
    struct Level;
    impl Drop for Level {
        fn drop(&mut self) {
            NESTING.set(NESTING.get() - 1);
        }
    }
    if NESTING.get() >= MAX_NESTING {
        return Err(serde::de::Error::custom("layout nested too deeply"));
    }
    NESTING.set(NESTING.get() + 1);
    let _level = Level;
    Vec::deserialize(parts)
}

/// One rectangle of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pane {
    /// The pane's ID.
    pub id: PaneId,
    /// What it shows.
    pub content: Content,
    /// The daemon what it shows lives on, its *home*, by its ID, when
    /// that's another than its workspace's: `None` for the workspace's
    /// own, and from a daemon older than protocol 91.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::FEDERATION }, _>",
        deserialize_with = "wire::since::<{ wire::FEDERATION }, _, _>"
    )]
    pub home: Option<MachineId>,
    /// For a pane on another machine than its workspace's, which the
    /// daemon there lent: the number the pane has there, by which that
    /// machine, or another reaching it, finds it ([`Lent::key`]). A window
    /// has no use for it. `None` for any other pane, and from a daemon
    /// older than protocol 94.
    #[serde(
        skip_serializing_if = "wire::before::<{ wire::MIXED }, _>",
        deserialize_with = "wire::since::<{ wire::MIXED }, _, _>"
    )]
    pub key: Option<u64>,
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
    /// A review of what agents changed. One pane shows each.
    Review(ReviewId),
    /// The page a chat's agent drives in clackatoo's browser, by the
    /// chat's ID. Several panes can show one.
    Browser(TerminalId),
    /// A folder, shown in an editor as its listing, to browse and edit as
    /// text, as oil.nvim shows one: a file explorer of its own, rooted at
    /// the folder, whose root follows the folder shown. One pane shows
    /// each.
    Folder(FilesId),
    /// A pane of a kind newer than the client, which a newer daemon sent
    /// in place of it: the client says it needs a newer clackatoo, naming
    /// the kind.
    Other {
        /// What kind of pane it is, as the daemon names it: `browser`.
        kind: PaneKind,
    },
}

/// What a pane of a kind the client doesn't know is called, such as
/// `browser`, for [`Content::Other`].
///
/// Sent as a string, of any length, and kept as at most [`MAX`] bytes of
/// it, in place, so that [`Content`] stays small and `Copy`.
///
/// [`MAX`]: Self::MAX
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneKind {
    len: u8,
    bytes: [u8; Self::MAX],
}

impl PaneKind {
    /// The most bytes of a name kept: a longer one is cut short, between
    /// characters.
    pub const MAX: usize = 23;

    /// The kind called `name`, cut short if it's long.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let name = &name[..name.floor_char_boundary(Self::MAX)];
        let mut bytes = [0; Self::MAX];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        #[allow(clippy::cast_possible_truncation, reason = "at most MAX, which fits")]
        let len = name.len() as u8;
        Self { len, bytes }
    }

    /// What it's called.
    #[must_use]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)]).unwrap_or_default()
    }
}

impl fmt::Debug for PaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for PaneKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for PaneKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PaneKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        /// Reads a string of any length, keeping what fits.
        struct Name;
        impl serde::de::Visitor<'_> for Name {
            type Value = PaneKind;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a pane kind's name")
            }
            fn visit_str<E: serde::de::Error>(self, name: &str) -> Result<PaneKind, E> {
                Ok(PaneKind::new(name))
            }
        }
        deserializer.deserialize_str(Name)
    }
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

impl From<ReviewId> for Content {
    fn from(review: ReviewId) -> Self {
        Self::Review(review)
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
            Self::Buffer(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Folder(_)
            | Self::Other { .. } => None,
        }
    }

    /// The buffer it is, if it's one.
    #[must_use]
    pub fn buffer(self) -> Option<BufferId> {
        match self {
            Self::Buffer(buffer) => Some(buffer),
            Self::Terminal(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Folder(_)
            | Self::Other { .. } => None,
        }
    }

    /// The file explorer it shows, if it's one, or a folder's listing,
    /// which is made from one.
    #[must_use]
    pub fn files(self) -> Option<FilesId> {
        match self {
            Self::Files(files) | Self::Folder(files) => Some(files),
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Other { .. } => None,
        }
    }

    /// Whether it's a folder, shown in an editor as its listing.
    #[must_use]
    pub fn is_folder(self) -> bool {
        matches!(self, Self::Folder(_))
    }

    /// The board it is, if it's one.
    #[must_use]
    pub fn board(self) -> Option<BoardId> {
        match self {
            Self::Board(board) => Some(board),
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Files(_)
            | Self::Review(_)
            | Self::Browser(_)
            | Self::Folder(_)
            | Self::Other { .. } => None,
        }
    }

    /// The review it is, if it's one.
    #[must_use]
    pub fn review(self) -> Option<ReviewId> {
        match self {
            Self::Review(review) => Some(review),
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Browser(_)
            | Self::Folder(_)
            | Self::Other { .. } => None,
        }
    }
    /// The chat whose page it is, if it's a browser's.
    #[must_use]
    pub fn browser(self) -> Option<TerminalId> {
        match self {
            Self::Browser(chat) => Some(chat),
            Self::Terminal(_)
            | Self::Buffer(_)
            | Self::Files(_)
            | Self::Board(_)
            | Self::Review(_)
            | Self::Folder(_)
            | Self::Other { .. } => None,
        }
    }

    /// What a window calls a pane showing it, for one shown as a pane to
    /// come ([`Content::Other`]): `editor`, `explorer`, and so on, or the
    /// name an `Other` gives, as far as it's one of those words.
    #[must_use]
    pub fn kind_name(self) -> &'static str {
        match self {
            Self::Terminal(_) => "terminal",
            Self::Buffer(_) | Self::Folder(_) => "editor",
            Self::Files(_) => "explorer",
            Self::Board(_) => "board",
            Self::Review(_) => "review",
            Self::Browser(_) => "browser",
            Self::Other { kind } => match kind.as_str() {
                "terminal" => "terminal",
                "editor" => "editor",
                "explorer" => "explorer",
                "board" => "board",
                "review" => "review",
                "browser" => "browser",
                _ => "pane",
            },
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
    /// A review of what agents changed.
    Review(NewReview),
    /// The page a chat's agent drives, by the chat's ID.
    Browser(TerminalId),
    /// An empty buffer with no file yet. New in protocol 88: a client asks
    /// [`wire::daemon_has`](crate::wire::daemon_has) with
    /// [`wire::BLANK`](crate::wire::BLANK) before asking for one.
    Blank(crate::OpenBlank),
    /// A pane on the machine of the daemon asking, which said which
    /// machine it is ([`Request::RelayAs`](crate::Request::RelayAs)): one
    /// it made in a workspace of its own, lent to this daemon, for this
    /// daemon's layout to show, as this daemon can't reach it to ask for
    /// one ([`Lent::placed`]). It's shown here as a pane to come
    /// ([`Content::Other`]), with its home and key set, which the daemon
    /// on that machine, and any other that reaches it, shows what it
    /// shows in place of. New in protocol 94: a daemon asks
    /// [`wire::daemon_has`](crate::wire::daemon_has) with
    /// [`wire::MIXED`](crate::wire::MIXED) before giving one.
    Lent {
        /// The number the pane has on that machine ([`Lent::key`]).
        key: u64,
        /// What kind of pane it is, as [`Content::Other`] names one.
        kind: PaneKind,
    },
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

    /// How many splits deep it goes: 0 for a single pane, 1 for a split of
    /// panes, and so on. Never more than [`MAX_NESTING`] in a layout that
    /// can be read.
    #[must_use]
    pub fn depth(&self) -> u32 {
        match self {
            Self::Pane(_) => 0,
            Self::Split { parts, .. } => 1 + parts.iter().map(Self::depth).max().unwrap_or(0),
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

    /// Calls `each` with each of its panes, left to right and top to
    /// bottom, to change it.
    pub fn for_each_pane_mut(&mut self, each: &mut impl FnMut(&mut Pane)) {
        match self {
            Self::Pane(pane) => each(pane),
            Self::Split { parts, .. } => {
                for part in parts {
                    part.for_each_pane_mut(each);
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_kind_goes_as_a_string_and_keeps_what_fits() {
        let kind = PaneKind::new("browser");
        assert_eq!(kind.as_str(), "browser");
        let sent = postcard::to_allocvec(&kind).unwrap();
        assert_eq!(sent, postcard::to_allocvec("browser").unwrap());
        assert_eq!(postcard::from_bytes::<PaneKind>(&sent).unwrap(), kind);
        // As it comes in a pane: the variant, then the name.
        let content = Content::Other { kind };
        let sent = postcard::to_allocvec(&content).unwrap();
        assert_eq!(sent[0], 7);
        assert_eq!(postcard::from_bytes::<Content>(&sent).unwrap(), content);
        // A long name, from a daemon that sent one, is cut between
        // characters.
        let long = "ü".repeat(40);
        let sent = postcard::to_allocvec(&long).unwrap();
        let kind = postcard::from_bytes::<PaneKind>(&sent).unwrap();
        assert_eq!(kind.as_str(), "ü".repeat(11));
        assert_eq!(PaneKind::new("").as_str(), "");
    }
}
