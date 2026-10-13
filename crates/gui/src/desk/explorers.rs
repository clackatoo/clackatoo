//! The desk's file explorers: panes showing a tree of a project's files
//! (see [`crate::explorer`]), opening the files picked in the tab's
//! editors, and asking the daemon to make, move, copy and delete files.
//!
//! The explorers of the workspace shown are attached, as its buffers are.
//! `<Space>e` opens one down the tab's left, or goes to it, or closes it
//! if it has the focus; `:Explore` opens one, or goes to it. Either way,
//! from an editor, it shows the editor's file.
//!
//! A file picked opens in the editor of the tab that last had the focus,
//! in place of its file, as `:e` would, so clicking down a tree doesn't
//! pile up panes; or, with no editor in the tab, in a new one beside the
//! explorer.
//!
//! What's expanded in an explorer, and where its cursor was, is kept when
//! it closes, by its root, for the next to open there in this window. It
//! isn't kept anywhere else: another window, or this one opened again,
//! starts afresh.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    ClientMessage, Content, Event, FileOp, FileOpError, FilesId, FilesUpdate, OpenFile, OpenFiles,
    PaneId, PaneTerminal, Request, Side, Size,
};

use super::{Asked, Desk};
use crate::explorer::{Ask, Explorer, Opening, View};
use crate::input::KeyPress;
use crate::layout::Placement;

/// How many panes focused are remembered, the latest first, to know where
/// a file picked in an explorer opens.
const RECENT: usize = 32;

/// The explorers, by the pane each is, and what they share.
#[derive(Debug, Default)]
pub(super) struct Explorers {
    panes: HashMap<PaneId, Explorer>,
    /// The explorers attached to on this connection.
    attached: HashSet<FilesId>,
    /// How explorers closed were shown, by their roots.
    kept: HashMap<String, View>,
    /// The file operations asked for and not yet answered, by their
    /// numbers: the explorer that asked, and what it asked.
    ops: HashMap<u64, (PaneId, FileOp)>,
    next_op: u64,
    /// What was cut or copied, to paste: its path, and whether it's to be
    /// moved.
    clipped: Option<(String, bool)>,
    /// The panes focused, the latest first.
    recent: Vec<PaneId>,
}

impl Desk {
    /// The explorer `pane` is, if it's one.
    pub(crate) fn explorer(&self, pane: PaneId) -> Option<&Explorer> {
        self.explorers.panes.get(&pane)
    }

    /// The focused pane's explorer, if it's one.
    pub(super) fn focused_explorer(&self) -> Option<&Explorer> {
        self.explorers.panes.get(&self.shown?.pane)
    }

    /// Makes an explorer for every pane showing one in the workspace
    /// shown, and attaches to them; forgets those of panes gone, keeping
    /// how each was shown, by its root.
    pub(super) fn attach_shown_explorers(&mut self) {
        let panes = |workspace: &clackatoo_protocol::Workspace| -> Vec<(PaneId, FilesId)> {
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter_map(|pane| Some((pane.id, pane.content.files()?)))
                .collect()
        };
        let anywhere: HashMap<PaneId, FilesId> =
            self.workspaces.list.iter().flat_map(panes).collect();
        let shown = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(panes)
            .unwrap_or_default();
        let explorers = &mut self.explorers;
        let gone: Vec<PaneId> = (explorers.panes.iter())
            .filter(|(pane, explorer)| anywhere.get(pane) != Some(&explorer.files()))
            .map(|(&pane, _)| pane)
            .collect();
        for pane in gone {
            let explorer = explorers.panes.remove(&pane).expect("just found");
            if let Some(root) = explorer.root() {
                explorers.kept.insert(root.to_owned(), explorer.view());
            }
        }
        let workspaces = &self.workspaces;
        explorers
            .recent
            .retain(|&pane| workspaces.find_pane(pane).is_some());
        explorers
            .ops
            .retain(|_, (pane, _)| anywhere.contains_key(pane));
        for &(pane, files) in &shown {
            explorers
                .panes
                .entry(pane)
                .or_insert_with(|| Explorer::new(files, &self.options));
        }
        if !self.connected {
            return;
        }
        let wanted: HashSet<FilesId> = explorers.panes.values().map(Explorer::files).collect();
        let gone: Vec<FilesId> = explorers.attached.difference(&wanted).copied().collect();
        let new: Vec<FilesId> = wanted.difference(&explorers.attached).copied().collect();
        for files in gone {
            self.explorers.attached.remove(&files);
            // A closed explorer went with its pane.
            if anywhere.values().any(|&shown| shown == files) {
                self.request(Request::DetachFiles { files }, Asked::Quiet);
            }
        }
        for files in new {
            self.explorers.attached.insert(files);
            self.request(Request::AttachFiles { files }, Asked::Quiet);
        }
    }

