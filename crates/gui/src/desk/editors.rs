//! The desk's editors: panes editing files, the window's copies of the
//! buffers they show, and the commands for files.
//!
//! The buffers of the workspace shown are attached, as its terminals are.
//! Every editor of one buffer shares the window's copy of it, so an edit
//! made in one, or by anyone else, moves the others' cursors with the text.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{
    BufferId, BufferUpdate, ClientMessage, Content, OpenFile, PaneId, PaneTerminal, Request, Size,
};

use super::{Asked, Desk};
use crate::command::Place;
use crate::editor::{BufferCopy, Editor, Register};
use crate::input::KeyPress;
use crate::pane::{Copy, Mode};

/// The editors, by the pane each is, and the buffers they show.
#[derive(Debug, Default)]
pub(super) struct Editors {
    panes: HashMap<PaneId, Editor>,
    buffers: HashMap<BufferId, BufferCopy>,
    /// What `p` puts, shared by every editor, as Vim's registers are.
    register: Register,
    /// The buffers attached to on this connection.
    attached: HashSet<BufferId>,
}

impl Editors {
    /// Forgets what was attached, as a new connection does. Changes not
    /// yet answered are lost with the old one.
    pub(super) fn reconnected(&mut self) {
        self.attached.clear();
        for copy in self.buffers.values_mut() {
            copy.reconnected();
        }
    }
}

impl Desk {
    /// The editor `pane` is, if it's one.
    pub(crate) fn editor(&self, pane: PaneId) -> Option<&Editor> {
        self.editors.panes.get(&pane)
    }

    /// The focused pane's editor and buffer, if it's an editor.
    pub(super) fn focused_editor(&self) -> Option<(&Editor, &BufferCopy)> {
        let editor = self.editors.panes.get(&self.shown?.pane)?;
        Some((editor, self.editors.buffers.get(&editor.buffer())?))
    }

    /// Makes an editor for every pane showing a buffer in the workspace
    /// shown, and attaches to their buffers; forgets the editors of panes
    /// gone, and the buffers no editor shows.
    pub(super) fn attach_shown_buffers(&mut self) {
        let panes = |workspace: &clackatoo_protocol::Workspace| -> Vec<(PaneId, BufferId)> {
            (workspace.tabs.iter())
                .flat_map(|tab| tab.layout.panes())
                .filter_map(|pane| Some((pane.id, pane.content.buffer()?)))
                .collect()
        };
        let anywhere: HashMap<PaneId, BufferId> =
            self.workspaces.list.iter().flat_map(panes).collect();
        let shown = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(panes)
            .unwrap_or_default();
        let editors = &mut self.editors;
        editors
            .panes
            .retain(|pane, editor| anywhere.get(pane) == Some(&editor.buffer()));
        for &(pane, buffer) in &shown {
            editors
                .panes
                .entry(pane)
                .or_insert_with(|| Editor::new(buffer));
            editors
                .buffers
                .entry(buffer)
                .or_insert_with(|| BufferCopy::new(buffer));
        }
        let wanted: HashSet<BufferId> = editors.panes.values().map(Editor::buffer).collect();
        editors.buffers.retain(|buffer, _| wanted.contains(buffer));
        if !self.connected {
            return;
        }
        let gone: Vec<BufferId> = editors.attached.difference(&wanted).copied().collect();
        let new: Vec<BufferId> = wanted.difference(&editors.attached).copied().collect();
        for buffer in gone {
            self.editors.attached.remove(&buffer);
            self.request(Request::DetachBuffer { buffer }, Asked::Quiet);
        }
        for buffer in new {
            self.editors.attached.insert(buffer);
            self.request(Request::AttachBuffer { buffer }, Asked::Quiet);
        }
    }

