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
//!
//! An explorer editing folders as text (see [`crate::explorer`]'s
//! `editing`) shows an editor in its pane, one of the editors the desk
//! keeps, on a buffer only the window has: a scratch copy, answering its
//! own changes. Each folder's listing has its own; those not shown wait
//! here, with their cursors and undo, and all of them while what their
//! edits come to is shown in the pane.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    BufferId, ClientMessage, Content, Event, FileOp, FileOpError, FilesId, FilesUpdate, OpenFile,
    OpenFiles, PaneId, PaneTerminal, Request, Side, Size,
};

use super::{Asked, Desk};
use crate::editor::{BufferCopy, Editor, Stroke};
use crate::explorer::{Ask, Explorer, Opening, Planned, View, listing};
use crate::input::KeyPress;
use crate::layout::Placement;
use crate::pane::Mode;

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
    /// The editors of the folders' listings explorers are editing, by
    /// their panes.
    listings: HashMap<PaneId, Listings>,
    /// How many scratch buffers have been numbered.
    scratch_buffers: u32,
}

/// The editors of the folders' listings one explorer is editing.
#[derive(Debug, Default)]
struct Listings {
    /// Each listing's buffer, by its place, as the explorer numbers them.
    buffers: Vec<BufferId>,
    /// The editors of those not shown, and their copies, by their buffers.
    waiting: HashMap<BufferId, (Editor, BufferCopy)>,
}

impl Desk {
    /// The explorer `pane` is, if it's one.
    pub(crate) fn explorer(&self, pane: PaneId) -> Option<&Explorer> {
        self.explorers.panes.get(&pane)
    }

    /// Keeps the explorer `pane` in `dir`, if it's rooted there and not
    /// kept somewhere already, as a lesson's explorer is kept in the
    /// lesson's folder.
    pub(super) fn fence_explorer(&mut self, pane: PaneId, dir: &str) {
        if let Some(explorer) = self.explorers.panes.get_mut(&pane)
            && !explorer.fenced()
            && explorer
                .root()
                .is_some_and(|root| std::path::Path::new(root).starts_with(dir))
        {
            explorer.fence(dir.to_owned());
        }
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
            .listings
            .retain(|pane, _| explorers.panes.contains_key(pane));
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

    /// Lays out the rows of the explorers in the tab shown, and of the
    /// reviews.
    pub(crate) fn settle_explorers(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        for pane in shown {
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                explorer.settle();
            }
        }
        self.settle_reviews();
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
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        self.settle_lesson();
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
            // The next of the steps a listing's edits come to.
            let asks = explorer.take_asks();
            for ask in asks {
                self.explorer_asked(pane, ask);
            }
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
    /// review and one is, as in an explorer. Returns whether it did.
    pub(super) fn stop_recording_in_review(&mut self, press: &KeyPress<'_>) -> bool {
        let quiet = self
            .focused_review()
            .is_some_and(|review| !review.pending());
        let q = crate::keymap::Chord::of(press) == Some(crate::keymap::Chord::typed('q'));
        if quiet && q && self.recording().is_some() {
            self.shared_mut().registers.stop_recording();
            return true;
        }
        false
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
            Ask::Edit { listing, text, at } => {
                self.show_listing(pane, listing, text, at.as_deref());
            }
            Ask::BackToText => {
                let shown = (self.explorers.panes.get(&pane)).and_then(Explorer::listing_shown);
                if let Some(listing) = shown {
                    self.show_listing(pane, listing, None, None);
                }
            }
            Ask::StopEditing => self.drop_listings(pane),
        }
    }

    // --- Folders as text ----------------------------------------------------

    /// Shows the explorer `pane`'s listing number `listing` in an editor in
    /// its pane, with the cursor on the line starting with the mark `at`,
    /// if one does: the editor kept for it, or a new one, of `text`. The
    /// one shown waits.
    fn show_listing(
        &mut self,
        pane: PaneId,
        listing: usize,
        text: Option<String>,
        at: Option<&str>,
    ) {
        let Some(explorer) = self.explorers.panes.get(&pane) else {
            return;
        };
        let size = explorer.size();
        let dir = explorer.folder_shown().unwrap_or_default().to_owned();
        if let Some((editor, copy)) = self.take_listing_editor(pane) {
            let listings = self.explorers.listings.entry(pane).or_default();
            listings.waiting.insert(copy.buffer(), (editor, copy));
        }
        let explorers = &mut self.explorers;
        let listings = explorers.listings.entry(pane).or_default();
        let kept =
            (listings.buffers.get(listing)).and_then(|buffer| listings.waiting.remove(buffer));
        let (mut editor, copy) = match kept {
            Some(kept) => kept,
            None if listing == listings.buffers.len() => {
                // Numbered down from the top, where the daemon's never get.
                let buffer = BufferId(u32::MAX - explorers.scratch_buffers);
                explorers.scratch_buffers += 1;
                listings.buffers.push(buffer);
                let copy = BufferCopy::scratch(buffer, &dir, &text.unwrap_or_default());
                let mut editor = Editor::new(buffer, &self.options);
                editor.hide_starts(listing::mark_len);
                (editor, copy)
            }
            None => {
                log::error!("explorer listing {listing} isn't one made");
                return;
            }
        };
        if let Some(size) = size {
            editor.resize(size);
        }
        if let Some(at) = at {
            let text = copy.text();
            let line =
                (0..text.len_lines()).find(|&line| text.line(line).to_string().starts_with(at));
            if let Some(line) = line {
                editor.go_to(line as u64, 0, &copy);
            }
        }
        self.put_listing_editor(pane, editor, copy);
    }