    /// Sizes the explorer of `pane` to its tile.
    pub(super) fn fit_explorer(&mut self, pane: PaneId, size: Size) {
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            explorer.resize(size);
        }
    }

    /// Passes the options on to every explorer, as they've changed.
    pub(super) fn explorer_options(&mut self) {
        for explorer in self.explorers.panes.values_mut() {
            explorer.set_options(&self.options);
        }
    }

    /// Lays out the rows of the explorers in the tab shown.
    pub(crate) fn settle_explorers(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        for pane in shown {
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                explorer.settle();
            }
        }
    }

    /// Forgets what was attached and asked, as a new connection does.
    pub(super) fn explorers_reconnected(&mut self) {
        self.explorers.attached.clear();
        self.explorers.ops.clear();
        for explorer in self.explorers.panes.values_mut() {
            explorer.reconnected();
        }
    }

    /// Notes that `pane` has the focus, for where files explorers pick
    /// open.
    pub(super) fn note_focus(&mut self, pane: PaneId) {
        let recent = &mut self.explorers.recent;
        recent.retain(|&other| other != pane);
        recent.insert(0, pane);
        recent.truncate(RECENT);
    }

    /// Takes in what the daemon says about explorers, or a file operation
    /// one asked for. Returns whether the window must be redrawn.
    pub(crate) fn explorer_event(&mut self, event: Event) -> bool {
        match event {
            Event::Files { files, update } => self.files_changed(files, update),
            Event::FileOp { id, outcome } => self.file_op_done(id, outcome),
            _ => false,
        }
    }

    /// Takes in what the daemon says about the explorer `files`. Returns
    /// whether it's shown, and so the window must be redrawn.
    pub(crate) fn files_changed(&mut self, files: FilesId, update: FilesUpdate) -> bool {
        let explorers = &mut self.explorers;
        let Some((&pane, explorer)) =
            (explorers.panes.iter_mut()).find(|(_, explorer)| explorer.files() == files)
        else {
            return false;
        };
        let first = explorer.root().is_none();
        explorer.update(update);
        // Shown as one on its root was, when it closed.
        if first
            && let Some(root) = explorer.root()
            && let Some(view) = explorers.kept.remove(root)
        {
            explorer.restore(view);
        }
        self.shown_tab()
            .is_some_and(|tab| tab.layout.contains(pane))
    }

    /// Takes how the file operation numbered `id` went, telling the
    /// explorer that asked. Returns whether the window must be redrawn.
    pub(crate) fn file_op_done(&mut self, id: u64, outcome: Result<(), FileOpError>) -> bool {
        let Some((pane, op)) = self.explorers.ops.remove(&id) else {
            return false;
        };
        if outcome.is_ok() {
            match &op {
                FileOp::Move { from, to, .. } => {
                    self.jumps.moved(from, to);
                    self.shared_mut().marks.moved(from, to);
                    // What was cut has gone where it was put.
                    if self
                        .explorers
                        .clipped
                        .as_ref()
                        .is_some_and(|(path, _)| path == from)
                    {
                        self.explorers.clipped = None;
                    }
                }
                FileOp::Delete { path, .. }
                    if self
                        .explorers
                        .clipped
                        .as_ref()
                        .is_some_and(|(clipped, _)| clipped == path) =>
                {
                    self.explorers.clipped = None;
                }
                _ => {}
            }
        }
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            explorer.answered(op, outcome);
        }
        true
    }

    /// The explorers' messages for the daemon.
    pub(super) fn take_explorer_outbox(&mut self) -> Vec<ClientMessage> {
        let connected = self.connected;
        let mut messages = Vec::new();
        for explorer in self.explorers.panes.values_mut() {
            let files = explorer.files();
            let actions = explorer.take_outbox();
            if connected {
                messages.extend(
                    (actions.into_iter()).map(|action| ClientMessage::Files { files, action }),
                );
            }
        }
        messages
    }

    /// Hands `press` to the focused explorer, then does what it asks.
    /// Returns whether the window must be redrawn.
    pub(super) fn explorer_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return false;
        };
        let mut redraw = false;
        for stroke in crate::editor::strokes(press, explorer.prompting()) {
            redraw |= explorer.stroke(stroke);
        }
        let asks = explorer.take_asks();
        redraw |= !asks.is_empty();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        redraw
    }

    /// Stops the macro being recorded, if `press` is `q` in the focused
    /// explorer and one is, as `q` does in an editor: rather than closing
    /// the explorer. Returns whether it did.
    pub(super) fn stop_recording_in_explorer(&mut self, press: &KeyPress<'_>) -> bool {
        let quiet = (self.focused_explorer())
            .is_some_and(|explorer| !explorer.prompting() && !explorer.pending());
        let q = crate::keymap::Chord::of(press) == Some(crate::keymap::Chord::typed('q'));
        if quiet && q && self.recording().is_some() {
            self.shared_mut().registers.stop_recording();
            return true;
        }
        false
    }

    /// A click at `cell` of the explorer `pane`, a double one if `clicks`
    /// is 2: the cursor goes to the line clicked, which a double click
    /// opens, as Enter does. Returns whether the window must be redrawn.
    pub(super) fn explorer_click(&mut self, pane: PaneId, cell: [i64; 2], clicks: u8) -> bool {
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return false;
        };
        let Ok(row) = usize::try_from(cell[0]) else {
            return false;
        };
        let redraw = explorer.click(row, clicks == 2);
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        redraw
    }

    /// Scrolls the explorer `pane` by `lines`, as the wheel does.
    pub(super) fn explorer_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        (self.explorers.panes.get_mut(&pane)).is_some_and(|explorer| explorer.scroll(lines))
    }

    /// Does what the explorer `pane` asked.
    fn explorer_asked(&mut self, pane: PaneId, ask: Ask) {
        match ask {
            Ask::Open { path, how } => self.open_from_explorer(pane, path, how),
            Ask::Op(op) => self.file_op(pane, op),
            Ask::Keep { path, cut } => self.explorers.clipped = Some((path, cut)),
            Ask::Paste { dir } => {
                let Some((from, cut)) = self.explorers.clipped.clone() else {
                    if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                        explorer.say("nothing's cut or copied: x or y does".to_owned());
                    }
                    return;
                };
                let name = from.rsplit('/').next().unwrap_or(&from);
                let to = if dir.ends_with('/') {
                    format!("{dir}{name}")
                } else {
                    format!("{dir}/{name}")
                };
                let op = if cut {
                    FileOp::Move {
                        from,
                        to,
                        overwrite: None,
                    }
                } else {
                    FileOp::Copy {
                        from,
                        to,
                        overwrite: None,
                    }
                };
                self.file_op(pane, op);
            }
            Ask::Close => self.request(Request::ClosePane { pane }, Asked::Command),
        }
    }

    /// Asks the daemon to do `op`, for the explorer `pane`.
    fn file_op(&mut self, pane: PaneId, op: FileOp) {
        if !self.connected {
            return;
        }
        let id = self.explorers.next_op;
        self.explorers.next_op += 1;
        self.explorers.ops.insert(id, (pane, op.clone()));
        self.outbox.push(ClientMessage::FileOp { id, op });
    }

    /// Opens the file at `path`, picked in the explorer `explorer`, as
    /// `how` says: in the tab's editor that last had the focus, or a
    /// split from it, or in a new one beside the explorer, or a new tab.
    fn open_from_explorer(&mut self, explorer: PaneId, path: String, how: Opening) {
        let Some(shown) = self.shown else { return };
        let file = PaneTerminal::File(OpenFile {
            path: path.clone(),
            cwd: None,
        });
        if how == Opening::Tab {
            let request = Request::NewTab {
                workspace: shown.workspace,
                terminal: file,
            };
            return self.request(request, Asked::Pane);
        }
        let editor = self.recent_in_tab(explorer, |desk, pane| desk.editor(pane).is_some());
        match (how, editor) {
            (Opening::Here, Some(editor)) => {
                self.show_pane(editor);
                if self.file_in(editor) != Some(path.as_str()) {
                    self.open_here(editor, path);
                }
            }
            (Opening::Split(side), Some(editor)) => {
                let request = Request::Split {
                    pane: editor,
                    side,
                    terminal: file,
                };
                self.request(request, Asked::Pane);
            }
            (_, None) => self.open_beside_explorer(explorer, file),
            (Opening::Tab, Some(_)) => unreachable!("opened above"),
        }
    }

    /// Opens `file` in a new pane beside the explorer `explorer`: split
    /// from the pane in the tab that last had the focus, on its side
    /// towards the explorer, or if the explorer is alone, in the rest of
    /// the tab, leaving the explorer as wide as it is.
    fn open_beside_explorer(&mut self, explorer: PaneId, file: PaneTerminal) {
        let Some(tab) = self.shown_tab() else { return };
        let tab_id = tab.id;
        let other = self
            .recent_in_tab(explorer, |_, pane| pane != explorer)
            .or_else(|| {
                (tab.layout.panes().iter())
                    .map(|pane| pane.id)
                    .find(|&pane| pane != explorer)
            });
        let request = match other {
            Some(pane) => {
                let placement = self.placement();
                let tile = |id| placement.as_ref().and_then(|placement| placement.tile(id));
                let left_of = (tile(explorer).zip(tile(pane)))
                    .is_some_and(|(explorer, pane)| explorer.origin[0] < pane.origin[0]);
                Request::Split {
                    pane,
                    side: if left_of { Side::Left } else { Side::Right },
                    terminal: file,
                }
            }
            None => Request::SplitTab {
                tab: tab_id,
                side: Side::Right,
                terminal: file,
                share: Some(1000 - self.explorer_share()),
            },
        };
        self.request(request, Asked::Pane);
    }

    /// The pane of the tab `beside` is in that last had the focus and that
    /// `wanted` takes, if any.
    fn recent_in_tab(
        &self,
        beside: PaneId,
        wanted: impl Fn(&Self, PaneId) -> bool,
    ) -> Option<PaneId> {
        let (_, tab, _) = self.workspaces.find_pane(beside)?;
        let recent = (self.explorers.recent.iter().copied())
            .find(|&pane| pane != beside && tab.layout.contains(pane) && wanted(self, pane));
        recent.or_else(|| {
            (tab.layout.panes().iter())
                .map(|pane| pane.id)
                .find(|&pane| pane != beside && wanted(self, pane))
        })
    }

    /// The share of the tab's width an explorer opened down its left
    /// takes, in thousandths, for `explorerwidth` cells.
    fn explorer_share(&self) -> u16 {
        let cols = self.area.map_or(0, |area| {
            let whole = clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(0),
                content: Content::Terminal(clackatoo_protocol::TerminalId(0)),
            });
            Placement::new(&whole, &area).panes[0].1.cols
        });
        if cols == 0 {
            return 250;
        }
        let share = u64::from(self.options.explorerwidth) * 1000 / u64::from(cols);
        u16::try_from(share.clamp(1, 900)).unwrap_or(250)
    }

    /// The explorer pane in the tab shown, if there's one.
    fn explorer_in_tab(&self) -> Option<PaneId> {
        let tab = self.shown_tab()?;
        (tab.layout.panes().iter())
            .find(|pane| matches!(pane.content, Content::Files(_)))
            .map(|pane| pane.id)
    }

    /// `<Space>e`: closes the tab's explorer if it has the focus, goes to
    /// it if it doesn't, or opens one down the tab's left.
    pub(super) fn toggle_explorer(&mut self) {
        match self.explorer_in_tab() {
            Some(pane) if self.shown.is_some_and(|shown| shown.pane == pane) => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
            _ => self.open_explorer(None),
        }
    }

    /// `:Explore`: goes to the tab's explorer, rooting it at `dir` if
    /// given, or opens one down the tab's left, rooted at `dir`, or the
    /// focused pane's project. Either way, from an editor, it shows the
    /// editor's file.
    pub(super) fn open_explorer(&mut self, dir: Option<String>) {
        let Some(shown) = self.shown else { return };
        let reveal = self.file_in(shown.pane).map(str::to_owned);
        if let Some(pane) = self.explorer_in_tab() {
            self.show_pane(pane);
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                if let Some(dir) = dir {
                    explorer.root_at(dir);
                }
                if let Some(path) = reveal {
                    explorer.reveal(path);
                }
            }
            return;
        }
        let request = Request::SplitTab {
            tab: shown.tab,
            side: Side::Left,
            terminal: PaneTerminal::Files(OpenFiles { dir }),
            share: Some(self.explorer_share()),
        };
        self.request(request, Asked::Explorer(reveal));
    }

    /// The explorer `pane`, made at the window's asking, is to show the
    /// file at `reveal`, once it can.
    pub(super) fn explorer_made(&mut self, pane: PaneId, reveal: Option<String>) {
        self.show_pane(pane);
        if let (Some(explorer), Some(path)) = (self.explorers.panes.get_mut(&pane), reveal) {
            explorer.reveal(path);
        }
    }

    /// Whether `pane` is an explorer, which is always in normal mode.
    pub(super) fn is_explorer(&self, pane: PaneId) -> bool {
        self.explorers.panes.contains_key(&pane)
    }
}
