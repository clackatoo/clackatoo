//! The desk's editors: panes editing files, the window's copies of the
//! buffers they show, and the commands for files.
//!
//! The buffers of the workspace shown are attached, as its terminals are.
//! Every editor of one buffer shares the window's copy of it, so an edit
//! made in one, or by anyone else, moves the others' cursors with the text.
//!
//! Every editor shares the registers and marks too. A key is handed to the
//! focused editor one stroke at a time, so what each does, such as a jump,
//! a mark set or a file opened, is seen to before the next. A macro's keys
//! come the same way, one at a time, from the desk (see [`super::macros`]).

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, ClientMessage, Content, FileLocation, OpenBlank, OpenFile,
    PaneId, PaneTerminal, Request, Size, wire,
};
use clackatoo_term::mouse::{MouseAction, MouseButton};
use clackatoo_text::Edit;

use super::jumps::{FilePlace, Jump, Jumps};
use super::{Asked, Desk, Purpose};
use crate::command::Place;
use crate::editor::{
    BufferCopy, Came, Done, Editor, GiveUp, Heard, HunkAction, Marks, Shared, Wrote,
};
use crate::explorer::Explorer;
use crate::finder::Finder;
use crate::input::{KeptPress, KeyPress};
use crate::options::{Clipboard, Options};
use crate::pane::{Copy, CopyTo, Yanked};
use crate::vim::Stroke;

/// The editors, by the pane each is, and the buffers they show.
#[derive(Debug, Default)]
pub(super) struct Editors {
    pub(super) panes: HashMap<PaneId, Editor>,
    pub(super) buffers: HashMap<BufferId, BufferCopy>,
    /// What every editor shares, as Vim's registers and marks are.
    shared: Shared,
    /// The buffers attached to on this connection.
    attached: HashSet<BufferId>,
    /// Places in files being opened in panes, which their cursors go to
    /// once the files' text has come.
    landings: HashMap<PaneId, FileLocation>,
    /// The files asked to open in panes, in place of theirs, that the
    /// daemon hasn't answered about yet, oldest first. Until it has, what
    /// a pane shows is about to change, so no cursor lands in it.
    opening: HashMap<PaneId, VecDeque<String>>,
    /// The keys and pastes that came while an editor's command waited for
    /// the grammar, oldest first, to be had once it's done.
    held: VecDeque<Held>,
    /// The number the window attaches to buffers as.
    pub(super) window: WindowNumber,
    /// Where editors the daemon made again as it started were, which their
    /// cursors and views go back to once their files' text has come (see
    /// [`Workspaces::restored`](clackatoo_protocol::Workspaces)).
    put_back: HashMap<PaneId, clackatoo_protocol::Place>,
    /// Where each editor was when the daemon was last told, for it to keep
    /// for a restart, as [`Editor::spot`] has it, and when that was.
    told: HashMap<PaneId, (crate::motion::Point, u64)>,
    told_at: Option<Instant>,
}

/// How often, at most, the daemon is told where editors are.
const TELL_PLACES: std::time::Duration = std::time::Duration::from_secs(1);

/// The number a window attaches to buffers as: chosen at random when it
/// starts, and the same on every connection, so the daemon can say which
/// of its changes to a buffer arrived before a connection was lost (see
/// [`Request::AttachBuffer`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct WindowNumber(pub(super) u64);

impl Default for WindowNumber {
    fn default() -> Self {
        Self(clackatoo_daemon::random_window())
    }
}

/// A key or a paste held while an editor's command waits for the grammar.
#[derive(Debug)]
pub(super) enum Held {
    /// A key: for the config's keypress handlers too if `handlers`, and
    /// typed by a binding if `typing`, as it came; and where it starts in
    /// the macro being recorded, so a `q` among them stops it there.
    Key {
        press: KeptPress,
        handlers: bool,
        typing: bool,
        recorded: Option<usize>,
    },
    Paste(String),
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
        // Nor will what the editors asked be answered.
        for editor in self.panes.values_mut() {
            editor.reconnected();
        }
    }

    /// Passes the options on to every editor, as they've changed. Inlay
    /// hints turned off are forgotten, and asked for afresh once they're
    /// turned on.
    pub(super) fn set_options(&mut self, options: &Options) {
        self.shared.registers.set_sync(options.clipboard);
        if !options.inlayhints {
            self.buffers.values_mut().for_each(BufferCopy::forget_hints);
        }
        for editor in self.panes.values_mut() {
            if let Some(copy) = self.buffers.get(&editor.buffer()) {
                editor.set_options(options, copy);
            }
        }
    }
}

/// Adds `text`, for the window to put on the clipboard or the primary
/// selection, in place of what's waiting to go there: only the last
/// counts, however many a macro copies.
fn copy_later(yanked: &mut Vec<(String, Copy)>, text: String, to: Copy) {
    yanked.retain(|&(_, waiting)| waiting != to);
    yanked.push((text, to));
}

/// Moves the places kept in the file `copy` is of, the jump list's and the
/// marks, through `made`, edits just made to it in turn, which `came` as
/// it says. Those the window made, or undid or redid, set the file's `.`
/// mark, at the last.
fn follow_places(
    jumps: &mut Jumps,
    marks: &mut Marks,
    copy: &BufferCopy,
    made: &[impl Borrow<Edit>],
    came: Came,
) {
    let ours = came != Came::Elsewhere;
    // A folder's listing isn't a file places are kept in, but for its
    // selections, for `gv`.
    if copy.is_scratch() {
        marks.edited_selections(copy.file(), made, copy.text(), came);
        return;
    }
    let (file, text) = (copy.file(), copy.text());
    jumps.edited(file, made, text);
    marks.edited(file, made, text, came);
    if ours
        && let Some(last) = made.last().map(Borrow::borrow)
        && let Some((at, _, _)) = last.changes().last()
    {
        let at = last.map(at, clackatoo_text::Bias::Before);
        marks.set('.', FilePlace::at(file, text, at));
    }
}

impl Desk {
    /// The editor `pane` is, if it's one.
    pub(crate) fn editor(&self, pane: PaneId) -> Option<&Editor> {
        self.editors.panes.get(&pane)
    }

    /// Whether `pane` is an editor a file opened from a link or an
    /// explorer can go in: any but a lesson's, one showing a folder
    /// included.
    pub(super) fn takes_files(&self, pane: PaneId) -> bool {
        self.editor(pane).is_some()
            && !self.is_lesson_pane(pane)
            && (!self.is_explorer(pane)
                || (self.is_folder(pane) && self.keeps_its_folder(pane).is_none()))
    }

    /// The editor `pane` is, if it's one, and its buffer, if it's come.
    pub(super) fn editor_and_copy(&mut self, pane: PaneId) -> Option<(&mut Editor, &BufferCopy)> {
        let editors = &mut self.editors;
        let editor = editors.panes.get_mut(&pane)?;
        let copy = (editors.buffers.get(&editor.buffer())).filter(|copy| copy.loaded())?;
        Some((editor, copy))
    }

    /// Every editor, by the pane it is.
    pub(super) fn editors_mut(&mut self) -> impl Iterator<Item = (PaneId, &mut Editor)> {
        (self.editors.panes.iter_mut()).map(|(&pane, editor)| (pane, editor))
    }

    /// The window's copy of `buffer`, if an editor shows it.
    pub(super) fn buffer_copy(&self, buffer: BufferId) -> Option<&BufferCopy> {
        self.editors.buffers.get(&buffer)
    }

    /// The registers every editor shares.
    pub(super) fn registers(&self) -> &crate::editor::Registers {
        &self.editors.shared.registers
    }

    /// Has `act` done to the editor `pane`, with its buffer, if it's an
    /// editor whose buffer has come.
    pub(super) fn with_editor(&mut self, pane: PaneId, act: impl FnOnce(&mut Editor, &BufferCopy)) {
        let editors = &mut self.editors;
        if let Some(editor) = editors.panes.get_mut(&pane)
            && let Some(copy) = editors
                .buffers
                .get(&editor.buffer())
                .filter(|copy| copy.loaded())
        {
            act(editor, copy);
        }
    }

    /// Puts `text` in place of all of `buffer`'s, unless it's that
    /// already, as one change of the user's, as the tutor sets a practice
    /// file's text: the editors showing it follow, as they do your typing.
    pub(super) fn replace_text(&mut self, buffer: BufferId, text: &str) {
        let editors = &mut self.editors;
        let Some(copy) = editors
            .buffers
            .get_mut(&buffer)
            .filter(|copy| copy.loaded())
        else {
            return;
        };
        if copy.text() == text {
            return;
        }
        let showing: Vec<PaneId> = (editors.panes.iter())
            .filter(|(_, editor)| editor.buffer() == buffer)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in &showing {
            editors.panes.get_mut(pane).expect("just found").hold(copy);
        }
        let old = copy.text().to_string();
        copy.edit(Edit::replace(old.len(), 0, &old, text), true);
        let made = copy.take_made();
        for pane in &showing {
            let editor = editors.panes.get_mut(pane).expect("just found");
            for edit in &made {
                editor.follow_edit(edit, copy);
            }
        }
        follow_places(
            &mut self.jumps,
            &mut editors.shared.marks,
            copy,
            &made,
            Came::Elsewhere,
        );
    }

