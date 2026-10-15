//! The window's mode: one insert or normal mode, shared by every pane,
//! tab and workspace the window shows, rather than one for each pane.
//!
//! The focused pane's own mode is the window's while it has one, so
//! everything that changes a pane's mode, from `i` and Esc to the mouse
//! and the config's keys, changes the window's with it. The window
//! remembers it as the focus goes, and every way this window moves the
//! focus puts the pane arrived at in it: moving with keys or the mouse,
//! switching tabs and workspaces, jumping, a pane it opens, or the one
//! focused closing as it asked, or as its program ends, which the daemon
//! says. A terminal arriving in insert mode goes back to the live
//! screen, a chat to the message being typed, and an editor types at its
//! cursor.
//!
//! The focus moved by anyone else, as when another window or an agent
//! closes the focused pane or opens a file in it, arrives in normal mode,
//! and puts the window in it: keys typed for one pane never go to a shell
//! or a file the user didn't choose.
//!
//! Panes with no insert mode, the explorer, a review, a board, a chat's
//! page and a lesson, show normal mode and leave the window's as it is,
//! so going on to a terminal through one still types into it. What's
//! opened or closed from one, by a key, a click or a command, leaves the
//! window in normal mode, as the bar said. An editor showing a folder's
//! listing is an editor, and follows the mode. So does a chat, but in
//! normal mode while it asks something, so typing can't answer it by
//! chance; an answer's number, pressed twice, answers it there, but only
//! a moment after the focus arrives, so keys meant for the pane before
//! don't.
//!
//! An editor whose text hasn't come stays in normal mode until it has,
//! and then goes to insert mode, if it still has the focus. Meanwhile
//! the keys insert mode would type are held for it, and only ever reach
//! it; any other key, or a click, gives up waiting and is had at once,
//! in normal mode. So does waiting as long as a command waits for the
//! grammar.
//!
//! Leaving a pane ends what was half done in it: a visual selection,
//! which leaves the window in normal mode, a command half typed, or a
//! search or a command line being typed. Where each pane was, a
//! terminal's place in history or an editor's cursor, stays.
//!
//! The mode is the window's alone: another window showing the same pane
//! has its own, and the daemon never hears of it.

use std::time::Instant;

use clackatoo_protocol::{Content, PaneId, Request, TerminalId};
use winit::keyboard::{Key, NamedKey};

use super::editors::Held;
use super::{Asked, Desk, Purpose};
use crate::editor::PATIENCE;
use crate::pane::Mode;

/// How soon after a key typed in this window reached a terminal its
/// program ending is taken for the user's doing, as `exit` is.
const KEYED_EXIT: std::time::Duration = std::time::Duration::from_secs(3);

/// The window's mode, which the pane it arrives at takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WindowMode {
    /// Keys go to the program, or type into the file or the message.
    #[default]
    Insert,
    /// Keys drive clackatoo.
    Normal,
}

impl WindowMode {
    fn of(mode: Mode) -> Self {
        match mode {
            Mode::Insert => Self::Insert,
            Mode::Normal | Mode::Visual(_) => Self::Normal,
        }
    }
}

impl Desk {
    /// The window's mode: the focused pane's, or if it has no insert
    /// mode, the one the window had before.
    pub(crate) fn mode(&self) -> WindowMode {
        self.focused_mode().map_or(self.mode, WindowMode::of)
    }

    /// The focused pane's own mode, if it has an insert mode: an editor,
    /// a terminal or a chat. Not a lesson, nor a chat kept in normal mode
    /// as it asks something; and an editor waiting for its text to go to
    /// insert mode is in insert mode already.
    pub(super) fn focused_mode(&self) -> Option<Mode> {
        let pane = self.shown?.pane;
        if self
            .insert_on_load
            .is_some_and(|(waiting, _)| waiting == pane)
        {
            return Some(Mode::Insert);
        }
        if let Some((editor, _)) = self.focused_editor() {
            return (!self.is_lesson_pane(pane)).then_some(editor.mode);
        }
        // An explorer, or an editor's folder whose listing hasn't come.
        if self.focused_explorer().is_some() {
            return None;
        }
        let mode = self.pane(self.focused_terminal()?)?.mode;
        if self.held_normal == Some(pane) && mode != Mode::Insert {
            return None;
        }
        Some(mode)
    }

    /// Takes the window's mode from the focused pane, as it may be about
    /// to go: before the focus moves, or the daemon's workspaces replace
    /// the pane or what it shows. Leaving a pane with no insert mode by
    /// what it does itself leaves it in normal mode.
    pub(super) fn remember_mode(&mut self) {
        self.mode = self.mode();
        if self.acting && self.focused_mode().is_none() {
            self.mode = WindowMode::Normal;
        }
    }