    /// Sizes the editor of `pane` to its tile.
    pub(super) fn fit_editor(&mut self, pane: PaneId, size: Size) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.resize(size);
        }
    }

    /// Takes in a change to `buffer`. Returns whether a pane showing it is
    /// shown, and so the window must be redrawn.
    pub(crate) fn buffer_changed(&mut self, buffer: BufferId, update: BufferUpdate) -> bool {
        let focused = self.shown.map(|shown| shown.pane);
        let editors = &mut self.editors;
        let Some(copy) = editors.buffers.get_mut(&buffer) else {
            return false;
        };
        let showing: Vec<PaneId> = (editors.panes.iter())
            .filter(|(_, editor)| editor.buffer() == buffer)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in &showing {
            editors.panes.get_mut(pane).expect("just found").hold(copy);
        }
        let heard = copy.update(update);
        for pane in &showing {
            let editor = editors.panes.get_mut(pane).expect("just found");
            editor.heard(&heard, Some(*pane) == focused, copy);
        }
        self.shown_tab().is_some_and(|tab| {
            (tab.layout.panes().iter()).any(|pane| pane.content == Content::Buffer(buffer))
        })
    }

    /// Hands `press` to the focused pane's editor, moving the cursors of
    /// the other editors of its buffer with any edits it makes. Returns
    /// whether the window must be redrawn.
    pub(super) fn editor_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(focused) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let editors = &mut self.editors;
        let Some(buffer) = editors.panes.get(&focused).map(Editor::buffer) else {
            return false;
        };
        let Some(copy) = editors.buffers.get_mut(&buffer) else {
            return false;
        };
        let others: Vec<PaneId> = (editors.panes.iter())
            .filter(|&(&pane, editor)| pane != focused && editor.buffer() == buffer)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in &others {
            editors.panes.get_mut(pane).expect("just found").hold(copy);
        }
        let editor = editors.panes.get_mut(&focused).expect("just found");
        let done = editor.key(press, copy, &mut editors.register);
        let made = copy.take_made();
        for pane in &others {
            let editor = editors.panes.get_mut(pane).expect("just found");
            for edit in &made {
                editor.follow_edit(edit, copy);
            }
        }
        if let Some(text) = done.yanked {
            self.yanked = Some((text, Copy::Clipboard));
        }
        done.redraw
    }

    /// Pastes `text` into the focused editor. Returns whether there is one.
    pub(super) fn paste_into_editor(&mut self, text: &str) -> bool {
        let Some(focused) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&focused) else {
            return false;
        };
        let Some(copy) = editors.buffers.get_mut(&editor.buffer()) else {
            return true;
        };
        editor.paste(text, copy);
        let _ = copy.take_made();
        true
    }

    /// Puts the focused editor in normal mode, as arriving from a pane in
    /// normal mode does.
    pub(super) fn editor_to_normal_mode(&mut self, pane: PaneId) {
        let editors = &mut self.editors;
        if let Some(editor) = editors.panes.get_mut(&pane)
            && let Some(copy) = editors.buffers.get_mut(&editor.buffer())
        {
            editor.enter_normal_mode(copy);
            let _ = copy.take_made();
        }
    }

    /// Whether `pane` is an editor in a mode other than insert.
    pub(super) fn editor_in_normal_mode(&self, pane: PaneId) -> Option<bool> {
        self.editors
            .panes
            .get(&pane)
            .map(|editor| editor.mode != Mode::Insert)
    }

    /// Scrolls the editor `pane` by `lines`, as the wheel does.
    pub(super) fn editor_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&pane) else {
            return false;
        };
        let Some(copy) = editors.buffers.get(&editor.buffer()) else {
            return false;
        };
        editor.wheel(lines, copy);
        true
    }

    /// Puts the editor `pane`'s cursor where it was clicked.
    pub(super) fn editor_click(&mut self, pane: PaneId, [line, col]: [i64; 2]) -> bool {
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&pane) else {
            return false;
        };
        let Some(copy) = editors.buffers.get(&editor.buffer()) else {
            return false;
        };
        let (Ok(line), Ok(col)) = (usize::try_from(line), usize::try_from(col)) else {
            return false;
        };
        editor.click(line, col, copy);
        true
    }

    /// Lays out the shown editors' lines, before the window is drawn.
    pub(crate) fn settle_editors(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        let editors = &mut self.editors;
        for pane in shown {
            if let Some(editor) = editors.panes.get_mut(&pane)
                && let Some(copy) = editors.buffers.get(&editor.buffer())
            {
                editor.settle(copy);
            }
        }
    }

    /// The soonest an editor's highlight on text just copied goes.
    pub(super) fn editor_flash_ends(&self) -> Option<std::time::Instant> {
        self.editors
            .panes
            .values()
            .filter_map(Editor::flash_ends)
            .min()
    }

    /// The name of the file `pane` edits, for its tab's title.
    pub(super) fn editor_title(&self, pane: PaneId) -> Option<&str> {
        let editor = self.editors.panes.get(&pane)?;
        let copy = self.editors.buffers.get(&editor.buffer())?;
        copy.loaded().then(|| copy.name())
    }

    /// The buffers' messages for the daemon.
    pub(super) fn take_buffer_outbox(&mut self) -> Vec<ClientMessage> {
        let connected = self.connected;
        (self.editors.buffers.values_mut())
            .flat_map(BufferCopy::take_outbox)
            .filter(|_| connected)
            .collect()
    }

    // --- Commands -----------------------------------------------------------

    /// `:e` and `:Editor`: a pane editing the file at `path`, from the
    /// focused pane's directory, in a new tab or beside the focused pane.
    pub(super) fn open_file(&mut self, place: Place, path: String) {
        let Some(shown) = self.shown else { return };
        let terminal = PaneTerminal::File(OpenFile { path, cwd: None });
        let request = match place {
            Place::Tab => Request::NewTab {
                workspace: shown.workspace,
                terminal,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
            },
        };
        self.request(request, Asked::Pane);
    }

    /// `:w`, and `:wq`: writes the focused pane's file, then closes the
    /// pane if `quit`.
    pub(super) fn write_file(&mut self, force: bool, quit: bool) {
        let Some(shown) = self.shown else { return };
        let Some((editor, _)) = self.focused_editor() else {
            self.message = Some("this pane has no file to write".into());
            return;
        };
        let buffer = editor.buffer();
        let then_close = quit.then_some(shown.pane);
        self.request(
            Request::Save { buffer, force },
            Asked::Saved { buffer, then_close },
        );
    }

    /// `:q`: closes the focused pane, unless it's the last showing a file
    /// with changes not saved, or if `force`, throwing them away.
    pub(super) fn quit(&mut self, force: bool) {
        let Some(shown) = self.shown else { return };
        if let Some((editor, copy)) = self.focused_editor() {
            let buffer = editor.buffer();
            let last = !(self.workspaces.list.iter())
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| tab.layout.panes())
                .any(|pane| pane.id != shown.pane && pane.content == Content::Buffer(buffer));
            if last && copy.modified() {
                if !force {
                    self.message = Some(format!(
                        "{} has changes that aren't saved: :w writes them, :q! throws them away",
                        copy.name()
                    ));
                    return;
                }
                self.request(Request::Discard { buffer }, Asked::Command);
            }
        }
        self.request(Request::ClosePane { pane: shown.pane }, Asked::Command);
    }

    /// `:e!`: throws away the focused pane's file's changes not saved.
    pub(super) fn discard_changes(&mut self) {
        match self.focused_editor() {
            Some((editor, _)) => {
                let buffer = editor.buffer();
                self.request(Request::Discard { buffer }, Asked::Command);
            }
            None => self.message = Some("this pane has no file".into()),
        }
    }

    /// The daemon's answer to a save of `buffer`.
    pub(super) fn saved(
        &mut self,
        buffer: BufferId,
        then_close: Option<PaneId>,
        response: &clackatoo_protocol::Response,
    ) {
        if let clackatoo_protocol::Response::Error(err) = response {
            self.message = Some(match err {
                clackatoo_protocol::Error::ChangedOnDisk(path) => {
                    format!("{path} has changed since it was read: :w! writes over it")
                }
                other => other.to_string(),
            });
            return;
        }
        let name = self.editors.buffers.get(&buffer).map(BufferCopy::name);
        self.message = Some(format!("{} written", name.unwrap_or("the file")));
        if let Some(pane) = then_close {
            self.request(Request::ClosePane { pane }, Asked::Command);
        }
    }

    /// Forgets what was attached on the last connection.
    pub(super) fn editors_reconnected(&mut self) {
        self.editors.reconnected();
    }
}