    /// The focused pane's editor and buffer, if it's an editor.
    pub(super) fn focused_editor(&self) -> Option<(&Editor, &BufferCopy)> {
        let editor = self.editors.panes.get(&self.shown?.pane)?;
        Some((editor, self.editors.buffers.get(&editor.buffer())?))
    }

    /// The editor of the focused pane, to change, if it shows one.
    pub(super) fn focused_editor_mut(&mut self) -> Option<&mut Editor> {
        self.editors.panes.get_mut(&self.shown?.pane)
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
        let explorers: HashSet<PaneId> = (self.workspaces.list.iter())
            .flat_map(|workspace| &workspace.tabs)
            .flat_map(|tab| tab.layout.panes())
            .filter(|pane| pane.content.files().is_some())
            .map(|pane| pane.id)
            .collect();
        let shown = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(panes)
            .unwrap_or_default();
        let editors = &mut self.editors;
        // An explorer's editor, of a folder's listing, goes with it.
        let scratch =
            |buffer: &BufferId| (editors.buffers.get(buffer)).is_some_and(BufferCopy::is_scratch);
        let kept: Vec<PaneId> = (editors.panes.iter())
            .filter(|(pane, editor)| {
                anywhere.get(pane) == Some(&editor.buffer())
                    || (explorers.contains(pane) && scratch(&editor.buffer()))
            })
            .map(|(&pane, _)| pane)
            .collect();
        editors.panes.retain(|pane, _| kept.contains(pane));
        // Where a closed pane's cursor was to go, it won't.
        editors
            .landings
            .retain(|pane, _| anywhere.contains_key(pane));
        editors
            .put_back
            .retain(|pane, _| anywhere.contains_key(pane));
        editors.told.retain(|pane, _| anywhere.contains_key(pane));
        self.drop_held_for_none();
        let machines: Vec<_> = (shown.iter())
            .map(|&(pane, _)| self.pane_machine(pane))
            .collect();
        // Undo steps that keep where the cursor was, and steps that change
        // nothing, as Vim's do, need a daemon that keeps them.
        let keeps = self.daemon_has(clackatoo_protocol::wire::UNDO_STATE);
        let (editors, options) = (&mut self.editors, &self.options);
        for (&(pane, buffer), machine) in shown.iter().zip(machines) {
            if let std::collections::hash_map::Entry::Vacant(entry) = editors.panes.entry(pane) {
                entry
                    .insert(Editor::new(buffer, options))
                    .share_finds(&self.finds);
                // Made again as the daemon started: where it was, it goes.
                let restored = (self.workspaces.restored.iter())
                    .find(|restored| restored.pane == pane)
                    .and_then(|restored| restored.place);
                if let Some(place) = restored {
                    editors.put_back.insert(pane, place);
                }
            }
            editors
                .buffers
                .entry(buffer)
                .or_insert_with(|| BufferCopy::new(buffer))
                .set_machine(machine);
            if let Some(copy) = editors.buffers.get_mut(&buffer)
                && !copy.is_scratch()
            {
                copy.set_keeps_steps(keeps);
            }
        }
        let wanted: HashSet<BufferId> = editors.panes.values().map(Editor::buffer).collect();
        editors.buffers.retain(|buffer, _| wanted.contains(buffer));
        // Only the daemon's buffers are attached to.
        let wanted: HashSet<BufferId> = (wanted.into_iter())
            .filter(|buffer| {
                !editors
                    .buffers
                    .get(buffer)
                    .is_some_and(BufferCopy::is_scratch)
            })
            .collect();
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
            let window = self.editors.window.0;
            // A copy the window had before it lost a connection is brought
            // up to date from what it knew.
            let resume = (self.editors.buffers.get(&buffer)).and_then(BufferCopy::resume);
            let attach = Request::AttachBuffer {
                buffer,
                window,
                resume,
            };
            self.request(attach, Asked::Quiet);
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
        editors.put_back.retain(|pane, place| {
            if editors.opening.contains_key(pane) {
                return true;
            }
            let Some(editor) = editors.panes.get_mut(pane) else {
                return true;
            };
            match editors.buffers.get(&editor.buffer()) {
                Some(copy) if copy.loaded() => {
                    editor.put_back(*place, copy);
                    false
                }
                _ => true,
            }
        });
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

    /// `pane` shows `path` in place of its file, or the folder it shows, as
    /// `:e` does, saying so if the file it showed has changes that aren't
    /// written, which are kept, unseen. A folder's listing with changes
    /// isn't left: they'd be lost. Where its cursor was to go before, it
    /// won't. Returns whether it asked the daemon.
    pub(super) fn open_here(&mut self, pane: PaneId, path: String) -> bool {
        if let Some(why) = self.stays(pane) {
            self.say(why.into());
            return false;
        }
        self.editors.landings.remove(&pane);
        self.forget_folder_landing(pane);
        // A relative path is from the folder shown.
        self.root_folder_first(pane);
        if self.connected {
            let opening = self.editors.opening.entry(pane).or_default();
            opening.push_back(path.clone());
        }
        let kept = (self.editors.panes.get(&pane))
            .and_then(|editor| self.editors.buffers.get(&editor.buffer()))
            .filter(|copy| !copy.is_scratch() && !copy.is_blank() && copy.modified())
            .filter(|copy| copy.path() != path)
            .map(|copy| copy.path().to_owned());
        if let Some(kept) = kept {
            self.say(format!(
                "{kept} has changes not written, kept unseen: :e it to go back"
            ));
        }
        let file = OpenFile { path, cwd: None };
        self.request(Request::OpenHere { pane, file }, Asked::OpenedHere(pane));
        true
    }

    /// Why `pane` can't show anything in place of what it shows, as `:e`
    /// would have it, if it can't: a folder's listing whose edits would be
    /// lost, folders being edited as text, or a buffer with no file yet
    /// with changes, which nothing could open again.
    fn stays(&self, pane: PaneId) -> Option<&'static str> {
        if let Some(why) = self.keeps_its_folder(pane) {
            return Some(why);
        }
        if self.is_explorer(pane) && !self.is_folder(pane) {
            return Some("editing folders as text: :w makes the changes, :q! leaves");
        }
        self.loses_blank(&[pane])
            .then_some("[No Name] has changes: :w NAME writes them, :e! drops them")
    }

