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
//!
//! An editor showing a folder (`:e .`, `:e DIR`, `:Editor`) is a pane of
//! the daemon's, [`Content::Folder`], with an explorer of its own that
//! shows only listings, as oil.nvim's buffers do: Enter on a file opens it
//! in the pane, in place of the listing, as `:e` would, and `-` in a file
//! shows its folder's listing, with the cursor on the file. Going into a
//! folder, up from one, and from a listing to a file are jumps, which
//! Ctrl+O comes back through. A listing with changes isn't left, nor
//! closed, until they're made or thrown away. A listing whose text hasn't
//! changed is made afresh whenever its folder changes on disk, as after
//! its edits are made.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    BufferId, ClientMessage, Content, Event, FileOp, FileOpError, FilesId, FilesUpdate, OpenFile,
    OpenFiles, PaneId, PaneTerminal, Request, Side, Size,
};

use super::jumps::FolderPlace;
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
    /// The entries the cursors of panes about to show folders go to,
    /// once their listings are made: as `-` in a file lands on the file.
    landings: HashMap<PaneId, Option<String>>,
}

/// The editors of the folders' listings one explorer is editing.
#[derive(Debug, Default)]
struct Listings {
    /// Each listing's buffer, by its place, as the explorer numbers them:
    /// none for one left with its text as it was made, which settles, its
    /// editor thrown away, until it's shown again.
    buffers: Vec<Option<BufferId>>,
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
        let panes = |workspace: &clackatoo_protocol::Workspace| -> Vec<(PaneId, FilesId, bool)> {
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter_map(|pane| {
                    let files = pane.content.files()?;
                    Some((pane.id, files, pane.content.is_folder()))
                })
                .collect()
        };
        let anywhere: HashMap<PaneId, FilesId> = (self.workspaces.list.iter())
            .flat_map(panes)
            .map(|(pane, files, _)| (pane, files))
            .collect();
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
        for &pane in &gone {
            let explorer = explorers.panes.remove(&pane).expect("just found");
            // An editor's folder isn't a tree to show again.
            if let Some(root) = explorer.root()
                && !explorer.is_folder()
            {
                explorers.kept.insert(root.to_owned(), explorer.view());
            }
        }
        // Its listing's editor goes with it, as the pane may stay, showing
        // another folder.
        for &pane in &gone {
            self.take_listing_editor(pane);
        }
        let explorers = &mut self.explorers;
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
        explorers
            .landings
            .retain(|pane, _| workspaces.find_pane(*pane).is_some());
        for &(pane, files, folder) in &shown {
            let landings = &mut explorers.landings;
            explorers.panes.entry(pane).or_insert_with(|| {
                if folder {
                    let at = landings.remove(&pane).flatten();
                    Explorer::of_folder(files, &self.options, at)
                } else {
                    Explorer::new(files, &self.options)
                }
            });
        }
        if !self.connected {
            return;
        }
        let explorers = &mut self.explorers;
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
            && !explorer.is_folder()
            && let Some(root) = explorer.root()
            && let Some(view) = explorers.kept.remove(root)
        {
            explorer.restore(view);
        }
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        self.settle_gone(pane);
        self.list_if_stale(pane);
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
            explorer.answered(id, op, outcome);
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
            // An editor's folder opens the file in its own pane.
            Ask::Open {
                path,
                how: Opening::Here,
            } if self.is_folder(pane) => {
                self.open_here(pane, path);
            }
            Ask::Open { path, how } => self.open_from_explorer(pane, path, how),
            Ask::Op(op) => self.file_op(pane, op),
            Ask::Step(op) => {
                let sent = self.send_file_op(pane, op);
                if let Some(explorer) = self.explorers.panes.get_mut(&pane) {
                    match sent {
                        Ok(id) => explorer.step_sent(id),
                        Err(why) => explorer.step_unsent(why),
                    }
                }
            }
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
                        expect: None,
                    }
                } else {
                    FileOp::Copy {
                        from,
                        to,
                        overwrite: None,
                        expect: None,
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
            Ask::List { dir, at } => self.list_again(pane, dir, at.as_deref()),
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
        // A settled listing is made again as it was made.
        let as_made = text.or_else(|| explorer.listing_as_made(listing).map(str::to_owned));
        self.leave_listing(pane, listing);
        let explorers = &mut self.explorers;
        let listings = explorers.listings.entry(pane).or_default();
        let kept = (listings.buffers.get(listing).copied().flatten())
            .and_then(|buffer| listings.waiting.remove(&buffer));
        let (mut editor, copy) = match kept {
            Some(kept) => kept,
            None if listing <= listings.buffers.len() => {
                // Numbered down from the top, where the daemon's never get.
                let buffer = BufferId(u32::MAX - explorers.scratch_buffers);
                explorers.scratch_buffers += 1;
                if listing == listings.buffers.len() {
                    listings.buffers.push(Some(buffer));
                } else {
                    listings.buffers[listing] = Some(buffer);
                }
                let copy = BufferCopy::scratch(buffer, &dir, &as_made.unwrap_or_default());
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
        // What the explorer says of the listing, as of names left out.
        if let Some(message) =
            (self.explorers.panes.get_mut(&pane)).and_then(Explorer::take_message)
        {
            self.message = Some(message);
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

    /// Takes the editor of the listing the explorer `pane` shows from it,
    /// as listing `next` is to be shown: it waits, unless its text is as
    /// it was made, when its folder settles and it's thrown away, so a
    /// long browse keeps neither editors nor the daemon's watches.
    fn leave_listing(&mut self, pane: PaneId, next: usize) {
        let Some((editor, copy)) = self.take_listing_editor(pane) else {
            return;
        };
        let listings = self.explorers.listings.entry(pane).or_default();
        let place = (listings.buffers.iter()).position(|&buffer| buffer == Some(copy.buffer()));
        let explorer = self.explorers.panes.get_mut(&pane);
        if let (Some(place), Some(explorer)) = (place, explorer)
            && place != next
            && explorer.listing_as_made(place) == Some(copy.text().to_string().as_str())
        {
            explorer.settle_listing(place);
            listings.buffers[place] = None;
            return;
        }
        listings.waiting.insert(copy.buffer(), (editor, copy));
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
        let explorer = self.explorers.panes.get(&pane);
        (listings.buffers.iter().enumerate())
            .map(|(place, &buffer)| {
                let Some(buffer) = buffer else {
                    // Settled, as it was made.
                    let made = explorer.and_then(|explorer| explorer.listing_as_made(place));
                    return made.unwrap_or_default().to_owned();
                };
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
        let folder = explorer.is_folder();
        match explorer.plan(&texts) {
            Planned::Nothing if quit && folder => {
                self.request(Request::ClosePane { pane }, Asked::Command);
            }
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

    /// `:q` in an explorer editing folders as text: back to the tree, or
    /// for an editor's folder, closing the pane, unless a listing has been
    /// changed, which `force` throws away.
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
        if self.is_folder(pane) {
            return self.request(Request::ClosePane { pane }, Asked::Command);
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

    /// Enter, `-` and `g.`, in normal mode, in an explorer editing folders
    /// as text, or an editor showing a folder, as oil.nvim has them: Enter
    /// on a folder's line shows its listing, and on a file's opens it; `-`
    /// shows the folder above's; `g.` shows hidden files, or hides them.
    /// In an editor showing a file, `-` shows its folder's listing. Returns
    /// whether `press` was one of them.
    pub(super) fn listing_key(&mut self, press: &crate::input::KeyPress<'_>) -> bool {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let Some((editor, _)) = self.focused_editor() else {
            return false;
        };
        let normal = editor.mode == Mode::Normal && !editor.prompting();
        let after_g = editor.typed_so_far()
            == [Stroke::Char {
                ch: 'g',
                ctrl: false,
            }];
        let pending = editor.pending();
        if !normal {
            return false;
        }
        let strokes = crate::editor::strokes(press, false);
        let char = |ch| [Stroke::Char { ch, ctrl: false }];
        if self.is_explorer(pane) && after_g && strokes == char('.') {
            self.with_editor(pane, |editor, _| editor.forget_typed());
            self.toggle_listed_hidden(pane);
            return true;
        }
        if pending {
            return false;
        }
        if !self.is_explorer(pane) {
            return self.file_up_key(pane, &strokes);
        }
        if strokes != [Stroke::Enter] && strokes != char('-') {
            return false;
        }
        self.listing_go(pane, strokes == [Stroke::Enter])
    }

    /// Opens what the line under the cursor of the explorer or folder
    /// `pane` names, as Enter does there, and a double click. Returns
    /// whether `pane` shows a listing to open from.
    pub(super) fn listing_enter(&mut self, pane: PaneId) -> bool {
        self.is_explorer(pane) && self.listing_go(pane, true)
    }

    /// Goes from the listing the editor `pane` shows: into what the
    /// cursor's line names, if `enter`, or else up a folder.
    fn listing_go(&mut self, pane: PaneId, enter: bool) -> bool {
        let line = self.listing_cursor_line(pane).unwrap_or_default();
        // Where it was, for the jump list, if it goes anywhere.
        let from = (self.is_folder(pane))
            .then(|| self.place_of(pane, None))
            .flatten();
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return false;
        };
        if enter {
            explorer.enter_line(&line);
        } else {
            explorer.edit_above();
        }
        if let Some(message) = explorer.take_message() {
            self.message = Some(message);
        }
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
        if let Some(from) = from
            && self.went_from(pane, &from)
        {
            self.record_place(from);
        }
        true
    }

    /// Whether the editor `pane`, showing a folder, has gone from `from`,
    /// or is on its way: to another folder's listing, or a file.
    fn went_from(&self, pane: PaneId, from: &crate::desk::jumps::Jump) -> bool {
        let waiting = (self.explorers.panes.get(&pane)).is_some_and(Explorer::waiting_for_listing);
        let now = self.place_of(pane, None);
        waiting || self.is_opening(pane) || now.is_some_and(|now| now.folder != from.folder)
    }

    /// The line the cursor's on in the listing the editor `pane` shows,
    /// without its line break.
    fn listing_cursor_line(&self, pane: PaneId) -> Option<String> {
        let editor = self.editor(pane)?;
        let copy = self.buffer_copy(editor.buffer())?;
        let text = copy.text();
        let byte = editor.cursor_byte(copy).min(text.len_bytes());
        let line = text.line(text.byte_to_line(byte)).to_string();
        Some(line.trim_end_matches('\n').to_owned())
    }

    /// Asks the daemon, before anything sent after, to root the editor
    /// `pane`'s folder at the folder it shows, if it shows one, as what's
    /// sent may be from there: the finder's search, a relative path, a new
    /// pane beside it.
    pub(super) fn root_folder_first(&mut self, pane: PaneId) {
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        if !explorer.is_folder() {
            return;
        }
        explorer.root_at_shown();
        let files = explorer.files();
        let actions = explorer.take_outbox();
        if self.connected {
            let messages =
                (actions.into_iter()).map(|action| ClientMessage::Files { files, action });
            self.outbox.extend(messages);
        }
    }

    /// `:e!` in an editor showing a folder: throws away every listing's
    /// changes, listing the folder shown afresh.
    pub(super) fn list_shown_again(&mut self, pane: PaneId) {
        if (self.explorers.panes.get(&pane)).is_some_and(|explorer| !explorer.editing_text()) {
            self.message = Some("y makes the changes, n goes back to the listing".into());
            return;
        }
        let at = self.path_at_listing_cursor(pane);
        let Some(dir) = (self.explorers.panes.get(&pane))
            .and_then(Explorer::folder_shown)
            .map(str::to_owned)
        else {
            return;
        };
        self.list_again(pane, dir, at.as_deref());
    }

    /// `-` in normal mode in the editor `pane`, showing a file, as
    /// `strokes`: shows the listing of the folder the file is in, in the
    /// pane, in place of the file, with the cursor on the file. Returns
    /// whether it was `-`.
    fn file_up_key(&mut self, pane: PaneId, strokes: &[Stroke]) -> bool {
        let up = [Stroke::Char {
            ch: '-',
            ctrl: false,
        }];
        if strokes != up || self.is_lesson_pane(pane) {
            return false;
        }
        let Some(path) = self.file_in(pane).map(str::to_owned) else {
            return false;
        };
        let Some((dir, _)) = path.rsplit_once('/') else {
            return false;
        };
        let from = self.place_of(pane, None);
        if self.open_here(pane, folder_path(dir)) {
            self.explorers.landings.insert(pane, Some(path));
            if let Some(from) = from {
                self.record_place(from);
            }
        }
        true
    }

    /// `g.` in a folder's listing in `pane`: shows hidden files, or hides
    /// them, listing the folder shown afresh, unless a listing has changes,
    /// which would be lost.
    fn toggle_listed_hidden(&mut self, pane: PaneId) {
        if self.listings_changed(pane) {
            self.message = Some(
                "the listings have changes: :w makes them, or :e! throws them away, before g."
                    .into(),
            );
            return;
        }
        let at = self.path_at_listing_cursor(pane);
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        let Some(dir) = explorer.folder_shown().map(str::to_owned) else {
            return;
        };
        explorer.toggle_hidden();
        if let Some(message) = explorer.take_message() {
            self.message = Some(message);
        }
        self.list_again(pane, dir, at.as_deref());
    }

    /// The path of the entry on the cursor's line in the listing `pane`
    /// shows, if it's on one.
    fn path_at_listing_cursor(&self, pane: PaneId) -> Option<String> {
        let editor = self.editor(pane)?;
        let copy = self.buffer_copy(editor.buffer())?;
        let text = copy.text();
        let byte = editor.cursor_byte(copy).min(text.len_bytes());
        let line = text.line(text.byte_to_line(byte)).to_string();
        self.explorers.panes.get(&pane)?.path_of_line(&line)
    }

    /// Shows the listing of the folder `dir` in `pane` afresh, with the
    /// cursor on the line of the entry at `at`, if it's there: the editors
    /// of every listing it was editing are thrown away, though not the
    /// session their lines' marks name (see [`Explorer::list_afresh`]).
    fn list_again(&mut self, pane: PaneId, dir: String, at: Option<&str>) {
        self.take_listing_editor(pane);
        self.explorers.listings.remove(&pane);
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        explorer.list_afresh(dir, at);
        let asks = explorer.take_asks();
        for ask in asks {
            self.explorer_asked(pane, ask);
        }
    }

    /// Lists the folder `pane` shows afresh, keeping the cursor on its
    /// entry, if a folder whose listing is made has changed on disk since,
    /// and no listing's text has: so a file made, or the changes just
    /// made, show. Not while a change is being typed, as in insert mode,
    /// which it would cut off.
    fn list_if_stale(&mut self, pane: PaneId) {
        let stale = (self.explorers.panes.get(&pane))
            .is_some_and(|explorer| explorer.editing_text() && explorer.listings_stale());
        let quiet = self.editor(pane).is_some_and(|editor| {
            editor.mode == Mode::Normal && !editor.pending() && !editor.prompting()
        });
        if !stale || !quiet || self.listings_changed(pane) {
            return;
        }
        let Some(dir) = (self.explorers.panes.get(&pane))
            .and_then(Explorer::folder_shown)
            .map(str::to_owned)
        else {
            return;
        };
        let at = self.path_at_listing_cursor(pane);
        self.list_again(pane, dir, at.as_deref());
    }

    /// Settles the listings of the explorer `pane` whose folders have gone
    /// from disk, if they have no changes: one with changes stays gone, so
    /// nothing is done of its listings' edits.
    fn settle_gone(&mut self, pane: PaneId) {
        let gone = (self.explorers.panes.get(&pane)).map_or_else(Vec::new, Explorer::gone_listings);
        if gone.is_empty() {
            return;
        }
        let texts = self.listing_texts(pane);
        let Some(explorer) = self.explorers.panes.get_mut(&pane) else {
            return;
        };
        for place in gone {
            if texts.get(place).map(String::as_str) == explorer.listing_as_made(place) {
                explorer.settle_listing(place);
            }
        }
    }

    /// Whether `pane` is an editor showing a folder.
    pub(super) fn is_folder(&self, pane: PaneId) -> bool {
        (self.explorers.panes.get(&pane)).is_some_and(Explorer::is_folder)
    }

    /// Why the editor `pane`, showing a folder, can't show anything else
    /// yet, if it can't: what its listings' edits come to is shown, or
    /// they have changes, which would be lost.
    pub(super) fn keeps_its_folder(&self, pane: PaneId) -> Option<&'static str> {
        let explorer = self.explorers.panes.get(&pane)?;
        if !explorer.is_folder() {
            return None;
        }
        if explorer.editing() && !explorer.editing_text() {
            return Some("y makes the changes, n goes back to the listing");
        }
        self.listings_changed(pane)
            .then_some("the listing has changes: :w makes them, :e! throws them away")
    }

    /// Where the editor `pane`'s cursor is, if it shows a folder: the
    /// folder shown, and the entry on the cursor's line.
    pub(super) fn folder_place(&self, pane: PaneId) -> Option<FolderPlace> {
        let explorer = self.explorers.panes.get(&pane)?;
        if !explorer.is_folder() {
            return None;
        }
        let dir = (explorer.folder_shown()).or_else(|| explorer.root())?;
        Some(FolderPlace {
            dir: dir.to_owned(),
            entry: self.path_at_listing_cursor(pane),
        })
    }

    /// Shows the folder of `place` in the editor `pane`, with the cursor on
    /// its entry: in the listings of the folder it shows, if it shows one,
    /// or else in place of its file, as `:e` would.
    pub(super) fn show_folder_place(&mut self, pane: PaneId, place: &FolderPlace) {
        // Not while what its listings' edits come to is shown, or being done.
        if let Some(explorer) = self.explorers.panes.get(&pane)
            && explorer.editing()
            && !explorer.editing_text()
        {
            self.message = Some("y makes the changes, n goes back to the listing".into());
            return;
        }
        if let Some(explorer) = self.explorers.panes.get_mut(&pane)
            && explorer.is_folder()
        {
            explorer.edit_folder(place.dir.clone(), place.entry.as_deref());
            if let Some(message) = explorer.take_message() {
                self.message = Some(message);
            }
            let asks = explorer.take_asks();
            for ask in asks {
                self.explorer_asked(pane, ask);
            }
            return;
        }
        // As a folder, so one gone since isn't made a file of that name.
        if self.takes_files(pane) && self.open_here(pane, folder_path(&place.dir)) {
            self.explorers.landings.insert(pane, place.entry.clone());
        }
    }

    /// The editor `pane`'s cursor won't go to an entry of a folder it was
    /// about to show.
    pub(super) fn forget_folder_landing(&mut self, pane: PaneId) {
        self.explorers.landings.remove(&pane);
    }

    /// Asks the daemon to do `op`, for the explorer `pane`, or says why
    /// it can't.
    pub(super) fn file_op(&mut self, pane: PaneId, op: FileOp) {
        if let Err(why) = self.send_file_op(pane, op)
            && let Some(explorer) = self.explorers.panes.get_mut(&pane)
        {
            explorer.say(why);
        }
    }

    /// Sends the daemon `op`, for the explorer `pane`: its number, which
    /// the answer comes with, or why it can't be sent.
    fn send_file_op(&mut self, pane: PaneId, op: FileOp) -> Result<u64, String> {
        if !self.connected {
            return Err("not connected: nothing was done".to_owned());
        }
        // A lesson's explorer touches nothing outside the lesson's folder.
        if let Some(explorer) = self.explorers.panes.get(&pane) {
            let paths: Vec<&str> = match &op {
                FileOp::Create { path, .. } | FileOp::Delete { path, .. } => vec![path],
                FileOp::Move { from, to, .. } | FileOp::Copy { from, to, .. } => vec![from, to],
            };
            if paths.iter().any(|path| explorer.fenced_out(path)) {
                return Err(crate::explorer::FENCED.to_owned());
            }
        }
        let id = self.explorers.next_op;
        self.explorers.next_op += 1;
        self.explorers.ops.insert(id, (pane, op.clone()));
        self.outbox.push(ClientMessage::FileOp { id, op });
        Ok(id)
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

/// The path of the folder `dir` as the daemon is asked to open it, as a
/// folder only: ending `/`, which a file's path never does, and the root
/// of everything if it's empty.
fn folder_path(dir: &str) -> String {
    if dir.ends_with('/') {
        dir.to_owned()
    } else if dir.is_empty() {
        "/".to_owned()
    } else {
        format!("{dir}/")
    }
}
