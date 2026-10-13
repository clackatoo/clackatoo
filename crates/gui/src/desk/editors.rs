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

use std::collections::{HashMap, HashSet, VecDeque};

use clackatoo_protocol::{
    Answer, BufferId, BufferUpdate, ClientMessage, Content, FileLocation, OpenFile, PaneId,
    PaneTerminal, Request, Size,
};
use clackatoo_text::Edit;

use super::jumps::{FilePlace, Jump, Jumps};
use super::{Asked, Desk};
use crate::command::Place;
use crate::editor::{BufferCopy, Done, Editor, GiveUp, Heard, HunkAction, Marks, Shared, Stroke};
use crate::input::{KeptPress, KeyPress};
use crate::options::Options;
use crate::pane::{Copy, Mode};

/// The editors, by the pane each is, and the buffers they show.
#[derive(Debug, Default)]
pub(super) struct Editors {
    panes: HashMap<PaneId, Editor>,
    buffers: HashMap<BufferId, BufferCopy>,
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

    /// Passes the options on to every editor, as they've changed.
    pub(super) fn set_options(&mut self, options: &Options) {
        self.shared.registers.set_sync(options.clipboard);
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
/// marks, through `made`, edits just made to it in turn. Those the window
/// made, if `ours`, set the file's `.` mark, at the last.
fn follow_places(
    jumps: &mut Jumps,
    marks: &mut Marks,
    copy: &BufferCopy,
    made: &[Edit],
    ours: bool,
) {
    let (path, text) = (copy.path(), copy.text());
    jumps.edited(path, made, text);
    marks.edited(path, made, text);
    if ours
        && let Some(last) = made.last()
        && let Some((at, _, _)) = last.changes().last()
    {
        let at = last.map(at, clackatoo_text::Bias::Before);
        marks.set('.', FilePlace::at(path, text, at));
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
        // Where a closed pane's cursor was to go, it won't.
        editors
            .landings
            .retain(|pane, _| anywhere.contains_key(pane));
        self.drop_held_for_none();
        let (editors, options) = (&mut self.editors, &self.options);
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
    pub(super) fn file_in(&self, pane: PaneId) -> Option<&str> {
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
        let mut heard = copy.update(update);
        // Completions, which may be thousands, are no format's answer.
        let answered = match &heard {
            Heard::Answer { id, answer } if !matches!(answer, Answer::Completions { .. }) => {
                Some((*id, answer.clone()))
            }
            _ => None,
        };
        let marks = &mut editors.shared.marks;
        match &heard {
            Heard::Edited(edit) | Heard::Reversed(edit) => {
                let ours = matches!(heard, Heard::Reversed(_));
                let made = std::slice::from_ref(edit);
                follow_places(&mut self.jumps, marks, copy, made, ours);
            }
            Heard::Replaced => {
                self.jumps.replaced(copy.path(), copy.text());
                marks.replaced(copy.path(), copy.text());
            }
            _ => {}
        }
        let mut opening = Vec::new();
        let mut left = Vec::new();
        let mut places = None;
        let mut resuming = Vec::new();
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
            || formatted
            || self.shown_tab().is_some_and(|tab| {
                (tab.layout.panes().iter()).any(|pane| pane.content == Content::Buffer(buffer))
            })
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
        self.editors.held.push_back(held());
        Some(false)
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
            self.message =
                Some("the editor waiting for the grammar closed: nothing was done".into());
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
    /// `<Space>hs` and `:Gitsigns` ask, or says the pane has no file.
    pub(super) fn editor_hunk(&mut self, action: HunkAction) {
        if self
            .in_focused_editor(|editor, copy, _| editor.hunk(action, copy))
            .is_none()
        {
            self.message = Some("this pane has no file".into());
        }
    }

    /// Has `act` done to the focused pane's editor and its buffer, moving
    /// the cursors of the other editors of its buffer with any edits it
    /// makes. Returns whether the window must be redrawn, or `None` if the
    /// focused pane isn't an editor.
    fn in_focused_editor(
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
        let done = act(editor, copy, &mut editors.shared);
        let left = editor.take_left();
        let opening = editor.take_opening();
        let failed = editor.take_failed();
        let asked_macro = editor.take_macro();
        let made = copy.take_made();
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
            true,
        );
        for (text, to) in editors.shared.registers.take_to_system() {
            copy_later(&mut self.yanked, text, to);
        }
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
        if let Some((keys, times)) = asked_macro {
            self.type_macro(&keys, times);
        }
        Some(done.redraw)
    }

    /// Puts `text` on the clipboard, or the primary selection, once the
    /// window sends it on, as copying in a terminal does: the editors' `"+`
    /// or `"*` hold it at once.
    pub(super) fn copied(&mut self, text: String, to: Copy) {
        self.editors.shared.registers.copied(to, text.clone());
        copy_later(&mut self.yanked, text, to);
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

    /// `:registers`: lists what the registers hold, in a panel by the
    /// focused editor's cursor.
    pub(super) fn show_registers(&mut self) {
        let editors = &mut self.editors;
        let Some(editor) = (self.shown).and_then(|shown| editors.panes.get_mut(&shown.pane)) else {
            self.message =
                Some("registers are listed by an editor's cursor: none has the focus".into());
            return;
        };
        let rows = editors.shared.registers.listing();
        if rows.is_empty() {
            self.message = Some("the registers are empty".into());
            return;
        }
        editor.show_listing("type name content", rows);
    }

    /// `:marks`: the finder, on the marks the focused editor's file can go
    /// to, its own and then the others, each with its line's text, if the
    /// window has it.
    pub(super) fn pick_mark(&mut self) {
        let Some((_, copy)) = self.focused_editor() else {
            self.message = Some("marks are a file's: an editor has no focus".into());
            return;
        };
        let path = copy.path().to_owned();
        let editors = &self.editors;
        let line_of = |place: &FilePlace| -> Option<String> {
            let copy = (editors.buffers.values())
                .find(|copy| copy.loaded() && copy.path() == place.path)?;
            let line = usize::try_from(place.line).ok()?;
            let text = copy.text().get_line(line)?.to_string();
            Some(text.trim().to_owned())
        };
        let found: Vec<FileLocation> = (editors.shared.marks.of(&path).into_iter())
            .map(|(mark, place)| FileLocation {
                path: place.path.clone(),
                line: place.line,
                col: place.col,
                offset: None,
                text: format!("{mark}  {}", line_of(place).unwrap_or_default()),
            })
            .collect();
        if found.is_empty() {
            self.message = Some("no marks are set: m and a letter sets one".into());
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
            true,
        );
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
            follow_places(
                &mut self.jumps,
                &mut editors.shared.marks,
                copy,
                &made,
                true,
            );
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

    /// Asks what editors have due to ask their language servers, now
    /// typing has paused, as of `now`, and gives up on commands waiting
    /// too long for the grammar, or in an editor the focus has left.
    /// Returns whether any asked anything, or gave up.
    pub(crate) fn tick_editors(&mut self, now: std::time::Instant) -> bool {
        let mut asked = self.give_up_off_focus();
        let editors = &mut self.editors;
        let mut gave_up = false;
        for editor in editors.panes.values_mut() {
            if let Some(copy) = editors.buffers.get_mut(&editor.buffer()) {
                asked |= editor.tick(now, copy);
            }
            // Given up on, as it waited too long, with no key to see to it.
            gave_up |= editor.take_gave_up();
        }
        if gave_up {
            self.gave_up();
        }
        // A macro goes on once what it waited for is done, as after
        // anything the daemon says, or gives up if it's waited too long.
        asked |= self.macro_patience(now);
        asked | self.type_macros()
    }

    /// When an editor next has something due to ask its language server,
    /// or a command waiting for the grammar to give up on, or a macro
    /// waiting is to give up.
    pub(crate) fn editor_wakes(&self) -> Option<std::time::Instant> {
        (self.editors.panes.values().filter_map(Editor::wakes))
            .chain(self.macro_gives_up())
            .min()
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