    /// Whether closing `panes` would lose a buffer with no file yet that
    /// has changes: one that only they show, which the daemon lets go, as
    /// nothing could open it again. Only the workspace shown's editors are
    /// known.
    pub(super) fn loses_blank(&self, panes: &[PaneId]) -> bool {
        let shown_elsewhere = |buffer| {
            (self.workspaces.list.iter())
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| tab.layout.panes())
                .any(|shown| !panes.contains(&shown.id) && shown.content == Content::Buffer(buffer))
        };
        (panes.iter())
            .filter_map(|pane| self.editors.panes.get(pane))
            .map(Editor::buffer)
            .any(|buffer| {
                (self.editors.buffers.get(&buffer)).is_some_and(BufferCopy::blank_changed)
                    && !shown_elsewhere(buffer)
            })
    }

    /// `:e` alone: an explorer of the focused pane's folder, as
    /// `<Space>e` finds or opens one, rooted there, with an empty buffer
    /// with no file yet where `:e PATH` would put the file: in place of an
    /// editor's, or in a new tab beside anything else. Its name, once
    /// `:w NAME` gives it one, is from that folder too. The explorer has
    /// the focus, to choose a file from.
    pub(super) fn edit_blank(&mut self) {
        let Some(shown) = self.shown else { return };
        if !self.needs_daemon(":e alone", wire::BLANK) {
            return;
        }
        let dir = (self.focused_folder()).map(|dir| dir.display().to_string());
        let blank = OpenBlank { cwd: dir.clone() };
        if let Some((_, copy)) = self.focused_editor() {
            let unchanged_blank = copy.is_blank() && !copy.blank_changed();
            if let Some(why) = self.stays(shown.pane) {
                self.say(why.into());
                return;
            }
            // One with no file and no changes is as good as a new one.
            if !unchanged_blank {
                self.blank_here(shown.pane, blank);
            }
            self.open_explorer_at(dir);
            return;
        }
        let request = Request::NewTab {
            workspace: shown.workspace,
            terminal: PaneTerminal::Blank(blank),
            on: None,
        };
        self.request(request, Asked::Blank(dir));
    }

    /// The editor `pane` shows an empty buffer with no file yet, `blank`,
    /// in place of its file, or the folder it shows, as
    /// [`open_here`](Self::open_here) shows a file there, saying so if the
    /// file it showed has changes that aren't written.
    fn blank_here(&mut self, pane: PaneId, blank: OpenBlank) {
        self.record_jump(pane, None);
        self.editors.landings.remove(&pane);
        self.forget_folder_landing(pane);
        let kept = (self.editors.panes.get(&pane))
            .and_then(|editor| self.editors.buffers.get(&editor.buffer()))
            .filter(|copy| !copy.is_scratch() && !copy.is_blank() && copy.modified())
            .map(|copy| copy.path().to_owned());
        if let Some(kept) = kept {
            self.say(format!(
                "{kept} has changes not written, kept unseen: :e it to go back"
            ));
        }
        self.request(Request::BlankHere { pane, blank }, Asked::Command);
    }

    /// The new pane `pane`, showing a buffer with no file yet for `:e`
    /// alone, has been made in a tab of its own: an explorer of `dir`, or
    /// where the daemon roots one beside it, goes down its left.
    pub(super) fn blank_made(&mut self, pane: PaneId, dir: Option<String>) {
        self.show_pane(pane);
        self.open_explorer_at(dir);
    }

    /// The daemon has answered about the oldest file asked to open in
    /// `pane`. If it `failed` to open, the cursor won't go to a place in
    /// it; a place in a file asked for since is still gone to.
    /// Whether a file or folder asked to open in `pane` hasn't been
    /// answered about yet.
    pub(super) fn is_opening(&self, pane: PaneId) -> bool {
        self.editors.opening.contains_key(&pane)
    }

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
        // Nor will the cursor go to an entry in a folder that failed to open.
        if failed {
            self.forget_folder_landing(pane);
        }
        self.land();
    }

    /// The file `pane` shows, or is about to: the last asked to open in
    /// it, if the daemon hasn't answered yet.
    pub(super) fn file_in(&self, pane: PaneId) -> Option<&str> {
        let editors = &self.editors;
        if let Some(path) = editors.opening.get(&pane).and_then(VecDeque::back) {
            return Some(path);
        }
        let editor = editors.panes.get(&pane)?;
        (editors.buffers.get(&editor.buffer()))
            // A buffer with no file yet, or whose text hasn't come, names none.
            .filter(|copy| !copy.is_scratch() && !copy.path().is_empty())
            .map(BufferCopy::path)
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
            let machine = self.pane_machine(pane);
            let text = (editors.buffers.values())
                .find(|copy| {
                    copy.loaded() && copy.machine() == machine && copy.path() == place.path
                })
                .map(BufferCopy::text);
            let landing = crate::editor::File {
                machine,
                path: &place.path,
            };
            let file = FilePlace::at_line(landing, place.line, place.col, text);
            return Some(Jump {
                pane,
                file: Some(file),
                folder: None,
            });
        }
        let file = (editors.panes.get(&pane))
            .and_then(|editor| Some((editor, editors.buffers.get(&editor.buffer())?)))
            // One with no file yet has no place to come back to.
            .filter(|(_, copy)| copy.loaded() && !copy.is_scratch() && !copy.is_blank())
            .map(|(editor, copy)| {
                let byte = byte.unwrap_or_else(|| editor.cursor_byte(copy));
                FilePlace::at(copy.file(), copy.text(), byte)
            });
        let folder = self.folder_place(pane);
        Some(Jump { pane, file, folder })
    }

    /// Puts the cursor of the editor `pane` on `place`, once its file's
    /// text is there, opening the file in the pane, as `:e` does, if it
    /// shows another, or is about to.
    pub(super) fn show_file_place(&mut self, pane: PaneId, place: &FilePlace) {
        if !self.editors.panes.contains_key(&pane) {
            return;
        }
        // Not a file on another machine than the pane's, where the same
        // path is another file, if there's one.
        if place.machine != self.pane_machine(pane) {
            self.say("that place is in a file on another machine".into());
            return;
        }
        // Not into a folder's listing that won't leave, having changes.
        if self.file_in(pane) != Some(place.path.as_str())
            && !self.open_here(pane, place.path.clone())
        {
            return;
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
        let loaded = matches!(update, BufferUpdate::Snapshot(_));
        let redraw = self.take_buffer_update(buffer, update);
        // A file's path is first known as its text comes.
        if loaded {
            self.close_leftovers();
        }
        self.settle_lesson();
        redraw
    }

    /// Takes in a change to `buffer`, as [`buffer_changed`](Self::buffer_changed)
    /// does.
    fn take_buffer_update(&mut self, buffer: BufferId, update: BufferUpdate) -> bool {
        // What the window asks and says, not the editor.
        match update {
            BufferUpdate::Offer {
                language,
                grammar,
                server,
            } => self.offered(buffer, language, grammar, server),
            BufferUpdate::Notice(notice) => {
                self.say(notice);
                true
            }
            // Attached again, as after a lost connection, keeping what's
            // waiting.
            BufferUpdate::Reattach => {
                self.reattach_buffer(buffer);
                false
            }
            BufferUpdate::ChangedOnDisk => self.changed_on_disk(buffer),
            update => self.copy_changed(buffer, update),
        }
    }

    /// Takes in a change to `buffer` that its copy, and the editors showing
    /// it, take in. Returns whether a pane showing it is shown, and what
    /// it shows may have changed.
    #[allow(
        clippy::too_many_lines,
        reason = "each thing an update can do, seen to in turn"
    )]
    fn copy_changed(&mut self, buffer: BufferId, update: BufferUpdate) -> bool {
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
        let was = copy.path().to_owned();
        let mut heard = copy.update(update);
        // Inlay hints the copy had already change nothing on screen.
        let unchanged = matches!(heard, Heard::Hinted { changed: false, .. });
        let filled = fill_finder(self.finder.as_mut(), &mut heard, copy.path());
        // Completions and uses, which may be thousands, are no format's
        // answer.
        let answered = match &heard {
            Heard::Answer { id, answer }
                if !matches!(answer, Answer::Completions { .. } | Answer::Uses(_)) =>
            {
                Some((*id, answer.clone()))
            }
            _ => None,
        };
        let marks = &mut editors.shared.marks;
        match &heard {
            Heard::Edited(edit) | Heard::Reversed { edit, .. } => {
                let (came, state) = match &heard {
                    Heard::Reversed { state, .. } => (Came::Undone, *state),
                    _ => (Came::Elsewhere, None),
                };
                // What the step kept says what its undo puts back, not
                // the window's last edit.
                if state.is_some() {
                    marks.forget_undoable(copy.file());
                }
                let made = std::slice::from_ref(edit);
                follow_places(&mut self.jumps, marks, copy, made, came);
                if let Some(visual) = state.and_then(|state| state.visual) {
                    marks.put_back(copy.file(), copy.text(), visual);
                }
                // Moved too, while disconnected.
                if copy.path() != was {
                    self.jumps.moved(copy.machine(), &was, copy.path());
                    marks.moved(copy.machine(), &was, copy.path());
                }
            }
            Heard::Replaced => {
                self.jumps.replaced(copy.file(), copy.text());
                marks.replaced(copy.file(), copy.text());
            }
            Heard::Moved { from } => {
                self.jumps.moved(copy.machine(), from, copy.path());
                marks.moved(copy.machine(), from, copy.path());
            }
            Heard::Kept { .. } | Heard::Rejoined => {
                match kept_across_a_reconnect(&heard, copy.path()) {
                    // Said through the fields, as the editors are held
                    // still.
                    Some(kept) => {
                        if matches!(heard, Heard::Kept { .. }) {
                            self.said.warned(&kept);
                        } else {
                            self.said.keep(&kept);
                        }
                        self.message = Some(kept);
                    }
                    None => self.message = None,
                }
            }
            _ => {}
        }
        let mut opening = Vec::new();
        let mut left = Vec::new();
        let mut places = None;
        let mut resuming = Vec::new();
        let mut correcting = Vec::new();
        let mut gave_up = false;
        for pane in &showing {
            let editor = editors.panes.get_mut(pane).expect("just found");
            editor.heard(&mut heard, Some(*pane) == focused, copy);
            gave_up |= editor.take_gave_up();
            left.extend(editor.take_left().map(|byte| (*pane, byte)));
            opening.extend(editor.take_opening().map(|place| (*pane, place)));
            places = places.or_else(|| editor.take_places());
            if editor.resumable() {
                resuming.push(*pane);
            }
            if editor.correcting() {
                correcting.push(*pane);
            }
        }
        let here = copy.path().to_owned();
        if gave_up {
            self.gave_up();
        }
        // A command that waited for the answer is done now, as a key
        // would do it.
        self.give_up_off_focus();
        for pane in resuming {
            self.in_editor(pane, Editor::resume);
            if (self.editors.panes.get_mut(&pane)).is_some_and(Editor::take_gave_up) {
                self.gave_up();
            }
        }
        // A new line's indent, put right as the grammar says.
        for pane in correcting {
            self.in_editor(pane, Editor::correct_indents);
        }
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
        // A macro whose key waited goes on, once what it waited for is
        // seen to, and then the keys held, which were typed after it.
        let typed = self.type_macros();
        typed
            || filled
            || formatted
            || (!unchanged
                && self.shown_tab().is_some_and(|tab| {
                    (tab.layout.panes().iter()).any(|pane| pane.content == Content::Buffer(buffer))
                }))
    }

    /// Holds `held`, if an editor's command is waiting for the grammar, or
    /// a macro for something its keys started, to be had once it's done.
    /// Returns `None` if nothing is waiting, or else whether the window
    /// must be redrawn.
    ///
    /// Unless what waits is to be given up on, with what's held: if the
    /// focus has moved to another pane, or it's waited too long, `held`
    /// isn't held, but had as if nothing were waiting, so it returns
    /// `None`; or if `cancel`, as Esc does, `held` goes too.
    pub(super) fn hold_while_waiting(
        &mut self,
        cancel: bool,
        held: impl FnOnce() -> Held,
    ) -> Option<bool> {
        if self.give_up_off_focus() {
            return None;
        }
        // Keys insert mode types, typed for an editor arrived at in insert
        // mode, wait for its text; any other gives up waiting.
        let mut unmade = Some(held);
        let mut given_back = None;
        if self.waits_to_insert() {
            let held = unmade.take().map(|make| make()).expect("not made yet");
            match self.hold_for_insert(held) {
                Some(held) => given_back = Some(held),
                None => return Some(false),
            }
        }
        // A chat's yank or search waits for the fonts, which the window
        // settles it with before it draws: keys wait with it, so the next
        // sees where it left the cursor, and what it yanked.
        if self.chat_waits_to_settle() {
            let held = given_back.or_else(|| unmade.map(|make| make()));
            self.editors.held.extend(held);
            return Some(false);
        }
        let now = std::time::Instant::now();
        if let Some(editor) =
            (self.editors.panes.values_mut()).find(|editor| editor.waiting_for_grammar())
        {
            if editor.waited_too_long(now) {
                editor.give_up(GiveUp::TooLong);
                // Said until the key after this one.
                editor.keep_status();
                editor.take_gave_up();
                self.gave_up();
                return None;
            }
            if cancel {
                editor.give_up(GiveUp::Cancelled);
                editor.take_gave_up();
                self.gave_up();
                return Some(true);
            }
        } else {
            if !self.macro_waiting() || self.macro_patience(now) {
                return None;
            }
            if cancel {
                self.stop_macro(Some("the macro was stopped".into()));
                self.drop_held();
                return Some(true);
            }
        }
        let held = given_back.or_else(|| unmade.map(|make| make()));
        self.editors.held.extend(held);
        Some(false)
    }

    /// Holds `held` until what waits is done.
    pub(super) fn hold(&mut self, held: Held) {
        self.editors.held.push_back(held);
    }

    /// Has the editor `pane` say `text` in the bar until the key after
    /// the next. Returns whether there's an editor there to.
    pub(super) fn say_past_key(&mut self, pane: PaneId, text: &str) -> bool {
        let Some(editor) = self.editors.panes.get_mut(&pane) else {
            return false;
        };
        editor.say(text);
        editor.keep_status();
        true
    }

    /// Whether any keys or pastes are held.
    pub(super) fn holding(&self) -> bool {
        !self.editors.held.is_empty()
    }

    /// What an editor's command waited for was given up on: what's held
    /// for it goes, and so does a macro whose command it was.
    fn gave_up(&mut self) {
        self.drop_held();
        self.stop_macro(None);
    }

    /// Drops the keys and pastes held while something waited.
    pub(super) fn drop_held(&mut self) {
        self.editors.held.clear();
    }

    /// Gives up on what waits if the focus has left the pane it waits in,
    /// as a click, or another window closing the pane, moves it: a command
    /// waiting for the grammar in an editor that no longer has the focus,
    /// or a macro waiting for something its keys started. What's held for
    /// it goes too, as it was typed for that pane. Returns whether it did.
    pub(super) fn give_up_off_focus(&mut self) -> bool {
        let focused = self.shown.map(|shown| shown.pane);
        let mut gave_up = false;
        for (&pane, editor) in &mut self.editors.panes {
            if Some(pane) != focused && editor.waiting_for_grammar() {
                editor.give_up(GiveUp::FocusMoved);
                editor.take_gave_up();
                gave_up = true;
            }
        }
        if self.macro_left_behind() {
            self.stop_macro(Some("the macro was stopped: the focus moved".into()));
            gave_up = true;
        }
        if gave_up {
            self.gave_up();
        }
        gave_up
    }

    /// Drops what's held for a command waiting for the grammar if nothing
    /// is waiting any more, as when the editor that was has gone, saying
    /// so.
    fn drop_held_for_none(&mut self) {
        if !self.editors_wait_for_grammar()
            && !self.macro_waiting()
            && !self.editors.held.is_empty()
        {
            self.editors.held.clear();
            self.say("the editor waiting for the grammar closed: nothing was done".into());
        }
    }

    /// Whether an editor's command waits for the grammar.
    pub(super) fn editors_wait_for_grammar(&self) -> bool {
        (self.editors.panes.values()).any(Editor::waiting_for_grammar)
    }

    /// What the editor `pane` waits for, if anything, that the next key a
    /// macro types may depend on: its file to open, or its text to come;
    /// or the language server or the daemon to answer it, about a query
    /// numbered `since` or later.
    pub(super) fn editor_waits_for(&self, pane: PaneId, since: u64) -> Option<&'static str> {
        let editors = &self.editors;
        if editors.opening.contains_key(&pane) || editors.landings.contains_key(&pane) {
            return Some("a file to open");
        }
        let editor = editors.panes.get(&pane)?;
        let copy = editors.buffers.get(&editor.buffer())?;
        if !copy.loaded() {
            return Some("a file to open");
        }
        editor
            .waiting_for_answer(copy, since)
            .then_some("the language server")
    }

    /// Forgets the keys of a command half typed in the editor `pane`, if
    /// it's one.
    pub(super) fn forget_typed_in(&mut self, pane: PaneId) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.forget_typed();
        }
    }

    /// What every editor shares, as Vim's registers and marks are.
    pub(super) fn shared_mut(&mut self) -> &mut Shared {
        &mut self.editors.shared
    }

    /// What every editor shares, to look at.
    pub(super) fn shared(&self) -> &Shared {
        &self.editors.shared
    }

    /// The search or name being typed in the editor `pane`'s bar, if it's
    /// an editor and one is (see [`Editor::prompt_line`]).
    pub(super) fn editor_prompt_line(
        &mut self,
        pane: PaneId,
    ) -> Option<(
        &mut crate::cmdline::Line,
        Option<crate::cmdline::recall::Kind>,
    )> {
        self.editors.panes.get_mut(&pane)?.prompt_line()
    }

    /// Does `act` to each buffer's copy.
    pub(super) fn each_copy(&mut self, act: impl FnMut(&mut BufferCopy)) {
        self.editors.buffers.values_mut().for_each(act);
    }

    /// What the bar of the editor `pane` says, and how many keys and
    /// pastes are held, for tests.
    #[cfg(test)]
    pub(super) fn editor_label_and_held(&self, pane: PaneId) -> (Option<String>, usize) {
        let label = (self.editors.panes.get(&pane)).and_then(|editor| {
            let copy = self.editors.buffers.get(&editor.buffer())?;
            Some(editor.label(copy))
        });
        (label, self.editors.held.len())
    }

    /// Makes every editor's command waiting as if asked `by` earlier, for
    /// tests of its patience.
    #[cfg(test)]
    pub(super) fn age_grammar_questions(&mut self, by: std::time::Duration) {
        for editor in self.editors.panes.values_mut() {
            editor.age_awaiting(by);
        }
    }

    /// Has the oldest key or paste held while an editor's command, or a
    /// macro, waited, if there is one. Returns whether there was.
    ///
    /// The rest are taken out while it's had, and put back after, so what
    /// it holds, should something start waiting meanwhile, comes before
    /// them.
    pub(super) fn have_held(&mut self) -> bool {
        let Some(held) = self.editors.held.pop_front() else {
            return false;
        };
        let rest = std::mem::take(&mut self.editors.held);
        match held {
            Held::Key {
                press,
                handlers,
                typing,
                recorded,
            } => {
                if let Some(at) = recorded {
                    self.editors.shared.registers.pressed_at(at);
                }
                let typing = std::mem::replace(&mut self.script.typing, typing);
                self.key_with(&press.press(), handlers);
                self.script.typing = typing;
            }
            Held::Paste(text) => self.paste(&text),
        }
        self.editors.held.extend(rest);
        true
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

    /// Asks `query` about byte `at` of `buffer`'s copy, returning its
    /// number, or `None` if the window has no copy of it.
    pub(super) fn ask_about(
        &mut self,
        buffer: BufferId,
        query: clackatoo_protocol::Query,
        at: usize,
    ) -> Option<u64> {
        let copy = self.editors.buffers.get_mut(&buffer)?;
        Some(copy.ask(query, at))
    }

    /// Hands `press` to the focused pane's editor, moving the cursors of
    /// the other editors of its buffer with any edits it makes. Returns
    /// whether the window must be redrawn.
    pub(super) fn editor_key(&mut self, press: &KeyPress<'_>) -> bool {
        let Some(strokes) = (self.shown)
            .and_then(|shown| self.editors.panes.get(&shown.pane))
            .map(|editor| editor.strokes(press))
        else {
            return false;
        };
        let mut redraw = false;
        for stroke in strokes {
            redraw |= self.editor_stroke(stroke);
        }
        redraw
    }

    /// Hands `stroke`, typed, to the focused pane's editor: by a macro,
    /// if the key comes from one.
    fn editor_stroke(&mut self, stroke: Stroke) -> bool {
        let Some(focused) = self.shown.map(|shown| shown.pane) else {
            return false;
        };
        let replaying = self.replaying();
        let mut menu = None;
        let typed = |editor: &mut Editor, copy: &mut BufferCopy, shared: &mut Shared| {
            let done = editor.typed(stroke, copy, shared, replaying);
            menu = editor.take_menu_keys();
            done
        };
        let redraw = self.in_editor(focused, typed).unwrap_or(false);
        self.handed_to_editor(stroke, menu);
        redraw
    }

    /// Does `action` to the hunk at the focused editor's cursor, as
    /// `<Space>hs` and `:gitsigns` ask, or says the pane has no file.
    pub(super) fn editor_hunk(&mut self, action: HunkAction) {
        if self
            .in_focused_editor(|editor, copy, _| editor.hunk(action, copy))
            .is_none()
        {
            self.say("this pane has no file".into());
        }
    }

    /// Has `act` done to the focused pane's editor and its buffer, moving
    /// the cursors of the other editors of its buffer with any edits it
    /// makes. Returns whether the window must be redrawn, or `None` if the
    /// focused pane isn't an editor.
    pub(super) fn in_focused_editor(
        &mut self,
        act: impl FnOnce(&mut Editor, &mut BufferCopy, &mut Shared) -> Done,
    ) -> Option<bool> {
        let focused = self.shown.map(|shown| shown.pane)?;
        self.in_editor(focused, act)
    }

    /// Has `act` done to the editor `focused` and its buffer, as
    /// [`in_focused_editor`](Self::in_focused_editor) does to the focused
    /// one. A key that failed in it stops the macro typing it, and a
    /// macro `@` asked for is typed after.
    fn in_editor(
        &mut self,
        focused: PaneId,
        act: impl FnOnce(&mut Editor, &mut BufferCopy, &mut Shared) -> Done,
    ) -> Option<bool> {
        let framed = self.key_from_frame();
        let editors = &mut self.editors;
        let buffer = editors.panes.get(&focused).map(Editor::buffer)?;
        let copy = editors.buffers.get_mut(&buffer)?;
        let others: Vec<PaneId> = (editors.panes.iter())
            .filter(|&(&pane, editor)| pane != focused && editor.buffer() == buffer)
            .map(|(&pane, _)| pane)
            .collect();
        for pane in &others {
            editors.panes.get_mut(pane).expect("just found").hold(copy);
        }
        let editor = editors.panes.get_mut(&focused).expect("just found");
        editor.typing_framed(framed);
        let done = act(editor, copy, &mut editors.shared);
        // What ended a selection the mouse made from insert mode goes back
        // there, however it was done: a key, a command or the config's
        // Lua. A macro or a binding typing keys goes back once it's done.
        if !framed {
            editor.back_to_insert();
        }
        let left = editor.take_left();
        let opening = editor.take_opening();
        let failed = editor.take_failed();
        let asked_macro = editor.take_macro();
        let format = editor.take_format_asked();
        let made = copy.take_made();
        let astray = copy.take_astray();
        for pane in &others {
            let editor = editors.panes.get_mut(pane).expect("just found");
            for edit in &made {
                editor.follow_edit(edit, copy);
            }
        }
        follow_places(
            &mut self.jumps,
            &mut editors.shared.marks,
            copy,
            &made,
            Came::Made,
        );
        self.registers_to_system();
        if let Some(byte) = left {
            self.record_jump(focused, Some(byte));
        }
        // A mark in another file opens it in the pane, as an answer does.
        if let Some(place) = opening {
            self.open_here(focused, place.path.clone());
            self.editors.landings.insert(focused, place);
        }
        if failed {
            self.stop_macro(None);
        }
        // `gq` or `:'<,'>Format`, as `:format` is, for part of the file.
        if let Some(range) = format {
            self.format_part(buffer, range);
        }
        // Attaching again sends a fresh snapshot, which puts it right.
        if astray && self.connected {
            let window = self.editors.window.0;
            let attach = Request::AttachBuffer {
                buffer,
                window,
                resume: None,
            };
            self.request(attach, Asked::Quiet);
        }
        if let Some((keys, times)) = asked_macro {
            self.type_macro(&keys, times);
        }
        self.answer_scratch();
        Some(done.redraw)
    }

    /// Hands back what scratch copies, of buffers only the window has,
    /// have answered themselves, as the daemon's answers are handed to
    /// the others: each in turn, until none has anything more.
    pub(super) fn answer_scratch(&mut self) {
        loop {
            let answers: Vec<(BufferId, BufferUpdate)> = (self.editors.buffers.values_mut())
                .filter(|copy| copy.is_scratch())
                .flat_map(|copy| {
                    let buffer = copy.buffer();
                    copy.take_answers()
                        .into_iter()
                        .map(move |answer| (buffer, answer))
                })
                .collect();
            if answers.is_empty() {
                return;
            }
            for (buffer, answer) in answers {
                self.buffer_changed(buffer, answer);
            }
        }
    }

    /// Shows `editor`, of `copy`, a folder's listing, in the explorer
    /// `pane`, in the window's mode if the pane has the focus.
    pub(super) fn put_listing_editor(&mut self, pane: PaneId, editor: Editor, copy: BufferCopy) {
        self.editors.buffers.insert(copy.buffer(), copy);
        self.editors.panes.insert(pane, editor);
        if self.shown.is_some_and(|shown| shown.pane == pane) {
            self.editor_to_mode(pane, self.mode == super::WindowMode::Insert);
        }
    }

    /// Takes the editor of a folder's listing the explorer `pane` shows,
    /// and its copy, if it shows one.
    pub(super) fn take_listing_editor(&mut self, pane: PaneId) -> Option<(Editor, BufferCopy)> {
        let editors = &mut self.editors;
        let buffer = editors.panes.get(&pane)?.buffer();
        if !editors.buffers.get(&buffer)?.is_scratch() {
            return None;
        }
        let editor = editors.panes.remove(&pane)?;
        let copy = editors.buffers.remove(&buffer)?;
        // The pane stays, with no editor: the window keeps its mode.
        self.remember_mode_of(pane, editor.mode);
        Some((editor, copy))
    }

    /// The text of the scratch buffer `buffer`, if an editor shows it.
    pub(super) fn scratch_text(&self, buffer: BufferId) -> Option<String> {
        (self.editors.buffers.get(&buffer))
            .filter(|copy| copy.is_scratch())
            .map(|copy| copy.text().to_string())
    }

    /// Puts `text` on the clipboard, or the primary selection, once the
    /// window sends it on, as copying in a terminal does: the editors' `"+`
    /// or `"*` hold it at once.
    pub(super) fn copied(&mut self, text: String, to: Copy) {
        self.editors.shared.registers.copied(to, text.clone());
        copy_later(&mut self.yanked, text, to);
    }

    /// Takes in what a terminal or a chat yanked: into the registers, as
    /// an editor's yank is, or onto the primary selection alone, as a
    /// mouse selection is, as Vim puts one in `"*` (see `pane::yank` and
    /// `chat::yank`).
    pub(super) fn pane_yanked(&mut self, yanked: Yanked) {
        let name = match yanked.to {
            CopyTo::Primary => return self.copied(yanked.register.text, Copy::Primary),
            CopyTo::Clipboard => Some('+'),
            CopyTo::Register(name) => name,
        };
        let registers = &mut self.editors.shared.registers;
        registers.wrote(name, yanked.register, Wrote::Yank);
        self.registers_to_system();
    }

    /// Sends on what the registers put on the clipboard or the primary
    /// selection, for the window to put there.
    fn registers_to_system(&mut self) {
        for (text, to) in self.editors.shared.registers.take_to_system() {
            copy_later(&mut self.yanked, text, to);
        }
    }

    /// The window is asking what's on the clipboard and the primary
    /// selection, to hear in [`clipboard_heard`](Self::clipboard_heard).
    pub(crate) fn clipboard_asked(&mut self) {
        self.editors.shared.registers.asking_system();
    }

    /// The clipboard, or the primary selection, holds `text`, as the
    /// window found when it got the focus back: `"+` or `"*` hold it, and
    /// so `p` puts it if `clipboard` says. It's dropped if the window has
    /// put something there since it asked.
    pub(crate) fn clipboard_heard(&mut self, from: Copy, text: String) {
        self.editors.shared.registers.heard_system(from, text);
    }

    /// What `"+` or `"*` hold: what was last put on the clipboard, or the
    /// primary selection, here or, as the window last heard, elsewhere.
    pub(super) fn held_on(&self, on: Copy) -> &str {
        let name = match on {
            Copy::Clipboard => '+',
            Copy::Primary => '*',
        };
        let held = self.editors.shared.registers.get(Some(name));
        held.map_or("", |register| register.text.as_str())
    }

    /// `:Registers`: lists what the registers hold, in a panel by the
    /// focused editor's cursor.
    pub(super) fn show_registers(&mut self) {
        let editors = &mut self.editors;
        let Some(editor) = (self.shown).and_then(|shown| editors.panes.get_mut(&shown.pane)) else {
            self.say("registers need an editor focused".into());
            return;
        };
        let rows = editors.shared.registers.listing();
        if rows.is_empty() {
            self.say("the registers are empty".into());
            return;
        }
        editor.show_listing("type name content", rows);
    }

    /// `:Marks`: the finder, on the marks the focused editor's file can go
    /// to, its own and then the others, each with its line's text, if the
    /// window has it.
    pub(super) fn pick_mark(&mut self) {
        let Some((_, copy)) = self.focused_editor() else {
            self.say("marks are a file's: an editor has no focus".into());
            return;
        };
        let path = copy.path().to_owned();
        let machine = copy.machine();
        let editors = &self.editors;
        let line_of = |place: &FilePlace| -> Option<String> {
            let copy = (editors.buffers.values())
                .find(|copy| copy.loaded() && place.is_in(copy.file()))?;
            let line = usize::try_from(place.line).ok()?;
            let text = copy.text().get_line(line)?.to_string();
            Some(text.trim().to_owned())
        };
        let here = crate::editor::File {
            machine,
            path: &path,
        };
        // Those in files on another machine than this file's can't be gone
        // to from here.
        let found: Vec<FileLocation> = (editors.shared.marks.of(here).into_iter())
            .filter(|(_, place)| place.machine == machine)
            .map(|(mark, place)| FileLocation {
                path: place.path.clone(),
                line: place.line,
                col: place.col,
                offset: None,
                text: format!("{mark}  {}", line_of(place).unwrap_or_default()),
            })
            .collect();
        if found.is_empty() {
            self.say("no marks are set: m and a letter sets one".into());
            return;
        }
        let case = crate::options::Case::of(&self.options);
        let finder = crate::finder::Finder::places(crate::finder::Kind::Marks, &found, &path, case);
        self.finder = Some(finder);
    }

    /// The register a macro is being recorded into, if one is.
    pub(super) fn recording(&self) -> Option<char> {
        self.editors.shared.registers.recording()
    }

    /// Notes `place`, just jumped from, as its file's `'` mark, which `''`
    /// goes back to.
    pub(super) fn jumped_from(&mut self, place: &FilePlace) {
        self.editors.shared.marks.set('\'', place.clone());
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
        follow_places(
            &mut self.jumps,
            &mut editors.shared.marks,
            copy,
            &made,
            Came::Made,
        );
        true
    }

    /// Puts the editor `pane`, if it's one, in insert mode if `insert`,
    /// or else normal mode, as the focus arriving at it does.
    pub(super) fn editor_to_mode(&mut self, pane: PaneId, insert: bool) {
        let editors = &mut self.editors;
        if let Some(editor) = editors.panes.get_mut(&pane)
            && let Some(copy) = editors.buffers.get_mut(&editor.buffer())
        {
            if insert {
                // Rendered Markdown isn't typed in: its text is.
                editor.show_text_to_edit(copy);
                editor.enter_insert_mode();
                return;
            }
            editor.enter_normal_mode(copy);
            let made = copy.take_made();
            follow_places(
                &mut self.jumps,
                &mut editors.shared.marks,
                copy,
                &made,
                Came::Made,
            );
        }
    }

    /// Closes the panel by the cursor of the editor `pane`, if it's one.
    pub(super) fn close_editor_panel(&mut self, pane: PaneId) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.close_panel();
        }
    }

    /// Whether `pane` is an editor whose file's text hasn't come yet.
    pub(super) fn editor_unloaded(&self, pane: PaneId) -> bool {
        (self.editors.panes.get(&pane))
            .and_then(|editor| self.editors.buffers.get(&editor.buffer()))
            .is_some_and(|copy| !copy.loaded())
    }

    /// Whether this window has asked the daemon to show a file or a
    /// folder in `pane` in place of what it shows, as `:e` does, and
    /// hasn't heard back.
    pub(super) fn opening_here(&self, pane: PaneId) -> bool {
        self.editors.opening.contains_key(&pane)
    }

    /// Ends what was half done in the editor `pane`, if it's one, as the
    /// focus leaves it.
    pub(super) fn editor_leaves(&mut self, pane: PaneId) {
        if let Some(editor) = self.editors.panes.get_mut(&pane) {
            editor.leave();
        }
        // Its drag ends with it, so the mouse is the panes' again.
        if self.editor_drag == Some(pane) {
            self.editor_drag = None;
        }
    }

    /// Scrolls the editor `pane` by `lines`, as the wheel does.
    pub(super) fn editor_wheel(&mut self, pane: PaneId, lines: i64) -> bool {
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&pane) else {
            return false;
        };
        if editor.wheel_rendered(lines) {
            return true;
        }
        let Some(copy) = editors.buffers.get(&editor.buffer()) else {
            return false;
        };
        editor.wheel(lines, copy);
        true
    }

    /// The mouse button went down at `cell` of the editor `pane`, the
    /// `clicks`th time in a row: the cursor goes there, or a word or line
    /// is selected, and the drag that follows is the editor's until
    /// [`editor_released`](Self::editor_released), selecting a block if
    /// `block`, as Alt held, with Ctrl or not, asks. In a folder's
    /// listing, a double click opens what's clicked, as Enter does, as the
    /// explorer's does.
    pub(super) fn editor_press(
        &mut self,
        pane: PaneId,
        [line, col]: [i64; 2],
        clicks: u8,
        block: bool,
    ) -> bool {
        if clicks == 2 && self.is_explorer(pane) {
            return self.listing_enter(pane);
        }
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&pane) else {
            return false;
        };
        let Some(copy) = editors.buffers.get_mut(&editor.buffer()) else {
            return false;
        };
        let (Ok(line), Ok(col)) = (usize::try_from(line), usize::try_from(col)) else {
            return false;
        };
        editor.press_with((line, col), clicks, block, copy);
        // What it ended or began is the file's selection, for `gv`.
        editor.note_selection(copy, &mut editors.shared.marks);
        self.editor_drag = Some(pane);
        true
    }

    /// The mouse moved to `point`, or a button went down or came up there,
    /// while dragging in an editor, if one is: whether the window must be
    /// redrawn, if it's the drag's. A press meanwhile means the release
    /// was missed: the drag ends, and the press is had as any is.
    pub(super) fn editor_drag_mouse(
        &mut self,
        point: [f32; 2],
        action: MouseAction,
    ) -> Option<bool> {
        let pane = self.editor_drag?;
        let moved = |desk: &mut Self| {
            (desk.cell_of(point, pane)).is_some_and(|cell| desk.editor_dragged(pane, cell))
        };
        Some(match action {
            MouseAction::Motion(_) => moved(self),
            MouseAction::Release(MouseButton::Left) => moved(self) | self.editor_released(),
            MouseAction::Release(_) => false,
            MouseAction::Press(_) => {
                self.editor_drag_lost();
                return None;
            }
        })
    }

    /// The mouse moved to `cell` of the editor being dragged in, which may
    /// be off it. Returns whether the window must be redrawn.
    fn editor_dragged(&mut self, pane: PaneId, cell: [i64; 2]) -> bool {
        let editors = &mut self.editors;
        let Some(editor) = editors.panes.get_mut(&pane) else {
            return false;
        };
        let Some(copy) = editors.buffers.get_mut(&editor.buffer()) else {
            return false;
        };
        editor.drag_to((cell[0], cell[1]), Instant::now(), copy)
    }

    /// The mouse button came up, ending the drag in an editor, if one was
    /// being dragged in: what it selected stays selected, and goes on the
    /// primary selection, as a terminal's does, unless the unnamed
    /// register goes along with that, as `clipboard=unnamed` makes it:
    /// then `p` over the selection would put the selection itself. Nothing
    /// selected, as in an empty file, leaves the primary selection as it
    /// was.
    /// Returns whether one was.
    pub(crate) fn editor_released(&mut self) -> bool {
        let Some(pane) = self.editor_drag.take() else {
            return false;
        };
        let wanted = self.options.clipboard != Clipboard::Unnamed;
        let editors = &mut self.editors;
        let selected = (editors.panes.get_mut(&pane)).and_then(|editor| {
            let copy = editors.buffers.get(&editor.buffer())?;
            let selected = editor.release(wanted, copy);
            editor.note_selection(copy, &mut editors.shared.marks);
            selected
        });
        if let Some(text) = selected {
            self.copied(text, Copy::Primary);
        }
        true
    }

    /// The window lost the focus mid-drag in an editor, so the button's
    /// release may never come: the drag ends, leaving what it selected.
    pub(super) fn editor_drag_lost(&mut self) {
        let editors = &mut self.editors;
        if let Some(pane) = self.editor_drag.take()
            && let Some(editor) = editors.panes.get_mut(&pane)
        {
            editor.drag_lost();
            if let Some(copy) = editors.buffers.get(&editor.buffer()) {
                editor.note_selection(copy, &mut editors.shared.marks);
            }
        }
    }

    /// Lays out the shown editors' lines, before the window is drawn.
    pub(crate) fn settle_editors(&mut self) {
        let Some(tab) = self.shown_tab() else { return };
        let shown: Vec<PaneId> = tab.layout.panes().iter().map(|pane| pane.id).collect();
        // The command being typed, which the focused editor shows what it
        // would do, as `inccommand` says.
        let typing = (self.input.as_ref())
            .filter(|input| input.purpose == Purpose::Command)
            .map(|input| input.line.text());
        let focused = self.shown.map(|shown| shown.pane);
        let editors = &mut self.editors;
        for pane in shown {
            if let Some(editor) = editors.panes.get_mut(&pane)
                && let Some(copy) = editors.buffers.get(&editor.buffer())
            {
                let line = typing.filter(|_| Some(pane) == focused);
                editor.preview(line, copy, &editors.shared);
                editor.settle(copy);
            }
        }
    }

    /// What the command line starts with as `:` opens it: in an editor in
    /// visual mode, `'<,'>`, the selection's lines, which it leaves.
    pub(super) fn editor_command_line(&mut self) -> String {
        let editors = &mut self.editors;
        let Some(editor) = (self.shown).and_then(|shown| editors.panes.get_mut(&shown.pane)) else {
            return String::new();
        };
        let Some(copy) = editors.buffers.get(&editor.buffer()) else {
            return String::new();
        };
        editor
            .command_line(copy, &mut editors.shared.marks)
            .to_owned()
    }

    /// One of an editor's commands, such as `:s` or `:g`, typed after `:`,
    /// done in the focused editor. One that can't be done is refused, which
    /// stops a macro typing it, as in Vim.
    pub(super) fn editor_command(&mut self, line: &str) {
        let replaying = self.replaying();
        let mut refused = None;
        let done = self.in_focused_editor(|editor, copy, shared| {
            editor
                .ex(line, copy, shared, replaying)
                .unwrap_or_else(|why| {
                    refused = Some(why);
                    Done::default()
                })
        });
        if done.is_none() {
            self.refuse(format!(":{line} is for an editor, and none has the focus"));
        } else if let Some(why) = refused {
            self.refuse(why);
        }
    }

    /// Asks what editors have due to ask their language servers, now
    /// typing has paused, as of `now`, and gives up on commands waiting
    /// too long for the grammar, or in an editor the focus has left.
    /// Returns whether any asked anything, or gave up.
    pub(crate) fn tick_editors(&mut self, now: std::time::Instant) -> bool {
        let mut asked = self.give_up_off_focus();
        // Only the focused editor asks where a name is used, of a daemon
        // that reads the question.
        let focused = (self.shown.map(|shown| shown.pane)).filter(|_| self.daemon_has(wire::USES));
        // Every editor shown asks for the inlay hints of the part of its
        // file it shows, of a daemon that reads the question.
        let hinting: Vec<PaneId> = match self.shown_tab() {
            Some(tab) if self.daemon_has(wire::INLAY_HINTS) => {
                tab.layout.panes().iter().map(|pane| pane.id).collect()
            }
            _ => Vec::new(),
        };
        let editors = &mut self.editors;
        let mut gave_up = false;
        for (pane, editor) in &mut editors.panes {
            if let Some(copy) = editors.buffers.get_mut(&editor.buffer()) {
                asked |= editor.tick(now, copy);
                asked |= editor.watch_uses(now, focused == Some(*pane), copy);
                asked |= editor.watch_hints(now, hinting.contains(pane), copy);
            }
            // Given up on, as it waited too long, with no key to see to it.
            gave_up |= editor.take_gave_up();
        }
        if gave_up {
            self.gave_up();
        }
        self.answer_scratch();
        // A macro goes on once what it waited for is done, as after
        // anything the daemon says, or gives up if it's waited too long.
        asked |= self.macro_patience(now);
        asked |= self.insert_patience(now);
        asked | self.type_macros()
    }

    /// When an editor next has something due to ask its language server,
    /// or a command waiting for the grammar to give up on, or a macro
    /// waiting is to give up.
    pub(crate) fn editor_wakes(&self) -> Option<std::time::Instant> {
        (self.editors.panes.values().filter_map(Editor::wakes))
            .chain(self.macro_gives_up())
            .chain(self.insert_gives_up())
            .min()
    }

    /// When the daemon is next to be told where editors are, if any has
    /// moved since it was last told: at once, or a moment after it last
    /// was, so it's told at most once every [`TELL_PLACES`].
    pub(crate) fn places_to_tell(&self) -> Option<Instant> {
        if !self.connected || !self.daemon_has(wire::RESTORED) {
            return None;
        }
        let editors = &self.editors;
        // As the window wakes: compared, not worked out.
        let moved = (editors.panes.iter()).any(|(pane, editor)| {
            let copy = editors.buffers.get(&editor.buffer());
            copy.is_some_and(|copy| {
                copy.loaded()
                    && !copy.is_scratch()
                    && editors.told.get(pane) != Some(&editor.spot())
            })
        });
        moved.then(|| {
            editors
                .told_at
                .map_or_else(Instant::now, |at| at + TELL_PLACES)
        })
    }

    /// Tells the daemon where each editor that has moved since it was last
    /// told is, if it's time to, for it to keep for a restart. Returns
    /// whether it did: there's nothing to draw for it.
    pub(crate) fn tell_places(&mut self, now: Instant) -> bool {
        if !self.connected || !self.daemon_has(wire::RESTORED) {
            return false;
        }
        let editors = &mut self.editors;
        if editors.told_at.is_some_and(|at| now < at + TELL_PLACES) {
            return false;
        }
        let mut told = false;
        for (&pane, editor) in &editors.panes {
            let Some(copy) = editors.buffers.get(&editor.buffer()) else {
                continue;
            };
            if !copy.loaded() || copy.is_scratch() {
                continue;
            }
            let spot = editor.spot();
            if editors.told.insert(pane, spot) != Some(spot) {
                let place = editor.place(copy);
                self.outbox.push(ClientMessage::Place { pane, place });
                told = true;
            }
        }
        if told {
            editors.told_at = Some(now);
        }
        told
    }

    /// A new connection, to a daemon that may know nothing of where
    /// editors are, as one just upgraded: it's told each again.
    pub(super) fn tell_places_again(&mut self) {
        self.editors.told.clear();
        self.editors.told_at = None;
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
            self.open_here(shown.pane, file.path);
            return;
        }
        let terminal = PaneTerminal::File(file);
        let request = match place {
            Place::Tab | Place::Here => Request::NewTab {
                workspace: shown.workspace,
                terminal,
                on: None,
            },
            Place::Beside(side) => Request::Split {
                pane: shown.pane,
                side,
                terminal,
                on: None,
            },
        };
        self.request(request, Asked::Pane);
    }

    /// `:w`, and `:wq`: writes the focused pane's file, formatting it
    /// first if `formatonsave` says, then closes the pane if `quit`. A
    /// buffer with no file yet is written to the file `name` says, as
    /// `:w NAME` names it, over one that's there only if `force`; with no
    /// name, the command line asks for one.
    pub(super) fn write_file(&mut self, force: bool, quit: bool, name: Option<String>) {
        let Some(shown) = self.shown else { return };
        let listings = self.explorer(shown.pane).is_some_and(Explorer::editing);
        if listings && name.is_none() {
            return self.write_listings(shown.pane, quit);
        }
        let Some((editor, copy)) = self.focused_editor().filter(|_| !listings) else {
            self.say("this pane has no file to write".into());
            return;
        };
        let buffer = editor.buffer();
        let blank = copy.is_blank();
        match (blank, &name) {
            (true, None) => return self.ask_for_name(force, quit),
            (false, Some(_)) => {
                let named = copy.name();
                self.say(format!(
                    ":w NAME names a buffer with no name: this is ‘{named}’, which :w writes"
                ));
                return;
            }
            _ => {}
        }
        // Text the window kept in place of the daemon's, after a lost
        // connection, replaces it only if asked, as the daemon's may have
        // changes the window hasn't: then it goes first, as one change, for
        // the file to have it.
        if let Some(copy) = self.editors.buffers.get_mut(&buffer)
            && let Some(why) = copy.kept_why()
        {
            if !force {
                let why = format!("{why}: :w! writes this window's text, :e! takes the daemon's");
                self.warn(why);
                return;
            }
            if !self.connected {
                self.warn("not connected: the file can't be written yet".into());
                return;
            }
            copy.resend_kept();
            let change = copy.take_outbox();
            self.outbox.extend(change);
        }
        let then_close = quit.then_some(shown.pane);
        // With no file, it has no language to format it by.
        if let Some(path) = name {
            let request = Request::SaveAs {
                buffer,
                path,
                force,
            };
            return self.request(request, Asked::Saved { buffer, then_close });
        }
        if self.options.formatonsave {
            return self.format_then_write(force, quit);
        }
        self.request(
            Request::Save { buffer, force },
            Asked::Saved { buffer, then_close },
        );
    }

    /// `:w` in a buffer with no file yet: the command line asks for its
    /// name, as `:w NAME`, `force`d or then quitting as the `:w` was. Not
    /// while something else is being typed there, or a macro types keys,
    /// which would go into it.
    fn ask_for_name(&mut self, force: bool, quit: bool) {
        if self.input.is_some() || self.replaying() {
            self.say("[No Name] has no file yet: :w NAME writes it".into());
            return;
        }
        let word = match (quit, force) {
            (false, false) => "w",
            (false, true) => "w!",
            (true, false) => "wq",
            (true, true) => "wq!",
        };
        self.input = Some(super::Input::new(Purpose::Command, format!("{word} ")));
        self.suggest_completion();
        self.say("[No Name] has no file yet: type its name".into());
    }

    /// `:q`: closes the focused pane, unless it's the last showing a file
    /// with changes not saved, or if `force`, throwing them away.
    pub(super) fn quit(&mut self, force: bool) {
        let Some(shown) = self.shown else { return };
        // In a lesson's panes, `:q` ends the lesson.
        if self.in_lesson_pane(shown.pane) {
            return self.end_lesson();
        }
        // In an explorer editing folders as text, it goes back to the tree;
        // in an editor showing a folder, it closes, as for a file.
        if self.explorer(shown.pane).is_some_and(Explorer::editing) {
            return self.leave_listings(shown.pane, force);
        }
        if let Some((editor, copy)) = self.focused_editor() {
            let buffer = editor.buffer();
            let last = !(self.workspaces.list.iter())
                .flat_map(|workspace| &workspace.tabs)
                .flat_map(|tab| tab.layout.panes())
                .any(|pane| pane.id != shown.pane && pane.content == Content::Buffer(buffer));
            if last && (copy.modified() || copy.blank_changed()) {
                if !force {
                    let write = if copy.is_blank() { ":w NAME" } else { ":w" };
                    self.say(format!(
                        "{} has unsaved changes: {write} writes them, :q! drops them",
                        copy.name()
                    ));
                    return;
                }
                self.request(Request::Discard { buffer }, Asked::Command);
            }
        }
        self.request(Request::ClosePane { pane: shown.pane }, Asked::Command);
    }

    /// Takes in that `buffer`'s file has changed on disk from outside, and
    /// it has no changes not saved: has it read again, if `autoread` says
    /// to, or says so. Returns whether the bar changed.
    fn changed_on_disk(&mut self, buffer: BufferId) -> bool {
        if self.options.autoread {
            // The daemon reads it only if it's still changed and not
            // modified, so another window asking too, or typing meanwhile,
            // loses nothing.
            self.request(Request::Reload { buffer }, Asked::Command);
            return false;
        }
        let name = (self.editors.buffers.get(&buffer)).map_or("the file", BufferCopy::name);
        self.warn(format!(
            "{name} changed on disk: :e! takes it, :w! keeps yours"
        ));
        true
    }

    /// `:e!`: throws away the focused pane's file's changes not saved.
    pub(super) fn discard_changes(&mut self) {
        if let Some(pane) = self.shown.map(|shown| shown.pane)
            && self.explorer(pane).is_some_and(Explorer::editing)
        {
            // An editor's folder throws away every listing's changes, as
            // they'd keep it from going anywhere else.
            if self.is_folder(pane) {
                return self.list_shown_again(pane);
            }
            return self.listing_as_made(pane);
        }
        match self.focused_editor() {
            Some((editor, _)) => {
                let buffer = editor.buffer();
                // Text the window kept in place of the daemon's gives way to
                // it.
                let copy = self.editors.buffers.get_mut(&buffer);
                if let Some(snapshot) = copy.and_then(BufferCopy::take_daemons) {
                    self.copy_changed(buffer, BufferUpdate::Snapshot(snapshot));
                    return;
                }
                self.request(Request::Discard { buffer }, Asked::Command);
            }
            None => self.say("this pane has no file".into()),
        }
    }

    /// `:recover`: takes the focused file's changes not saved that were
    /// set aside as the daemon started, its file having changed since they
    /// were kept, or with `drop`, throws them away. Otherwise shows the
    /// next buffer whose changes came back that no pane shows, such as one
    /// with no file yet: in place of the focused editor's file, or in a new
    /// tab beside anything else. The daemon decides which, and says.
    pub(super) fn recover(&mut self, drop: bool) {
        let Some(shown) = self.shown else { return };
        if !self.needs_daemon(":recover", wire::RECOVER) {
            return;
        }
        let request = Request::Recover {
            pane: shown.pane,
            drop,
        };
        self.request(request, Asked::Recovered);
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
            self.warn(match err {
                clackatoo_protocol::Error::ChangedOnDisk(path) => {
                    format!("{path} has changed since it was read: :w! writes over it")
                }
                other => other.to_string(),
            });
            return;
        }
        let name = self.editors.buffers.get(&buffer).map(BufferCopy::name);
        let name = name.unwrap_or("the file");
        self.say(match unformatted {
            Some(why) => format!("{name} written, not formatted: {why}"),
            None => format!("{name} written"),
        });
        if let Some(pane) = then_close {
            self.request(Request::ClosePane { pane }, Asked::Command);
        }
    }

    /// An update to `buffer` from a newer daemon couldn't be read: the
    /// window attaches to it again, to be brought up to date as after a
    /// lost connection, so its copy can't go astray from the daemon's
    /// text. What its editors asked won't be answered.
    ///
    /// Every miss counts towards how often it asks, those while it waits
    /// too, so a copy whose answer never comes readable says so. One that
    /// couldn't be read, being a snapshot or the edits it resumes with if
    /// `snapshot`, is the answer it waited for, so it asks again.
    pub(crate) fn missed_buffer(&mut self, buffer: BufferId, snapshot: bool) {
        if !self.connected || !self.editors.attached.contains(&buffer) {
            return;
        }
        let Some(missing) = (self.editors.buffers.get(&buffer)).map(BufferCopy::is_missing) else {
            return;
        };
        if !self.ask_again(super::Missed::Buffer(buffer)) || (missing && !snapshot) {
            return;
        }
        let Some(copy) = self.editors.buffers.get_mut(&buffer) else {
            return;
        };
        if !missing && !copy.missed() {
            return;
        }
        self.attach_again_from(buffer);
    }

    /// Attaches to `buffer` again, as after a lost connection, keeping what
    /// its copy has waiting, as a daemon relaying another machine's buffer
    /// asks ([`BufferUpdate::Reattach`]): however often it asks, as each
    /// time its copy would otherwise drift from the text there. Nothing it
    /// says of the buffer is taken in until what that brings has come.
    fn reattach_buffer(&mut self, buffer: BufferId) {
        if !self.connected || !self.editors.attached.contains(&buffer) {
            return;
        }
        let Some(copy) = self.editors.buffers.get_mut(&buffer) else {
            return;
        };
        // Attaching again already: what that brings is what's asked for.
        if copy.is_missing() || !copy.missed() {
            return;
        }
        self.attach_again_from(buffer);
    }

    /// Asks for `buffer` again, from what its copy knew, as after a lost
    /// connection, its editors' questions given up on.
    fn attach_again_from(&mut self, buffer: BufferId) {
        let Some(copy) = self.editors.buffers.get(&buffer) else {
            return;
        };
        let resume = copy.resume();
        for editor in self.editors.panes.values_mut() {
            if editor.buffer() == buffer {
                editor.reconnected();
            }
        }
        let window = self.editors.window.0;
        let attach = Request::AttachBuffer {
            buffer,
            window,
            resume,
        };
        self.request(attach, Asked::Quiet);
    }

    /// Forgets what was attached on the last connection.
    pub(super) fn editors_reconnected(&mut self) {
        self.editors.reconnected();
    }
}

/// Gives `finder` the answer `heard` is, if it's the one the finder waits
/// for, the symbols in the file at `path`, leaving nothing in its place:
/// they may be thousands, and are the finder's alone. Returns whether it
/// was.
fn fill_finder(finder: Option<&mut Finder>, heard: &mut Heard, path: &str) -> bool {
    let Heard::Answer { id, answer } = heard else {
        return false;
    };
    let Some(finder) = finder.filter(|finder| finder.asking() == Some(*id)) else {
        return false;
    };
    finder.answered(path, std::mem::replace(answer, Answer::Nothing));
    true
}

/// What the bar says of the window's unsaved changes to the file at
/// `path`, kept across a lost connection, as `heard` says.
fn kept_across_a_reconnect(heard: &Heard, path: &str) -> Option<String> {
    match heard {
        Heard::Kept { why } => Some(format!(
            "{why}; your changes to {path} are kept here: :w! writes them, :e! takes the \
             daemon's"
        )),
        Heard::Rejoined => Some(format!(
            "reconnected; your unsaved changes to {path} were kept"
        )),
        _ => None,
    }
}