    /// Takes the window's mode from `mode`, the mode of the focused
    /// pane's editor, which is going though the pane stays.
    pub(super) fn remember_mode_of(&mut self, pane: PaneId, mode: Mode) {
        if self.shown.is_some_and(|shown| shown.pane == pane) && !self.is_lesson_pane(pane) {
            self.mode = WindowMode::of(mode);
        }
    }

    /// One of the keys that go to normal mode from anywhere, pressed in
    /// a pane with no insert mode, puts the window in normal mode, so the
    /// next pane arrived at is in it too.
    pub(super) fn normal_mode_asked(&mut self) {
        if self.focused_mode().is_none() {
            self.mode = WindowMode::Normal;
        }
    }

    /// A key is pressed: a chat kept in normal mode as it asked something
    /// that's since gone to insert mode has its mode the window's again.
    pub(super) fn key_coming(&mut self) {
        let insert = |desk: &Self| {
            let terminal = desk.focused_terminal()?;
            Some(desk.pane(terminal)?.mode == Mode::Insert)
        };
        if self.held_normal == self.shown.map(|shown| shown.pane) && insert(self) == Some(true) {
            self.held_normal = None;
        }
    }

    /// Does `act`, a key or a click in the focused pane, as that pane's
    /// own doing: if it has no insert mode, the focus it moves arrives in
    /// normal mode, as the bar said. Moving through such a pane with the
    /// window's keys leaves the window's mode as it was.
    pub(super) fn acting_from_list<R>(&mut self, act: impl FnOnce(&mut Self) -> R) -> R {
        let was = std::mem::replace(&mut self.acting, true);
        let done = act(self);
        self.acting = was;
        done
    }

    /// The window is sending `request`, asked as `asked`: one that opens
    /// a pane or a file, or closes panes, sent from a pane with no insert
    /// mode, by a key, a click, a command or the finder, leaves the window
    /// in normal mode. And the panes it closes are noted, so the focus
    /// they move as they go is this window's doing.
    pub(super) fn sending(&mut self, request: &Request, asked: &Asked) {
        let opens = matches!(
            asked,
            Asked::Pane | Asked::PaneAt(_) | Asked::OpenedHere(_) | Asked::Explorer { .. }
        );
        let closing = self.closed_by(request);
        if (opens || closing.is_some()) && self.focused_mode().is_none() {
            self.mode = WindowMode::Normal;
        }
        if let Some(panes) = closing {
            self.closing.extend(panes.iter().copied());
            self.closes_asked.push_back((self.asks_sent, panes));
        }
    }

    /// The window's request numbered `number` was answered, refused if
    /// `refused`: the panes a refused close was for are no longer closing.
    pub(super) fn heard_back(&mut self, number: u64, refused: bool) {
        while let Some((asked, panes)) = self.closes_asked.front() {
            if *asked > number {
                break;
            }
            if *asked == number && refused {
                for pane in panes {
                    self.closing.remove(pane);
                }
            }
            self.closes_asked.pop_front();
        }
    }

    /// The panes `request` closes, if it closes any.
    fn closed_by(&self, request: &Request) -> Option<Vec<PaneId>> {
        let panes = |tabs: Vec<&clackatoo_protocol::Tab>| -> Vec<PaneId> {
            (tabs.into_iter())
                .flat_map(|tab| tab.layout.panes())
                .map(|pane| pane.id)
                .collect()
        };
        Some(match request {
            Request::ClosePane { pane } => vec![*pane],
            Request::CloseTab { tab } => {
                (self.workspaces.find_tab(*tab)).map_or_else(Vec::new, |(_, tab)| panes(vec![tab]))
            }
            Request::CloseWorkspace { workspace } => (self.workspaces.get(*workspace))
                .map_or_else(Vec::new, |workspace| panes(workspace.tabs.iter().collect())),
            _ => return None,
        })
    }

    /// The daemon says `terminal`'s program has ended, at `now`: the pane
    /// about to close is the user's own doing, as after `exit` and Enter
    /// or Ctrl+D, if a key typed in this window reached it within
    /// [`KEYED_EXIT`]. A program an agent killed, or that crashed, isn't.
    pub(crate) fn terminal_exited(&mut self, terminal: TerminalId, now: Instant) {
        let since = now.checked_sub(KEYED_EXIT).unwrap_or(now);
        if self
            .pane(terminal)
            .is_some_and(|pane| pane.keyed_since(since))
        {
            self.exited.insert(terminal);
        }
    }

