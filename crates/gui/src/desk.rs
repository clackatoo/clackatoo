//! What the window shows of the daemon's workspaces, and what keys do to
//! them: which workspace, tab and pane are shown, the panes' terminals,
//! the command line, and the messages for the daemon. It draws nothing
//! and does no I/O, so tests can drive it as a window would.
//!
//! Every terminal of the workspace shown is attached, including those in
//! tabs not shown: each sends one update, which waits undrawn and
//! unacknowledged, so the daemon sends no more until the tab is shown, and
//! then it shows at once. Panes of other workspaces keep their state, such
//! as their mode and scroll, for when their workspace is shown again.
//!
//! The mode follows the focus around: moving from a pane in normal mode to
//! another, or opening one, arrives in normal mode too.

use std::collections::{HashMap, HashSet, VecDeque};

use clackatoo_protocol::{
    Axis, ClientMessage, Content, Layout, NewAgent, NewTerminal, PaneId, PaneTerminal, Request,
    Response, Side, Size, Tab, TabId, TerminalId, ViewUpdate, WorkspaceId, Workspaces,
};
use clackatoo_term::mouse::{MouseAction, MouseButton};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{ModeName, Status};
use crate::command::{self, Command, Kind, Place};
use crate::input::{self, KeyPress, Modifiers};
use crate::layout::{Area, Direction, Divider, Placement};
use crate::pane::{self, Copy, Mode, Pane, PaneMouse, Selection};

/// What a mouse event did, for the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MouseOutcome {
    /// The window must be redrawn.
    pub(crate) redraw: bool,
    /// The window is to paste the primary selection into the focused pane.
    pub(crate) paste_primary: bool,
}

/// What the window shows: a workspace, one of its tabs, and the pane in
/// that tab with the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) workspace: WorkspaceId,
    pub(crate) tab: TabId,
    pub(crate) pane: PaneId,
}

/// Where typed text goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextTarget {
    /// The focused pane's program, in insert mode.
    Pane,
    /// The bottom bar: the command line, or a search being typed.
    Bar,
}

/// Who sent a request, and so gets its answer.
#[derive(Clone, Copy, Debug)]
enum Asker {
    /// A pane, which keeps track of its own requests.
    Pane(TerminalId),
    /// The window itself.
    Window(Asked),
}

/// What the window asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// A new pane, to switch to once made.
    Pane,
    /// Something typed on the command line, whose failure is shown.
    Command,
    /// Something done for the window's own sake, whose failure is logged.
    Quiet,
    /// A split resized, whose shares are shown ahead of the daemon's until
    /// it has answered.
    Resize,
}

mod jumps;
mod prefix;

/// Something being typed in the palette.
#[derive(Debug)]
struct Input {
    purpose: Purpose,
    text: String,
    /// Which of the choices listed under it is picked, if it lists any.
    selected: usize,
}

/// What's typed in the palette is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    /// A command, after `:`.
    Command,
    /// A workspace's new name.
    RenameWorkspace(WorkspaceId),
    /// A tab's new name: empty for none.
    RenameTab(TabId),
    /// A workspace to switch to, or the name of one to make.
    Workspace,
}

/// The window's view of the daemon's workspaces.
#[derive(Debug, Default)]
pub(crate) struct Desk {
    /// The daemon's workspaces, as last heard.
    workspaces: Workspaces,
    /// What's shown, once connected.
    shown: Option<Shown>,
    /// Panes by the terminal each shows: those of the workspace shown, and
    /// of others shown before.
    panes: HashMap<TerminalId, Pane>,
    /// Whether there's a connection to send messages on.
    connected: bool,
    /// The terminals attached to on this connection.
    attached: HashSet<TerminalId>,
    /// Who sent each request not yet answered, oldest first: answers come
    /// in the order requests were sent.
    asked: VecDeque<Asker>,
    /// Messages for the daemon, oldest first, besides the panes' own.
    outbox: Vec<ClientMessage>,
    /// The space the panes' tiles share, once the window is open.
    area: Option<Area>,
    /// What's being typed in the palette, such as a command after `:`.
    input: Option<Input>,
    /// A message for the focused pane's label, until the next key, such as
    /// why a command failed.
    message: Option<String>,
    /// Text copied, waiting for the window to put on the clipboard or the
    /// primary selection.
    yanked: Option<(String, Copy)>,
    /// The terminal whose pane a mouse button went down in, and the
    /// button: it gets the drag, wherever the pointer goes.
    dragging: Option<(TerminalId, MouseButton)>,
    /// The cell the mouse was last over, so motion is passed on once per
    /// cell, not once per pixel.
    mouse_cell: Option<(TerminalId, [i64; 2])>,
    /// The window doesn't have the focus, so no pane does.
    unfocused: bool,
    /// The divider being dragged, as it was when grabbed.
    dragging_divider: Option<Divider>,
    /// Shares given to a split of a tab here, shown ahead of the daemon's
    /// while a divider is dragged and until the daemon has answered every
    /// resize sent, so the divider never jumps back.
    split_shares: Option<(TabId, Vec<u16>, Vec<u32>)>,
    /// Resizes sent and not answered yet.
    resizes_pending: usize,
    /// Every workspace has closed, so the window should too.
    finished: bool,
    /// What Ctrl+B is waiting for, after it's pressed.
    prefix: Option<prefix::Prefix>,
    /// Where the focus has been.
    jumps: jumps::Jumps,
}

/// Whether `press` is Ctrl+Shift+O, back through where the focus has been
/// (`Some(true)`), or Ctrl+Shift+I, forward again (`Some(false)`).
fn global_jump(press: &KeyPress<'_>) -> Option<bool> {
    let modifiers = press.modifiers;
    if !(modifiers.ctrl && modifiers.shift && !modifiers.alt) {
        return None;
    }
    match press.key {
        Key::Character(text) if text.eq_ignore_ascii_case("o") => Some(true),
        Key::Character(text) if text.eq_ignore_ascii_case("i") => Some(false),
        _ => None,
    }
}

impl Desk {
    // --- What's shown -----------------------------------------------------

    pub(crate) fn shown(&self) -> Option<Shown> {
        self.shown
    }

    pub(crate) fn pane(&self, terminal: TerminalId) -> Option<&Pane> {
        self.panes.get(&terminal)
    }

    pub(crate) fn pane_mut(&mut self, terminal: TerminalId) -> Option<&mut Pane> {
        self.panes.get_mut(&terminal)
    }

    /// Passes what an input method is composing to the focused pane, if
    /// it's a chat being typed in, which shows it in its box. Returns
    /// whether it was: the window shows it over a terminal itself.
    pub(crate) fn compose_in_chat(
        &mut self,
        composing: Option<(String, Option<(usize, usize)>)>,
    ) -> bool {
        self.focused_pane()
            .is_some_and(|pane| pane.compose_in_chat(composing))
    }

    /// Carries out what was typed or clicked in chats, which needs the
    /// fonts. Returns whether the window must be redrawn.
    pub(crate) fn settle_chats(&mut self, fonts: &mut crate::fonts::Fonts) -> bool {
        let mut redraw = false;
        for pane in self.panes.values_mut() {
            redraw |= pane.settle_chat(fonts);
        }
        redraw
    }

