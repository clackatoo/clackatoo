//! The desk's editors: panes editing files, the window's copies of the
//! buffers they show, and the commands for files.
//!
//! The buffers of the workspace shown are attached, as its terminals are.
//! Every editor of one buffer shares the window's copy of it, so an edit
//! made in one, or by anyone else, moves the others' cursors with the text.

use std::collections::{HashMap, HashSet, VecDeque};

use clackatoo_protocol::{
    BufferId, BufferUpdate, ClientMessage, Content, FileLocation, OpenFile, PaneId, PaneTerminal,
    Request, Size,
};

use super::jumps::{FilePlace, Jump};
use super::{Asked, Desk};
use crate::command::Place;
use crate::editor::{BufferCopy, Editor, Heard, Register};
use crate::input::KeyPress;
use crate::options::Options;
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
    /// Places in files being opened in panes, which their cursors go to
    /// once the files' text has come.
    landings: HashMap<PaneId, FileLocation>,
    /// The files asked to open in panes, in place of theirs, that the
    /// daemon hasn't answered about yet, oldest first. Until it has, what
    /// a pane shows is about to change, so no cursor lands in it.
    opening: HashMap<PaneId, VecDeque<String>>,
}

impl Editors {
    /// Forgets what was attached, as a new connection does. Changes not
    /// yet answered are lost with the old one, as are files being opened.
    pub(super) fn reconnected(&mut self) {
        self.attached.clear();
        self.opening.clear();
        for copy in self.buffers.values_mut() {
            copy.reconnected();
        }
    }