    /// Whether the focused terminal's program has ended, as the daemon
    /// said.
    pub(super) fn focused_exited(&self) -> bool {
        (self.focused_terminal()).is_some_and(|terminal| self.exited.contains(&terminal))
    }

    /// Closes a command line being typed, which was for the pane it was
    /// begun in, as its range from a selection is.
    fn drop_command_line(&mut self) {
        if (self.input.as_ref()).is_some_and(|input| input.purpose == Purpose::Command) {
            self.input = None;
        }
    }

    /// Ends what was half done in `pane` as the focus leaves it: a
    /// visual selection, a command half typed, a search or a command line
    /// being typed, an editor's panel by its cursor, and keys waiting for
    /// its text.
    pub(super) fn leave_pane(&mut self, pane: PaneId) {
        self.drop_command_line();
        if self.held_normal == Some(pane) {
            self.held_normal = None;
        }
        if self
            .insert_on_load
            .is_some_and(|(waiting, _)| waiting == pane)
        {
            self.stop_waiting_to_insert(None);
        }
        self.close_editor_panel(pane);
        self.editor_leaves(pane);
        let terminal =
            (self.workspaces.find_pane(pane)).and_then(|(_, _, found)| found.content.terminal());
        if let Some(found) = terminal.and_then(|terminal| self.panes.get_mut(&terminal)) {
            found.leave();
        }
    }

    /// The focus has arrived at the pane shown: it takes the window's
    /// mode, but for a lesson, which stays in normal mode, a chat asking
    /// something, which arrives in normal mode, and an editor whose text
    /// hasn't come, which waits for it.
    pub(super) fn arrive(&mut self) {
        let Some(arrived) = self.shown_content() else {
            return;
        };
        self.arrived = Some(arrived);
        self.insert_on_load = None;
        let (pane, content) = arrived;
        let insert = self.mode == WindowMode::Insert && !self.is_lesson_pane(pane);
        if insert && self.editor_unloaded(pane) {
            self.insert_on_load = Some((pane, Instant::now()));
        } else {
            self.editor_to_mode(pane, insert);
        }
        let Some(found) = (content.terminal()).and_then(|terminal| self.panes.get_mut(&terminal))
        else {
            return;
        };
        found.arrived();
        if insert && found.asking() {
            found.enter_normal_mode();
            self.held_normal = Some(pane);
        } else if insert {
            found.enter_insert_mode();
        } else {
            found.enter_normal_mode();
        }
    }

    /// Whether keys wait for the focused editor's text, to be typed in
    /// insert mode once it's come.
    pub(super) fn waits_to_insert(&self) -> bool {
        self.insert_on_load.is_some_and(|(pane, _)| {
            self.shown.is_some_and(|shown| shown.pane == pane) && self.editor_unloaded(pane)
        })
    }

    /// Holds `held`, typed while the focused editor waits for its text,
    /// if it's a key insert mode would type, or a paste. Any other gives
    /// up waiting, leaving the editor in normal mode, and is handed back,
    /// to be had at once.
    pub(super) fn hold_for_insert(&mut self, held: Held) -> Option<Held> {
        if !types_in_insert(&held) {
            self.stop_waiting_to_insert(None);
            return Some(held);
        }
        self.held_for = self.insert_on_load.map(|(pane, _)| pane);
        self.hold(held);
        None
    }

    /// Gives up waiting for the focused editor's text to go to insert
    /// mode: it stays in normal mode, and the keys held for it go, as the
    /// bar says, with `why` or else that they were dropped.
    pub(super) fn stop_waiting_to_insert(&mut self, why: Option<&str>) {
        if self.insert_on_load.take().is_some() {
            self.drop_held_for_insert(why);
        }
    }

    /// Drops the keys held for an editor's text, if any are, saying so.
    /// Said by the editor they were for, if it has the focus, past the
    /// key that gave up waiting, which it then has.
    fn drop_held_for_insert(&mut self, why: Option<&str>) {
        let Some(pane) = self.held_for.take() else {
            return;
        };
        if !self.holding() {
            return;
        }
        self.drop_held();
        let why = why
            .unwrap_or("the keys typed before the file's text came were dropped")
            .to_owned();
        if self.shown.is_none_or(|shown| shown.pane != pane) || !self.say_past_key(pane, &why) {
            self.say(why);
        }
    }

    /// Before keys held are had: those held for an editor's text go only
    /// to it, so if the focus has left it, they go.
    pub(super) fn keep_held_with_their_editor(&mut self) {
        let Some(pane) = self.held_for else { return };
        if !self.holding() {
            self.held_for = None;
        } else if self.shown.map(|shown| shown.pane) != Some(pane) {
            self.drop_held_for_insert(None);
        }
    }

