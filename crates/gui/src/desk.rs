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
    Axis, ClientMessage, Content, NewTerminal, PaneId, PaneTerminal, Request, Response, Side, Size,
    Tab, TabId, TerminalId, ViewUpdate, WorkspaceId, Workspaces,
};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::Status;
use crate::command::{self, Command, Kind, Place};
use crate::input::KeyPress;
use crate::layout::{Area, Direction, Placement};
use crate::pane::{Mode, Pane};

/// What the window shows: a workspace, one of its tabs, and the pane in
/// that tab with the focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shown {
    pub(crate) workspace: WorkspaceId,
    pub(crate) tab: TabId,
    pub(crate) pane: PaneId,
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
    /// The space the panes' cards share, once the window is open.
    area: Option<Area>,
    /// The command line being typed, after `:`.
    command: Option<String>,
    /// A message for the focused pane's label, until the next key, such as
    /// why a command failed.
    message: Option<String>,
    /// Text copied, waiting for the window to put on the clipboard.
    yanked: Option<String>,
    /// Every workspace has closed, so the window should too.
    finished: bool,
}

impl Desk {
    // --- What's shown -----------------------------------------------------

    pub(crate) fn shown(&self) -> Option<Shown> {
        self.shown
    }

    pub(crate) fn pane(&self, terminal: TerminalId) -> Option<&Pane> {
        self.panes.get(&terminal)
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// Takes text copied with `y`, for the window to put on the clipboard.
    pub(crate) fn take_yanked(&mut self) -> Option<String> {
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

    /// Where the shown tab's panes go.
    pub(crate) fn placement(&self) -> Option<Placement> {
        Some(Placement::new(
            &self.shown_tab()?.layout,
            self.area.as_ref()?,
        ))
    }

    /// The title for the window: the focused pane's program's.
    pub(crate) fn title(&self) -> Option<&str> {
        let terminal = self.focused_terminal()?;
        let title = &self.panes.get(&terminal)?.screen()?.title;
        (!title.is_empty()).then_some(title.as_str())
    }

    /// Every workspace, by name, and whether it's the one shown.
    pub(crate) fn workspace_names(&self) -> Vec<(WorkspaceId, String, bool)> {
        let shown = self.shown.map(|shown| shown.workspace);
        self.workspaces
            .list
            .iter()
            .map(|workspace| {
                (
                    workspace.id,
                    workspace.name.clone(),
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
        if let Some(line) = &self.command {
            return Status::Typing(format!(":{line}▏"));
        }
        let pane = self
            .focused_terminal()
            .and_then(|terminal| self.panes.get(&terminal));
        if let Some(pane) = pane
            && pane.prompting()
            && let Some(prompt) = pane.label()
        {
            return Status::Typing(prompt);
        }
        Status::Tabs {
            note: self.message.clone().or_else(|| pane.and_then(Pane::label)),
        }
    }

    /// What the bar calls a tab: its number, and its focused pane's title.
    fn tab_name(&self, index: usize, tab: &Tab) -> String {
        /// Longer titles are cut short, to leave room for the other tabs.
        const MAX_TITLE: usize = 24;
        let number = index + 1;
        let title = self
            .workspaces
            .find_pane(tab.focused)
            .and_then(|(_, _, pane)| {
                let Content::Terminal(terminal) = pane.content;
                self.panes.get(&terminal)?.screen()
            })
            .map(|screen| screen.title.trim())
            .filter(|title| !title.is_empty());
        match title {
            Some(title) if title.chars().count() > MAX_TITLE => {
                let short: String = title.chars().take(MAX_TITLE - 1).collect();
                format!("{number} {short}…")
            }
            Some(title) => format!("{number} {title}"),
            None => number.to_string(),
        }
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

    /// Focuses the pane whose card is at `point`, as when it's clicked.
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
        let normal = self
            .focused_pane()
            .is_some_and(|left| left.mode != Mode::Insert);
        self.shown = Some(shown);
        // Before the mode: a pane of another workspace may not exist yet.
        self.attach_shown();
        if normal && let Some(arrived) = self.focused_pane() {
            arrived.enter_normal_mode();
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
        for (id, card) in placement.panes {
            let Some(pane) = panes.iter().find(|pane| pane.id == id) else {
                continue;
            };
            let Content::Terminal(terminal) = pane.content;
            if let (Some(pane), Ok(size)) = (
                self.panes.get_mut(&terminal),
                Size::new(card.cols, card.rows),
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
        self.attached.clear();
        self.asked.clear();
        self.outbox.clear();
        for pane in self.panes.values_mut() {
            pane.reconnected();
        }
        self.workspaces_changed(workspaces);
    }

    /// The connection was lost: nothing is attached, and nothing asked
    /// will be answered.
    pub(crate) fn lost(&mut self) {
        self.connected = false;
        self.attached.clear();
        self.asked.clear();
        self.outbox.clear();
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
        if self.command.is_some() {
            self.command_key(press);
            return true;
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
        if let Some(line) = &mut self.command {
            line.extend(text.chars().filter(|ch| !ch.is_control()));
        } else if let Some(pane) = self.focused_pane() {
            pane.paste(text);
        }
    }

    /// Scrolls the focused pane `lines` up, or down if negative. Returns
    /// whether the window must be redrawn.
    pub(crate) fn wheel(&mut self, lines: i64) -> bool {
        self.focused_pane().is_some_and(|pane| pane.wheel(lines))
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
                self.command = Some(String::new());
                return true;
            }
            Some('h') if ctrl => Direction::Left,
            Some('j') if ctrl => Direction::Down,
            Some('k') if ctrl => Direction::Up,
            Some('l') if ctrl => Direction::Right,
            _ => return false,
        };
        if let Some(shown) = self.shown
            && let Some(next) = self
                .placement()
                .and_then(|placement| placement.neighbour(shown.pane, direction))
        {
            self.show_pane(next);
        }
        true
    }

    /// Handles a key while the command line is open.
    fn command_key(&mut self, press: &KeyPress<'_>) {
        let Some(line) = &mut self.command else {
            return;
        };
        match press.key {
            Key::Named(NamedKey::Escape) => self.command = None,
            Key::Named(NamedKey::Enter) => {
                let line = std::mem::take(line);
                self.command = None;
                match command::parse(&line) {
                    Ok(command) => self.run(command),
                    Err(message) => self.message = Some(message),
                }
            }
            Key::Named(NamedKey::Backspace) => {
                // Deleting past the colon closes the line, as in Vim.
                if line.pop().is_none() {
                    self.command = None;
                }
            }
            _ if press.modifiers.ctrl => {}
            _ => {
                if let Some(text) = press.text {
                    line.extend(text.chars().filter(|ch| !ch.is_control()));
                }
            }
        }
    }

    /// Carries out a command.
    fn run(&mut self, command: Command) {
        let Some(shown) = self.shown else { return };
        let new = |size| {
            PaneTerminal::New(NewTerminal {
                size,
                command: None,
                cwd: None,
                record_to: None,
            })
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
                kind: Kind::Terminal,
                daemon: None,
            } => {
                let terminal = new(self.new_pane_size(None));
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
                kind: Kind::Terminal,
                daemon: None,
            } => {
                let terminal = new(self.new_pane_size(Some(side)));
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
                    let terminal = new(self.new_pane_size(None));
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
            .and_then(|(shown, placement)| placement.card(shown.pane));
        let (cols, rows) = match (side, focused) {
            (Some(side), Some(card)) => match side.axis() {
                Axis::LeftToRight => ((card.cols / 2).max(1), card.rows),
                Axis::TopToBottom => (card.cols, (card.rows / 2).max(1)),
            },
            _ => (whole.cols, whole.rows),
        };
        Size::new(cols, rows).unwrap_or(fallback)
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Layout, Pane as LayoutPane, View, Workspace};
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
            parts,
        }
    }

    /// A tab, focused on its first pane.
    fn tab(id: u32, layout: Layout) -> Tab {
        Tab {
            id: TabId(id),
            focused: layout.panes()[0].id,
            layout,
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
                ClientMessage::FrameDone { .. } | ClientMessage::Hello { .. } => None,
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
}