    /// Stops the explorer `pane` editing folders as text, throwing away
    /// the editors and what was typed in them.
    fn drop_listings(&mut self, pane: PaneId) {
        self.take_listing_editor(pane);
        self.explorers.listings.remove(&pane);
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            explorer.stop_editing();
        }
    }

    /// The texts of the explorer `pane`'s listings, by their places.
    fn listing_texts(&self, pane: PaneId) -> Vec<String> {
        let Some(listings) = self.explorers.listings.get(&pane) else {
            return Vec::new();
        };
        (listings.buffers.iter())
            .map(|&buffer| {
                (listings.waiting.get(&buffer))
                    .map(|(_, copy)| copy.text().to_string())
                    .or_else(|| self.scratch_text(buffer))
                    .unwrap_or_default()
            })
            .collect()
    }

    /// Whether any of the explorer `pane`'s listings has been changed.
    fn listings_changed(&self, pane: PaneId) -> bool {
        let Some(explorer) = self.explorers.panes.get(&pane) else {
            return false;
        };
        (self.listing_texts(pane).iter().enumerate())
            .any(|(listing, text)| explorer.listing_as_made(listing) != Some(text.as_str()))
    }

    /// `:w` in an explorer editing folders as text: shows what the edits
    /// to its listings come to, asking whether to do it, or why it can't
    /// be done, at the line that says so. Leaves, with `:wq`, if nothing's
    /// changed.
    pub(super) fn write_listings(&mut self, pane: PaneId, quit: bool) {
        let texts = self.listing_texts(pane);
        let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        if !explorer.editing_text() {
            self.message = Some("y makes the changes, n goes back to the listing".into());
            return;
        }
        match explorer.plan(&texts) {
            Planned::Nothing if quit => self.drop_listings(pane),
            Planned::Nothing => self.message = Some("nothing has changed".into()),
            Planned::Asking if !self.connected => {
                if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                    explorer.back_to_text();
                }
                self.message = Some("not connected: nothing can be changed".into());
            }
            Planned::Asking => {
                if let Some((editor, copy)) = self.take_listing_editor(pane) {
                    let listings = self.explorers.listings.entry(pane).or_default();
                    listings.waiting.insert(copy.buffer(), (editor, copy));
                }
            }
            Planned::Refused { at, why } => {
                if let Some((listing, line)) = at
                    && let Some(explorer) = self.explorers.panes.get_mut(&pane)
                    && let Some(dir) = explorer.folder_of(listing)
                {
                    explorer.edit_folder(dir, None);
                    let asks = explorer.take_asks();
                    for ask in asks {
                        self.explorer_asked(pane, ask);
                    }
                    self.with_editor(pane, |editor, copy| editor.go_to(line, 0, copy));
                }
                self.message = Some(format!("can't: {why}"));
            }
        }
    }

    /// `:q` in an explorer editing folders as text: back to the tree,
    /// unless a listing has been changed, which `force` throws away.
    pub(super) fn leave_listings(&mut self, pane: PaneId, force: bool) {
        if (self.explorers.panes.get(&pane)).is_some_and(|explorer| !explorer.editing_text()) {
            self.message = Some("y makes the changes, n goes back to the listing".into());
            return;
        }
        if !force && self.listings_changed(pane) {
            self.message =
                Some("the listings have changes: :w makes them, :q! throws them away".into());
            return;
        }
        self.drop_listings(pane);
    }

    /// `:e!` in an explorer editing folders as text: puts the listing shown
    /// back as it was made.
    pub(super) fn listing_as_made(&mut self, pane: PaneId) {
        let Some(explorer) = self.explorers.panes.get(&pane) else {
            return;
        };
        let Some(text) = (explorer.listing_shown())
            .and_then(|listing| explorer.listing_as_made(listing))
            .map(str::to_owned)
        else {
            return;
        };
        let Some(buffer) = self.editor(pane).map(Editor::buffer) else {
            return;
        };
        self.replace_text(buffer, &text);
        self.answer_scratch();
    }

    /// Enter and `-`, in normal mode, in an explorer editing folders as
    /// text, as oil.nvim has them: Enter on a folder's line shows its
    /// listing, and on a file's opens it; `-` shows the folder above's.
    /// Returns whether `press` was one of them.
    pub(super) fn listing_key(&mut self, press: &crate::input::KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        if !self.is_explorer(pane) {
            return false;
        }
        let Some((editor, copy)) = self.focused_editor() else {
            return false;
        };
        if editor.mode != Mode::Normal || editor.pending() || editor.prompting() {
            return false;
        }
        let strokes = crate::editor::strokes(press, false);
        let line = {
            let text = copy.text();
            let byte = editor.cursor_byte(copy).min(text.len_bytes());
            let line = text.line(text.byte_to_line(byte)).to_string();
            line.trim_end_matches('\n').to_owned()
        };
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return false;
        };
        match strokes.as_slice() {
            [Stroke::Enter] => explorer.enter_line(&line),
            [
                Stroke::Char {
                    ch: '-',
                    ctrl: false,
                },
            ] => explorer.edit_above(),
            _ => return false,
        }
        if let Some(message) = explorer.take_message() {
            self.message = Some(message);
        }
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        true
    }

    /// Asks the daemon to do `op`, for the explorer `pane`.
    pub(super) fn file_op(&mut self, pane: PaneId, op: FileOp) {
        if !self.connected {
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                let why = "not connected: nothing was done".to_owned();
                explorer.answered(op, Err(FileOpError::Failed(why)));
            }
            return;
        }
        // A lesson's explorer touches nothing outside the lesson's folder.
        if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
            let paths: Vec<&str> = match &op {
                FileOp::Create { path, .. } | FileOp::Delete { path, .. } => vec![path],
                FileOp::Move { from, to, .. } | FileOp::Copy { from, to, .. } => vec![from, to],
            };
            if paths.iter().any(|path| explorer.fenced_out(path)) {
                explorer.say(crate::explorer::FENCED.to_owned());
                return;
            }
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
        let editor = self.recent_in_tab(explorer, Self::takes_files);
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
    pub(super) fn recent_in_tab(
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
    pub(super) fn explorer_share(&self) -> u16 {
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
            // Not while what's typed in its listings would be lost.
            Some(pane)
                if self.shown.is_some_and(|shown| shown.pane == pane)
                    && self.listings_changed(pane) =>
            {
                self.message =
                    Some("the listings have changes: :w makes them, :q! throws them away".into());
            }
            Some(pane) if self.shown.is_some_and(|shown| shown.pane == pane) => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
            _ => self.open_explorer(None, false),
        }
    }

    /// `:Explore`: goes to the tab's explorer, rooting it at `dir` if
    /// given, or opens one down the tab's left, rooted at `dir`, or the
    /// focused pane's project. Either way, from an editor, it shows the
    /// editor's file. With `edit`, as `:Explore!`, it edits the folder
    /// the cursor's in as text, or with `dir`, the root.
    pub(super) fn open_explorer(&mut self, dir: Option<String>, edit: bool) {
        let Some(shown) = self.shown else { return };
        let reveal = self.file_in(shown.pane).map(str::to_owned);
        if let Some(pane) = self.explorer_in_tab() {
            self.show_pane(pane);
            if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                if edit && explorer.editing() {
                    return;
                }
                let rooting = dir.is_some();
                if let Some(dir) = dir {
                    explorer.root_at(dir);
                }
                if let Some(path) = reveal {
                    explorer.reveal(path);
                }
                if edit {
                    explorer.edit_soon(rooting);
                    let asks = explorer.take_asks();
                    for ask in asks {
                        self.explorer_asked(pane, ask);
                    }
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
        self.request(request, Asked::Explorer { reveal, edit });
    }

    /// The explorer `pane`, made at the window's asking, is to show the
    /// file at `reveal`, once it can, and edit the folder it's in, or its
    /// root, as text if `edit`.
    pub(super) fn explorer_made(&mut self, pane: PaneId, reveal: Option<String>, edit: bool) {
        self.show_pane(pane);
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        let rooted = reveal.is_none();
        if let Some(path) = reveal {
            explorer.reveal(path);
        }
        if edit {
            explorer.edit_soon(rooted);
        }
    }

    /// Whether `pane` is an explorer, which is always in normal mode.
    pub(super) fn is_explorer(&self, pane: PaneId) -> bool {
        self.explorers.panes.contains_key(&pane)
    }
}