    /// The editor arrived at in insert mode before its text came: once
    /// the text has come, it goes to insert mode, if it still has the
    /// focus and the window is still in insert mode, and then the keys
    /// held for it are had. Should the focus have gone, or the wait
    /// lasted as long as a command waits for the grammar, they go.
    pub(super) fn insert_once_loaded(&mut self) {
        let Some((pane, since)) = self.insert_on_load else {
            return;
        };
        if self.shown.map(|shown| shown.pane) != Some(pane) {
            self.stop_waiting_to_insert(None);
            return;
        }
        if self.editor_unloaded(pane) {
            if Instant::now() >= since + PATIENCE {
                self.insert_waited_too_long();
            }
            return;
        }
        self.insert_on_load = None;
        self.editor_to_mode(pane, self.mode == WindowMode::Insert);
    }

    /// Gives up waiting for an editor's text if it's waited as long as a
    /// command waits for the grammar by `now`. Returns whether it did.
    pub(super) fn insert_patience(&mut self, now: Instant) -> bool {
        let due = self.insert_gives_up().is_some_and(|due| due <= now);
        if due && self.waits_to_insert() {
            self.insert_waited_too_long();
        }
        due
    }

    /// Gives up waiting for an editor's text: it stays in normal mode,
    /// and the bar says so if keys held for it go.
    fn insert_waited_too_long(&mut self) {
        self.stop_waiting_to_insert(Some(
            "the file's text took too long to come: the keys typed for it were dropped",
        ));
    }

    /// When waiting for an editor's text is to be given up on.
    pub(super) fn insert_gives_up(&self) -> Option<Instant> {
        self.insert_on_load.map(|(_, since)| since + PATIENCE)
    }

    /// After the daemon's workspaces changed: the pane shown takes the
    /// window's mode if it's another than the focus last arrived at, as
    /// when the one focused closed, or shows something else, as after
    /// `:e`, if this window asked for that, or its program ended
    /// (`ended`). If anyone else moved the focus, it arrives in normal
    /// mode, which the window goes to. The first pane a window shows keeps
    /// its own mode, which the window starts in: insert mode for a
    /// terminal, normal mode for an editor, as in Vim, and for a pane with
    /// no insert mode, as its bar says.
    pub(super) fn arrive_if_moved(&mut self, ended: bool) {
        let now = self.shown_content();
        match (self.arrived, now) {
            (Some(arrived), Some(now)) if arrived != now => {
                let (left, _) = arrived;
                let asked = if left == now.0 {
                    self.opening_here(left)
                } else if self.workspaces.find_pane(left).is_none() {
                    self.closing.contains(&left) || ended
                } else {
                    // Moved away from a pane that's still there.
                    self.leave_pane(left);
                    false
                };
                // Whatever's left of the pane, a command line begun in it
                // is no one else's.
                self.drop_command_line();
                if !asked {
                    self.mode = WindowMode::Normal;
                }
                self.arrive();
            }
            (None, Some(now)) => {
                self.arrived = Some(now);
                self.mode = match self.focused_mode() {
                    Some(Mode::Insert) => WindowMode::Insert,
                    _ => WindowMode::Normal,
                };
            }
            _ => {}
        }
        let workspaces = &self.workspaces;
        self.closing
            .retain(|&pane| workspaces.find_pane(pane).is_some());
        if !self.exited.is_empty() {
            let anywhere: std::collections::HashSet<TerminalId> = (workspaces.list.iter())
                .flat_map(clackatoo_protocol::Workspace::terminals)
                .collect();
            self.exited.retain(|terminal| anywhere.contains(terminal));
        }
    }

    /// The pane shown, and what it shows.
    fn shown_content(&self) -> Option<(PaneId, Content)> {
        let shown = self.shown?;
        let (_, _, pane) = self.workspaces.find_pane(shown.pane)?;
        Some((pane.id, pane.content))
    }
}

/// Whether `held` is what insert mode types: text, Backspace, Enter, Tab
/// or an arrow, or a paste.
fn types_in_insert(held: &Held) -> bool {
    let Held::Key { press, .. } = held else {
        return true;
    };
    let press = press.press();
    if press.modifiers.ctrl || press.modifiers.alt {
        return false;
    }
    matches!(
        press.key,
        Key::Character(_)
            | Key::Named(
                NamedKey::Space
                    | NamedKey::Backspace
                    | NamedKey::Enter
                    | NamedKey::Tab
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::ArrowUp
                    | NamedKey::ArrowDown
            )
    )
}