    /// Where typed text goes now, if keys type text rather than being
    /// commands, as they are in normal mode.
    pub(crate) fn text_goes_to(&self) -> Option<TextTarget> {
        if self.input.is_some() {
            return Some(TextTarget::Bar);
        }
        let pane = self.pane(self.focused_terminal()?)?;
        if pane.prompting() {
            Some(TextTarget::Bar)
        } else {
            (pane.mode == Mode::Insert).then_some(TextTarget::Pane)
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// When a pane's highlight on text just copied goes, the soonest of
    /// them: the window must be drawn again then.
    pub(crate) fn flash_ends(&self) -> Option<std::time::Instant> {
        self.panes.values().filter_map(Pane::flash_ends).min()
    }

    /// Takes text copied, for the window to put on the clipboard or the
    /// primary selection.
    pub(crate) fn take_yanked(&mut self) -> Option<(String, Copy)> {
        self.yanked.take()
    }

    /// The terminal the focused pane shows.
    pub(crate) fn focused_terminal(&self) -> Option<TerminalId> {
        let shown = self.shown?;
        let (_, _, pane) = self.workspaces.find_pane(shown.pane)?;
        let Content::Terminal(terminal) = pane.content;
        Some(terminal)
    }

    fn focused_pane(&mut self) -> Option<&mut Pane> {
        let terminal = self.focused_terminal()?;
        self.panes.get_mut(&terminal)
    }

    /// The tab shown.
    pub(crate) fn shown_tab(&self) -> Option<&Tab> {
        let shown = self.shown?;
        self.workspaces.get(shown.workspace)?.tab(shown.tab)
    }

    /// Where the shown tab's panes go, with any split being resized at its
    /// new shares.
    pub(crate) fn placement(&self) -> Option<Placement> {
        let tab = self.shown_tab()?;
        let area = self.area.as_ref()?;
        if let Some((resized, path, new_shares)) = &self.split_shares
            && *resized == tab.id
        {
            let mut layout = tab.layout.clone();
            if let Some(Layout::Split { shares, .. }) = layout.at_mut(path)
                && shares.len() == new_shares.len()
            {
                shares.clone_from(new_shares);
            }
            return Some(Placement::new(&layout, area));
        }
        Some(Placement::new(&tab.layout, area))
    }

    /// The title for the window: the focused pane's program's.
    pub(crate) fn title(&self) -> Option<&str> {
        let terminal = self.focused_terminal()?;
        let title = self.panes.get(&terminal)?.title()?;
        (!title.is_empty()).then_some(title)
    }

    /// Every workspace, by name, and whether it's the one shown.
    pub(crate) fn workspace_names(&self) -> Vec<(WorkspaceId, String, usize, bool)> {
        let shown = self.shown.map(|shown| shown.workspace);
        self.workspaces
            .list
            .iter()
            .map(|workspace| {
                (
                    workspace.id,
                    workspace.name.clone(),
                    workspace.tabs.len(),
                    Some(workspace.id) == shown,
                )
            })
            .collect()
    }

    /// The shown workspace's tabs, by name, and whether each is the one
    /// shown.
    pub(crate) fn tab_names(&self) -> Vec<(TabId, String, bool)> {
        let Some(shown) = self.shown else {
            return Vec::new();
        };
        let Some(workspace) = self.workspaces.get(shown.workspace) else {
            return Vec::new();
        };
        workspace
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| (tab.id, self.tab_name(index, tab), tab.id == shown.tab))
            .collect()
    }

    /// What the status line shows: the command line or a search being
    /// typed, or else the tabs, with a message or the focused pane's mode
    /// and scroll position.
    pub(crate) fn status(&self) -> Status {
        if let Some(input) = &self.input {
            let (label, prefix) = match input.purpose {
                Purpose::Command => (None, ":"),
                Purpose::RenameWorkspace(_) => (Some("Rename workspace"), ""),
                Purpose::RenameTab(_) => (Some("Rename tab"), ""),
                Purpose::Workspace => (Some("Workspace"), ""),
            };
            let choices = match input.purpose {
                Purpose::Workspace => {
                    let mut choices: Vec<(String, bool)> = self
                        .workspace_choices(&input.text)
                        .into_iter()
                        .enumerate()
                        .map(|(index, (_, name))| (name, index == input.selected))
                        .collect();
                    let name = input.text.trim();
                    if choices.is_empty() && !name.is_empty() {
                        choices.push((format!("New workspace “{name}”"), true));
                    }
                    choices
                }
                _ => Vec::new(),
            };
            return Status::Typing {
                label: label.map(str::to_owned),
                text: format!("{prefix}{}▏", input.text),
                choices,
            };
        }
        let pane = self
            .focused_terminal()
            .and_then(|terminal| self.panes.get(&terminal));
        if let Some(pane) = pane
            && pane.prompting()
            && let Some(prompt) = pane.label()
        {
            return Status::Typing {
                label: None,
                text: prompt,
                choices: Vec::new(),
            };
        }
        let mode = pane.map(|pane| match pane.mode {
            Mode::Insert => ModeName::Insert,
            Mode::Normal => ModeName::Normal,
            Mode::Visual(Selection::Characters) => ModeName::Visual,
            Mode::Visual(Selection::Lines) => ModeName::VisualLine,
        });
        Status::Tabs {
            mode,
            note: self
                .prefix_note()
                .map(str::to_owned)
                .or_else(|| self.message.clone())
                .or_else(|| pane.and_then(Pane::label)),
            panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
        }
    }

    /// What the bar calls a tab: its number, and its name, or else its
    /// focused pane's title.
    fn tab_name(&self, index: usize, tab: &Tab) -> String {
        /// Longer titles are cut short, to leave room for the other tabs.
        const MAX_TITLE: usize = 24;
        let number = index + 1;
        let title = tab.name.as_deref().or_else(|| self.tab_title(tab));
        match title {
            Some(title) if title.chars().count() > MAX_TITLE => {
                let short: String = title.chars().take(MAX_TITLE - 1).collect();
                format!("{number} {short}…")
            }
            Some(title) => format!("{number} {title}"),
            None => number.to_string(),
        }
    }

    /// The title of `tab`'s focused pane's program, if it set one.
    fn tab_title(&self, tab: &Tab) -> Option<&str> {
        self.workspaces
            .find_pane(tab.focused)
            .and_then(|(_, _, pane)| {
                let Content::Terminal(terminal) = pane.content;
                self.panes.get(&terminal)?.title()
            })
            .map(str::trim)
            .filter(|title| !title.is_empty())
    }

    /// Opens the palette to rename `workspace`, starting from its name.
    pub(crate) fn rename_workspace(&mut self, workspace: WorkspaceId) {
        if let Some(found) = self.workspaces.get(workspace) {
            self.input = Some(Input {
                purpose: Purpose::RenameWorkspace(workspace),
                text: found.name.clone(),
                selected: 0,
            });
        }
    }

    /// Opens the palette to name `tab`, starting from its name or title.
    pub(crate) fn rename_tab(&mut self, tab: TabId) {
        if let Some((_, found)) = self.workspaces.find_tab(tab) {
            let text = found
                .name
                .as_deref()
                .or_else(|| self.tab_title(found))
                .unwrap_or_default()
                .to_owned();
            self.input = Some(Input {
                purpose: Purpose::RenameTab(tab),
                text,
                selected: 0,
            });
        }
    }

    /// Closes `workspace`, with every terminal in it.
    pub(crate) fn close_workspace(&mut self, workspace: WorkspaceId) {
        self.request(Request::CloseWorkspace { workspace }, Asked::Command);
    }

    /// Closes `tab`, with every terminal in it.
    pub(crate) fn close_tab(&mut self, tab: TabId) {
        self.request(Request::CloseTab { tab }, Asked::Command);
    }

    /// Makes a workspace, and switches to it.
    pub(crate) fn new_workspace(&mut self) {
        self.run(Command::Workspace(None));
    }

    /// Opens a terminal in a new tab of the workspace shown.
    pub(crate) fn new_tab(&mut self) {
        self.run(Command::Open {
            place: Place::Tab,
            kind: Kind::Terminal,
            daemon: None,
        });
    }

    /// Switches to `workspace`, at its active tab and that tab's focused
    /// pane, as when its name is clicked.
    pub(crate) fn show_workspace(&mut self, workspace: WorkspaceId) {
        let Some(workspace) = self.workspaces.get(workspace) else {
            return;
        };
        let pane = workspace
            .tab(workspace.active_tab)
            .unwrap_or(&workspace.tabs[0])
            .focused;
        self.show_pane(pane);
    }

    /// Switches to `tab` of the workspace shown, at its focused pane.
    pub(crate) fn show_tab(&mut self, tab: TabId) {
        if let Some(tab) = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .and_then(|workspace| workspace.tab(tab))
        {
            self.show_pane(tab.focused);
        }
    }

    /// Focuses the pane whose tile is at `point`, as when it's clicked.
    /// Returns whether one is.
    pub(crate) fn focus_at(&mut self, point: [f32; 2]) -> bool {
        let pane = self
            .placement()
            .and_then(|placement| placement.pane_at(point));
        if let Some(pane) = pane {
            self.show_pane(pane);
        }
        pane.is_some()
    }

    /// Sets the space the panes share, as the window opens or resizes, and
    /// sizes the terminals shown to fit.
    pub(crate) fn set_area(&mut self, area: Option<Area>) {
        self.area = area;
        self.fit_panes();
    }

    /// Makes sure what's shown still exists, after the workspaces changed:
    /// what was shown if it's still there, or else the nearest to it.
    fn settle(&mut self) {
        let workspaces = &self.workspaces;
        let Some(workspace) = self
            .shown
            .and_then(|shown| workspaces.get(shown.workspace))
            .or_else(|| workspaces.active.and_then(|id| workspaces.get(id)))
            .or_else(|| workspaces.list.first())
        else {
            self.shown = None;
            return;
        };
        let tab = self
            .shown
            .and_then(|shown| workspace.tab(shown.tab))
            .or_else(|| workspace.tab(workspace.active_tab))
            .unwrap_or(&workspace.tabs[0]);
        let pane = self
            .shown
            .map(|shown| shown.pane)
            .filter(|&pane| tab.layout.contains(pane))
            .unwrap_or(tab.focused);
        self.shown = Some(Shown {
            workspace: workspace.id,
            tab: tab.id,
            pane,
        });
    }

    /// Switches to `pane`, wherever it is. From a pane in normal mode, it
    /// arrives in normal mode too. The pane left stays as it was.
    fn show_pane(&mut self, pane: PaneId) {
        self.move_focus(pane, true);
    }

    /// Goes back through the places the focus has been, or forward again:
    /// within the tab shown, or anywhere.
    fn jump(&mut self, back: bool, within_tab: bool) {
        let Some(shown) = self.shown else { return };
        let workspaces = &self.workspaces;
        let wanted = |place: &Shown| {
            *place != shown
                && workspaces
                    .find_pane(place.pane)
                    .is_some_and(|(_, tab, _)| !within_tab || tab.id == shown.tab)
        };
        if let Some((index, place)) = self.jumps.step(back, wanted) {
            self.jumps.go(index);
            self.move_focus(place.pane, false);
        }
    }

    /// Switches to `pane`, as [`show_pane`](Self::show_pane) does, adding
    /// the move to where the focus has been if `record`.
    fn move_focus(&mut self, pane: PaneId, record: bool) {
        let Some((workspace, tab, _)) = self.workspaces.find_pane(pane) else {
            return;
        };
        let shown = Shown {
            workspace: workspace.id,
            tab: tab.id,
            pane,
        };
        if self.shown == Some(shown) {
            return;
        }
        if record {
            self.jumps.moved(self.shown, shown);
        }
        let unfocused = self.unfocused;
        let normal = self.focused_pane().is_some_and(|left| {
            if !unfocused {
                left.focus_changed(false);
            }
            left.mode != Mode::Insert
        });
        self.shown = Some(shown);
        // Before the mode: a pane of another workspace may not exist yet.
        self.attach_shown();
        if let Some(arrived) = self.focused_pane() {
            if normal {
                arrived.enter_normal_mode();
            }
            if !unfocused {
                arrived.focus_changed(true);
            }
        }
        self.request(Request::Focus { pane }, Asked::Quiet);
        self.fit_panes();
    }

    /// Attaches to every terminal in the workspace shown, and detaches from
    /// the rest, making panes for terminals new to the window and
    /// forgetting those of terminals gone.
    fn attach_shown(&mut self) {
        let shown: HashSet<TerminalId> = self
            .shown
            .and_then(|shown| self.workspaces.get(shown.workspace))
            .map(|workspace| workspace.terminals().into_iter().collect())
            .unwrap_or_default();
        let anywhere: HashSet<TerminalId> = self
            .workspaces
            .list
            .iter()
            .flat_map(clackatoo_protocol::Workspace::terminals)
            .collect();
        self.panes.retain(|terminal, _| anywhere.contains(terminal));
        for &terminal in &shown {
            self.panes
                .entry(terminal)
                .or_insert_with(|| Pane::new(terminal));
        }
        if !self.connected {
            return;
        }
        let gone: Vec<TerminalId> = self.attached.difference(&shown).copied().collect();
        for terminal in gone {
            self.attached.remove(&terminal);
            // A closed terminal's view went with it.
            if anywhere.contains(&terminal) {
                self.request(Request::Detach { terminal }, Asked::Quiet);
            }
        }
        let new: Vec<TerminalId> = shown.difference(&self.attached).copied().collect();
        for terminal in new {
            self.attached.insert(terminal);
            self.request(Request::Attach { terminal }, Asked::Quiet);
        }
    }

    /// Asks the daemon to size each terminal shown to its pane.
    fn fit_panes(&mut self) {
        let Some(placement) = self.placement() else {
            return;
        };
        let Some(tab) = self.shown_tab() else { return };
        let panes = tab.layout.panes();
        for (id, tile) in placement.panes {
            let Some(pane) = panes.iter().find(|pane| pane.id == id) else {
                continue;
            };
            let Content::Terminal(terminal) = pane.content;
            if let (Some(pane), Ok(size)) = (
                self.panes.get_mut(&terminal),
                Size::new(tile.cols, tile.rows),
            ) {
                pane.resize(size);
            }
        }
    }

    // --- The daemon's news ------------------------------------------------

    /// A new connection, watching `workspaces`. Nothing is attached or
    /// asked yet, and panes kept from the last one start over, keeping
    /// their mode, scroll and history.
    pub(crate) fn connected(&mut self, workspaces: Workspaces) {
        self.connected = true;
        self.forget_asked();
        for pane in self.panes.values_mut() {
            pane.reconnected();
        }
        self.workspaces_changed(workspaces);
    }

    /// The connection was lost: nothing is attached, and nothing asked
    /// will be answered.
    pub(crate) fn lost(&mut self) {
        self.connected = false;
        self.forget_asked();
    }

    /// Forgets everything sent on the last connection, which will never be
    /// answered, and what was waiting on it: a split's new shares, and
    /// drags under way.
    fn forget_asked(&mut self) {
        self.attached.clear();
        self.asked.clear();
        self.outbox.clear();
        self.resizes_pending = 0;
        self.split_shares = None;
        self.dragging_divider = None;
        self.dragging = None;
        self.mouse_cell = None;
    }

    /// Takes the daemon's workspaces, as they are now.
    pub(crate) fn workspaces_changed(&mut self, workspaces: Workspaces) {
        self.workspaces = workspaces;
        if self.workspaces.list.is_empty() {
            // Every pane closed, as when the last shell exits.
            self.finished = true;
            return;
        }
        self.settle();
        self.attach_shown();
        self.fit_panes();
    }

    /// Applies a view update for `terminal`. Returns whether it's shown,
    /// and so the window must be redrawn.
    pub(crate) fn view_changed(&mut self, terminal: TerminalId, update: &ViewUpdate) -> bool {
        let Some(pane) = self.panes.get_mut(&terminal) else {
            return false;
        };
        if !pane.view_changed(update) {
            return false;
        }
        // Tabs not shown wait, undrawn, until they are.
        self.shown_tab().is_some_and(|tab| {
            tab.layout
                .panes()
                .iter()
                .any(|pane| pane.content == Content::Terminal(terminal))
        })
    }

    /// Hands the daemon's answer to whoever asked. Returns whether the
    /// window must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        match self.asked.pop_front() {
            Some(Asker::Pane(terminal)) => {
                let Some(pane) = self.panes.get_mut(&terminal) else {
                    return false;
                };
                let redraw = pane.answered(response);
                if let Some(text) = pane.take_yanked() {
                    self.yanked = Some(text);
                }
                redraw
            }
            Some(Asker::Window(Asked::Resize)) => {
                self.resizes_pending -= 1;
                if let Response::Error(err) = response {
                    log::warn!("the daemon wouldn't resize the split: {err}");
                }
                // The daemon's workspaces, sent before this answer, have the
                // last shares asked for.
                if self.resizes_pending == 0 && self.dragging_divider.is_none() {
                    self.split_shares = None;
                }
                false
            }
            Some(Asker::Window(asked)) => match (asked, response) {
                (Asked::Quiet, Response::Error(err)) => {
                    log::warn!("the daemon refused a request: {err}");
                    false
                }
                (_, Response::Error(err)) => {
                    self.message = Some(err.to_string());
                    true
                }
                // The workspaces came first, so the pane is there to show.
                (Asked::Pane, Response::PaneCreated(pane)) => {
                    self.show_pane(pane);
                    true
                }
                _ => false,
            },
            None => {
                log::warn!("the daemon answered a request nobody sent");
                false
            }
        }
    }

    /// Tells the daemon `terminals` are on screen, so each sends its next
    /// update.
    pub(crate) fn drawn(&mut self, terminals: &[TerminalId]) {
        for terminal in terminals {
            if let Some(pane) = self.panes.get_mut(terminal) {
                pane.drawn();
            }
        }
    }

    // --- Talking to the daemon --------------------------------------------

    /// Queues a request of the window's own.
    fn request(&mut self, request: Request, asked: Asked) {
        if self.connected {
            self.asked.push_back(Asker::Window(asked));
            self.outbox.push(ClientMessage::Request(request));
        }
    }

    /// Takes the messages waiting for the daemon: the window's, then each
    /// pane's.
    pub(crate) fn take_outbox(&mut self) -> Vec<ClientMessage> {
        let mut messages = std::mem::take(&mut self.outbox);
        for (&terminal, pane) in &mut self.panes {
            for message in pane.take_outbox() {
                if !self.connected {
                    continue;
                }
                if matches!(message, ClientMessage::Request(_)) {
                    self.asked.push_back(Asker::Pane(terminal));
                }
                messages.push(message);
            }
        }
        messages
    }

    // --- Keys -------------------------------------------------------------

    /// Handles a key press: the command line's, the window's own, then the
    /// focused pane's. Returns whether the window must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        // The keyboard reports Shift, Ctrl and the like as keys of their
        // own, on the way to a chord such as Ctrl+Shift+C. They mustn't
        // count as the next key: that would drop a selection or a message,
        // or the first key of a two-key command, before the chord arrives.
        if input::is_modifier(press.key) {
            return false;
        }
        // Ctrl+Shift+O and Ctrl+Shift+I: the terminal's own keys, as
        // Ctrl+Shift+C is, so programs lose nothing.
        if self.input.is_none()
            && let Some(back) = global_jump(press)
        {
            self.prefix = None;
            self.jump(back, false);
            return true;
        }
        if let Some(prefix) = self.prefix.take() {
            return self.prefix_key(prefix, press);
        }
        let prompting = self
            .focused_terminal()
            .and_then(|terminal| self.panes.get(&terminal))
            .is_some_and(Pane::prompting);
        if self.input.is_none() && !prompting && prefix::is_ctrl_b(press) {
            self.prefix = Some(prefix::Prefix::Waiting);
            self.message = None;
            return true;
        }
        if self.input.is_some() {
            // Ctrl+; and its kin close the command line, and go on to the
            // pane, which stays in normal mode.
            if !pane::enters_normal_mode(press) {
                self.input_key(press);
                return true;
            }
            self.input = None;
        }
        let had_message = self.message.take().is_some();
        let window_key = match self.focused_pane() {
            Some(pane) if pane.mode != Mode::Insert && !pane.prompting() => self.window_key(press),
            _ => false,
        };
        if window_key {
            return true;
        }
        let redraw = self.focused_pane().is_some_and(|pane| pane.key(press));
        redraw || had_message
    }

    /// Pastes `text` into the command line, if it's open, or else the
    /// focused pane.
    pub(crate) fn paste(&mut self, text: &str) {
        if let Some(input) = &mut self.input {
            input
                .text
                .extend(text.chars().filter(|ch| !ch.is_control()));
        } else if let Some(pane) = self.focused_pane() {
            pane.paste(text);
        }
    }

    /// Scrolls the pane at `point` `lines` up, or down if negative, as the
    /// mouse wheel does. Returns whether the window must be redrawn.
    pub(crate) fn wheel(&mut self, point: [f32; 2], lines: i64, modifiers: Modifiers) -> bool {
        // Away from the panes, as over the bars, the focused pane scrolls.
        let Some((terminal, cell, _)) = self
            .cell_at(point, None)
            .or_else(|| Some((self.focused_terminal()?, [0, 0], [0.0; 2])))
        else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.wheel(lines, cell, modifiers))
    }

    // --- The mouse --------------------------------------------------------

    /// Handles a mouse button going down or up, or the mouse moving, at
    /// `point`: `clicks` is 2 for a double click and 3 for a triple. A
    /// press focuses the pane it's in, and that pane gets the drag that
    /// follows, wherever it goes.
    pub(crate) fn mouse(
        &mut self,
        point: [f32; 2],
        action: MouseAction,
        clicks: u8,
        modifiers: Modifiers,
    ) -> MouseOutcome {
        let mut outcome = MouseOutcome::default();
        let dragging = self.dragging.map(|(terminal, _)| terminal);
        let Some((terminal, cell, pixel)) = self.cell_at(point, dragging) else {
            return outcome;
        };
        let action = match action {
            MouseAction::Press(button) => {
                outcome.redraw = self.focus_at(point);
                self.dragging = Some((terminal, button));
                action
            }
            MouseAction::Release(_) => {
                self.dragging = None;
                action
            }
            // Motion is passed on once per cell, with the button held.
            // A chat follows the mouse by the pixel.
            MouseAction::Motion(_) => {
                let chat = self.panes.get(&terminal).is_some_and(Pane::is_chat);
                if self.mouse_cell == Some((terminal, cell)) && !chat {
                    return outcome;
                }
                MouseAction::Motion(self.dragging.map(|(_, button)| button))
            }
        };
        self.mouse_cell = Some((terminal, cell));
        let Some(pane) = self.panes.get_mut(&terminal) else {
            return outcome;
        };
        // A middle click pastes the primary selection, unless the program
        // gets the mouse.
        if action == MouseAction::Press(MouseButton::Middle) && !pane.reports_mouse(modifiers.shift)
        {
            outcome.paste_primary = true;
            return outcome;
        }
        let event = PaneMouse {
            action,
            line: cell[0],
            col: cell[1],
            pixel,
            clicks,
            modifiers,
        };
        outcome.redraw |= pane.mouse(&event);
        outcome
    }

    /// Whether the program in the pane at `point` gets the mouse, rather
    /// than clackatoo.
    pub(crate) fn reports_mouse_at(&self, point: [f32; 2], shift: bool) -> bool {
        self.cell_at(point, None)
            .and_then(|(terminal, _, _)| self.panes.get(&terminal))
            .is_some_and(|pane| pane.reports_mouse(shift))
    }

    // --- Dividers -----------------------------------------------------------

    /// Which way the divider at `point` runs, if there is one: dragging it
    /// resizes the panes either side.
    pub(crate) fn divider_at(&self, point: [f32; 2]) -> Option<Axis> {
        if let Some(divider) = &self.dragging_divider {
            return Some(divider.axis);
        }
        Some(self.placement()?.divider_at(point)?.axis)
    }

    /// Grabs the divider at `point`, if there is one, to drag it. A double
    /// click shares its split's space equally instead. Returns whether
    /// there was one.
    pub(crate) fn grab_divider(&mut self, point: [f32; 2], clicks: u8) -> bool {
        let Some(divider) = self
            .placement()
            .and_then(|placement| placement.divider_at(point).cloned())
        else {
            return false;
        };
        if clicks == 2 {
            let equal = vec![1; divider.parts.len()];
            self.resize_split(&divider.split, equal);
        } else {
            self.dragging_divider = Some(divider);
        }
        true
    }

    /// Moves the divider being dragged to `point`. Returns whether the
    /// window must be redrawn.
    pub(crate) fn drag_divider(&mut self, point: [f32; 2]) -> bool {
        let (Some(divider), Some(area)) = (&self.dragging_divider, &self.area) else {
            return false;
        };
        let position = match divider.axis {
            Axis::LeftToRight => point[0],
            Axis::TopToBottom => point[1],
        };
        let shares = divider.shares_at(position, area);
        let path = divider.split.clone();
        let unchanged = self
            .split_shares
            .as_ref()
            .is_some_and(|(_, at, current)| *at == path && *current == shares);
        if unchanged {
            return false;
        }
        self.resize_split(&path, shares);
        true
    }

    /// Lets go of the divider being dragged. Returns whether one was.
    pub(crate) fn drop_divider(&mut self) -> bool {
        let dropped = self.dragging_divider.take().is_some();
        if self.resizes_pending == 0 {
            self.split_shares = None;
        }
        dropped
    }

    /// In normal mode, Alt with `h`, `j`, `k` or `l`: moves an edge of the
    /// focused pane a cell that way, the edge on that side if it has one,
    /// or else the other.
    fn move_edge(&mut self, direction: Direction) {
        let (Some(shown), Some(placement), Some(area)) = (self.shown, self.placement(), self.area)
        else {
            return;
        };
        let Some(tile) = placement.tile(shown.pane) else {
            return;
        };
        let rect = tile.rect;
        let middle = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
        let half_gap = area.gap / 2.0;
        let (near, far, step) = match direction {
            Direction::Left => (
                [rect.x - half_gap, middle[1]],
                [rect.x + rect.width + half_gap, middle[1]],
                -area.cell[0],
            ),
            Direction::Right => (
                [rect.x + rect.width + half_gap, middle[1]],
                [rect.x - half_gap, middle[1]],
                area.cell[0],
            ),
            Direction::Up => (
                [middle[0], rect.y - half_gap],
                [middle[0], rect.y + rect.height + half_gap],
                -area.cell[1],
            ),
            Direction::Down => (
                [middle[0], rect.y + rect.height + half_gap],
                [middle[0], rect.y - half_gap],
                area.cell[1],
            ),
        };
        let Some(divider) = placement
            .divider_at(near)
            .or_else(|| placement.divider_at(far))
        else {
            return;
        };
        let shares = divider.shares_at(divider.position(&area) + step, &area);
        let path = divider.split.clone();
        self.resize_split(&path, shares);
    }

    /// Shows the split at `path` in the tab shown with `shares` at once,
    /// and asks the daemon to keep them.
    fn resize_split(&mut self, path: &[u16], shares: Vec<u32>) {
        let Some(tab) = self.shown.map(|shown| shown.tab) else {
            return;
        };
        self.split_shares = Some((tab, path.to_vec(), shares.clone()));
        self.resizes_pending += 1;
        self.request(
            Request::ResizeSplit {
                tab,
                split: path.to_vec(),
                shares,
            },
            Asked::Resize,
        );
        self.fit_panes();
    }

    /// The window gained or lost the focus: the focused pane's program is
    /// told, if it asked.
    pub(crate) fn window_focus(&mut self, focused: bool) {
        self.unfocused = !focused;
        if let Some(pane) = self.focused_pane() {
            pane.focus_changed(focused);
        }
    }

    /// The terminal of the pane at `point`, or of the pane showing `within`
    /// wherever `point` is, and the cell of its view under `point`, which
    /// is off the view if `point` is off the pane, and `point` from the
    /// pane's top left.
    fn cell_at(
        &self,
        point: [f32; 2],
        within: Option<TerminalId>,
    ) -> Option<(TerminalId, [i64; 2], [f32; 2])> {
        let area = self.area.as_ref()?;
        let placement = self.placement()?;
        let tab = self.shown_tab()?;
        let panes = tab.layout.panes();
        let terminal_of = |id: PaneId| {
            let pane = panes.iter().find(|pane| pane.id == id)?;
            let Content::Terminal(terminal) = pane.content;
            Some(terminal)
        };
        let (terminal, tile) = if let Some(within) = within {
            placement.panes.iter().find_map(|&(id, tile)| {
                (terminal_of(id) == Some(within)).then_some((within, tile))
            })?
        } else {
            let id = placement.pane_at(point)?;
            let tile = placement.panes.iter().find(|(pane, _)| *pane == id)?.1;
            (terminal_of(id)?, tile)
        };
        #[allow(clippy::cast_possible_truncation)]
        let cell = |offset: f32, size: f32| (offset / size).floor() as i64;
        let pixel = [point[0] - tile.origin[0], point[1] - tile.origin[1]];
        Some((
            terminal,
            [cell(pixel[1], area.cell[1]), cell(pixel[0], area.cell[0])],
            pixel,
        ))
    }

    /// Takes a key in the focused pane's normal mode if it's the window's:
    /// `:` opens the command line, and Ctrl with `h`, `j`, `k` or `l` moves
    /// to the pane to the left, below, above or right. Returns whether it
    /// took it.
    fn window_key(&mut self, press: &KeyPress<'_>) -> bool {
        let character = match press.key {
            Key::Character(text) => text.chars().next(),
            _ => None,
        };
        let ctrl = press.modifiers.ctrl && !press.modifiers.alt;
        let direction = match character {
            Some(':') if !ctrl => {
                self.input = Some(Input {
                    purpose: Purpose::Command,
                    text: String::new(),
                    selected: 0,
                });
                return true;
            }
            Some(letter @ ('h' | 'j' | 'k' | 'l')) if press.modifiers.alt && !ctrl => {
                self.move_edge(match letter {
                    'h' => Direction::Left,
                    'j' => Direction::Down,
                    'k' => Direction::Up,
                    _ => Direction::Right,
                });
                return true;
            }
            Some('h') if ctrl => Direction::Left,
            Some('j') if ctrl => Direction::Down,
            Some('k') if ctrl => Direction::Up,
            Some('l') if ctrl => Direction::Right,
            // Back and forward through the tab's panes, as through Vim's
            // jump list.
            Some(key @ ('o' | 'i')) if ctrl => {
                self.jump(key == 'o', true);
                return true;
            }
            _ => return false,
        };
        self.focus_towards(direction);
        true
    }

    /// Focuses the pane beside the focused one towards `direction`, if
    /// there is one.
    fn focus_towards(&mut self, direction: Direction) {
        if let Some(shown) = self.shown
            && let Some(next) = self
                .placement()
                .and_then(|placement| placement.neighbour(shown.pane, direction))
        {
            self.show_pane(next);
        }
    }

    /// Handles a key while the palette is open.
    fn input_key(&mut self, press: &KeyPress<'_>) {
        let Some(input) = &mut self.input else {
            return;
        };
        match press.key {
            Key::Named(NamedKey::Escape) => self.input = None,
            Key::Named(NamedKey::ArrowDown | NamedKey::ArrowUp | NamedKey::Tab)
                if input.purpose == Purpose::Workspace =>
            {
                let up = *press.key == Key::Named(NamedKey::ArrowUp)
                    || (*press.key == Key::Named(NamedKey::Tab) && press.modifiers.shift);
                let text = input.text.clone();
                let count = self.workspace_choices(&text).len();
                let Some(input) = &mut self.input else { return };
                input.selected = if up {
                    input.selected.saturating_sub(1)
                } else {
                    (input.selected + 1).min(count.saturating_sub(1))
                };
            }
            Key::Named(NamedKey::Enter) => {
                let Some(Input {
                    purpose,
                    text,
                    selected,
                }) = self.input.take()
                else {
                    return;
                };
                match purpose {
                    Purpose::Command => match command::parse(&text) {
                        Ok(command) => self.run(command),
                        Err(message) => self.message = Some(message),
                    },
                    Purpose::RenameWorkspace(workspace) => self.request(
                        Request::RenameWorkspace {
                            workspace,
                            name: text,
                        },
                        Asked::Command,
                    ),
                    Purpose::RenameTab(tab) => {
                        self.request(Request::RenameTab { tab, name: text }, Asked::Command);
                    }
                    Purpose::Workspace => self.pick_workspace(&text, selected),
                }
            }
            Key::Named(NamedKey::Backspace) => {
                // Deleting past a command's colon closes the palette, as
                // in Vim.
                if input.text.pop().is_none() && input.purpose == Purpose::Command {
                    self.input = None;
                }
            }
            _ if press.modifiers.ctrl => {}
            _ => {
                if let Some(text) = press.text {
                    input
                        .text
                        .extend(text.chars().filter(|ch| !ch.is_control()));
                    input.selected = 0;
                }
            }
        }
    }

    /// Carries out a command.
    fn run(&mut self, command: Command) {
        let Some(shown) = self.shown else { return };
        let new = |kind, size| match kind {
            Kind::Terminal => PaneTerminal::New(NewTerminal {
                size,
                command: None,
                cwd: None,
                record_to: None,
            }),
            Kind::Agent => PaneTerminal::Agent(NewAgent { size, cwd: None }),
        };
        match command {
            Command::Open {
                daemon: Some(daemon),
                ..
            } => {
                self.message = Some(format!(
                    "panes on {daemon} come with :connect, which isn't built yet"
                ));
            }
            Command::Open {
                place: Place::Tab,
                kind,
                daemon: None,
            } => {
                let terminal = new(kind, self.new_pane_size(None));
                self.request(
                    Request::NewTab {
                        workspace: shown.workspace,
                        terminal,
                    },
                    Asked::Pane,
                );
            }
            Command::Open {
                place: Place::Beside(side),
                kind,
                daemon: None,
            } => {
                let terminal = new(kind, self.new_pane_size(Some(side)));
                self.request(
                    Request::Split {
                        pane: shown.pane,
                        side,
                        terminal,
                    },
                    Asked::Pane,
                );
            }
            Command::Workspace(name) => {
                let existing = name.as_deref().and_then(|name| {
                    self.workspaces
                        .list
                        .iter()
                        .find(|workspace| workspace.name == name.trim())
                });
                if let Some(workspace) = existing {
                    self.show_workspace(workspace.id);
                } else {
                    let terminal = new(Kind::Terminal, self.new_pane_size(None));
                    self.request(Request::NewWorkspace { name, terminal }, Asked::Pane);
                }
            }
            Command::Tab(number) => {
                let tabs = self
                    .workspaces
                    .get(shown.workspace)
                    .map_or(&[][..], |workspace| &workspace.tabs[..]);
                match number.checked_sub(1).and_then(|index| tabs.get(index)) {
                    Some(tab) => self.show_pane(tab.focused),
                    None => {
                        self.message = Some(match tabs.len() {
                            1 => format!("there's no tab {number}; this workspace has one"),
                            count => format!("there's no tab {number}; this workspace has {count}"),
                        });
                    }
                }
            }
            Command::Rename(name) => self.request(
                Request::RenameWorkspace {
                    workspace: shown.workspace,
                    name,
                },
                Asked::Command,
            ),
            Command::Close => {
                self.request(Request::ClosePane { pane: shown.pane }, Asked::Command);
            }
        }
    }

    /// About the size a new pane will be: the size of every pane together,
    /// for a new tab or workspace, or half the focused pane's, beside it on
    /// `side`. The window resizes it to fit as soon as it's shown.
    fn new_pane_size(&self, side: Option<Side>) -> Size {
        let fallback = Size::new(80, 24).expect("80x24 is a valid size");
        let Some(area) = self.area else {
            return fallback;
        };
        let whole = Placement::new(
            &clackatoo_protocol::Layout::Pane(clackatoo_protocol::Pane {
                id: PaneId(0),
                content: Content::Terminal(TerminalId(0)),
            }),
            &area,
        )
        .panes[0]
            .1;
        let focused = self
            .shown
            .zip(self.placement())
            .and_then(|(shown, placement)| placement.tile(shown.pane));
        let (cols, rows) = match (side, focused) {
            (Some(side), Some(tile)) => match side.axis() {
                Axis::LeftToRight => ((tile.cols / 2).max(1), tile.rows),
                Axis::TopToBottom => (tile.cols, (tile.rows / 2).max(1)),
            },
            _ => (whole.cols, whole.rows),
        };
        Size::new(cols, rows).unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Layout, Pane as LayoutPane, View, Workspace};
    use clackatoo_term::mouse::{MouseAction, MouseButton};
    use clackatoo_term::{Config, Terminal};
    use winit::keyboard::{KeyCode, KeyLocation};

    use super::*;
    use crate::input::Modifiers;

    fn pane(id: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(id),
            content: Content::Terminal(TerminalId(id)),
        })
    }

    fn side_by_side(parts: Vec<Layout>) -> Layout {
        Layout::Split {
            axis: Axis::LeftToRight,
            shares: vec![1; parts.len()],
            parts,
        }
    }

    /// A tab, focused on its first pane.
    fn tab(id: u32, layout: Layout) -> Tab {
        Tab {
            id: TabId(id),
            focused: layout.panes()[0].id,
            layout,
            name: None,
        }
    }

    fn workspace(id: u32, name: &str, tabs: Vec<Tab>) -> Workspace {
        Workspace {
            id: WorkspaceId(id),
            name: name.into(),
            active_tab: tabs[0].id,
            tabs,
        }
    }

    fn workspaces(list: Vec<Workspace>) -> Workspaces {
        Workspaces {
            active: Some(list[0].id),
            list,
        }
    }

    /// A desk with 810 × 480 pixels for panes, connected to `workspaces`, with
    /// what it asked for so far answered.
    fn desk(workspaces: Workspaces) -> Desk {
        let mut desk = Desk::default();
        desk.set_area(Some(Area {
            rect: crate::layout::Rect {
                x: 0.0,
                y: 0.0,
                width: 810.0,
                height: 480.0,
            },
            cell: [10.0, 20.0],
            gap: 10.0,
            inset: 5.0,
        }));
        desk.connected(workspaces);
        answer_all(&mut desk);
        desk
    }

    /// Answers every request waiting with "done", and returns them.
    fn answer_all(desk: &mut Desk) -> Vec<Request> {
        let requests: Vec<Request> = desk
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                ClientMessage::FrameDone { .. }
                | ClientMessage::Hello { .. }
                | ClientMessage::Upgrade { .. } => None,
            })
            .collect();
        for _ in &requests {
            desk.answered(Response::Done);
        }
        requests
    }

    /// Gives `terminal` its first screen, as attaching does.
    fn snapshot(desk: &mut Desk, terminal: u32) {
        let screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        desk.view_changed(
            TerminalId(terminal),
            &ViewUpdate::Snapshot(View::capture(&screen)),
        );
    }

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(desk: &mut Desk, key: Key, code: Option<KeyCode>, modifiers: Modifiers) {
        let text = match &key {
            Key::Character(text) if !modifiers.ctrl => Some(text.to_string()),
            _ => None,
        };
        desk.key(&KeyPress {
            key: &key,
            text: text.as_deref(),
            location: KeyLocation::Standard,
            code,
            modifiers,
        });
    }

    const CTRL: Modifiers = Modifiers {
        shift: false,
        ctrl: true,
        alt: false,
    };

    fn ctrl(desk: &mut Desk, letter: &str) {
        press(desk, Key::Character(letter.into()), None, CTRL);
    }

    fn normal_mode(desk: &mut Desk) {
        press(
            desk,
            Key::Character(";".into()),
            Some(KeyCode::Semicolon),
            CTRL,
        );
    }

    /// Types `line` on the command line, and Enter.
    fn command(desk: &mut Desk, line: &str) {
        for ch in format!(":{line}").chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
        press(
            desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
    }

    fn mode(desk: &Desk, terminal: u32) -> Mode {
        desk.pane(TerminalId(terminal)).expect("a pane").mode
    }

    fn shown_pane(desk: &Desk) -> PaneId {
        desk.shown().expect("something shown").pane
    }

    #[test]
    fn ctrl_hjkl_move_between_panes_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )]));
        snapshot(&mut desk, 1);
        snapshot(&mut desk, 2);
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal, "the mode came along");
        assert_eq!(mode(&desk, 1), Mode::Normal, "the pane left kept its mode");
        assert!(answer_all(&mut desk).contains(&Request::Focus { pane: PaneId(2) }));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2), "nothing further right");
        ctrl(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(1));
    }

    #[test]
    fn ctrl_semicolon_goes_to_normal_mode_from_anywhere() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut desk, 1);
        let typing = |desk: &Desk| matches!(desk.status(), Status::Typing { .. });
        let key = |desk: &mut Desk, text: &str| {
            press(
                desk,
                Key::Character(text.into()),
                None,
                Modifiers::default(),
            );
        };
        normal_mode(&mut desk);
        assert_eq!(mode(&desk, 1), Mode::Normal);
        // From a visual selection.
        key(&mut desk, "v");
        assert_eq!(mode(&desk, 1), Mode::Visual(Selection::Characters));
        normal_mode(&mut desk);
        assert_eq!(mode(&desk, 1), Mode::Normal);
        // From a search being typed, and from the command line.
        for opens in ["/", ":"] {
            key(&mut desk, opens);
            key(&mut desk, "x");
            assert!(typing(&desk), "{opens} opens a prompt");
            normal_mode(&mut desk);
            assert!(!typing(&desk), "{opens}'s prompt closed");
            assert_eq!(mode(&desk, 1), Mode::Normal);
        }
        // Ctrl+\ twice from insert mode still reaches the program.
        key(&mut desk, "i");
        ctrl(&mut desk, "\\");
        assert_eq!(mode(&desk, 1), Mode::Normal);
        ctrl(&mut desk, "\\");
        assert_eq!(mode(&desk, 1), Mode::Insert);
    }

    #[test]
    fn a_split_opens_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        command(&mut desk, "|");
        let split = desk.take_outbox();
        assert!(matches!(
            split.as_slice(),
            [ClientMessage::Request(Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                ..
            })]
        ));
        // The daemon tells of the new pane, then answers.
        desk.workspaces_changed(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(3)]))],
        )]));
        desk.answered(Response::PaneCreated(PaneId(3)));
        assert_eq!(shown_pane(&desk), PaneId(3));
        assert_eq!(mode(&desk, 3), Mode::Normal);
        // Its cursor goes where the program's is, once there's a screen.
        snapshot(&mut desk, 3);
        let (_, cursor) = desk.pane(TerminalId(3)).unwrap().scene(24);
        assert!(cursor.is_some());
    }

    #[test]
    fn switching_workspaces_keeps_the_mode_and_each_panes_state() {
        let mut desk = desk(workspaces(vec![
            workspace(1, "a", vec![tab(1, pane(1))]),
            workspace(2, "b", vec![tab(2, pane(2))]),
        ]));
        snapshot(&mut desk, 1);
        normal_mode(&mut desk);
        command(&mut desk, "ws b");
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal, "the mode came along");
        let requests = answer_all(&mut desk);
        assert!(requests.contains(&Request::Detach {
            terminal: TerminalId(1)
        }));
        assert!(requests.contains(&Request::Attach {
            terminal: TerminalId(2)
        }));

        // Back in a, its pane is as it was left.
        press(
            &mut desk,
            Key::Character("i".into()),
            None,
            Modifiers::default(),
        );
        assert_eq!(mode(&desk, 2), Mode::Insert);
        normal_mode(&mut desk);
        command(&mut desk, "ws a");
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(mode(&desk, 1), Mode::Normal);
    }

    #[test]
    fn a_new_workspace_opens_in_normal_mode() {
        let mut desk = desk(workspaces(vec![workspace(1, "a", vec![tab(1, pane(1))])]));
        normal_mode(&mut desk);
        command(&mut desk, "ws b");
        assert!(matches!(
            desk.take_outbox().as_slice(),
            [ClientMessage::Request(Request::NewWorkspace { name: Some(name), .. })] if name == "b"
        ));
        desk.workspaces_changed(workspaces(vec![
            workspace(1, "a", vec![tab(1, pane(1))]),
            workspace(2, "b", vec![tab(2, pane(2))]),
        ]));
        desk.answered(Response::PaneCreated(PaneId(2)));
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
        assert_eq!(mode(&desk, 2), Mode::Normal);
    }

    /// Ctrl+B, then `keys`, each as typed.
    fn ctrl_b(desk: &mut Desk, keys: &str) {
        ctrl(desk, "b");
        type_keys(desk, keys);
    }

    fn type_keys(desk: &mut Desk, keys: &str) {
        for ch in keys.chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
    }

    fn three_tabs() -> Desk {
        desk(workspaces(vec![workspace(
            1,
            "work",
            vec![
                tab(1, side_by_side(vec![pane(1), pane(2), pane(3)])),
                tab(2, pane(4)),
                tab(3, pane(5)),
            ],
        )]))
    }

    #[test]
    fn ctrl_b_moves_between_tabs_and_panes_in_either_mode() {
        let mut desk = three_tabs();
        // In insert mode, the keys don't reach the program.
        ctrl_b(&mut desk, "3");
        assert_eq!(desk.shown().unwrap().tab, TabId(3));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::SendInput { .. }))
        );
        ctrl_b(&mut desk, "]");
        assert_eq!(desk.shown().unwrap().tab, TabId(1), "round to the first");
        ctrl_b(&mut desk, "[");
        assert_eq!(desk.shown().unwrap().tab, TabId(3), "round to the last");
        ctrl_b(&mut desk, "1");
        // Panes by number, shown on screen while one is awaited.
        ctrl_b(&mut desk, "p");
        assert!(desk.numbering_panes());
        assert_eq!(
            desk.status(),
            Status::Tabs {
                mode: Some(ModeName::Insert),
                note: Some("pane number".into()),
                panes: 3
            }
        );
        type_keys(&mut desk, "3");
        assert!(!desk.numbering_panes());
        assert_eq!(shown_pane(&desk), PaneId(3));
        // A click cancels it, as the window says when the mouse goes down.
        ctrl_b(&mut desk, "p");
        assert!(desk.cancel_prefix());
        assert!(!desk.numbering_panes());
        ctrl_b(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(2));
        // And in normal mode alike, where Ctrl+B used to page.
        normal_mode(&mut desk);
        ctrl_b(&mut desk, "p1");
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(mode(&desk, 1), Mode::Normal);
    }

    #[test]
    fn ctrl_b_makes_and_closes_panes_and_passes_itself_on() {
        let mut desk = three_tabs();
        answer_all(&mut desk);
        ctrl_b(&mut desk, "c");
        ctrl_b(&mut desk, "|");
        ctrl_b(&mut desk, "-");
        ctrl_b(&mut desk, "x");
        let requests = answer_all(&mut desk);
        assert!(matches!(requests[0], Request::NewTab { .. }));
        assert!(matches!(
            requests[1],
            Request::Split {
                side: Side::Right,
                ..
            }
        ));
        assert!(matches!(
            requests[2],
            Request::Split {
                side: Side::Below,
                ..
            }
        ));
        assert!(matches!(requests[3], Request::ClosePane { .. }));
        // Twice sends one to the program.
        ctrl_b(&mut desk, "");
        ctrl(&mut desk, "b");
        assert!(answer_all(&mut desk).contains(&Request::SendInput {
            terminal: TerminalId(1),
            bytes: vec![0x02],
        }));
        // Anything else does nothing, and goes nowhere.
        ctrl_b(&mut desk, "z");
        assert_eq!(desk.prefix, None);
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn ctrl_b_w_picks_a_workspace_or_makes_one() {
        let mut desk = desk(workspaces(vec![
            workspace(1, "clackatoo", vec![tab(1, pane(1))]),
            workspace(2, "peios", vec![tab(2, pane(2))]),
            workspace(3, "homelab", vec![tab(3, pane(3))]),
        ]));
        ctrl_b(&mut desk, "w");
        let choices = |desk: &Desk| match desk.status() {
            Status::Typing { choices, .. } => choices,
            Status::Tabs { .. } => panic!("the picker closed"),
        };
        assert_eq!(choices(&desk).len(), 3);
        // Typing narrows them; Down picks the next.
        type_keys(&mut desk, "o");
        let names: Vec<String> = choices(&desk).into_iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["clackatoo", "peios", "homelab"]);
        type_keys(&mut desk, "s");
        assert_eq!(choices(&desk), [("peios".to_owned(), true)]);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
        // A name nothing has makes a workspace by it.
        ctrl_b(&mut desk, "wnas");
        assert_eq!(choices(&desk), [("New workspace “nas”".to_owned(), true)]);
        answer_all(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::NewWorkspace { name: Some(name), .. }] if name == "nas"
        ));
        // Down and Up move through the choices.
        ctrl_b(&mut desk, "w");
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowDown),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::ArrowUp),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.shown().unwrap().workspace, WorkspaceId(2));
    }

    #[test]
    fn ctrl_o_and_ctrl_i_go_back_and_forth_within_the_tab_or_anywhere() {
        let mut desk = three_tabs();
        normal_mode(&mut desk);
        // Pane 2, tab 2's pane 4, back to tab 1 (at pane 1), then pane 3.
        ctrl_b(&mut desk, "p2");
        ctrl_b(&mut desk, "2");
        ctrl_b(&mut desk, "1");
        ctrl_b(&mut desk, "p3");
        // Within the tab: back to pane 1, then past tab 2's pane to pane 2,
        // and forward again past it to pane 1.
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(2));
        ctrl(&mut desk, "i");
        assert_eq!(shown_pane(&desk), PaneId(1));
        // Anywhere, in insert mode too: back to tab 2's pane, and forward.
        type_keys(&mut desk, "i");
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        press(&mut desk, Key::Character("O".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(4));
        press(&mut desk, Key::Character("I".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::SendInput { .. })),
            "the program heard nothing"
        );
    }

    #[test]
    fn numbers_move_between_tabs() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1)), tab(2, pane(2))],
        )]));
        normal_mode(&mut desk);
        command(&mut desk, "2");
        assert_eq!(desk.shown().unwrap().tab, TabId(2));
        command(&mut desk, "3");
        assert_eq!(desk.shown().unwrap().tab, TabId(2));
        assert_eq!(
            desk.message.as_deref(),
            Some("there's no tab 3; this workspace has 2")
        );
    }

    #[test]
    fn workspaces_and_tabs_are_renamed_in_the_palette() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        desk.rename_workspace(WorkspaceId(1));
        assert_eq!(
            desk.status(),
            Status::Typing {
                label: Some("Rename workspace".into()),
                text: "work▏".into(),
                choices: Vec::new(),
            }
        );
        for _ in 0..4 {
            press(
                &mut desk,
                Key::Named(NamedKey::Backspace),
                None,
                Modifiers::default(),
            );
        }
        press(
            &mut desk,
            Key::Character("x".into()),
            None,
            Modifiers::default(),
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            answer_all(&mut desk),
            [Request::RenameWorkspace {
                workspace: WorkspaceId(1),
                name: "x".into()
            }]
        );
        // Backspace past the start of a name leaves the palette open.
        desk.rename_tab(TabId(1));
        for _ in 0..3 {
            press(
                &mut desk,
                Key::Named(NamedKey::Backspace),
                None,
                Modifiers::default(),
            );
        }
        assert!(matches!(desk.status(), Status::Typing { .. }));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(matches!(desk.status(), Status::Tabs { .. }));
    }

    #[test]
    fn answers_go_to_whoever_asked() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, pane(1))],
        )]));
        normal_mode(&mut desk);
        command(&mut desk, "rename taken");
        desk.take_outbox();
        desk.answered(Response::Error(clackatoo_protocol::Error::NameTaken(
            "taken".into(),
        )));
        assert_eq!(
            desk.message.as_deref(),
            Some("there's already a workspace called \"taken\"")
        );
        // The next key clears it.
        press(
            &mut desk,
            Key::Character("j".into()),
            None,
            Modifiers::default(),
        );
        assert_eq!(desk.message.as_deref(), None);
    }

    #[test]
    fn a_drag_stays_with_the_pane_it_started_in() {
        let mut desk = desk(workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )]));
        // Both panes' programs ask for drags, in SGR.
        let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        screen.advance(b"\x1b[?1002h\x1b[?1006h");
        for terminal in [1, 2] {
            desk.view_changed(
                TerminalId(terminal),
                &ViewUpdate::Snapshot(View::capture(&screen)),
            );
        }
        desk.take_outbox();
        let mouse = |desk: &mut Desk, point, action| {
            desk.mouse(point, action, 1, Modifiers::default());
        };
        let left = MouseButton::Left;
        // The left tile's cells start 5 pixels in, and are 10 × 20.
        mouse(&mut desk, [50.0, 25.0], MouseAction::Press(left));
        mouse(&mut desk, [60.0, 25.0], MouseAction::Motion(None));
        // Still in the same cell: nothing more.
        mouse(&mut desk, [62.0, 30.0], MouseAction::Motion(None));
        // Over the right pane, but the drag is the left one's, at its edge.
        mouse(&mut desk, [500.0, 25.0], MouseAction::Motion(None));
        mouse(&mut desk, [500.0, 25.0], MouseAction::Release(left));

        let sent: Vec<(u32, String)> = desk
            .take_outbox()
            .into_iter()
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { terminal, bytes }) => {
                    Some((terminal.0, String::from_utf8(bytes).unwrap()))
                }
                _ => None,
            })
            .collect();
        let expected = [
            "\x1b[<0;5;2M",
            "\x1b[<32;6;2M",
            "\x1b[<32;40;2M",
            "\x1b[<0;40;2m",
        ];
        assert_eq!(sent, expected.map(|bytes| (1, bytes.to_owned())).to_vec());
    }

    /// The width of `pane`'s tile.
    fn width(desk: &Desk, pane: u32) -> f32 {
        desk.placement()
            .unwrap()
            .tile(PaneId(pane))
            .unwrap()
            .rect
            .width
    }

    /// The shares of every split resize asked of the daemon, oldest first.
    fn resizes(desk: &mut Desk) -> Vec<Vec<u32>> {
        answer_all(desk)
            .into_iter()
            .filter_map(|request| match request {
                Request::ResizeSplit { shares, .. } => Some(shares),
                _ => None,
            })
            .collect()
    }

    fn two_side_by_side() -> Workspaces {
        workspaces(vec![workspace(
            1,
            "work",
            vec![tab(1, side_by_side(vec![pane(1), pane(2)]))],
        )])
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn dragging_a_divider_resizes_the_panes_at_once_and_tells_the_daemon() {
        let mut desk = desk(two_side_by_side());
        // 810 pixels: 400, a gap at 400 to 410, and 400.
        assert_eq!(desk.divider_at([405.0, 100.0]), Some(Axis::LeftToRight));
        assert_eq!(desk.divider_at([300.0, 100.0]), None);
        assert!(desk.grab_divider([405.0, 100.0], 1));
        assert!(desk.drag_divider([303.0, 100.0]));
        assert_eq!(width(&desk, 1), 300.0);
        assert!(!desk.drag_divider([304.0, 100.0]), "within the same cell");
        desk.drop_divider();

        // The daemon's workspaces, from before it heard, don't move it back.
        desk.workspaces_changed(two_side_by_side());
        assert_eq!(width(&desk, 1), 300.0);
        assert_eq!(resizes(&mut desk), [vec![300 * 16, 500 * 16]]);
        // Once answered, the daemon's shares rule, as they would be by now.
        let mut resized = two_side_by_side();
        if let Layout::Split { shares, .. } = &mut resized.list[0].tabs[0].layout {
            *shares = vec![3, 5];
        }
        desk.workspaces_changed(resized);
        assert_eq!(width(&desk, 1), 300.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn alt_moves_the_focused_panes_edges_and_a_double_click_evens_them() {
        let mut desk = desk(two_side_by_side());
        normal_mode(&mut desk);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        // The left pane has its right edge only: Alt+L grows it, and Alt+H
        // shrinks it, both moving that edge a column.
        press(&mut desk, Key::Character("l".into()), None, alt);
        assert_eq!(width(&desk, 1), 410.0);
        press(&mut desk, Key::Character("h".into()), None, alt);
        press(&mut desk, Key::Character("h".into()), None, alt);
        assert_eq!(width(&desk, 1), 390.0);
        // Up and down have no edge to move here.
        press(&mut desk, Key::Character("j".into()), None, alt);
        assert_eq!(resizes(&mut desk).len(), 3);

        // Answered, with this test's daemon still sharing equally.
        assert!(desk.grab_divider([405.0, 100.0], 2));
        desk.drop_divider();
        assert_eq!(resizes(&mut desk), [vec![1, 1]]);
    }

    #[test]
    fn ctrl_shift_c_copies_a_mouse_selection_typed_a_key_at_a_time() {
        let mut desk = desk(two_side_by_side());
        let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
        screen.advance(b"hello world");
        desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&screen)));
        answer_all(&mut desk);
        let left = MouseButton::Left;
        // Cells start 5 pixels in, and are 10 × 20: "hello" is 7 to 47.
        desk.mouse(
            [7.0, 10.0],
            MouseAction::Press(left),
            1,
            Modifiers::default(),
        );
        desk.mouse(
            [47.0, 10.0],
            MouseAction::Motion(None),
            1,
            Modifiers::default(),
        );
        desk.mouse(
            [47.0, 10.0],
            MouseAction::Release(left),
            1,
            Modifiers::default(),
        );
        // As a keyboard sends it: Shift, then Ctrl, then C, each a key.
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let both = Modifiers {
            ctrl: true,
            ..shift
        };
        press(&mut desk, Key::Named(NamedKey::Shift), None, shift);
        press(&mut desk, Key::Named(NamedKey::Control), None, both);
        press(&mut desk, Key::Character("C".into()), None, both);
        let copies: Vec<_> = answer_all(&mut desk)
            .into_iter()
            .filter(|request| matches!(request, Request::Text { .. }))
            .collect();
        assert_eq!(
            copies.len(),
            2,
            "to the primary selection, then the clipboard"
        );
    }
}