    /// Passes the options on to every editor, as they've changed.
    pub(super) fn set_options(&mut self, options: &Options) {
        for editor in self.panes.values_mut() {
            if let Some(copy) = self.buffers.get(&editor.buffer()) {
                editor.set_options(options, copy);
            }
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
        let (editors, options) = (&mut self.editors, &self.options);
        editors
            .panes
            .retain(|pane, editor| anywhere.get(pane) == Some(&editor.buffer()));
        // Where a closed pane's cursor was to go, it won't.
        editors
            .landings
            .retain(|pane, _| anywhere.contains_key(pane));
        for &(pane, buffer) in &shown {
            editors
                .panes
                .entry(pane)
                .or_insert_with(|| Editor::new(buffer, options));
            editors
                .buffers
                .entry(buffer)
                .or_insert_with(|| BufferCopy::new(buffer));
        }
        let wanted: HashSet<BufferId> = editors.panes.values().map(Editor::buffer).collect();
        editors.buffers.retain(|buffer, _| wanted.contains(buffer));
        // A file opened to show a place in it may be open already.
        self.land();
        if !self.connected {
            return;
        }
        let attached = &self.editors.attached;
        let gone: Vec<BufferId> = attached.difference(&wanted).copied().collect();
        let new: Vec<BufferId> = wanted.difference(attached).copied().collect();
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

    /// Puts the cursors of panes opened to show a place in another file on
    /// it, once the file's text has come, and every file asked to open in
    /// the pane has opened, or failed to.
    fn land(&mut self) {
        let editors = &mut self.editors;
        editors.landings.retain(|pane, place| {
            if editors.opening.contains_key(pane) {
                return true;
            }
            let Some(editor) = editors.panes.get_mut(pane) else {
                return true;
            };
            match editors.buffers.get(&editor.buffer()) {
                Some(copy) if copy.loaded() && copy.path() == place.path => {
                    editor.go_to(place.line, place.col, copy);
                    false
                }
                _ => true,
            }
        });
    }

    /// Puts `pane`'s cursor on `place`, in the file it's opening, once the
    /// file's text has come.
    pub(super) fn land_at(&mut self, pane: PaneId, place: FileLocation) {
        self.editors.landings.insert(pane, place);
        self.land();
    }

    /// `pane` shows `path` in place of its file, as `:e` does, saying so if
    /// the file it showed has changes that aren't written, which are kept,
    /// unseen. Where its cursor was to go before, it won't.
    pub(super) fn open_here(&mut self, pane: PaneId, path: String) {
        self.editors.landings.remove(&pane);
        if self.connected {
            let opening = self.editors.opening.entry(pane).or_default();
            opening.push_back(path.clone());
        }
        let kept = (self.editors.panes.get(&pane))
            .and_then(|editor| self.editors.buffers.get(&editor.buffer()))
            .filter(|copy| copy.modified() && copy.path() != path)
            .map(|copy| copy.path().to_owned());
        if let Some(kept) = kept {
            self.message = Some(format!(
                "{kept} has changes not written, kept unseen: :e it to go back"
            ));
        }
        let file = OpenFile { path, cwd: None };
        self.request(Request::OpenHere { pane, file }, Asked::OpenedHere(pane));
    }

    /// The daemon has answered about the oldest file asked to open in
    /// `pane`. If it `failed` to open, the cursor won't go to a place in
    /// it; a place in a file asked for since is still gone to.
    pub(super) fn opened_here(&mut self, pane: PaneId, failed: bool) {
        let editors = &mut self.editors;
        let Some(opening) = editors.opening.get_mut(&pane) else {
            return;
        };
        let path = opening.pop_front();
        if opening.is_empty() {
            editors.opening.remove(&pane);
        }
        if failed
            && let Some(path) = path
            && (editors.landings.get(&pane)).is_some_and(|place| place.path == path)
        {
            editors.landings.remove(&pane);
        }
        self.land();
    }

    /// The file `pane` shows, or is about to: the last asked to open in
    /// it, if the daemon hasn't answered yet.
    fn file_in(&self, pane: PaneId) -> Option<&str> {
        let editors = &self.editors;
        if let Some(path) = editors.opening.get(&pane).and_then(VecDeque::back) {
            return Some(path);
        }
        let editor = editors.panes.get(&pane)?;
        editors.buffers.get(&editor.buffer()).map(BufferCopy::path)
    }

    /// Where `pane` is, for the jump list: in an editor whose file has
    /// come, where its cursor is, or `byte` of its file if given. One
    /// whose cursor is on its way to a place, as in a file being opened,
    /// is at that place.
    pub(super) fn place_of(&self, pane: PaneId, byte: Option<usize>) -> Option<Jump> {
        self.workspaces.find_pane(pane)?;
        let editors = &self.editors;
        if byte.is_none()
            && let Some(place) = editors.landings.get(&pane)
        {
            let text = (editors.buffers.values())
                .find(|copy| copy.loaded() && copy.path() == place.path)
                .map(BufferCopy::text);
            let file = FilePlace::at_line(&place.path, place.line, place.col, text);
            return Some(Jump {
                pane,
                file: Some(file),
            });
        }
        let file = (editors.panes.get(&pane))
            .and_then(|editor| Some((editor, editors.buffers.get(&editor.buffer())?)))
            .filter(|(_, copy)| copy.loaded())
            .map(|(editor, copy)| {
                let byte = byte.unwrap_or_else(|| editor.cursor_byte(copy));
                FilePlace::at(copy.path(), copy.text(), byte)
            });
        Some(Jump { pane, file })
    }

    /// Puts the cursor of the editor `pane` on `place`, once its file's
    /// text is there, opening the file in the pane, as `:e` does, if it
    /// shows another, or is about to.
    pub(super) fn show_file_place(&mut self, pane: PaneId, place: &FilePlace) {
        if !self.editors.panes.contains_key(&pane) {
            return;
        }
        if self.file_in(pane) != Some(place.path.as_str()) {
            self.open_here(pane, place.path.clone());
        }
        let location = FileLocation {
            path: place.path.clone(),
            line: place.line,
            col: place.col,
            offset: None,
            text: String::new(),
        };
        self.land_at(pane, location);
    }

    /// Takes in a change to `buffer`. Returns whether a pane showing it is
    /// shown, and so the window must be redrawn.
    pub(crate) fn buffer_changed(&mut self, buffer: BufferId, update: BufferUpdate) -> bool {
        if let BufferUpdate::Offer {
            language,
            grammar,
            server,
        } = update
        {
            return self.offered(language, grammar, server);
        }
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
        let answered = match &heard {
            Heard::Answer { id, answer } => Some((*id, answer.clone())),
            _ => None,
        };
        match &heard {
            Heard::Edited(edit) | Heard::Reversed(edit) => {
                (self.jumps).edited(copy.path(), std::slice::from_ref(edit), copy.text());
            }
            Heard::Replaced => self.jumps.replaced(copy.path(), copy.text()),
            _ => {}
        }
        let mut opening = Vec::new();
        let mut left = Vec::new();
        let mut places = None;
        for pane in &showing {
            let editor = editors.panes.get_mut(pane).expect("just found");
            editor.heard(&heard, Some(*pane) == focused, copy);
            left.extend(editor.take_left().map(|byte| (*pane, byte)));
            opening.extend(editor.take_opening().map(|place| (*pane, place)));
            places = places.or_else(|| editor.take_places());
        }
        let here = copy.path().to_owned();
        for (pane, byte) in left {
            self.record_jump(pane, Some(byte));
        }
        // An answer that goes to another file opens it in the pane, and
        // the cursor goes to the place once its text has come; one that
        // gives several places opens the finder on them.
        for (pane, place) in opening {
            self.open_here(pane, place.path.clone());
            self.editors.landings.insert(pane, place);
        }
        if let Some(places) = places {
            self.pick_place(&places, &here);
        }
        self.land();
        let formatted = answered.is_some_and(|(id, answer)| self.formatted(id, &answer));
        formatted
            || self.shown_tab().is_some_and(|tab| {
                (tab.layout.panes().iter()).any(|pane| pane.content == Content::Buffer(buffer))
            })
    }

    /// Asks the language server of the focused editor's file `query`,
    /// which isn't about a place in it. The buffer and the query's
    /// number, or `None` if no editor has the focus.
    pub(super) fn ask_about_focused_file(
        &mut self,
        query: clackatoo_protocol::Query,
    ) -> Option<(BufferId, u64)> {
        let editor = self.editors.panes.get(&self.shown?.pane)?;
        let buffer = editor.buffer();
        let copy = self.editors.buffers.get_mut(&buffer)?;
        Some((buffer, copy.ask(query, 0)))
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
        let left = editor.take_left();
        let made = copy.take_made();
        for pane in &others {
            let editor = editors.panes.get_mut(pane).expect("just found");
            for edit in &made {
                editor.follow_edit(edit, copy);
            }
        }
        self.jumps.edited(copy.path(), &made, copy.text());
        if let Some(byte) = left {
            self.record_jump(focused, Some(byte));
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
        let made = copy.take_made();
        self.jumps.edited(copy.path(), &made, copy.text());
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
            let made = copy.take_made();
            self.jumps.edited(copy.path(), &made, copy.text());
        }
    }

    /// Closes the panel by the cursor of the editor `pane`, if it's one.
    pub(super) fn close_editor_panel(&mut self, pane: PaneId) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.close_panel();
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
        let file = OpenFile { path, cwd: None };
        if place == Place::Here && self.focused_editor().is_some() {
            self.record_jump(shown.pane, None);
            return self.open_here(shown.pane, file.path);
        }
        let terminal = PaneTerminal::File(file);
        let request = match place {
            Place::Tab | Place::Here => Request::NewTab {
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

    /// `:w`, and `:wq`: writes the focused pane's file, formatting it
    /// first if `formatonsave` says, then closes the pane if `quit`.
    pub(super) fn write_file(&mut self, force: bool, quit: bool) {
        let Some(shown) = self.shown else { return };
        let Some((editor, _)) = self.focused_editor() else {
            self.message = Some("this pane has no file to write".into());
            return;
        };
        if self.options.formatonsave {
            return self.format_then_write(force, quit);
        }
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
        let unformatted = self.unformatted.remove(&buffer);
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
        let name = name.unwrap_or("the file");
        self.message = Some(match unformatted {
            Some(why) => format!("{name} written, not formatted: {why}"),
            None => format!("{name} written"),
        });
        if let Some(pane) = then_close {
            self.request(Request::ClosePane { pane }, Asked::Command);
        }
    }

    /// Forgets what was attached on the last connection.
    pub(super) fn editors_reconnected(&mut self) {
        self.editors.reconnected();
    }
}
