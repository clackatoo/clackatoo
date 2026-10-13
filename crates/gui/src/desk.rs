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
    Axis, BufferId, ClientMessage, Content, FileLocation, Layout, NewAgent, NewTerminal, PaneId,
    PaneTerminal, Request, Response, Side, Size, Tab, TabId, TerminalId, ViewUpdate, WorkspaceId,
    Workspaces,
};
use clackatoo_term::mouse::{MouseAction, MouseButton};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{ModeName, Status};
use crate::command::{self, Command, Kind, Place};
use crate::finder::Finder;
use crate::input::{self, KeptPress, KeyPress, Modifiers};
use crate::keymap::{Chord, Pressed};
use crate::layout::{Area, Direction, Divider, Placement};
use crate::options::Options;
use crate::pane::{self, Copy, Mode, Pane, PaneMouse, Selection};
use bindings::Binding;

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
#[derive(Clone, Debug)]
enum Asker {
    /// A pane, which keeps track of its own requests.
    Pane(TerminalId),
    /// The window itself.
    Window(Asked),
}

/// What the window asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// A pane made for the config's Lua, which waits for it by this
    /// request's number.
    Lua(u64),
    /// A file written, and the pane to close once it is, as after `:wq`.
    Saved {
        buffer: BufferId,
        then_close: Option<PaneId>,
    },
    /// A file opened in place of the one this pane showed, whose failure
    /// is shown, and forgets where its cursor was to go in that file.
    OpenedHere(PaneId),
    /// A new pane showing a file, to switch to once made, with its cursor
    /// put at this place once the file's text has come.
    PaneAt(Option<FileLocation>),
    /// A new file explorer, to switch to once made, showing the file at
    /// this path once it can.
    Explorer(Option<String>),
}

mod bindings;
mod clipboard;
mod editors;
mod explorers;
mod finding;
mod formatting;
mod jumps;
mod links;
mod macros;
mod offers;
mod script;
mod settings;

pub(crate) use script::LuaStart;

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
    /// The daemon's offer to install a language, asked in the bar until
    /// the next key.
    offer: Option<offers::Offer>,
    /// Text copied, waiting for the window to put on the clipboard or the
    /// primary selection, oldest first.
    yanked: Vec<(String, Copy)>,
    /// URLs links asked to open, waiting for the window to hand to the
    /// desktop's opener.
    opened: Vec<String>,
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
    /// What keys are bound to, and those waiting for the rest of a
    /// binding, as after Ctrl+B.
    keymap: bindings::Keys,
    /// The config's Lua, if there's a config.
    script: script::Script,
    /// Panes editing files.
    editors: editors::Editors,
    /// Panes showing file explorers.
    explorers: explorers::Explorers,
    /// The macros being typed, and what's noted of keys pressed for the
    /// one being recorded.
    macros: macros::Macros,
    /// Where you've been, to go back to.
    jumps: jumps::Jumps,
    /// The options, as config and `:set` have set them.
    options: Options,
    /// The finder, while it's open.
    finder: Option<Finder>,
    /// How many searches the window has asked the daemon for, which
    /// numbers them.
    searches: u64,
    /// When the finder's grep is to search for what's been typed, a moment
    /// after the last key.
    grep_at: Option<std::time::Instant>,
    /// Files being formatted, by the number of the query that asked.
    formatting: HashMap<u64, formatting::Formatting>,
    /// Why files being written weren't formatted first, to say when
    /// they're written.
    unformatted: HashMap<BufferId, String>,
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

    /// How the mouse pointer looks at `point`, if it's over a chat, which
    /// says by what it drew there.
    pub(crate) fn chat_pointer(&self, point: [f32; 2]) -> Option<crate::chat::Pointer> {
        let (terminal, _, pixel) = self.cell_at(point, None)?;
        self.panes.get(&terminal)?.chat_pointer(pixel)
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
        if self.input.is_some() || self.finding() {
            return Some(TextTarget::Bar);
        }
        if let Some((editor, _)) = self.focused_editor() {
            return if editor.prompting() {
                Some(TextTarget::Bar)
            } else {
                (editor.mode == Mode::Insert).then_some(TextTarget::Pane)
            };
        }
        if let Some(explorer) = self.focused_explorer() {
            return explorer.prompting().then_some(TextTarget::Bar);
        }
        let pane = self.pane(self.focused_terminal()?)?;
        if pane.prompting() {
            Some(TextTarget::Bar)
        } else {
            (pane.mode == Mode::Insert).then_some(TextTarget::Pane)
        }
    }

    /// The options, as config and `:set` have set them.
    pub(crate) fn options(&self) -> &Options {
        &self.options
    }

    pub(crate) fn finished(&self) -> bool {
        self.finished
    }

    /// When a pane's highlight on text just copied goes, the soonest of
    /// them: the window must be drawn again then.
    pub(crate) fn flash_ends(&self) -> Option<std::time::Instant> {
        let panes = self.panes.values().filter_map(Pane::flash_ends);
        panes.chain(self.editor_flash_ends()).min()
    }

    /// Takes text copied, for the window to put on the clipboard or the
    /// primary selection, oldest first.
    pub(crate) fn take_yanked(&mut self) -> Vec<(String, Copy)> {
        std::mem::take(&mut self.yanked)
    }

    /// The terminal the focused pane shows.
    pub(crate) fn focused_terminal(&self) -> Option<TerminalId> {
        let shown = self.shown?;
        let (_, _, pane) = self.workspaces.find_pane(shown.pane)?;
        pane.content.terminal()
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

    /// The title for the window: the focused pane's program's, or file's.
    pub(crate) fn title(&self) -> Option<&str> {
        if let Some(title) = self.shown.and_then(|shown| self.editor_title(shown.pane)) {
            return Some(title);
        }
        if let Some(explorer) = self.focused_explorer() {
            return Some(explorer.title());
        }
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
        if let Some(status) = self.offer_status() {
            return status;
        }
        if let Some((editor, copy)) = self.focused_editor() {
            let label = editor.label(copy);
            if editor.prompting() {
                return Status::Typing {
                    label: None,
                    text: label,
                    choices: Vec::new(),
                };
            }
            let note = (self.keymap.hint())
                .or_else(|| self.message.clone())
                .unwrap_or(label);
            let note = match self.recording() {
                Some(register) => format!("recording @{register}  {note}"),
                None => note,
            };
            return Status::Tabs {
                mode: Some(mode_name(editor.mode)),
                note: Some(note),
                panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
            };
        }
        if let Some(explorer) = self.focused_explorer() {
            let label = explorer.label();
            if explorer.prompting() {
                return Status::Typing {
                    label: None,
                    text: label,
                    choices: Vec::new(),
                };
            }
            let note = (self.keymap.hint())
                .or_else(|| self.message.clone())
                .unwrap_or(label);
            let note = match self.recording() {
                Some(register) => format!("recording @{register}  {note}"),
                None => note,
            };
            return Status::Tabs {
                mode: Some(ModeName::Normal),
                note: Some(note),
                panes: self.shown_tab().map_or(0, |tab| tab.layout.panes().len()),
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
        let mode = pane.map(|pane| mode_name(pane.mode));
        Status::Tabs {
            mode,
            note: self
                .keymap
                .hint()
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
            .and_then(|(_, _, pane)| match pane.content {
                Content::Terminal(terminal) => self.panes.get(&terminal)?.title(),
                Content::Buffer(_) => self.editor_title(pane.id),
                Content::Files(_) => self.explorer(pane.id).map(crate::explorer::Explorer::title),
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

    /// Switches to `pane`, as [`show_pane`](Self::show_pane) does, adding
    /// the pane left to the jump list if `record`.
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
        if record && let Some(left) = self.shown {
            self.record_jump(left.pane, None);
        }
        let unfocused = self.unfocused;
        let left_editor = self
            .shown
            .and_then(|left| self.editor_in_normal_mode(left.pane));
        let left_explorer = self.shown.is_some_and(|left| self.is_explorer(left.pane));
        let normal = left_editor.unwrap_or(false)
            || left_explorer
            || self.focused_pane().is_some_and(|left| {
                if !unfocused {
                    left.focus_changed(false);
                }
                left.mode != Mode::Insert
            });
        // A panel by the cursor goes with the focus.
        if let Some(left) = self.shown {
            self.close_editor_panel(left.pane);
        }
        self.shown = Some(shown);
        self.note_focus(pane);
        // Before the mode: a pane of another workspace may not exist yet.
        self.attach_shown();
        if normal {
            self.editor_to_normal_mode(pane);
        }
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
            self.panes.entry(terminal).or_insert_with(|| {
                let mut pane = Pane::new(terminal);
                pane.set_options(&self.options);
                pane
            });
        }
        self.attach_shown_buffers();
        self.attach_shown_explorers();
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
            let terminal = match pane.content {
                Content::Terminal(terminal) => terminal,
                Content::Buffer(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_editor(id, size);
                    }
                    continue;
                }
                Content::Files(_) => {
                    if let Ok(size) = Size::new(tile.cols, tile.rows) {
                        self.fit_explorer(id, size);
                    }
                    continue;
                }
            };
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
        // A macro can't go on without the daemon, nor wait for it.
        if self.macro_waiting() {
            self.stop_macro(Some(
                "the macro was stopped: the daemon is out of reach".into(),
            ));
            self.drop_held();
        }
        // Its searches went with the connection.
        self.finder = None;
    }

    /// Forgets everything sent on the last connection, which will never be
    /// answered, and what was waiting on it: a split's new shares, and
    /// drags under way.
    fn forget_asked(&mut self) {
        self.attached.clear();
        self.editors_reconnected();
        self.explorers_reconnected();
        self.asked.clear();
        self.macros.requests_forgotten();
        self.outbox.clear();
        self.formatting.clear();
        self.unformatted.clear();
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
        let shown = self.shown;
        self.settle();
        // A pane a macro waiting closed, as with `:q`, moves the focus as it
        // asked: it goes on in the pane that has it. Closed by anyone else,
        // the macro stops, as its keys were for that pane.
        if self.macro_asked() && self.shown != shown {
            self.macro_follows_focus();
        }
        self.attach_shown();
        self.fit_panes();
        self.forget_gone_handlers();
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

    /// Hands the daemon's answer to whoever asked, and goes on with a
    /// macro that waited for it, unless it's refused, which stops it, as a
    /// key that fails does. Returns whether the window must be redrawn.
    pub(crate) fn answered(&mut self, response: Response) -> bool {
        if matches!(response, Response::Error(_))
            && self.asked.front().is_some_and(macros::waits_for)
        {
            self.stop_macro(None);
        }
        // An answer the macro waited for may move the focus, as to a pane
        // it asked for: the macro goes on there.
        let asked_by_macro = self.macro_asked();
        if let Some(asker) = self.asked.front() {
            self.macros.request_answered(asker);
        }
        let shown = self.shown;
        let redraw = self.hand_answer(response);
        if asked_by_macro && self.shown != shown {
            self.macro_follows_focus();
        }
        redraw | self.type_macros()
    }

    /// Hands the daemon's answer to whoever asked. Returns whether the
    /// window must be redrawn.
    fn hand_answer(&mut self, response: Response) -> bool {
        match self.asked.pop_front() {
            Some(Asker::Pane(terminal)) => {
                let Some(pane) = self.panes.get_mut(&terminal) else {
                    return false;
                };
                let redraw = pane.answered(response);
                if let Some((text, to)) = pane.take_yanked() {
                    self.copied(text, to);
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
            Some(Asker::Window(Asked::Lua(request))) => {
                let result = match response {
                    Response::PaneCreated(pane) => {
                        // As a pane split with a key is.
                        self.show_pane(pane);
                        Ok(pane)
                    }
                    Response::Error(err) => Err(err.to_string()),
                    other => Err(format!("the daemon answered {other:?}")),
                };
                self.answer_lua(request, result);
                true
            }
            Some(Asker::Window(Asked::Saved { buffer, then_close })) => {
                self.saved(buffer, then_close, &response);
                true
            }
            Some(Asker::Window(Asked::PaneAt(landing))) => {
                match response {
                    Response::PaneCreated(pane) => {
                        self.show_pane(pane);
                        if let Some(landing) = landing {
                            self.land_at(pane, landing);
                        }
                    }
                    Response::Error(err) => self.message = Some(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::Explorer(reveal))) => {
                match response {
                    Response::PaneCreated(pane) => self.explorer_made(pane, reveal),
                    Response::Error(err) => self.message = Some(err.to_string()),
                    _ => {}
                }
                true
            }
            Some(Asker::Window(Asked::OpenedHere(pane))) => {
                let failed = match response {
                    Response::Error(err) => Some(err.to_string()),
                    _ => None,
                };
                self.opened_here(pane, failed.is_some());
                if let Some(err) = failed {
                    self.message = Some(err);
                }
                true
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
            let window = Asker::Window(asked);
            self.macros.requested(&window);
            self.asked.push_back(window);
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
        messages.extend(self.take_buffer_outbox());
        messages.extend(self.take_explorer_outbox());
        messages
    }

    // --- Keys -------------------------------------------------------------

    /// Handles a key press: the command line's, a binding's, then the
    /// focused pane's, noting it in the macro being recorded, if one is,
    /// and then the keys of any macro it types. Returns whether the
    /// window must be redrawn.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> bool {
        if input::is_modifier(press.key) {
            return false;
        }
        self.record_press(press);
        let redraw = self.key_with(press, true);
        let typed = self.type_macros();
        self.recorded_press();
        redraw || typed
    }

    /// Handles a key press, as [`key`](Self::key) does, but for the
    /// config's keypress handlers if not `handlers`, as for a key they've
    /// had already, and without noting it in a macro. Keys a binding types
    /// meet neither them nor the config's bindings, only the window's.
    fn key_with(&mut self, press: &KeyPress<'_>, handlers: bool) -> bool {
        // The keyboard reports Shift, Ctrl and the like as keys of their
        // own, on the way to a chord such as Ctrl+Shift+C. They mustn't
        // count as the next key: that would drop a selection or a message,
        // or the first key of a two-key command, before the chord arrives.
        if input::is_modifier(press.key) {
            return false;
        }
        // While an editor's command waits for the grammar, or a macro for
        // what its keys started, keys wait too, so none, not even a
        // binding's, gets ahead of it.
        let typing = self.script.typing;
        let recorded = self.shared_mut().registers.press_start();
        let held = || editors::Held::Key {
            press: KeptPress::of(press),
            handlers,
            typing,
            recorded,
        };
        // Esc pressed cancels it; one a macro or binding types waits its
        // turn.
        let escape = *press.key == Key::Named(NamedKey::Escape) || pane::enters_normal_mode(press);
        let cancel = escape && !typing && !self.key_from_frame();
        if let Some(redraw) = self.hold_while_waiting(cancel, held) {
            return redraw;
        }
        let asked = self.offer_asked();
        if asked && self.offer_key(press) {
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
        } else if self.finding() {
            // And so the finder.
            if !pane::enters_normal_mode(press) {
                self.finder_key(press);
                return true;
            }
            self.close_finder();
        } else if let Some(chord) = Chord::of(press) {
            // The config's keypress handlers come first, then the bindings.
            if handlers && !self.script.typing && self.keypress_handled(chord, press) {
                return true;
            }
            let mode = self.key_mode();
            // A key a half-typed command in the pane waits for is the
            // pane's, as `d<Space>`'s Space is, not a binding's first.
            let pressed = if self.keymap.waiting().is_empty() && self.pane_pending() {
                Pressed::Unbound
            } else {
                self.keymap.press(mode, chord)
            };
            match pressed {
                Pressed::Bound(Binding::Window(action)) => {
                    self.forget_waiting();
                    self.message = None;
                    self.act(action);
                    return true;
                }
                // Keys a binding types are past the config's bindings.
                Pressed::Bound(Binding::Lua(_) | Binding::Keys(_)) if self.script.typing => {
                    self.type_bound(chord, press);
                    return true;
                }
                Pressed::Bound(Binding::Lua(binding)) => {
                    self.message = None;
                    self.run_lua_binding(binding, chord, press);
                    return true;
                }
                Pressed::Waiting => {
                    self.keep_waiting(chord, press);
                    self.message = None;
                    return true;
                }
                Pressed::Bound(Binding::Keys(to)) => {
                    self.forget_waiting();
                    self.message = None;
                    self.type_keys(&to);
                    return true;
                }
                Pressed::Dropped => {
                    if !self.type_dropped() {
                        return true;
                    }
                    // Looked up again on its own, past the handlers,
                    // which have had it.
                    return self.key_with(press, false);
                }
                Pressed::Unbound => {}
            }
        }
        let had_message = self.message.take().is_some() || asked;
        if self.focused_editor().is_some() {
            return self.editor_key(press) || had_message;
        }
        if self.focused_explorer().is_some() {
            if self.stop_recording_in_explorer(press) {
                return true;
            }
            return self.explorer_key(press) || had_message;
        }
        if self.stop_recording_in_terminal(press) {
            return true;
        }
        let redraw = self.focused_pane().is_some_and(|pane| pane.key(press));
        if let Some(terminal) = self.focused_terminal() {
            self.open_pane_link(terminal);
        }
        redraw || had_message
    }

    /// Pastes `text` into the command line, if it's open, or else the
    /// focused pane.
    pub(crate) fn paste(&mut self, text: &str) {
        if self
            .hold_while_waiting(false, || editors::Held::Paste(text.to_owned()))
            .is_some()
        {
            return;
        }
        if let Some(input) = &mut self.input {
            input
                .text
                .extend(text.chars().filter(|ch| !ch.is_control()));
        } else if self.finding() {
            self.paste_into_finder(text);
        } else if !self.paste_into_editor(text)
            && let Some(pane) = self.focused_pane()
        {
            pane.paste(text);
        }
    }

    /// Scrolls the pane at `point` `lines` up, or down if negative, as the
    /// mouse wheel does. Returns whether the window must be redrawn.
    pub(crate) fn wheel(&mut self, point: [f32; 2], lines: i64, modifiers: Modifiers) -> bool {
        if let Some((pane, _)) = self.editor_at(point) {
            return self.editor_wheel(pane, lines);
        }
        if let Some((pane, _)) = self.explorer_at(point) {
            return self.explorer_wheel(pane, lines);
        }
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
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.editor_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                outcome.redraw = self.focus_at(point) | self.editor_click(pane, cell);
            }
            return outcome;
        }
        if self.dragging.is_none()
            && let Some((pane, cell)) = self.explorer_at(point)
        {
            outcome.redraw |= self.mouse_left();
            if let MouseAction::Press(MouseButton::Left) = action {
                outcome.redraw = self.focus_at(point) | self.explorer_click(pane, cell, clicks);
            }
            return outcome;
        }
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
        // The pane the mouse was over no longer has a link under it.
        if let Some((before, _)) = self.mouse_cell
            && before != terminal
        {
            outcome.redraw |= self.mouse_left();
        }
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
        self.open_pane_link(terminal);
        outcome
    }

    /// The mouse has left the pane it was over, which no longer has a
    /// link under it. Returns whether the window must be redrawn.
    pub(crate) fn mouse_left(&mut self) -> bool {
        let Some((terminal, _)) = self.mouse_cell.take() else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.hover(None))
    }

    /// Shift, Ctrl or Alt went down or up: holding Ctrl underlines the
    /// link under the mouse. Returns whether the window must be redrawn.
    pub(crate) fn modifiers_changed(&mut self, modifiers: Modifiers) -> bool {
        let Some((terminal, [line, col])) = self.mouse_cell else {
            return false;
        };
        self.panes
            .get_mut(&terminal)
            .is_some_and(|pane| pane.hover(Some((line, col, modifiers))))
    }

    /// Whether a click at `point` with `modifiers` held would open a link.
    pub(crate) fn link_at(&self, point: [f32; 2], modifiers: Modifiers) -> bool {
        let Some((terminal, [line, col], _)) = self.cell_at(point, None) else {
            return false;
        };
        self.panes.get(&terminal).is_some_and(|pane| {
            pane.link_opens(modifiers) && pane.link_in_view(line, col).is_some()
        })
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
            pane.content.terminal()
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

    /// The editor at `point`, and the cell of its view under it.
    fn editor_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.editor(pane).is_some())
    }

    /// The explorer at `point`, if there's one there, and the cell of it
    /// there, as [`editor_at`](Self::editor_at) finds an editor's.
    fn explorer_at(&self, point: [f32; 2]) -> Option<(PaneId, [i64; 2])> {
        self.cell_in(point, |desk, pane| desk.explorer(pane).is_some())
    }

    /// The pane at `point`, if `wanted` takes it, and the cell of it there,
    /// as its line and column.
    fn cell_in(
        &self,
        point: [f32; 2],
        wanted: impl Fn(&Self, PaneId) -> bool,
    ) -> Option<(PaneId, [i64; 2])> {
        let area = self.area.as_ref()?;
        let placement = self.placement()?;
        let id = placement.pane_at(point)?;
        if !wanted(self, id) {
            return None;
        }
        let tile = placement.panes.iter().find(|(pane, _)| *pane == id)?.1;
        #[allow(clippy::cast_possible_truncation)]
        let cell = |offset: f32, size: f32| (offset / size).floor() as i64;
        let pixel = [point[0] - tile.origin[0], point[1] - tile.origin[1]];
        Some((
            id,
            [cell(pixel[1], area.cell[1]), cell(pixel[0], area.cell[0])],
        ))
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
                        Err(message) => self.refuse(message),
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
        match &command {
            Command::Reload => return self.reload_lua(),
            Command::Set(words) => return self.run_set(words),
            _ => {}
        }
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
                place: Place::Tab | Place::Here,
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
            // In an editor, a number is a line, as in Vim.
            Command::Tab(number) if self.focused_editor().is_some() => {
                self.editor_command(&number.to_string());
            }
            Command::Tab(number) => self.show_tab_numbered(shown.workspace, number),
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
            Command::Edit { place, path } => self.open_file(place, path),
            Command::Quit { force } => self.quit(force),
            Command::Write { force, quit } => self.write_file(force, quit),
            Command::Discard => self.discard_changes(),
            Command::Format => self.format_file(),
            Command::Find { kind, query } => self.open_finder(kind, query),
            Command::Hunk(action) => self.editor_hunk(action),
            Command::Registers => self.show_registers(),
            Command::Marks => self.open_finder(crate::finder::Kind::Marks, String::new()),
            Command::Editor(line) => self.editor_command(&line),
            Command::Explore(dir) => self.open_explorer(dir),
            Command::Reload | Command::Set(_) => {}
        }
    }

    /// Shows tab `number` of `workspace`, counting from 1, or says there's
    /// no such tab.
    fn show_tab_numbered(&mut self, workspace: WorkspaceId, number: usize) {
        let tabs =
            (self.workspaces.get(workspace)).map_or(&[][..], |workspace| &workspace.tabs[..]);
        match number.checked_sub(1).and_then(|index| tabs.get(index)) {
            Some(tab) => self.show_pane(tab.focused),
            None => self.refuse(match tabs.len() {
                1 => format!("there's no tab {number}; this workspace has one"),
                count => format!("there's no tab {number}; this workspace has {count}"),
            }),
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

/// The bar's name for `mode`.
fn mode_name(mode: Mode) -> ModeName {
    match mode {
        Mode::Insert => ModeName::Insert,
        Mode::Normal => ModeName::Normal,
        Mode::Visual(Selection::Characters) => ModeName::Visual,
        Mode::Visual(Selection::Lines) => ModeName::VisualLine,
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
                | ClientMessage::Edit { .. }
                | ClientMessage::Query { .. }
                | ClientMessage::Rename { .. }
                | ClientMessage::Find { .. }
                | ClientMessage::StopFinding { .. }
                | ClientMessage::Stage { .. }
                | ClientMessage::Clipboard { .. }
                | ClientMessage::Files { .. }
                | ClientMessage::FileOp { .. }
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
        // While Ctrl+B waits, the bar says what it takes.
        ctrl(&mut desk, "b");
        assert!(matches!(
            desk.status(),
            Status::Tabs { note: Some(note), .. }
                if note == "tab 1-9 c [ ] ,  pane p h j k l | - x  jump o i  agent a  workspace w"
        ));
        assert!(desk.cancel_keys());
        // Panes by number, shown on screen while one is awaited.
        ctrl_b(&mut desk, "p");
        assert!(desk.numbering_panes());
        assert_eq!(
            desk.status(),
            Status::Tabs {
                mode: Some(ModeName::Insert),
                note: Some("pane 1-9".into()),
                panes: 3
            }
        );
        type_keys(&mut desk, "3");
        assert!(!desk.numbering_panes());
        assert_eq!(shown_pane(&desk), PaneId(3));
        // A click cancels it, as the window says when the mouse goes down.
        ctrl_b(&mut desk, "p");
        assert!(desk.cancel_keys());
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
        assert!(desk.keymap.waiting().is_empty());
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn the_configs_handlers_and_bindings_come_before_the_pane() {
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.keys.map("niv", '<C-b>"', function(ev) ev.pane:split("below") end)
            client.keys.unmap("niv", "<C-b>x")
            client.keys.map("i", "jk", function(ev) ev:propagate() end)
            client.on("keypress", function(ev) if ev.key == "q" then ev:stop() end end)
            "#,
        );
        let sent = |desk: &mut Desk| -> Vec<u8> {
            (answer_all(desk).into_iter())
                .filter_map(|request| match request {
                    Request::SendInput { bytes, .. } => Some(bytes),
                    _ => None,
                })
                .flatten()
                .collect()
        };
        answer_all(&mut desk);
        ctrl_b(&mut desk, "\"");
        assert!(matches!(
            answer_all(&mut desk)[..],
            [Request::Split {
                pane: PaneId(1),
                side: Side::Below,
                ..
            }]
        ));
        ctrl_b(&mut desk, "x");
        assert!(answer_all(&mut desk).is_empty(), "unbound");
        type_keys(&mut desk, "q");
        assert_eq!(sent(&mut desk), b"", "stopped");
        type_keys(&mut desk, "jkw");
        assert_eq!(sent(&mut desk), b"jkw", "let go on, in order");
        // Keys given up on, as a click gives up on Ctrl+B, aren't.
        ctrl(&mut desk, "b");
        assert!(desk.cancel_keys());
        type_keys(&mut desk, "jk");
        assert_eq!(sent(&mut desk), b"jk");

        // A reload starts again from the window's own keys.
        normal_mode(&mut desk);
        command(&mut desk, "reload");
        assert_eq!(desk.message.as_deref(), Some("reloaded the config"));
        desk.lua_ready();
        ctrl_b(&mut desk, "x");
        assert!(answer_all(&mut desk).is_empty(), "unbound again");
    }

    #[test]
    fn options_are_set_by_config_and_set_and_lua_reads_them() {
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.opt.rnu = false
            client.opt.numberwidth = 6
            client.opt.cc = { 80, 120 }
            client.opt.ic = false
            client.keys.map("n", "<C-b>n", function()
                client.opt.number = not client.opt.number
            end)
            "#,
        );
        assert!(!desk.options.relativenumber);
        assert_eq!(desk.options.numberwidth, 6);
        normal_mode(&mut desk);
        command(&mut desk, "set rnu? nu");
        assert_eq!(desk.message.as_deref(), Some("norelativenumber"));
        command(&mut desk, "set nonu");
        assert!(!desk.options.number);
        // Lua's copy heard, so it turns it back on.
        ctrl_b(&mut desk, "n");
        assert!(desk.options.number);
        command(&mut desk, "set");
        assert_eq!(
            desk.message.as_deref(),
            Some("norelativenumber  numberwidth=6  colorcolumn=80,120  noignorecase")
        );
        command(&mut desk, "set cc= ts=8");
        command(&mut desk, "set cc? ts");
        assert_eq!(desk.message.as_deref(), Some("colorcolumn=  tabstop=8"));
        // The terminal's search heard: without ignorecase, it's exact.
        answer_all(&mut desk);
        type_keys(&mut desk, "/x");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::Search { exact: true, .. }]
        ));
        command(&mut desk, "set nuw=99");
        assert_eq!(
            desk.message.as_deref(),
            Some("numberwidth is a whole number from 1 to 20")
        );
        // A reload starts again from the options as they start.
        command(&mut desk, "reload");
        assert!(desk.options.relativenumber);
        desk.lua_ready();
        assert!(!desk.options.relativenumber);
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

    /// Types `text`, a character at a time.
    fn type_text(desk: &mut Desk, text: &str) {
        for ch in text.chars() {
            press(
                desk,
                Key::Character(ch.to_string().into()),
                None,
                Modifiers::default(),
            );
        }
    }

    /// The changes to buffers waiting to go to the daemon.
    fn buffer_changes(desk: &mut Desk) -> Vec<(u64, clackatoo_protocol::Change)> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Edit { seq, change, .. } => Some((seq, change)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_editor_asks_its_language_server_once_typing_pauses() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate, Query, ServerFeatures, Trigger};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
        });
        let mut desk = Desk::default();
        desk.connected(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        answer_all(&mut desk);
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/src/a.rs".into(),
                text: "x\n".into(),
                version: 0,
                modified: false,
            }),
        );
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(buffer, BufferUpdate::Features(features));
        type_text(&mut desk, "Aab");
        assert!(desk.editor_wakes().is_some(), "the window wakes to ask");
        // Not yet: the edits go, answered as the daemon would.
        let mut version = 0;
        let mut queries = Vec::new();
        let mut ticked = false;
        for _ in 0..10 {
            for message in desk.take_outbox() {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(buffer, BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query { query, at, .. } => queries.push((at, query)),
                    _ => {}
                }
            }
            if !ticked && queries.is_empty() {
                let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
                assert!(desk.tick_editors(later));
                ticked = true;
            }
        }
        assert_eq!(queries, [(3, Query::Completion(Trigger::Invoked))]);
        assert_eq!(desk.editor_wakes(), None);
    }

    #[test]
    fn e_opens_a_file_in_place_of_the_one_shown() {
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(BufferId(7)),
        });
        let mut desk = Desk::default();
        desk.connected(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        answer_all(&mut desk);
        command(&mut desk, "e src/other.rs");
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: clackatoo_protocol::OpenFile {
                    path: "src/other.rs".into(),
                    cwd: None,
                },
            }]
        );
    }

    #[test]
    fn a_language_offered_is_asked_about_as_languageinstall_says() {
        use clackatoo_protocol::BufferUpdate;

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
        });
        let mut desk = Desk::default();
        desk.connected(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        answer_all(&mut desk);
        let offer = || BufferUpdate::Offer {
            language: "toml".into(),
            grammar: true,
            server: Some("taplo".into()),
        };
        let install = Request::Install {
            language: "toml".into(),
            grammar: true,
            server: Some("taplo".into()),
        };
        let asked = |desk: &Desk| {
            matches!(desk.status(), Status::Typing { text, .. }
                if text == "Install toml's grammar and taplo? [Y/n]")
        };

        // Asked, and y installs.
        assert!(desk.buffer_changed(buffer, offer()));
        assert!(asked(&desk));
        type_keys(&mut desk, "y");
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), std::slice::from_ref(&install));
        desk.installed(Ok("toml's grammar and taplo".into()));
        assert!(
            matches!(desk.status(), Status::Tabs { note: Some(note), .. }
            if note == "Installed toml's grammar and taplo")
        );

        // n declines for good.
        desk.buffer_changed(buffer, offer());
        type_keys(&mut desk, "n");
        assert_eq!(
            answer_all(&mut desk),
            [Request::Decline {
                language: "toml".into()
            }]
        );

        // Not asked while the command line is open: n is typed there.
        type_keys(&mut desk, ":");
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        type_keys(&mut desk, "n");
        assert!(matches!(desk.status(), Status::Typing { text, .. } if text == ":n▏"));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(asked(&desk), "asked once it's closed");

        // Any other key leaves it unanswered, and goes on to the editor.
        type_keys(&mut desk, "j");
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), []);

        // Nor while the finder is open: y and Enter are its.
        command(&mut desk, "Buffers");
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        type_keys(&mut desk, "y");
        assert_eq!(desk.finder_view((10, 60), None).unwrap().query, "y▏");
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert!(asked(&desk), "asked once it's closed");
        assert!(!answer_all(&mut desk).contains(&install));
        type_keys(&mut desk, "j");

        // Or it's installed without asking, or never.
        command(&mut desk, "set languageinstall=always");
        answer_all(&mut desk);
        desk.buffer_changed(buffer, offer());
        assert!(!asked(&desk));
        assert_eq!(answer_all(&mut desk), [install]);
        command(&mut desk, "set li=never");
        answer_all(&mut desk);
        assert!(!desk.buffer_changed(buffer, offer()));
        assert_eq!(answer_all(&mut desk), []);
    }

    #[test]
    fn a_file_is_edited_written_and_kept_open_while_it_has_changes() {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate, Change, Edit};

        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
        });
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
        desk.connected(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        let asked = answer_all(&mut desk);
        assert!(
            asked.contains(&Request::AttachBuffer { buffer }),
            "{asked:?}"
        );
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: "/src/notes.txt".into(),
                text: "notes\n".into(),
                version: 0,
                modified: false,
            }),
        );
        assert_eq!(desk.title(), Some("notes.txt"));

        type_text(&mut desk, "A!");
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Pane));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        let sent = buffer_changes(&mut desk);
        let edit = Edit::replace(6, 5, "", "!");
        assert_eq!(
            sent,
            [(
                0,
                Change::Edit {
                    edit,
                    base: 0,
                    step: true
                }
            )]
        );
        desk.buffer_changed(buffer, BufferUpdate::Acked { version: 1, seq: 0 });
        desk.buffer_changed(buffer, BufferUpdate::Modified(true));
        let Status::Tabs { mode, note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(mode, Some(ModeName::Normal));
        assert_eq!(note.as_deref(), Some("notes.txt [+]  1:6"));

        // :q won't throw the change away; :w writes it.
        command(&mut desk, "q");
        assert!(answer_all(&mut desk).is_empty());
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains(":q!"))
        );
        command(&mut desk, "w");
        let asked = desk.take_outbox();
        assert!(matches!(
            asked.as_slice(),
            [ClientMessage::Request(Request::Save { force: false, .. })]
        ));
        desk.answered(Response::Done);
        assert_eq!(desk.message.as_deref(), Some("notes.txt written"));
        desk.buffer_changed(buffer, BufferUpdate::Modified(false));
        // What the daemon says of the file goes in the bar.
        let notice = "agent changed notes.txt where you have unsaved edits";
        assert!(desk.buffer_changed(buffer, BufferUpdate::Notice(notice.into())));
        assert_eq!(desk.message.as_deref(), Some(notice));
        command(&mut desk, "q");
        assert_eq!(
            answer_all(&mut desk),
            [Request::ClosePane { pane: PaneId(1) }]
        );
    }

    // --- The jump list, in editors ------------------------------------------

    fn file_pane(pane: u32, buffer: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(pane),
            content: Content::Buffer(BufferId(buffer)),
        })
    }

    /// One workspace of one tab laid out as `layout`.
    fn one_tab(layout: Layout) -> Workspaces {
        workspaces(vec![workspace(1, "w", vec![tab(1, layout)])])
    }

    /// Gives `buffer` its text, as attaching does.
    fn load(desk: &mut Desk, buffer: u32, path: &str, text: &str) {
        let snapshot = clackatoo_protocol::BufferSnapshot {
            path: path.into(),
            text: text.into(),
            version: 0,
            modified: false,
        };
        desk.buffer_changed(
            BufferId(buffer),
            clackatoo_protocol::BufferUpdate::Snapshot(snapshot),
        );
    }

    /// What the bar says of the focused pane: for an editor, the file and
    /// where its cursor is.
    fn note(desk: &Desk) -> String {
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        note.unwrap_or_default()
    }

    /// Answers what was asked, which must include opening the file at
    /// `path` in pane 1, and opens it, as the daemon would, as `buffer`,
    /// with `text`.
    fn open_asked(desk: &mut Desk, buffer: u32, path: &str, text: &str) {
        let asked = answer_all(desk);
        let opened: Vec<(PaneId, &str)> = (asked.iter())
            .filter_map(|request| match request {
                Request::OpenHere { pane, file } => Some((*pane, file.path.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(opened, [(PaneId(1), path)], "{asked:?}");
        desk.workspaces_changed(one_tab(file_pane(1, buffer)));
        answer_all(desk);
        load(desk, buffer, path, text);
    }

    /// The files asked to open in panes in place of theirs, not answered
    /// yet.
    fn opens_asked(desk: &mut Desk) -> Vec<String> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::OpenHere { file, .. }) => Some(file.path),
                _ => None,
            })
            .collect()
    }

    /// The daemon opens the oldest file asked for in pane 1, as `buffer`:
    /// it shows the pane with it, then answers.
    fn opened(desk: &mut Desk, buffer: u32) {
        desk.workspaces_changed(one_tab(file_pane(1, buffer)));
        desk.answered(Response::Done);
    }

    fn b_rs_line_3() -> clackatoo_protocol::FileLocation {
        clackatoo_protocol::FileLocation {
            path: "/src/b.rs".into(),
            line: 2,
            col: 1,
            offset: None,
            text: String::new(),
        }
    }

    const A_RS: &str = "fn a() {}\nfn b() {}\nfn c() {}\n";
    const B_RS: &str = "x\ny\nzzz\n";

    #[test]
    fn a_second_jump_while_a_file_opens_goes_where_it_points() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/c.rs", TEN_LINES);
        type_text(&mut desk, "5G");
        command(&mut desk, "e /b.rs");
        open_asked(&mut desk, 8, "/b.rs", B_RS);
        command(&mut desk, "e /c.rs");
        open_asked(&mut desk, 9, "/c.rs", TEN_LINES);
        type_text(&mut desk, "8j");
        // Back to b.rs, and before it's open, back again to c.rs's line 5.
        ctrl(&mut desk, "o");
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/b.rs", "/c.rs"]);
        opened(&mut desk, 10);
        load(&mut desk, 10, "/b.rs", B_RS);
        opened(&mut desk, 11);
        answer_all(&mut desk);
        load(&mut desk, 11, "/c.rs", TEN_LINES);
        assert_eq!(note(&desk), "c.rs  5:1");
    }

    #[test]
    fn ctrl_o_while_gd_opens_a_file_comes_back_and_ctrl_i_goes_there() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        // Before b.rs is open: you're where gd goes, as far as the jump
        // list knows, so there's somewhere to come back to.
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/a.rs"]);
        opened(&mut desk, 8);
        load(&mut desk, 8, "/src/b.rs", B_RS);
        opened(&mut desk, 9);
        answer_all(&mut desk);
        load(&mut desk, 9, "/src/a.rs", A_RS);
        assert_eq!(note(&desk), "a.rs  2:4");
        ctrl(&mut desk, "i");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  3:2");
    }

    #[test]
    fn a_file_that_fails_to_open_keeps_a_later_jumps_place() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        type_text(&mut desk, "j");
        ctrl(&mut desk, "o");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/a.rs"]);
        // b.rs can't be opened; a.rs is shown already.
        desk.answered(Response::Error(clackatoo_protocol::Error::NotAFile(
            PaneId(1),
        )));
        assert!(desk.message.take().is_some());
        desk.answered(Response::Done);
        assert_eq!(note(&desk), "a.rs  2:4");
    }

    #[test]
    fn opening_a_file_forgets_where_the_cursor_was_to_go_before() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwgd");
        answer_query(&mut desk, 7, vec![b_rs_line_3()]);
        // Another file, before b.rs is open.
        command(&mut desk, "e /src/c.rs");
        assert_eq!(opens_asked(&mut desk), ["/src/b.rs", "/src/c.rs"]);
        opened(&mut desk, 8);
        load(&mut desk, 8, "/src/b.rs", B_RS);
        opened(&mut desk, 9);
        answer_all(&mut desk);
        load(&mut desk, 9, "/src/c.rs", "c\n");
        // b.rs opened later isn't where gd went.
        command(&mut desk, "e /src/b.rs");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  1:1");
    }

    #[test]
    fn going_back_in_insert_mode_keeps_typing_where_it_was_as_a_new_step() {
        use clackatoo_protocol::{BufferUpdate, Change};

        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/t.txt", "abc\nxyz\n");
        type_text(&mut desk, "Ad");
        assert_eq!(buffer_changes(&mut desk).len(), 1);
        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version: 1, seq: 0 });
        // To the terminal and back, still in insert mode.
        ctrl_b(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(2));
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        };
        press(&mut desk, Key::Character("O".into()), None, ctrl_shift);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(note(&desk), "t.txt  1:5", "past the line's end");
        type_text(&mut desk, "e");
        let sent = buffer_changes(&mut desk);
        let [(_, Change::Edit { edit, step, .. })] = sent.as_slice() else {
            panic!("one edit: {sent:?}");
        };
        assert_eq!(*edit, clackatoo_text::Edit::replace(9, 4, "", "e"));
        assert!(*step, "a new undo step");
    }

    /// Answers the language server query just asked with `found`.
    fn answer_query(desk: &mut Desk, buffer: u32, found: Vec<clackatoo_protocol::FileLocation>) {
        let mut id = None;
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Query { id: asked, .. } => id = Some(asked),
                ClientMessage::Request(_) => {
                    desk.answered(Response::Done);
                }
                _ => {}
            }
        }
        let update = clackatoo_protocol::BufferUpdate::Answer {
            id: id.expect("a query"),
            version: 0,
            answer: clackatoo_protocol::Answer::Locations(found),
        };
        desk.buffer_changed(BufferId(buffer), update);
    }

    fn escape(desk: &mut Desk) {
        press(
            desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
    }

    const TEN_LINES: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";

    #[test]
    fn ctrl_o_comes_back_from_gd_to_another_file_and_ctrl_i_goes_again() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", "fn a() {}\nfn b() {}\n");
        type_text(&mut desk, "jw");
        assert_eq!(note(&desk), "a.rs  2:4");
        type_text(&mut desk, "gd");
        let there = clackatoo_protocol::FileLocation {
            path: "/src/b.rs".into(),
            line: 2,
            col: 1,
            offset: None,
            text: String::new(),
        };
        answer_query(&mut desk, 7, vec![there]);
        // Each file opens in the pane in turn, as the daemon would, with a
        // buffer of its own, and the cursor lands once its text is there.
        open_asked(&mut desk, 8, "/src/b.rs", "x\ny\nzzz\n");
        assert_eq!(note(&desk), "b.rs  3:2");
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/src/a.rs", "fn a() {}\nfn b() {}\n");
        assert_eq!(note(&desk), "a.rs  2:4", "back where gd left");
        ctrl(&mut desk, "i");
        open_asked(&mut desk, 10, "/src/b.rs", "x\ny\nzzz\n");
        assert_eq!(note(&desk), "b.rs  3:2", "forward again");
        // Nothing further forward.
        ctrl(&mut desk, "i");
        assert!(answer_all(&mut desk).is_empty());
    }

    #[test]
    fn going_back_then_jumping_elsewhere_forgets_the_way_forward() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "G");
        assert_eq!(note(&desk), "t.txt  10:1");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1");
        type_text(&mut desk, "5G");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1", "line 10 is forgotten");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1", "nothing further back");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1");
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  5:1");
        assert!(
            answer_all(&mut desk).is_empty(),
            "nothing opened: it's the file shown"
        );
    }

    #[test]
    fn jumps_from_one_line_one_after_another_are_remembered_once() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", "a foo foo\nb\nfoo\n");
        // Two jumps along line 1, then off it.
        type_text(&mut desk, "/foo");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(note(&desk), "t.txt  1:3");
        type_text(&mut desk, "n");
        assert_eq!(note(&desk), "t.txt  1:7");
        type_text(&mut desk, "n");
        assert_eq!(note(&desk), "t.txt  3:1");
        // Back to where line 1 was left last, and no further, even from
        // another line.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:7");
        type_text(&mut desk, "j");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  2:1");
        type_text(&mut desk, "k$");
        // A search that finds nothing isn't a jump.
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  3:1");
        type_text(&mut desk, "/nowhere");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:7");
    }

    #[test]
    fn places_in_panes_closed_since_are_skipped() {
        let layout = side_by_side(vec![pane(1), file_pane(2, 8), pane(3)]);
        let mut desk = desk(one_tab(layout));
        load(&mut desk, 8, "/t.txt", TEN_LINES);
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        type_text(&mut desk, "G");
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        // The editor closes, its jumps with it.
        desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), pane(3)])));
        answer_all(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "i");
        assert_eq!(shown_pane(&desk), PaneId(3));
        assert!(
            !answer_all(&mut desk)
                .iter()
                .any(|request| matches!(request, Request::OpenHere { .. })),
            "nothing opened in place of what's shown"
        );
    }

    #[test]
    fn places_move_with_edits_made_since() {
        use clackatoo_protocol::{Author, BufferUpdate};
        use clackatoo_text::Edit;

        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "5Gl");
        type_text(&mut desk, "G");
        // Someone else puts a line in at the top...
        let edit = Edit::replace(TEN_LINES.len(), 0, "", "new\n");
        let update = BufferUpdate::Edited {
            version: 1,
            edit,
            author: Author::Agent(TerminalId(9)),
            answers: None,
        };
        desk.buffer_changed(BufferId(7), update);
        assert_eq!(note(&desk), "t.txt  11:1");
        // ...and so does this window, after going to the top.
        type_text(&mut desk, "ggOtop");
        escape(&mut desk);
        assert_eq!(note(&desk), "t.txt  1:3");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  12:1", "where gg left");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  7:2", "where G left");
        // The start of the file, where both lines went in, went down with
        // its line.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  3:1", "where 5G left");
        // As it does with a line opened above it from there.
        type_text(&mut desk, "Oabove");
        escape(&mut desk);
        ctrl(&mut desk, "i");
        assert_eq!(note(&desk), "t.txt  8:2", "where G left, moved down");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  4:1", "where 5G left, moved down");
    }

    #[test]
    fn a_file_opened_again_keeps_its_places_within_its_text() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/a.txt", TEN_LINES);
        type_text(&mut desk, "3G9G");
        command(&mut desk, "e /b.txt");
        open_asked(&mut desk, 8, "/b.txt", "b\n");
        // a.txt was cut short while it wasn't open here: line 9 is gone.
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/a.txt", "a\nb\nc\nd\ne\nf\n");
        assert_eq!(note(&desk), "a.txt  6:1");
        // Its places are in the new text, and move with edits to it.
        type_text(&mut desk, "5kOx");
        escape(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "a.txt  4:1", "line 3, moved down by one");
    }

    /// The finder's messages for the daemon, answering every request with
    /// "done" on the way.
    fn finds(desk: &mut Desk) -> Vec<ClientMessage> {
        let messages = desk.take_outbox();
        for message in &messages {
            if let ClientMessage::Request(_) = message {
                desk.answered(Response::Done);
            }
        }
        (messages.into_iter())
            .filter(|message| {
                matches!(
                    message,
                    ClientMessage::Find { .. } | ClientMessage::StopFinding { .. }
                )
            })
            .collect()
    }

    /// The number and what's asked of the one search in `messages`, besides
    /// previews.
    fn search_in(messages: &[ClientMessage]) -> (u64, clackatoo_protocol::Search) {
        use clackatoo_protocol::Search;
        let found: Vec<(u64, Search)> = (messages.iter())
            .filter_map(|message| match message {
                ClientMessage::Find { id, search, .. }
                    if !matches!(search, Search::Preview { .. }) =>
                {
                    Some((*id, search.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), 1, "{messages:?}");
        found[0].clone()
    }

    /// A desk with one pane, editing `path`, whose text is `text`.
    fn editing(path: &str, text: &str) -> Desk {
        use clackatoo_protocol::{BufferSnapshot, BufferUpdate};
        let buffer = BufferId(7);
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(buffer),
        });
        let mut desk = desk(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        desk.buffer_changed(
            buffer,
            BufferUpdate::Snapshot(BufferSnapshot {
                path: path.into(),
                text: text.into(),
                version: 0,
                modified: false,
            }),
        );
        answer_all(&mut desk);
        desk
    }

    /// Gives the search numbered `id` the root `/p` and `files`.
    fn found_files(desk: &mut Desk, id: u64, files: &[&str]) {
        use clackatoo_protocol::Found;
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        desk.found(
            id,
            Found::Files(files.iter().map(|&file| file.to_owned()).collect()),
        );
    }

    /// The text of each row the finder shows.
    fn finder_rows(desk: &mut Desk) -> Vec<String> {
        let view = desk
            .finder_view((10, 60), Some((10, 60)))
            .expect("the finder");
        view.rows.into_iter().map(|row| row.text).collect()
    }

    #[test]
    fn a_new_lines_indent_is_put_right_when_the_grammar_answers() {
        use clackatoo_protocol::{Answer, BufferUpdate, Change, LineIndent, Query};
        let mut desk = editing("/p/a.rs", "{\n}\n");
        type_keys(&mut desk, "ox");
        // Its edit first, which the question waits behind.
        let changes = buffer_changes(&mut desk);
        assert_eq!(changes.len(), 1, "{changes:?}");
        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version: 1, seq: 0 });
        let asked: Vec<(u64, usize, Query)> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Query { id, at, query, .. } => Some((id, at, query)),
                _ => None,
            })
            .collect();
        let [(id, 2, Query::Indent { end: 2, .. })] = asked[..] else {
            panic!("asked {asked:?}");
        };
        // The x typed meanwhile wasn't held.
        assert_eq!(editor_text(&desk), "{\nx\n}\n");
        let answer = BufferUpdate::Answer {
            id,
            version: 1,
            answer: Answer::Indents(vec![LineIndent {
                line: 2,
                width: Some(4),
            }]),
        };
        assert!(desk.buffer_changed(BufferId(7), answer));
        assert_eq!(editor_text(&desk), "{\n    x\n}\n");
        // Sent once the x is, as part of the same change.
        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version: 2, seq: 1 });
        let changes = buffer_changes(&mut desk);
        let [(2, Change::Edit { step: false, .. })] = changes[..] else {
            panic!("sent {changes:?}");
        };
    }

    #[test]
    fn an_object_from_the_grammar_is_copied_to_the_clipboard_once_it_answers() {
        use clackatoo_protocol::{Answer, BufferUpdate, Construct, Piece, Query};
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        // Into `"+`, so the x's delete doesn't go to the clipboard after.
        command(&mut desk, "set clipboard=none");
        type_keys(&mut desk, "j\"+yafx");
        let asked: Vec<(u64, usize, Query)> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Query { id, at, query, .. } => Some((id, at, query)),
                _ => None,
            })
            .collect();
        assert_eq!(asked.len(), 1);
        let (id, at, query) = asked[0];
        assert_eq!((at, query), (10, Query::Pieces(Construct::Function)));
        assert!(desk.yanked.is_empty());
        let piece = Piece {
            start: 10,
            end: 19,
            inner: None,
        };
        let answer = BufferUpdate::Answer {
            id,
            version: 0,
            answer: Answer::Pieces(vec![piece]),
        };
        assert!(desk.buffer_changed(BufferId(7), answer));
        assert_eq!(desk.yanked, [("fn b() {}".to_owned(), Copy::Clipboard)]);
        // The x held meanwhile is read after it.
        let changes = buffer_changes(&mut desk);
        assert_eq!(changes.len(), 1, "{changes:?}");
    }

    /// The grammar question the desk's editor asked last: its number and
    /// place.
    fn grammar_question(desk: &mut Desk) -> (u64, usize) {
        let asked = (desk.take_outbox().into_iter())
            .rev()
            .find_map(|message| match message {
                ClientMessage::Query { id, at, .. } => Some((id, at)),
                _ => None,
            });
        asked.expect("a question")
    }

    /// Answers the question numbered `id` of the editor of `desk`'s
    /// buffer with `pieces`.
    fn answer_pieces(desk: &mut Desk, id: u64, pieces: Vec<clackatoo_protocol::Piece>) {
        use clackatoo_protocol::{Answer, BufferUpdate};
        let answer = BufferUpdate::Answer {
            id,
            version: 0,
            answer: Answer::Pieces(pieces),
        };
        desk.buffer_changed(BufferId(7), answer);
    }

    /// The text of `desk`'s focused editor.
    fn editor_text(desk: &Desk) -> String {
        desk.focused_editor()
            .expect("an editor")
            .1
            .text()
            .to_string()
    }

    const FUNCTIONS: &str = "fn a() {}\nfn b() {}\n";

    fn function_b() -> clackatoo_protocol::Piece {
        clackatoo_protocol::Piece {
            start: 10,
            end: 19,
            inner: None,
        }
    }

    #[test]
    fn the_windows_keys_and_pastes_wait_for_the_grammar_too() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        // `:` would open the command line at once, and the paste go in.
        type_keys(&mut desk, "jdaf:");
        desk.paste("zz");
        assert!(desk.input.is_none(), "the : is held");
        assert_eq!(editor_text(&desk), FUNCTIONS, "and the paste");
        let (id, _) = grammar_question(&mut desk);
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert!(desk.input.is_some(), "the command line opened after");
        // The paste went into the command line, as it would have.
        assert_eq!(desk.input.as_ref().unwrap().text, "zz");
        assert_eq!(editor_text(&desk), "fn a() {}\n\n");
    }

    /// What the focused editor's bar says.
    fn editor_label(desk: &Desk) -> String {
        let (editor, copy) = desk.focused_editor().expect("an editor");
        editor.label(copy)
    }

    #[test]
    fn a_command_waiting_too_long_is_given_up_on_by_the_clock() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jcifabc");
        let (id, _) = grammar_question(&mut desk);
        let deadline = desk.editor_wakes().expect("woken to give up");
        assert!(desk.tick_editors(deadline));
        assert_eq!(
            editor_label(&desk),
            "the grammar took too long: nothing was done"
        );
        // Neither the command nor the keys held, nor the answer, when it
        // comes; and the next key isn't eaten.
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS);
        assert!(buffer_changes(&mut desk).is_empty());
        type_keys(&mut desk, "daf");
        assert_eq!(grammar_question(&mut desk).1, 10, "the d was had");
    }

    #[test]
    fn a_key_just_as_the_wait_runs_out_is_had_and_the_bar_says_why() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        desk.age_grammar_questions(std::time::Duration::from_secs(2));
        type_keys(&mut desk, "x");
        assert_eq!(editor_text(&desk), "fn a() {}\nn b() {}\n", "the x");
        assert_eq!(
            editor_label(&desk),
            "the grammar took too long: nothing was done"
        );
        type_keys(&mut desk, "l");
        assert!(editor_label(&desk).starts_with("a.rs"));
    }

    #[test]
    fn esc_cancels_a_command_waiting_for_the_grammar() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        let (id, _) = grammar_question(&mut desk);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert_eq!(editor_label(&desk), "cancelled: nothing was done");
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS, "nor the x");
        type_keys(&mut desk, "x");
        assert_eq!(editor_text(&desk), "fn a() {}\nn b() {}\n");
    }

    #[test]
    fn keys_a_held_key_sets_off_come_before_the_rest_held() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        desk.lua_from(r#"client.keys.map("n", "-", "ggdafox<Esc>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "jdaf-");
        type_keys(&mut desk, "Oz");
        let (first, _) = grammar_question(&mut desk);
        answer_pieces(&mut desk, first, vec![function_b()]);
        // The binding's daf asks in turn, once the daemon has the first's
        // edit; its o, x and Esc wait ahead of O and z.
        let mut second = None;
        for message in desk.take_outbox() {
            match message {
                ClientMessage::Edit { seq, .. } => {
                    let acked = clackatoo_protocol::BufferUpdate::Acked { version: 1, seq };
                    desk.buffer_changed(BufferId(7), acked);
                }
                ClientMessage::Query { id, .. } => second = Some(id),
                _ => {}
            }
        }
        let second = second.unwrap_or_else(|| grammar_question(&mut desk).0);
        let a = clackatoo_protocol::Piece {
            start: 0,
            end: 9,
            inner: None,
        };
        let answer = clackatoo_protocol::BufferUpdate::Answer {
            id: second,
            version: 1,
            answer: clackatoo_protocol::Answer::Pieces(vec![a]),
        };
        desk.buffer_changed(BufferId(7), answer);
        assert_eq!(editor_text(&desk), "\nz\nx\n\n");
    }

    #[test]
    fn a_focus_moved_or_an_editor_gone_while_waiting_gives_up() {
        let mut desk = editing("/p/a.rs", FUNCTIONS);
        type_keys(&mut desk, "jdafx");
        let (id, _) = grammar_question(&mut desk);
        // As a click on another pane moves it.
        let shown = desk.shown.unwrap();
        desk.shown = Some(Shown {
            pane: PaneId(2),
            ..shown
        });
        assert!(desk.tick_editors(std::time::Instant::now()));
        let (label, held) = desk.editor_label_and_held(PaneId(1));
        assert_eq!(label.as_deref(), Some("the focus moved: nothing was done"));
        assert_eq!(held, 0, "the x is dropped");
        desk.shown = Some(shown);
        answer_pieces(&mut desk, id, vec![function_b()]);
        assert_eq!(editor_text(&desk), FUNCTIONS, "nor the x");

        // The editor gone, as its pane shows another file.
        type_keys(&mut desk, "dafx");
        let layout = Layout::Pane(LayoutPane {
            id: PaneId(1),
            content: Content::Buffer(BufferId(8)),
        });
        desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
        assert!(
            (desk.message.as_deref()).is_some_and(|message| message.contains("nothing was done")),
            "{:?}",
            desk.message
        );
        assert_eq!(desk.editor_label_and_held(PaneId(1)).1, 0);
    }

    #[test]
    fn space_p_f_finds_the_projects_files_and_opens_the_one_picked() {
        use clackatoo_protocol::{OpenFile, Search};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        type_keys(&mut desk, " pf");
        let asked = finds(&mut desk);
        assert!(matches!(
            asked[..],
            [ClientMessage::Find {
                pane: Some(PaneId(1)),
                search: Search::Files,
                ..
            }]
        ));
        let (id, _) = search_in(&asked);
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Bar));
        found_files(&mut desk, id, &["src/main.rs", "src/other.rs", "README"]);
        assert_eq!(
            finder_rows(&mut desk),
            ["README", "src/main.rs", "src/other.rs"]
        );
        // The one picked is previewed as it's drawn, and the next once
        // it's picked.
        let preview = |messages: &[ClientMessage]| {
            (messages.iter())
                .find_map(|message| match message {
                    ClientMessage::Find {
                        id,
                        search: Search::Preview { path, line, .. },
                        ..
                    } => Some((*id, path.clone(), *line)),
                    _ => None,
                })
                .expect("a preview asked for")
        };
        let (first, path, line) = preview(&finds(&mut desk));
        assert_eq!((path.as_str(), line), ("/p/README", 0));
        finder_rows(&mut desk);
        assert!(finds(&mut desk).is_empty(), "asked for once");
        type_keys(&mut desk, "oth");
        assert_eq!(finder_rows(&mut desk), ["src/other.rs"]);
        let asked = finds(&mut desk);
        assert!(asked.contains(&ClientMessage::StopFinding { id: first }));
        assert_eq!(preview(&asked).1, "/p/src/other.rs");

        // Enter opens it in place of the file shown, as :e does.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(desk.finder_view((10, 60), None).is_none(), "closed");
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: OpenFile {
                    path: "/p/src/other.rs".into(),
                    cwd: None
                },
            }]
        );

        // Ctrl+V beside it, and Esc closes, stopping the search.
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        found_files(&mut desk, id, &["README"]);
        ctrl(&mut desk, "v");
        assert!(matches!(
            &answer_all(&mut desk)[..],
            [Request::Split {
                pane: PaneId(1),
                side: Side::Right,
                terminal: PaneTerminal::File(OpenFile { path, .. }),
            }] if path == "/p/README"
        ));
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        assert_eq!(finds(&mut desk), [ClientMessage::StopFinding { id }]);
        assert!(!desk.found(id, clackatoo_protocol::Found::Files(vec!["a".into()])));
    }

    #[test]
    fn a_grep_searches_again_as_typed_and_goes_to_the_line_picked() {
        use clackatoo_protocol::{Found, FoundLine, Search};
        let mut desk = editing("/p/src/main.rs", "use a;\nfn main() {}\n");
        command(&mut desk, "Grep fn");
        let (first, search) = search_in(&finds(&mut desk));
        assert_eq!(
            search,
            Search::Grep {
                pattern: "fn".into(),
                exact: false
            }
        );
        // A capital matches case, as `smartcase` says. Typing searches once
        // it pauses.
        type_keys(&mut desk, "M");
        assert!(finds(&mut desk).is_empty(), "not yet");
        assert!(desk.grep_at().is_some(), "the window wakes for it");
        desk.grep_if_due(std::time::Instant::now() + finding::GREP_WAIT);
        let asked = finds(&mut desk);
        assert!(asked.contains(&ClientMessage::StopFinding { id: first }));
        let (id, search) = search_in(&asked);
        assert_eq!(
            search,
            Search::Grep {
                pattern: "fnM".into(),
                exact: true
            }
        );
        let line = FoundLine {
            path: "src/main.rs".into(),
            line: 1,
            col: 3,
            text: "fn main() {}".into(),
            matches: vec![(0, 2)],
        };
        assert!(
            !desk.found(first, Found::Lines(vec![line.clone()])),
            "too late"
        );
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        desk.found(id, Found::Lines(vec![line]));
        assert_eq!(finder_rows(&mut desk), ["src/main.rs:2: fn main() {}"]);
        // In the file shown, the cursor goes there with nothing opened.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert!(answer_all(&mut desk).is_empty());
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(note.as_deref(), Some("main.rs  2:4"));
    }

    #[test]
    fn space_ds_lists_whats_wrong_in_the_project() {
        use clackatoo_protocol::{Found, FoundLine, Search};
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        type_keys(&mut desk, " ds");
        let asked = finds(&mut desk);
        let (id, search) = search_in(&asked);
        assert_eq!(search, Search::Diagnostics);
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let line = |path: &str, line, text: &str| FoundLine {
            path: path.into(),
            line,
            col: 1,
            text: text.into(),
            matches: Vec::new(),
        };
        desk.found(
            id,
            Found::Lines(vec![
                line("a.rs", 2, "error: wrong"),
                line("b.rs", 0, "warning: unused"),
            ]),
        );
        assert_eq!(
            finder_rows(&mut desk),
            ["a.rs:3: error: wrong", "b.rs:1: warning: unused"]
        );
        assert_eq!(
            desk.finder_view((10, 60), None).unwrap().title,
            "Diagnostics"
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert!(note.unwrap().starts_with("a.rs  3:2"));
    }

    #[test]
    fn format_formats_and_formatonsave_formats_before_writing() {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut desk = editing("/p/a.rs", "one\n");
        // The query formatting asks, and none else is sent.
        let format_asked = |desk: &mut Desk| {
            let messages = desk.take_outbox();
            let saves = (messages.iter())
                .filter(|message| matches!(message, ClientMessage::Request(Request::Save { .. })))
                .count();
            let id = (messages.into_iter()).find_map(|message| match message {
                ClientMessage::Query {
                    id,
                    query: Query::Format { tab_size, spaces },
                    ..
                } => {
                    assert_eq!((tab_size, spaces), (4, true), "as >> indents");
                    Some(id)
                }
                _ => None,
            });
            (id.expect("formatting asked"), saves)
        };
        let answer = |desk: &mut Desk, id, answer| {
            desk.buffer_changed(
                BufferId(7),
                BufferUpdate::Answer {
                    id,
                    version: 0,
                    answer,
                },
            )
        };
        let note = |desk: &Desk| match desk.status() {
            Status::Tabs { note, .. } => note.unwrap_or_default(),
            Status::Typing { .. } => panic!("the tabs"),
        };
        command(&mut desk, "Format");
        let (id, saves) = format_asked(&mut desk);
        assert_eq!(saves, 0);
        assert!(answer(&mut desk, id, Answer::Formatted { changed: true }));
        assert_eq!(note(&desk), "formatted");

        // With formatonsave, :w formats, then writes once it's answered,
        // whatever the answer.
        command(&mut desk, "set fos");
        command(&mut desk, "w");
        let (id, saves) = format_asked(&mut desk);
        assert_eq!(saves, 0, "not yet");
        answer(
            &mut desk,
            id,
            Answer::Failed("its language server doesn't format files".into()),
        );
        assert!(matches!(
            answer_all(&mut desk)[..],
            [Request::Save { force: false, .. }]
        ));
        // A file being formatted isn't asked again: a :w then writes it
        // once the answer comes.
        command(&mut desk, "Format");
        let (id, _) = format_asked(&mut desk);
        command(&mut desk, "w");
        assert!(desk.take_outbox().is_empty(), "not asked twice");
        answer(&mut desk, id, Answer::Formatted { changed: true });
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));

        // Or after a moment, if the server is slow.
        command(&mut desk, "w");
        format_asked(&mut desk);
        assert!(desk.format_at().is_some(), "the window wakes for it");
        assert!(!desk.format_if_due(std::time::Instant::now()));
        assert!(desk.format_if_due(std::time::Instant::now() + std::time::Duration::from_secs(4)));
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));
        assert!(note(&desk).ends_with("written, not formatted: its language server took too long"));
        assert!(desk.format_at().is_none());
    }

    #[test]
    fn several_places_an_answer_gives_are_picked_from_in_the_finder() {
        use clackatoo_protocol::{Answer, BufferUpdate, FileLocation, OpenFile};
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        type_keys(&mut desk, "gd");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        let place = |path: &str, line, text: &str| FileLocation {
            path: path.into(),
            line,
            col: 1,
            offset: None,
            text: text.into(),
        };
        let found = vec![place("/p/a.rs", 2, "three"), place("/p/lib/b.rs", 0, "b")];
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Locations(found),
            },
        );
        assert_eq!(finder_rows(&mut desk), ["a.rs:3: three", "lib/b.rs:1: b"]);
        assert_eq!(desk.finder_view((10, 60), None).unwrap().title, "Places");
        // One here goes there.
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert_eq!(note.as_deref(), Some("a.rs  3:2"));

        // One elsewhere opens there.
        type_keys(&mut desk, "gd");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        let found = vec![place("/p/a.rs", 2, "three"), place("/p/lib/b.rs", 0, "b")];
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Locations(found),
            },
        );
        ctrl(&mut desk, "n");
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(
            answer_all(&mut desk),
            [Request::OpenHere {
                pane: PaneId(1),
                file: OpenFile {
                    path: "/p/lib/b.rs".into(),
                    cwd: None
                },
            }]
        );
    }

    #[test]
    fn keys_a_binding_types_reach_the_windows_keys_and_a_key_ending_a_prefix_is_looked_up() {
        // `:w<CR>` typed by a binding opens the command line and writes.
        let mut desk = editing("/p/a.rs", "one\n");
        desk.lua_from(r#"client.keys.map("n", "<Space>w", ":w<CR>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, " w");
        assert!(matches!(answer_all(&mut desk)[..], [Request::Save { .. }]));

        // In a terminal's insert mode, with `jk` bound: `j` then Ctrl+B
        // types `j`, and Ctrl+B starts its own keys, not ^B.
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(r#"client.keys.map("i", "jk", "<C-;>")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "j");
        assert!(answer_all(&mut desk).is_empty(), "j waits for k");
        ctrl(&mut desk, "b");
        let sent: Vec<Vec<u8>> = (answer_all(&mut desk).into_iter())
            .filter_map(|request| match request {
                Request::SendInput { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [b"j".to_vec()], "not ^B");
        assert!(
            matches!(desk.status(), Status::Tabs { note: Some(note), .. } if note.starts_with("tab 1-9"))
        );
        assert!(desk.cancel_keys());
        // And `jk` types Ctrl+;, found by where it is on the keyboard,
        // which leaves insert mode.
        type_keys(&mut desk, "jk");
        assert!(matches!(
            desk.status(),
            Status::Tabs {
                mode: Some(ModeName::Normal),
                ..
            }
        ));
    }

    #[test]
    fn the_config_maps_keys_to_keys_and_a_prefix_that_goes_nowhere_is_typed() {
        use clackatoo_protocol::Query;
        let mut desk = editing("/p/a.rs", "one\ntwo\nthree\n");
        desk.lua_from(r#"client.keys.map("n", "gi", "gri")"#);
        answer_all(&mut desk);
        let queries = |desk: &mut Desk| -> Vec<Query> {
            (desk.take_outbox().into_iter())
                .filter_map(|message| match message {
                    ClientMessage::Query { query, .. } => Some(query),
                    _ => None,
                })
                .collect()
        };
        // `gi` types `gri`, as the config says.
        type_keys(&mut desk, "gi");
        assert_eq!(queries(&mut desk), [Query::Implementation]);
        // `g` waits for `i`; with another key, both are typed, as if no
        // binding had them: `G` then `gg` still go where they went.
        type_keys(&mut desk, "Ggg");
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        assert!(note.unwrap().starts_with("a.rs  1:1"));
        type_keys(&mut desk, "gd");
        assert_eq!(queries(&mut desk), [Query::Definition]);
    }

    #[test]
    fn the_config_binds_keys_to_commands_such_as_the_finders() {
        use clackatoo_protocol::Search;
        let mut desk = three_tabs();
        snapshot(&mut desk, 1);
        desk.lua_from(
            r#"
            client.keys.unmap("nv", "<Space>pf")
            client.keys.map("n", "<Space>ff", function() client.cmd("Files") end)
            client.keys.map("n", "<Space>x", function() client.cmd("Nonsense") end)
            "#,
        );
        answer_all(&mut desk);
        normal_mode(&mut desk);
        type_keys(&mut desk, " pf");
        assert!(finds(&mut desk).is_empty(), "unbound");
        type_keys(&mut desk, " ff");
        let asked = finds(&mut desk);
        assert_eq!(search_in(&asked).1, Search::Files);
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        type_keys(&mut desk, " x");
        assert!(
            (desk.message.as_deref()).is_some_and(|message| message.contains("Nonsense")),
            "{:?}",
            desk.message
        );
        // The other finders' keys, as jack's Telescope keys are.
        type_keys(&mut desk, " pgf");
        assert!(
            desk.finder_view((10, 60), None)
                .is_some_and(|view| view.title == "Grep")
        );
        press(
            &mut desk,
            Key::Named(NamedKey::Escape),
            None,
            Modifiers::default(),
        );
        type_keys(&mut desk, " ?");
        assert_eq!(search_in(&finds(&mut desk)).1, Search::Buffers);
    }

    #[test]
    fn ctrl_o_comes_back_from_a_file_or_line_the_finder_opened() {
        use clackatoo_protocol::{Found, FoundLine};
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/p/a.rs", "fn a() {}\nfn b() {}\n");
        type_text(&mut desk, "jw");
        type_keys(&mut desk, " pf");
        let (id, _) = search_in(&finds(&mut desk));
        found_files(&mut desk, id, &["b.rs"]);
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        open_asked(&mut desk, 8, "/p/b.rs", "x\ny\n");
        assert_eq!(note(&desk), "b.rs  1:1");
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 9, "/p/a.rs", "fn a() {}\nfn b() {}\n");
        assert_eq!(note(&desk), "a.rs  2:4", "back where the finder left");

        // A line in the file shown is a jump too.
        command(&mut desk, "Grep fn");
        let (id, _) = search_in(&finds(&mut desk));
        desk.found(
            id,
            Found::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let line = FoundLine {
            path: "a.rs".into(),
            line: 0,
            col: 3,
            text: "fn a() {}".into(),
            matches: vec![(0, 2)],
        };
        desk.found(id, Found::Lines(vec![line]));
        press(
            &mut desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
        assert_eq!(note(&desk), "a.rs  1:4");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "a.rs  2:4");
    }

    #[test]
    fn space_h_keys_act_on_the_hunk_at_the_cursor() {
        use clackatoo_protocol::{BufferUpdate, Change, Edit, Hunk};
        let mut desk = editing("/p/a.txt", "a\nB\n");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Hunks {
                version: 0,
                hunks: vec![Hunk {
                    start: 2,
                    end: 4,
                    base: "b\n".into(),
                }],
            },
        );
        type_keys(&mut desk, "j hs");
        let stages: Vec<usize> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Stage { at, .. } => Some(at),
                _ => None,
            })
            .collect();
        assert_eq!(stages, [2]);
        type_keys(&mut desk, " hr");
        let edit = Edit::replace(4, 2, "B\n", "b\n");
        assert_eq!(
            buffer_changes(&mut desk),
            [(
                0,
                Change::Edit {
                    edit,
                    base: 0,
                    step: true
                }
            )]
        );
        // And from the command line, as gitsigns.nvim's command.
        command(&mut desk, "Gitsigns preview_hunk");
        assert_eq!(note_of(&desk), "nothing's changed here since git's index");
    }

    #[test]
    fn a_key_a_half_typed_command_waits_for_is_never_a_bindings() {
        use clackatoo_protocol::{Change, Edit};
        let mut desk = editing("/p/a.txt", "abc\ndef\nghi\n");
        // `d<Space>` deletes a character, as in Vim, though Space starts
        // the finder's keys, and `j` and `k` only move after it.
        type_keys(&mut desk, "d jk");
        assert!(desk.finder_view((10, 60), None).is_none());
        let edit = Edit::replace(12, 0, "a", "");
        assert_eq!(
            buffer_changes(&mut desk),
            [(
                0,
                Change::Edit {
                    edit,
                    base: 0,
                    step: true
                }
            )]
        );
        // And so `f:` finds a colon rather than opening the command line.
        let mut desk = editing("/p/b.txt", "a:b\n");
        type_keys(&mut desk, "f:");
        assert!(matches!(desk.status(), Status::Tabs { .. }), "no palette");
        assert_eq!(note_of(&desk), "b.txt  1:2");
    }

    fn note_of(desk: &Desk) -> String {
        let Status::Tabs { note, .. } = desk.status() else {
            panic!("the tabs");
        };
        note.unwrap_or_default()
    }

    #[test]
    fn the_hover_panel_closes_as_the_finder_opens() {
        use clackatoo_protocol::{Answer, BufferUpdate};
        let mut desk = editing("/p/a.rs", "fn a() {}\n");
        type_keys(&mut desk, "K");
        let id = (desk.take_outbox().into_iter())
            .find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(id),
                _ => None,
            })
            .expect("a query");
        desk.buffer_changed(
            BufferId(7),
            BufferUpdate::Answer {
                id,
                version: 0,
                answer: Answer::Hover {
                    text: "fn a()".into(),
                    markdown: false,
                },
            },
        );
        let panel = |desk: &Desk| {
            (desk.editor(PaneId(1)))
                .and_then(crate::editor::Editor::panel)
                .is_some()
        };
        desk.settle_editors();
        assert!(panel(&desk));
        type_keys(&mut desk, " pf");
        desk.settle_editors();
        assert!(desk.finder_view((10, 60), None).is_some());
        assert!(!panel(&desk));
    }

    // --- Links in terminals ---------------------------------------------------

    /// Gives `terminal` a screen `cols` wide showing `output`, as attaching
    /// does.
    fn screen_showing(desk: &mut Desk, terminal: u32, cols: u16, output: &str) {
        let mut screen = Terminal::new(Size::new(cols, 24).unwrap(), &Config::default());
        screen.advance(output.as_bytes());
        desk.view_changed(
            TerminalId(terminal),
            &ViewUpdate::Snapshot(View::capture(&screen)),
        );
    }

    /// The point in the middle of the cell at `line` and `col` of the
    /// first pane, whose cells start 5 pixels in and are 10 × 20.
    fn cell_point(line: u16, col: u16) -> [f32; 2] {
        [10.0 + f32::from(col) * 10.0, 15.0 + f32::from(line) * 20.0]
    }

    fn click(desk: &mut Desk, point: [f32; 2], modifiers: Modifiers) {
        let left = MouseButton::Left;
        desk.mouse(point, MouseAction::Press(left), 1, modifiers);
        desk.mouse(point, MouseAction::Release(left), 1, modifiers);
    }

    const LINKS: &str = "\x1b]7;file:///p\x07see https://x.example/a now\r\n--> src/a.rs:3:2\r\n";

    #[test]
    fn ctrl_click_hands_urls_to_the_desktop_and_a_plain_click_doesnt() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 8), Modifiers::default());
        assert!(desk.take_opened().is_empty());
        assert!(desk.link_at(cell_point(0, 8), CTRL));
        assert!(!desk.link_at(cell_point(0, 2), CTRL));
        assert!(!desk.link_at(cell_point(0, 8), Modifiers::default()));
        click(&mut desk, cell_point(0, 8), CTRL);
        assert_eq!(desk.take_opened(), ["https://x.example/a"]);
        // The desktop opens it: nothing goes to the daemon.
        assert_eq!(answer_all(&mut desk), []);
    }

    #[test]
    fn holding_ctrl_over_a_link_underlines_it_until_ctrl_or_the_mouse_goes() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        let underlined = |desk: &Desk| {
            let pane = desk.pane(TerminalId(1)).unwrap();
            (pane.highlights(24).into_iter())
                .filter(|highlight| highlight.kind == crate::render::HighlightKind::Link)
                .map(|highlight| (highlight.line, highlight.cols))
                .collect::<Vec<_>>()
        };
        let motion = MouseAction::Motion(None);
        desk.mouse(cell_point(1, 6), motion, 1, Modifiers::default());
        assert_eq!(underlined(&desk), []);
        assert!(desk.modifiers_changed(CTRL));
        assert_eq!(underlined(&desk), [(1, 4..16)]);
        // Moving along the link changes nothing; off it, it goes.
        assert!(!desk.mouse(cell_point(1, 8), motion, 1, CTRL).redraw);
        assert!(desk.mouse(cell_point(1, 30), motion, 1, CTRL).redraw);
        assert_eq!(underlined(&desk), []);
        desk.mouse(cell_point(1, 8), motion, 1, CTRL);
        assert!(desk.modifiers_changed(Modifiers::default()));
        assert_eq!(underlined(&desk), []);
        desk.modifiers_changed(CTRL);
        assert!(desk.mouse_left());
        assert_eq!(underlined(&desk), []);
    }

    /// Where the requests asked for files to open.
    fn files_opened(requests: Vec<Request>) -> Vec<String> {
        (requests.into_iter())
            .filter_map(|request| match request {
                Request::NewTab {
                    terminal: PaneTerminal::File(file),
                    ..
                } => Some(format!("tab {}", file.path)),
                Request::Split {
                    pane,
                    side,
                    terminal: PaneTerminal::File(file),
                } => Some(format!("{side:?} of {} {}", pane.0, file.path)),
                Request::OpenHere { pane, file } => Some(format!("in {} {}", pane.0, file.path)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_file_from_a_narrow_terminal_opens_in_a_tab_of_its_own() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["tab /p/src/a.rs"]);
    }

    #[test]
    fn a_file_from_a_wide_terminal_opens_beside_it() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 200, LINKS);
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(
            files_opened(answer_all(&mut desk)),
            ["Right of 1 /p/src/a.rs"]
        );
    }

    #[test]
    fn a_file_opens_in_the_tabs_editor_at_its_line_and_ctrl_o_comes_back() {
        let mut desk = desk(one_tab(side_by_side(vec![pane(1), file_pane(2, 7)])));
        screen_showing(&mut desk, 1, 40, LINKS);
        load(&mut desk, 7, "/p/b.rs", "b\n");
        answer_all(&mut desk);
        click(&mut desk, cell_point(1, 6), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["in 2 /p/src/a.rs"]);
        assert_eq!(shown_pane(&desk), PaneId(2));
        desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), file_pane(2, 8)])));
        answer_all(&mut desk);
        load(&mut desk, 8, "/p/src/a.rs", "one\ntwo\nthree\n");
        assert_eq!(note(&desk), "a.rs  3:2");
        normal_mode(&mut desk);
        ctrl(&mut desk, "o");
        assert_eq!(shown_pane(&desk), PaneId(1));
    }

    #[test]
    fn a_relative_path_with_no_working_directory_is_left_to_the_daemon() {
        let mut desk = desk(one_tab(side_by_side(vec![pane(1), file_pane(2, 7)])));
        screen_showing(&mut desk, 1, 40, "src/a.rs:3\r\n");
        load(&mut desk, 7, "/p/b.rs", "b\n");
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 2), CTRL);
        // Not in the editor, whose directory it isn't from, but in a tab
        // the daemon opens from the terminal's.
        let asked = answer_all(&mut desk);
        assert!(
            matches!(&asked[..], [Request::NewTab { terminal: PaneTerminal::File(file), .. }]
                if file.path == "src/a.rs" && file.cwd.is_none()),
            "{asked:?}"
        );
    }

    #[test]
    fn a_file_link_on_another_machine_says_so_and_opens_nothing() {
        let mut desk = desk(one_tab(pane(1)));
        let output = "\x1b]8;;file://laptop/home/j/a.rs#0\x07there\x1b]8;;\x07";
        screen_showing(&mut desk, 1, 40, output);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 2), CTRL);
        assert!(desk.take_opened().is_empty());
        assert!(files_opened(answer_all(&mut desk)).is_empty());
        assert_eq!(
            note(&desk),
            "file://laptop/home/j/a.rs#0 is on laptop, not this machine"
        );
    }

    #[test]
    fn a_file_link_to_line_0_opens_the_file_at_its_top() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(
            &mut desk,
            1,
            40,
            "\x1b]8;;file:///p/a.rs#0\x07a\x1b]8;;\x07",
        );
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 0), CTRL);
        assert_eq!(files_opened(answer_all(&mut desk)), ["tab /p/a.rs"]);
    }

    #[test]
    fn gx_in_normal_mode_opens_the_link_under_the_cursor() {
        let mut desk = desk(one_tab(pane(1)));
        screen_showing(&mut desk, 1, 40, "https://x.example/b\r\n");
        answer_all(&mut desk);
        normal_mode(&mut desk);
        type_keys(&mut desk, "kgx");
        assert_eq!(desk.take_opened(), ["https://x.example/b"]);
    }

    #[test]
    fn ctrl_click_goes_to_a_program_that_takes_the_mouse_and_ctrl_shift_click_opens() {
        let mut desk = desk(one_tab(pane(1)));
        let output = format!("\x1b[?1000h\x1b[?1006h{LINKS}");
        screen_showing(&mut desk, 1, 40, &output);
        answer_all(&mut desk);
        click(&mut desk, cell_point(0, 8), CTRL);
        assert!(desk.take_opened().is_empty());
        let sent = answer_all(&mut desk);
        assert!(
            matches!(&sent[..], [Request::SendInput { bytes, .. }, ..] if bytes == b"\x1b[<16;9;1M"),
            "{sent:?}"
        );
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert!(desk.link_at(cell_point(0, 8), ctrl_shift));
        click(&mut desk, cell_point(0, 8), ctrl_shift);
        assert_eq!(desk.take_opened(), ["https://x.example/a"]);
    }

    // --- Registers, macros and marks, in editors ------------------------------

    /// A function of `start`, through `end`, its insides `inner`.
    fn function(
        start: usize,
        end: usize,
        inner: Option<(usize, usize)>,
    ) -> clackatoo_protocol::Piece {
        clackatoo_protocol::Piece { start, end, inner }
    }

    /// Answers the edits sent, as the daemon would, each taking `version`
    /// on by one, until none are left, and returns the last grammar
    /// question asked meanwhile: its number and place, if there was one.
    /// A question waits for the edits before it.
    fn serve_edits(desk: &mut Desk, version: &mut u64) -> Option<(u64, usize)> {
        let mut asked = None;
        loop {
            let mut sent_more = false;
            for message in desk.take_outbox() {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        *version += 1;
                        let acked_update = clackatoo_protocol::BufferUpdate::Acked {
                            version: *version,
                            seq,
                        };
                        desk.buffer_changed(BufferId(7), acked_update);
                        sent_more = true;
                    }
                    ClientMessage::Query { id, at, .. } => asked = Some((id, at)),
                    _ => {}
                }
            }
            if !sent_more {
                return asked;
            }
        }
    }

    /// Answers the edits sent to buffer 7, and its questions how lines are
    /// indented, as the daemon does for a file with no grammar, until
    /// none are left.
    fn serve_without_grammar(desk: &mut Desk) {
        use clackatoo_protocol::{Answer, BufferUpdate, Query};
        let mut version = 0;
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                return;
            }
            for message in messages {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query {
                        id,
                        query: Query::Indent { .. },
                        ..
                    } => {
                        let answer = Answer::Nothing;
                        let answer = BufferUpdate::Answer {
                            id,
                            version,
                            answer,
                        };
                        desk.buffer_changed(BufferId(7), answer);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Answers the grammar question `id` with `pieces`, counted in the
    /// daemon's text at `version`.
    fn answer_at(desk: &mut Desk, id: u64, version: u64, pieces: Vec<clackatoo_protocol::Piece>) {
        let answer = clackatoo_protocol::BufferUpdate::Answer {
            id,
            version,
            answer: clackatoo_protocol::Answer::Pieces(pieces),
        };
        desk.buffer_changed(BufferId(7), answer);
    }

    #[test]
    fn a_macro_waits_for_the_grammar_at_each_key_that_asks_in_turn() {
        let text = "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        type_keys(&mut desk, "qadaf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\nfn d() {}\n");
        // Each daf asks once the keys before it are done, and the keys
        // after it wait for its answer, as the x typed meanwhile does.
        type_keys(&mut desk, "2@ax");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 1, "on b's line");
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\nfn d() {}\n");
        answer_at(&mut desk, id, version, vec![function(1, 10, None)]);
        assert_eq!(editor_text(&desk), "\n\nfn c() {}\nfn d() {}\n");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 2, "the second go, on c's line, after the j");
        answer_at(&mut desk, id, version, vec![function(2, 11, None)]);
        assert_eq!(
            editor_text(&desk),
            "\n\n\nn d() {}\n",
            "then the j, and the x typed after"
        );
    }

    #[test]
    fn keys_typed_while_recording_waits_for_the_grammar_are_recorded_once() {
        let text = "fn a() {1}\nfn b() {2}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // The zz comes while cif waits, and is held, then read.
        type_keys(&mut desk, "qacifzz");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 10, Some((8, 9)))]);
        escape(&mut desk);
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "fn a() {zz}\nfn b() {2}\n");
        type_keys(&mut desk, "@a");
        let (id, at) = serve_edits(&mut desk, &mut version).expect("a question");
        assert_eq!(at, 21);
        let b = function(12, 22, Some((20, 21)));
        answer_at(&mut desk, id, version, vec![b]);
        assert_eq!(editor_text(&desk), "fn a() {zz}\nfn b() {zz}\n");
        // What was recorded, once.
        type_keys(&mut desk, "o");
        escape(&mut desk);
        type_keys(&mut desk, "\"ap");
        assert_eq!(
            editor_text(&desk),
            "fn a() {zz}\nfn b() {zz}\ncifzz<Esc>j\n"
        );
    }

    #[test]
    fn a_macro_waiting_for_the_grammar_stops_if_it_is_given_up_on() {
        let mut desk = editing("/p/a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
        let mut version = 0;
        type_keys(&mut desk, "qadaf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        type_keys(&mut desk, "jq2@a");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        // Esc gives up on it, and the rest of the macro goes too: the next
        // key types no more of it, and the answer, when it comes, nothing.
        escape(&mut desk);
        type_keys(&mut desk, "k");
        answer_at(&mut desk, id, version, vec![function(1, 10, None)]);
        assert_eq!(
            serve_edits(&mut desk, &mut version),
            None,
            "no daf asks again"
        );
        assert_eq!(editor_text(&desk), "\nfn b() {}\nfn c() {}\n");
    }

    #[test]
    fn marks_move_with_edits_made_here_and_by_others() {
        use clackatoo_protocol::{Author, BufferUpdate};
        use clackatoo_text::Edit;

        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/t.txt", TEN_LINES);
        type_text(&mut desk, "5Glma");
        // Someone else puts a line in at the top...
        let edit = Edit::replace(TEN_LINES.len(), 0, "", "new\n");
        let update = BufferUpdate::Edited {
            version: 1,
            edit,
            author: Author::Agent(TerminalId(9)),
            answers: None,
        };
        desk.buffer_changed(BufferId(7), update);
        // ...and so does this window.
        type_text(&mut desk, "ggOtop");
        escape(&mut desk);
        type_text(&mut desk, "G`a");
        assert_eq!(
            note(&desk),
            "t.txt  7:2",
            "down two lines, on its character"
        );
        // `.` is where the last change was.
        type_text(&mut desk, "'.");
        assert_eq!(note(&desk), "t.txt  1:1");
        type_text(&mut desk, "`.");
        assert_eq!(note(&desk), "t.txt  1:3");
        // A mark on a line deleted goes to where the line was.
        type_text(&mut desk, "7Gdd'a");
        assert_eq!(note(&desk), "t.txt  7:1");
    }

    #[test]
    fn a_mark_in_another_file_opens_it_in_place() {
        let mut desk = desk(one_tab(file_pane(1, 7)));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        type_text(&mut desk, "jwmA");
        command(&mut desk, "e /src/b.rs");
        open_asked(&mut desk, 8, "/src/b.rs", B_RS);
        // A file's own marks are its own, `'` among them.
        type_text(&mut desk, "`a");
        let said = note(&desk);
        assert!(said.starts_with("mark a isn't set"), "{said}");
        type_text(&mut desk, "''");
        let said = note(&desk);
        assert!(said.starts_with("mark ' isn't set"), "{said}");
        assert!(answer_all(&mut desk).is_empty(), "a.rs isn't opened");
        type_text(&mut desk, "jjma`A");
        open_asked(&mut desk, 9, "/src/a.rs", A_RS);
        assert_eq!(note(&desk), "a.rs  2:4");
        // It was a jump, which Ctrl+O comes back from.
        ctrl(&mut desk, "o");
        open_asked(&mut desk, 10, "/src/b.rs", B_RS);
        assert_eq!(note(&desk), "b.rs  3:1");
        type_text(&mut desk, "gg`a");
        assert_eq!(note(&desk), "b.rs  3:1", "b.rs's a is still there");
    }

    #[test]
    fn quote_quote_goes_back_and_forth_from_the_last_jump() {
        let mut desk = editing("/t.txt", "one two\nthree\nfour\nfive five\n");
        type_text(&mut desk, "wG");
        assert_eq!(note(&desk), "t.txt  4:1");
        type_text(&mut desk, "``");
        assert_eq!(note(&desk), "t.txt  1:5");
        type_text(&mut desk, "``");
        assert_eq!(note(&desk), "t.txt  4:1");
        type_text(&mut desk, "w''");
        assert_eq!(note(&desk), "t.txt  1:1", "to the line's start");
        // Ctrl+O knows of the jumps `''` made.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  4:6");
        // `m'` makes the place a jump to come back to.
        type_text(&mut desk, "kkm'G''");
        assert_eq!(note(&desk), "t.txt  2:1");
        // And it's a motion too.
        type_text(&mut desk, "Gd''");
        assert_eq!(editor_text(&desk), "one two\n");
    }

    /// What the register `name` holds.
    fn register(desk: &mut Desk, name: char) -> Option<String> {
        let registers = &desk.shared_mut().registers;
        registers
            .get(Some(name))
            .map(|register| register.text.clone())
    }

    #[test]
    fn macros_type_keys_through_the_bindings_as_they_are_bound_now() {
        let mut desk = editing("/t.txt", "a\nb\nc\nd\ne\n");
        desk.lua_from(
            r#"
            client.keys.map("n", "<Space>x", "A!<Esc>")
            client.keys.map("n", "<Space>m", "@qx")
            "#,
        );
        answer_all(&mut desk);
        // The keys pressed are recorded, not what the binding typed...
        type_keys(&mut desk, "qq x");
        type_keys(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "a!\nb\nc\nd\ne\n");
        assert_eq!(register(&mut desk, 'q').as_deref(), Some(" xj"));
        // ...so they're typed through the bindings, as pressing them would
        // be. A binding that types `@qx` types q's keys, and then the x,
        // as in Vim.
        type_keys(&mut desk, " m");
        assert_eq!(editor_text(&desk), "a!\nb!\n\nd\ne\n");
        // Bound to something else, the macro does what that does now.
        desk.lua_from(r#"client.keys.map("n", "<Space>x", "dd")"#);
        answer_all(&mut desk);
        type_keys(&mut desk, "@q");
        assert_eq!(editor_text(&desk), "a!\nb!\nd\ne\n");
        assert_eq!(note(&desk), "t.txt  4:1");
    }

    #[test]
    fn a_macro_sets_marks_and_jumps_as_it_goes() {
        let mut desk = editing("/t.txt", TEN_LINES);
        // Each line it puts in at the top takes the mark it set down.
        type_text(&mut desk, "qajmaggOx");
        escape(&mut desk);
        type_text(&mut desk, "`aq");
        assert_eq!(note(&desk), "t.txt  3:1");
        type_text(&mut desk, "@a");
        // It waits at the O for the grammar to say how the line's
        // indented: there's none for a .txt.
        assert_eq!(note(&desk), "t.txt  1:1");
        serve_without_grammar(&mut desk);
        assert_eq!(note(&desk), "t.txt  5:1");
        assert_eq!(&editor_text(&desk)[..10], "x\nx\nl1\nl2\n");
        // Its jumps are in the jump list, in order.
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  1:1", "where `a left");
        ctrl(&mut desk, "o");
        assert_eq!(note(&desk), "t.txt  5:1", "where gg left");
    }

    #[test]
    fn recording_is_shown_in_the_bar_until_q() {
        let mut desk = editing("/t.txt", "x\n");
        type_text(&mut desk, "qz");
        assert_eq!(note(&desk), "recording @z  t.txt  1:1");
        type_text(&mut desk, "ihi");
        assert_eq!(note(&desk), "recording @z  t.txt  1:3");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(note(&desk), "t.txt  1:2");
        // `q` in insert mode is typed, and so is a macro's.
        type_text(&mut desk, "@z");
        assert_eq!(editor_text(&desk), "hhiix\n");
        // Keys the window takes, such as `:`, and Esc on the command line,
        // are recorded too, and recording goes on.
        type_text(&mut desk, "qy:");
        escape(&mut desk);
        type_text(&mut desk, "xq");
        assert_eq!(register(&mut desk, 'y').as_deref(), Some(":<Esc>x"));
        assert_eq!(editor_text(&desk), "hhix\n");
        type_text(&mut desk, "@y");
        assert_eq!(editor_text(&desk), "hhx\n");
    }

    #[test]
    fn yanks_go_to_the_clipboard_and_what_was_copied_elsewhere_is_put() {
        let mut desk = editing("/t.txt", "a\nb\n");
        type_text(&mut desk, "yy");
        assert_eq!(desk.take_yanked(), [("a\n".to_owned(), Copy::Clipboard)]);
        // Copied in another program, while the window hadn't the focus.
        desk.clipboard_asked();
        desk.clipboard_heard(Copy::Clipboard, "z\n".into());
        type_text(&mut desk, "p");
        assert_eq!(editor_text(&desk), "a\nz\nb\n");
        // Copied in a terminal: `"*` holds it at once.
        desk.copied("sel".into(), Copy::Primary);
        type_text(&mut desk, "\"*P");
        assert_eq!(editor_text(&desk), "a\nselz\nb\n");
        assert_eq!(desk.take_yanked(), [("sel".to_owned(), Copy::Primary)]);
        // A macro that copies again and again leaves the clipboard one
        // copy to make.
        type_text(&mut desk, "qmyyjq3@m");
        assert_eq!(desk.take_yanked(), [("b\n".to_owned(), Copy::Clipboard)]);
        // Not, once `clipboard` says neither.
        command(&mut desk, "set clipboard=none");
        type_text(&mut desk, "dd");
        assert!(desk.take_yanked().is_empty());
    }

    #[test]
    fn colon_registers_and_marks_list_them() {
        let mut desk = editing("/p/t.txt", "one\ntwo\n");
        command(&mut desk, "registers");
        assert_eq!(note(&desk), "the registers are empty");
        type_text(&mut desk, "\"ayyjmb");
        command(&mut desk, "reg");
        desk.settle_editors();
        let editor = desk.editor(PaneId(1)).expect("an editor");
        let lines: Vec<String> = (editor.panel().expect("a panel").lines.iter())
            .map(crate::editor::PanelLine::text)
            .collect();
        assert_eq!(
            lines,
            ["type name content", "l  \"\"   one^J", "l  \"a   one^J"]
        );
        command(&mut desk, "marks");
        assert_eq!(finder_rows(&mut desk), ["t.txt:2: b  two"]);
    }

    // --- Macros, through the desk --------------------------------------------

    /// The daemon's side of each buffer, by buffer.
    type Daemons = HashMap<BufferId, crate::editor::Daemon>;

    /// The daemon's side of buffer 7, whose text is `text`.
    fn daemon_of_7(text: &str) -> Daemons {
        HashMap::from([(BufferId(7), crate::editor::Daemon::new(text))])
    }

    /// Makes the changes to buffers sent, with `daemons`, as the daemon
    /// does, and answers them, until none are left. Returns whether each
    /// edit sent started an undo step, with its buffer, and the other
    /// messages sent meanwhile, unanswered.
    fn serve(
        desk: &mut Desk,
        daemons: &mut Daemons,
    ) -> (Vec<(BufferId, bool)>, Vec<ClientMessage>) {
        let (mut steps, mut others) = (Vec::new(), Vec::new());
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                return (steps, others);
            }
            for message in messages {
                let ClientMessage::Edit { buffer, change, .. } = &message else {
                    others.push(message);
                    continue;
                };
                let buffer = *buffer;
                if let clackatoo_protocol::Change::Edit { step, .. } = change {
                    steps.push((buffer, *step));
                }
                let daemon = daemons.get_mut(&buffer).expect("a daemon for the buffer");
                let (answer, _) = daemon.take(message);
                desk.buffer_changed(buffer, answer);
            }
        }
    }

    /// The requests among `messages`.
    fn requests(messages: &[ClientMessage]) -> Vec<&Request> {
        (messages.iter())
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect()
    }

    /// The text the daemon has for `buffer`.
    fn daemon_text(daemons: &Daemons, buffer: u32) -> String {
        daemons[&BufferId(buffer)].text.to_string()
    }

    fn enter(desk: &mut Desk) {
        press(
            desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
    }

    /// The byte the focused editor's cursor is on.
    fn cursor_byte(desk: &Desk) -> usize {
        let (editor, copy) = desk.focused_editor().expect("an editor");
        editor.cursor_byte(copy)
    }

    /// The line the focused editor's cursor is on, from 0.
    fn cursor_line(desk: &Desk) -> usize {
        let (_, copy) = desk.focused_editor().expect("an editor");
        copy.text().byte_to_line(cursor_byte(desk))
    }

    /// The number of the finder's search, which must have been asked for.
    fn finder_search(desk: &mut Desk) -> u64 {
        let asked = (desk.take_outbox().into_iter()).find_map(|message| match message {
            ClientMessage::Find { id, .. } => Some(id),
            _ => None,
        });
        asked.expect("a search")
    }

    /// Gives the finder's search `id` the files `a.rs` and `b.rs`, in `/p`,
    /// and says that's all.
    fn found_a_and_b(desk: &mut Desk, id: u64) {
        found_files(desk, id, &["a.rs", "b.rs"]);
        let done = clackatoo_protocol::Found::Done(clackatoo_protocol::Done::All);
        desk.found(id, done);
    }

    #[test]
    fn a_macro_types_through_insert_mode_and_a_count_types_it_again() {
        let text = "a\nb\nc\nd\ne\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qaA!");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("A!<Esc>j"));
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "2@a");
        // Its edits are one undo step, as in Vim, which `u` undoes and
        // Ctrl+R redoes.
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(steps, [(BufferId(7), true), (BufferId(7), false)]);
        assert_eq!(daemon_text(&daemons, 7), "a!\nb!\nc!\nd\ne\n");
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb\nc\nd\ne\n");
        ctrl(&mut desk, "r");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd\ne\n");
        type_text(&mut desk, "4G@@");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd!\ne\n");
        // `"ap` puts the keys, which can be changed and typed again.
        type_text(&mut desk, "G\"ap");
        assert_eq!(editor_text(&desk), "a!\nb!\nc!\nd!\neA!<Esc>j\n");
        type_text(&mut desk, "0f!r?0l\"by$gg@b");
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!?\nb!\nc!\nd!\neA?<Esc>j\n");
    }

    #[test]
    fn a_macro_runs_commands_and_waits_for_the_daemon_to_answer_them() {
        let text = "a\nb\nc\nd\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qaA!");
        escape(&mut desk);
        command(&mut desk, "w");
        type_text(&mut desk, "jq");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("A!<Esc>:w<CR>j"));
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert_eq!(requests(&sent), [&save]);
        desk.answered(Response::Done);
        // Typed again, it writes the file, and goes on once the daemon
        // has, with an x pressed meanwhile waiting behind it.
        type_text(&mut desk, "@ax");
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert_eq!(requests(&sent), [&save]);
        assert_eq!(editor_text(&desk), "a!\nb!\nc\nd\n");
        assert_eq!(cursor_line(&desk), 1, "waiting after the :w");
        desk.answered(Response::Done);
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "a!\nb!\n\nd\n", "then the j, and the x");
        // A write the daemon refuses stops it, as a key that fails does.
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        let changed = clackatoo_protocol::Error::ChangedOnDisk("/t.txt".into());
        desk.answered(Response::Error(changed));
        serve(&mut desk, &mut daemons);
        assert_eq!(cursor_line(&desk), 2, "no j");
        assert_eq!(daemon_text(&daemons, 7), "a!\nb!\n!\nd\n");
    }

    #[test]
    fn keys_written_in_a_register_type_through_the_command_line() {
        let text = "x\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        // A line of keys, yanked into a register, types as it reads.
        type_text(&mut desk, "O:w<CR>A?<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        assert_eq!(requests(&sent), [&save]);
        desk.answered(Response::Done);
        serve(&mut desk, &mut daemons);
        assert_eq!(editor_text(&desk), "x?\n");
        // A command that isn't one stops it, saying so.
        type_text(&mut desk, "O:nosuch<CR>A!<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"by$dd");
        desk.message = None;
        type_text(&mut desk, "@b");
        serve(&mut desk, &mut daemons);
        assert!(desk.message.is_some(), "why it stopped");
        assert_eq!(editor_text(&desk), "x?\n");
    }

    /// Whether each edit sent to `buffer`, of those `steps` lists, started
    /// an undo step.
    fn starts_in(steps: &[(BufferId, bool)], buffer: u32) -> Vec<bool> {
        (steps.iter())
            .filter(|&&(of, _)| of == BufferId(buffer))
            .map(|&(_, step)| step)
            .collect()
    }

    #[test]
    fn a_macro_moves_between_panes_and_is_one_undo_step_in_each_file() {
        let mut desk = desk(one_tab(side_by_side(vec![
            file_pane(1, 7),
            file_pane(2, 8),
        ])));
        let (left, right) = ("abc1\nabc2\nabc3\n", "xyz1\nxyz2\nxyz3\n");
        load(&mut desk, 7, "/a.txt", left);
        load(&mut desk, 8, "/b.txt", right);
        let mut daemons = HashMap::from([
            (BufferId(7), crate::editor::Daemon::new(left)),
            (BufferId(8), crate::editor::Daemon::new(right)),
        ]);
        // With the window's own keys for moving between panes.
        type_text(&mut desk, "qaxxj");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "xxj");
        ctrl(&mut desk, "h");
        type_text(&mut desk, "q");
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some("xxj<C-l>xxj<C-h>")
        );
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "@a");
        assert_eq!(shown_pane(&desk), PaneId(1));
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "c1\nc2\nabc3\n");
        assert_eq!(daemon_text(&daemons, 8), "z1\nz2\nxyz3\n");
        // One undo step in each file, which its first edit starts.
        assert_eq!(starts_in(&steps, 7), [true, false]);
        assert_eq!(starts_in(&steps, 8), [true, false]);
        // So `u` in each undoes all the macro did there, and no more.
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "c1\nabc2\nabc3\n");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 8), "z1\nxyz2\nxyz3\n");
        // And the next macro starts steps of its own.
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(starts_in(&steps, 7), [true, false]);
        assert_eq!(starts_in(&steps, 8), [true, false]);
    }

    #[test]
    fn a_key_that_fails_in_any_pane_stops_the_macro() {
        let mut desk = desk(one_tab(side_by_side(vec![
            file_pane(1, 7),
            file_pane(2, 8),
        ])));
        let (left, right) = ("a\nb\n", "x\ny\n");
        load(&mut desk, 7, "/a.txt", left);
        load(&mut desk, 8, "/b.txt", right);
        let mut daemons = HashMap::from([
            (BufferId(7), crate::editor::Daemon::new(left)),
            (BufferId(8), crate::editor::Daemon::new(right)),
        ]);
        type_text(&mut desk, "qa");
        ctrl(&mut desk, "l");
        type_text(&mut desk, "jA!");
        escape(&mut desk);
        ctrl(&mut desk, "h");
        type_text(&mut desk, "A?");
        escape(&mut desk);
        type_text(&mut desk, "q");
        serve(&mut desk, &mut daemons);
        // The j on the right's last line fails: nothing after it is typed.
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert_eq!(daemon_text(&daemons, 8), "x\ny!\n");
        assert_eq!(daemon_text(&daemons, 7), "a?\nb\n");
    }

    #[test]
    fn a_macro_that_types_itself_stops_where_a_key_fails_or_after_too_many() {
        let mut desk = editing("/t.txt", "1\n2\n3\n4\n");
        // Cleared first, so recording `@a` types nothing.
        type_text(&mut desk, "qaqqaI- ");
        escape(&mut desk);
        type_text(&mut desk, "j@aq");
        assert_eq!(editor_text(&desk), "- 1\n2\n3\n4\n");
        type_text(&mut desk, "@a");
        assert_eq!(
            editor_text(&desk),
            "- 1\n- 2\n- 3\n- 4\n",
            "j on the last line stops it"
        );
        // A count stops too, where it fails, rather than going on with
        // what's left of it.
        let mut desk = editing("/t.txt", "a\nb\n");
        type_text(&mut desk, "qbA.");
        escape(&mut desk);
        type_text(&mut desk, "jq5@b");
        assert_eq!(editor_text(&desk), "a.\nb.\n");
        // Keys that make nothing stop it, as unknown keys do in Vim.
        let mut desk = editing("/t.txt", "x\n");
        type_text(&mut desk, "qcA1");
        escape(&mut desk);
        type_text(&mut desk, "Q");
        ctrl(&mut desk, "k");
        type_text(&mut desk, "A2");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(editor_text(&desk), "x12\n", "recording, it goes on");
        type_text(&mut desk, "@c");
        assert_eq!(editor_text(&desk), "x121\n", "replaying, it stops at Q");
        // And one that never fails is stopped after so many keys.
        type_text(&mut desk, "qdqqd0@dq@d");
        let said = desk.message.clone().unwrap_or_default();
        assert!(said.contains("stopped after"), "{said}");
        type_text(&mut desk, "x");
        assert_eq!(editor_text(&desk), "121\n", "it's done with");
    }

    #[test]
    fn gj_g0_and_g_dollar_are_typed_by_macros() {
        let text = format!("{}\nshort\n", "a".repeat(400));
        let mut desk = editing("/t.txt", &text);
        type_text(&mut desk, "qagjq");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("gj"));
        let row = cursor_byte(&desk);
        assert!(row > 1, "a row down the wrapped line: {row}");
        type_text(&mut desk, "@a");
        assert_eq!(cursor_byte(&desk), 2 * row);
        type_text(&mut desk, "2@a");
        assert_eq!(cursor_byte(&desk), 4 * row);
        // gj that can't go on, on the last row, fails, stopping the macro;
        // g$ and g0 never do.
        type_text(&mut desk, "Gqbg$g0gjxq");
        assert_eq!(editor_text(&desk).lines().nth(1), Some("hort"));
        type_text(&mut desk, "@b");
        assert_eq!(editor_text(&desk).lines().nth(1), Some("hort"), "no x");
    }

    #[test]
    fn a_search_that_finds_nothing_stops_a_macro() {
        let mut desk = editing("/t.txt", "x x x\n");
        type_text(&mut desk, "qa/x");
        enter(&mut desk);
        type_text(&mut desk, "r-A!");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("/x<CR>r-A!<Esc>"));
        assert_eq!(editor_text(&desk), "x - x!\n");
        type_text(&mut desk, "5@a");
        assert_eq!(editor_text(&desk), "- - -!!!\n", "two found, then none");
    }

    #[test]
    fn a_macro_types_into_the_finder_and_waits_for_what_it_finds() {
        let mut desk = editing("/p/a.rs", "a\n");
        type_text(&mut desk, "qa pfb.rs");
        let id = finder_search(&mut desk);
        found_a_and_b(&mut desk, id);
        enter(&mut desk);
        open_asked(&mut desk, 8, "/p/b.rs", "b\n");
        type_text(&mut desk, "q");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some(" pfb.rs<CR>"));
        command(&mut desk, "e /p/a.rs");
        open_asked(&mut desk, 9, "/p/a.rs", "a\n");
        // Typed again, it types into the finder at once, and Enter waits
        // for what it finds.
        type_text(&mut desk, "@a");
        let id = finder_search(&mut desk);
        assert_eq!(desk.finder.as_ref().map(Finder::query), Some("b.rs"));
        found_a_and_b(&mut desk, id);
        open_asked(&mut desk, 10, "/p/b.rs", "b\n");
        assert!(!desk.finding());
        assert_eq!(note(&desk), "b.rs  1:1");
        // With nothing it finds picked, Enter stops the macro.
        type_text(&mut desk, "O pfzzz<CR>A!<Esc>");
        escape(&mut desk);
        type_text(&mut desk, "0\"by$dd@b");
        let id = finder_search(&mut desk);
        found_a_and_b(&mut desk, id);
        assert_eq!(
            desk.message.as_deref(),
            Some("nothing in the finder is picked")
        );
        escape(&mut desk);
        assert_eq!(editor_text(&desk), "b\n", "no A!");
    }

    #[test]
    fn a_macro_waits_for_the_language_server_and_gives_up_on_it() {
        let mut desk = editing("/src/a.rs", A_RS);
        let mut daemons = daemon_of_7(A_RS);
        // Answers gd, asked once the edits before it are made, with a
        // place on `line`.
        let answer = |desk: &mut Desk, daemons: &mut Daemons, line| {
            let (_, sent) = serve(desk, daemons);
            let id = (sent.iter()).find_map(|message| match message {
                ClientMessage::Query { id, .. } => Some(*id),
                _ => None,
            });
            let place = clackatoo_protocol::FileLocation {
                path: "/src/a.rs".into(),
                line,
                col: 3,
                offset: None,
                text: String::new(),
            };
            let update = clackatoo_protocol::BufferUpdate::Answer {
                id: id.expect("a query"),
                version: daemons[&BufferId(7)].version(),
                answer: clackatoo_protocol::Answer::Locations(vec![place]),
            };
            desk.buffer_changed(BufferId(7), update);
            serve(desk, daemons);
        };
        type_text(&mut desk, "qagd");
        answer(&mut desk, &mut daemons, 2);
        type_text(&mut desk, "xq");
        assert_eq!(editor_text(&desk), "fn a() {}\nfn b() {}\nfn () {}\n");
        // The x waits for gd's answer, and goes where it went.
        type_text(&mut desk, "gg@a");
        assert_eq!(editor_text(&desk), "fn a() {}\nfn b() {}\nfn () {}\n");
        answer(&mut desk, &mut daemons, 1);
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
        // Esc stops a macro waiting, with the keys pressed meanwhile.
        type_text(&mut desk, "gg@al");
        escape(&mut desk);
        assert_eq!(desk.message.as_deref(), Some("the macro was stopped"));
        answer(&mut desk, &mut daemons, 0);
        assert_eq!(cursor_byte(&desk), 3, "gd went, and no more");
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
        // And it gives up after a while, if no answer comes.
        type_text(&mut desk, "gg@a");
        let later = std::time::Instant::now() + std::time::Duration::from_secs(6);
        assert!(desk.editor_wakes().is_some_and(|wake| wake < later));
        desk.tick_editors(later);
        let said = desk.message.clone().unwrap_or_default();
        assert_eq!(
            said,
            "the macro was stopped: it waited too long for the language server"
        );
        answer(&mut desk, &mut daemons, 0);
        assert_eq!(daemon_text(&daemons, 7), "fn a() {}\nfn () {}\nfn () {}\n");
    }

    #[test]
    fn q_in_a_terminal_stops_recording_as_in_an_editor() {
        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/t.txt", "abc\n");
        snapshot(&mut desk, 2);
        type_text(&mut desk, "qa");
        ctrl(&mut desk, "l");
        assert_eq!(mode(&desk, 2), Mode::Normal);
        type_text(&mut desk, "q");
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("<C-l>"));
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        assert_eq!(shown_pane(&desk), PaneId(2));
    }

    #[test]
    fn a_macro_types_what_a_completion_put_in_without_asking() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        let mut desk = editing("/src/a.rs", "\n\n");
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaial");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        let call = CompletionItem {
            label: "call".into(),
            kind: 3,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(0),
            text: "call(${1:a})$0".into(),
            snippet: true,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![call],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "y");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "call(a)\n\n");
        // Not the key that accepted it, but what it put in, with the cursor
        // where the snippet left it.
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some("ial<BS><BS>call(a)<Left><Esc>j")
        );
        serve_edits(&mut desk, &mut version);
        type_text(&mut desk, "@a");
        desk.tick_editors(later);
        assert_eq!(editor_text(&desk), "call(a)\ncall(a)\n");
        let asked = (desk.take_outbox().into_iter())
            .any(|message| matches!(message, ClientMessage::Query { .. }));
        assert!(!asked, "the server isn't asked");
    }

    #[test]
    fn a_macro_types_what_a_snippet_left_rather_than_the_keys_filling_it_in() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        // Filled in on a line indented four...
        let mut desk = editing("/src/a.rs", "    \n\n");
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaAif");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        // A mirror, lines indented more and less, and a place left with
        // the cursor in it.
        let snippet = CompletionItem {
            label: "if".into(),
            kind: 15,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: None,
            text: "if ${1:x} {\n\t${2:y}($1)\n}$0".into(),
            snippet: true,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![snippet],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "y");
        type_text(&mut desk, "a");
        let tab = |desk: &mut Desk| {
            press(desk, Key::Named(NamedKey::Tab), None, Modifiers::default());
        };
        tab(&mut desk);
        type_text(&mut desk, "b");
        escape(&mut desk);
        type_text(&mut desk, "jjq");
        let filled = "    if a {\n        b(a)\n    }\n";
        assert_eq!(editor_text(&desk), format!("{filled}\n"));
        assert_eq!(
            register(&mut desk, 'a').as_deref(),
            Some(
                "Aif<BS><BS>if a {<CR>    b(a)<CR><BS><BS><BS><BS>}<Up><End><Left><Left><Left><Esc>jj"
            )
        );
        // ...and typed again on a line indented none, by a file with no
        // grammar to indent it.
        serve_edits(&mut desk, &mut version);
        type_text(&mut desk, "@a");
        loop {
            let messages = desk.take_outbox();
            if messages.is_empty() {
                break;
            }
            for message in messages {
                match message {
                    ClientMessage::Edit { seq, .. } => {
                        version += 1;
                        desk.buffer_changed(BufferId(7), BufferUpdate::Acked { version, seq });
                    }
                    ClientMessage::Query {
                        id,
                        query: clackatoo_protocol::Query::Indent { .. },
                        ..
                    } => {
                        let answer = BufferUpdate::Answer {
                            id,
                            version,
                            answer: Answer::Nothing,
                        };
                        desk.buffer_changed(BufferId(7), answer);
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(
            editor_text(&desk),
            format!("{filled}if a {{\n    b(a)\n}}\n")
        );
    }

    #[test]
    fn a_macro_waiting_for_the_grammar_is_still_one_undo_step() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut daemons = daemon_of_7(text);
        type_text(&mut desk, "qadaf");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let question = |sent: &[ClientMessage]| {
            (sent.iter())
                .find_map(|message| match message {
                    ClientMessage::Query { id, .. } => Some(*id),
                    _ => None,
                })
                .expect("a question")
        };
        let id = question(&sent);
        answer_at(&mut desk, id, 0, vec![function(0, 9, None)]);
        type_text(&mut desk, "ix");
        escape(&mut desk);
        type_text(&mut desk, "jq");
        serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "x\nfn b() {}\n");
        // The x waits for the answer, and goes in after, in the same step.
        type_text(&mut desk, "@a");
        let (steps, sent) = serve(&mut desk, &mut daemons);
        assert!(steps.is_empty());
        let version = daemons[&BufferId(7)].version();
        answer_at(
            &mut desk,
            question(&sent),
            version,
            vec![function(2, 11, None)],
        );
        let (steps, _) = serve(&mut desk, &mut daemons);
        assert_eq!(daemon_text(&daemons, 7), "x\nx\n");
        assert_eq!(steps, [(BufferId(7), true), (BufferId(7), false)]);
        type_text(&mut desk, "u");
        serve(&mut desk, &mut daemons);
        assert_eq!(
            daemon_text(&daemons, 7),
            "x\nfn b() {}\n",
            "one u undoes it all"
        );
    }

    /// The number of the last query among `sent`, which must hold one.
    fn query_in(sent: &[ClientMessage]) -> u64 {
        let asked = (sent.iter().rev()).find_map(|message| match message {
            ClientMessage::Query { id, .. } => Some(*id),
            _ => None,
        });
        asked.expect("a query")
    }

    /// Answers the query `id` about buffer 7 with a place on `line`, at
    /// its fourth column, as gd's answer.
    fn answer_gd(desk: &mut Desk, daemons: &Daemons, id: u64, path: &str, line: u64) {
        let place = clackatoo_protocol::FileLocation {
            path: path.into(),
            line,
            col: 3,
            offset: None,
            text: String::new(),
        };
        let update = clackatoo_protocol::BufferUpdate::Answer {
            id,
            version: daemons[&BufferId(7)].version(),
            answer: clackatoo_protocol::Answer::Locations(vec![place]),
        };
        desk.buffer_changed(BufferId(7), update);
    }

    #[test]
    fn a_binding_a_macros_last_key_sets_off_is_part_of_the_macro() {
        let text = "abc\ndef\n";
        let mut desk = editing("/t.txt", text);
        let mut daemons = daemon_of_7(text);
        desk.lua_from(r#"client.keys.map("n", "<Space>m", ":w<CR>j")"#);
        answer_all(&mut desk);
        type_text(&mut desk, "Ox m");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd");
        serve(&mut desk, &mut daemons);
        // The binding the macro's last key sets off waits, as the macro's
        // own keys do: the j waits for the write.
        type_text(&mut desk, "@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        let save = Request::Save {
            buffer: BufferId(7),
            force: false,
        };
        assert_eq!(requests(&sent), [&save]);
        assert_eq!(
            (editor_text(&desk).as_str(), cursor_line(&desk)),
            ("bc\ndef\n", 0)
        );
        desk.answered(Response::Done);
        assert_eq!(cursor_line(&desk), 1);
    }

    #[test]
    fn a_macro_waiting_stops_if_the_focus_moves_but_goes_where_it_asks() {
        let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        load(&mut desk, 7, "/src/a.rs", A_RS);
        snapshot(&mut desk, 2);
        let mut daemons = daemon_of_7(A_RS);
        type_text(&mut desk, "Ogdx");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd");
        serve(&mut desk, &mut daemons);
        // Clicked away from while it waits for gd: it stops, and the x
        // doesn't go to the terminal, nor anywhere.
        type_text(&mut desk, "@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        desk.show_pane(PaneId(2));
        answer_gd(&mut desk, &daemons, query_in(&sent), "/src/a.rs", 1);
        let (_, sent) = serve(&mut desk, &mut daemons);
        let typed = (requests(&sent).into_iter())
            .any(|request| matches!(request, Request::SendInput { .. }));
        assert!(!typed, "nothing for the terminal: {sent:?}");
        assert_eq!(
            desk.message.as_deref(),
            Some("the macro was stopped: the focus moved")
        );
        assert_eq!(daemon_text(&daemons, 7), A_RS);
        // So too if another window closes its pane meanwhile.
        ctrl(&mut desk, "h");
        type_text(&mut desk, "@a");
        serve(&mut desk, &mut daemons);
        desk.workspaces_changed(one_tab(pane(2)));
        desk.tick_editors(std::time::Instant::now());
        assert_eq!(
            desk.message.as_deref(),
            Some("the macro was stopped: the focus moved")
        );
        assert_eq!(shown_pane(&desk), PaneId(2));
        // But a pane the macro makes, the focus going to it, is where it
        // goes on.
        let mut desk = editing("/t.txt", "abc\n");
        let mut daemons = daemon_of_7("abc\n");
        type_text(&mut desk, "O<C-b>|<C-b>p1x");
        escape(&mut desk);
        type_text(&mut desk, "0\"ay$dd@a");
        let (_, sent) = serve(&mut desk, &mut daemons);
        assert!(matches!(requests(&sent)[..], [Request::Split { .. }]));
        desk.workspaces_changed(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
        desk.answered(Response::PaneCreated(PaneId(2)));
        serve(&mut desk, &mut daemons);
        assert_eq!(shown_pane(&desk), PaneId(1));
        assert_eq!(daemon_text(&daemons, 7), "bc\n");
    }

    #[test]
    fn keys_held_behind_a_wait_that_fail_do_as_they_would_typed() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // Held while daf waits: a $ and an l, which fails at the line's
        // end, then the finder's keys, which open it as typed.
        type_keys(&mut desk, "jdaf$l pf");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(10, 19, None)]);
        assert_eq!(editor_text(&desk), "fn a() {}\n\n");
        assert!(desk.finding(), "the keys after the l");
    }

    #[test]
    fn a_q_held_while_recording_stops_it_where_it_was_pressed() {
        let text = "fn a() {}\nfn b() {}\n";
        let mut desk = editing("/p/a.rs", text);
        let mut version = 0;
        // The q and y come while daf waits, and are held.
        type_keys(&mut desk, "qadafqy");
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a question");
        answer_at(&mut desk, id, version, vec![function(0, 9, None)]);
        assert_eq!(desk.recording(), None);
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("daf"));
    }

    #[test]
    fn a_key_a_binding_types_that_a_menu_takes_is_recorded_as_pressed() {
        use clackatoo_protocol::{Answer, BufferUpdate, CompletionItem, ServerFeatures};

        let mut desk = editing("/src/a.rs", "\n\n");
        desk.lua_from(r#"client.keys.map("i", "<C-j>", "<C-y>")"#);
        answer_all(&mut desk);
        let features = ServerFeatures {
            completion: Some(vec!['.']),
            ..ServerFeatures::default()
        };
        desk.buffer_changed(BufferId(7), BufferUpdate::Features(features));
        let mut version = 0;
        type_text(&mut desk, "qaial");
        serve_edits(&mut desk, &mut version);
        let later = std::time::Instant::now() + std::time::Duration::from_secs(1);
        desk.tick_editors(later);
        let (id, _) = serve_edits(&mut desk, &mut version).expect("a completion asked");
        let call = CompletionItem {
            label: "call".into(),
            kind: 3,
            detail: String::new(),
            filter: None,
            sort: None,
            preselect: false,
            start: Some(0),
            text: "call".into(),
            snippet: false,
            more: Vec::new(),
        };
        let answer = BufferUpdate::Answer {
            id,
            version,
            answer: Answer::Completions {
                items: vec![call],
                incomplete: false,
            },
        };
        desk.buffer_changed(BufferId(7), answer);
        ctrl(&mut desk, "j");
        escape(&mut desk);
        type_text(&mut desk, "q");
        assert_eq!(editor_text(&desk), "call\n\n");
        assert_eq!(register(&mut desk, 'a').as_deref(), Some("ial<C-j><Esc>"));
    }

    #[test]
    fn a_macro_waits_only_for_what_it_asked() {
        let mut desk = editing("/src/a.rs", A_RS);
        let mut daemons = daemon_of_7(A_RS);
        type_text(&mut desk, "qaxq");
        serve(&mut desk, &mut daemons);
        // A gd from before the macro, never answered, isn't waited for.
        type_text(&mut desk, "gd");
        serve(&mut desk, &mut daemons);
        type_text(&mut desk, "@ax");
        assert_eq!(editor_text(&desk), "a() {}\nfn b() {}\nfn c() {}\n");
        // Nor, once the connection's lost, is anything asked on it.
        let waiting = |desk: &Desk| {
            let (editor, copy) = desk.focused_editor().expect("an editor");
            editor.waiting_for_answer(copy, 0)
        };
        assert!(waiting(&desk));
        desk.lost();
        desk.connected(one_tab(file_pane(1, 7)));
        assert!(!waiting(&desk));
    }

    #[test]
    #[ignore = "a measurement, to run optimised: cargo test --release -- --ignored replaying"]
    fn replaying_a_20_key_macro_1000_times_on_10000_lines() {
        let lines: Vec<String> = (0..10_000)
            .map(|line| format!("line {line} of the file\n"))
            .collect();
        let text = lines.concat();
        let mut desk = editing("/big.txt", &text);
        // Twenty keys: motions, a change, an insert at the end, a delete.
        type_text(&mut desk, "qa0wcwnew");
        escape(&mut desk);
        type_text(&mut desk, "A end");
        escape(&mut desk);
        type_text(&mut desk, "bxj^llq");
        let mut times = Vec::new();
        for _ in 0..5 {
            let started = std::time::Instant::now();
            type_text(&mut desk, "1000@a");
            times.push(started.elapsed().as_secs_f64() * 1000.0);
            // What the daemon would be sent, so the next go starts afresh.
            desk.take_outbox();
        }
        let line = |n: usize| editor_text(&desk).lines().nth(n).unwrap().to_owned();
        assert_eq!(line(5000), "line new of the file nd");
        times.sort_by(f64::total_cmp);
        eprintln!(
            "1000@a of 20 keys on 10,000 lines: best {:.1} ms, median {:.1} ms, worst {:.1} ms \
             ({:.2} µs a key at best)",
            times[0],
            times[2],
            times[4],
            times[0] * 1000.0 / 20_000.0
        );
    }

    #[test]
    fn editor_commands_run_from_the_command_line_and_stop_a_macro_when_refused() {
        let mut desk = editing("/t.txt", "x\nx\ny\nx\n");
        type_text(&mut desk, "qa:s/x/-/");
        enter(&mut desk);
        type_text(&mut desk, "jq");
        assert_eq!(editor_text(&desk), "-\nx\ny\nx\n");
        // The third line has no x: the macro stops there, as in Vim.
        type_text(&mut desk, "5@a");
        assert_eq!(editor_text(&desk), "-\n-\ny\nx\n");
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains("isn't in"))
        );
        // A mistake is said in the bar.
        type_text(&mut desk, ":1,2frob");
        enter(&mut desk);
        assert!(
            desk.message
                .as_deref()
                .is_some_and(|message| message.contains("frob"))
        );
    }

    #[test]
    fn the_command_being_typed_is_previewed_in_the_focused_editor_until_esc() {
        let rows = |desk: &mut Desk| -> Vec<String> {
            desk.settle_editors();
            let editor = desk.editor(PaneId(1)).expect("an editor");
            editor
                .scene()
                .0
                .iter()
                .map(|row| row.text().trim_end().to_owned())
                .collect()
        };
        let mut desk = editing("/t.txt", "one two\n");
        let before = rows(&mut desk);
        type_text(&mut desk, ":%s/two/2/");
        assert!(rows(&mut desk)[0].ends_with("one 2"));
        assert_eq!(editor_text(&desk), "one two\n");
        escape(&mut desk);
        assert_eq!(rows(&mut desk), before);
        // From a selection, the command line starts with its lines.
        type_text(&mut desk, "V:");
        assert!(matches!(desk.status(), Status::Typing { text, .. } if text == ":'<,'>▏"));
        type_text(&mut desk, "s/one/1/");
        enter(&mut desk);
        assert_eq!(editor_text(&desk), "1 two\n");
    }

    #[test]
    fn a_number_is_a_line_in_an_editor() {
        let mut desk = editing("/t.txt", "1\n2\n3\n4\n");
        type_text(&mut desk, ":3");
        enter(&mut desk);
        let (editor, copy) = desk.focused_editor().expect("an editor");
        assert_eq!(editor.cursor_byte(copy), 4);
        // Ctrl+O comes back, as from a jump.
        ctrl(&mut desk, "o");
        let (editor, copy) = desk.focused_editor().expect("an editor");
        assert_eq!(editor.cursor_byte(copy), 0);
    }

    // --- File explorers ------------------------------------------------------

    fn files_pane(pane: u32, files: u32) -> Layout {
        Layout::Pane(LayoutPane {
            id: PaneId(pane),
            content: Content::Files(clackatoo_protocol::FilesId(files)),
        })
    }

    fn entry(name: &str, dir: bool) -> clackatoo_protocol::Entry {
        clackatoo_protocol::Entry {
            name: name.into(),
            kind: if dir {
                clackatoo_protocol::EntryKind::Dir
            } else {
                clackatoo_protocol::EntryKind::File { executable: false }
            },
            ignored: false,
        }
    }

    /// Gives explorer 1 the root `/p`, holding `src/` and `a.txt`, with
    /// `src` holding `main.rs` if it's asked for, and answers what's
    /// asked.
    fn explore_p(desk: &mut Desk) {
        use clackatoo_protocol::{FilesId, FilesUpdate};
        desk.files_changed(
            FilesId(1),
            FilesUpdate::Root {
                path: "/p".into(),
                name: "~/p".into(),
            },
        );
        let listed = |dir: &str, entries| FilesUpdate::Listed {
            dir: dir.into(),
            entries,
            cut_short: false,
        };
        desk.files_changed(
            FilesId(1),
            listed("/p", vec![entry("src", true), entry("a.txt", false)]),
        );
        let src_asked = (desk.take_outbox().iter()).any(|message| {
            matches!(message, ClientMessage::Files { action: clackatoo_protocol::FilesAction::Expand(dir), .. } if dir == "/p/src")
        });
        if src_asked {
            desk.files_changed(FilesId(1), listed("/p/src", vec![entry("main.rs", false)]));
        }
    }

    /// The file operations asked for, by their numbers.
    fn file_ops(desk: &mut Desk) -> Vec<(u64, clackatoo_protocol::FileOp)> {
        (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::FileOp { id, op } => Some((id, op)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn space_e_opens_an_explorer_down_the_tabs_left_at_the_editors_file_then_closes_it() {
        use clackatoo_protocol::{FilesId, OpenFiles};
        let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
        type_keys(&mut desk, " e");
        let asked: Vec<Request> = (desk.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(request) => Some(request),
                _ => None,
            })
            .collect();
        assert_eq!(
            asked,
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Left,
                terminal: PaneTerminal::Files(OpenFiles { dir: None }),
                share: Some(375),
            }],
            "30 of the tab's 80 columns"
        );
        // The daemon makes it, telling of the pane first.
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
        desk.workspaces_changed(one_tab(layout));
        desk.answered(Response::PaneCreated(PaneId(2)));
        assert_eq!(shown_pane(&desk), PaneId(2));
        assert!(
            answer_all(&mut desk).contains(&Request::AttachFiles { files: FilesId(1) }),
            "attached"
        );
        explore_p(&mut desk);
        assert_eq!(note(&desk), "~/p  src/main.rs", "at the editor's file");
        assert_eq!(desk.title(), Some("~/p"));
        type_keys(&mut desk, " e");
        assert_eq!(
            answer_all(&mut desk),
            [Request::ClosePane { pane: PaneId(2) }]
        );
    }

    #[test]
    fn a_file_picked_opens_in_the_editor_last_focused_or_a_split_or_a_tab() {
        use clackatoo_protocol::OpenFile;
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7), pane(3)]);
        let mut desk = desk(one_tab(layout));
        load(&mut desk, 7, "/p/b.txt", "b\n");
        assert_eq!(shown_pane(&desk), PaneId(2));
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "h");
        assert_eq!(shown_pane(&desk), PaneId(2));
        answer_all(&mut desk);
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        let file = |path: &str| OpenFile {
            path: path.into(),
            cwd: None,
        };
        let mut asked = answer_all(&mut desk);
        asked.retain(|request| !matches!(request, Request::Resize { .. }));
        assert_eq!(
            asked,
            [
                Request::Focus { pane: PaneId(1) },
                Request::OpenHere {
                    pane: PaneId(1),
                    file: file("/p/a.txt"),
                }
            ]
        );
        assert_eq!(shown_pane(&desk), PaneId(1));
        ctrl(&mut desk, "h");
        answer_all(&mut desk);
        ctrl(&mut desk, "v");
        ctrl(&mut desk, "t");
        assert_eq!(
            answer_all(&mut desk),
            [
                Request::Split {
                    pane: PaneId(1),
                    side: Side::Right,
                    terminal: PaneTerminal::File(file("/p/a.txt")),
                },
                Request::NewTab {
                    workspace: WorkspaceId(1),
                    terminal: PaneTerminal::File(file("/p/a.txt")),
                }
            ]
        );
    }

    #[test]
    fn with_no_editor_a_file_picked_opens_beside_the_explorer() {
        use clackatoo_protocol::OpenFile;
        let file = PaneTerminal::File(OpenFile {
            path: "/p/a.txt".into(),
            cwd: None,
        });
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        assert_eq!(
            answer_all(&mut desk),
            [Request::Split {
                pane: PaneId(3),
                side: Side::Left,
                terminal: file.clone(),
            }],
            "between the explorer and the terminal"
        );
        // Alone, the explorer keeps its width.
        let mut desk = super::tests::desk(one_tab(files_pane(2, 1)));
        explore_p(&mut desk);
        type_keys(&mut desk, "j");
        enter(&mut desk);
        assert_eq!(
            answer_all(&mut desk),
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Right,
                terminal: file,
                share: Some(625),
            }]
        );
    }

    #[test]
    fn file_operations_are_asked_in_the_bar_and_what_comes_of_them_said() {
        use clackatoo_protocol::{FileOp, FileOpError};
        let mut desk = desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        explore_p(&mut desk);
        type_keys(&mut desk, "jd");
        assert_eq!(desk.text_goes_to(), Some(TextTarget::Bar));
        assert!(matches!(
            desk.status(),
            Status::Typing { text, .. } if text == "delete ~/p/a.txt, to the trash? (y/n)"
        ));
        type_keys(&mut desk, "y");
        let trashed = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: true,
        };
        assert_eq!(file_ops(&mut desk), [(0, trashed)]);
        desk.file_op_done(0, Err(FileOpError::NoTrash("no trash there".into())));
        assert!(matches!(
            desk.status(),
            Status::Typing { text, .. } if text == "no trash there: delete ~/p/a.txt for good? (y/n)"
        ));
        type_keys(&mut desk, "y");
        let deleted = FileOp::Delete {
            path: "/p/a.txt".into(),
            trash: false,
        };
        assert_eq!(file_ops(&mut desk), [(1, deleted)]);
        desk.file_op_done(1, Ok(()));
        assert_eq!(note(&desk), "deleted ~/p/a.txt for good");
        // Cut here, put there: a move, which places in files follow.
        type_keys(&mut desk, "xkp");
        let moved = FileOp::Move {
            from: "/p/a.txt".into(),
            to: "/p/src/a.txt".into(),
            overwrite: None,
        };
        assert_eq!(file_ops(&mut desk), [(2, moved)]);
        desk.file_op_done(2, Ok(()));
        assert_eq!(note(&desk), "moved to ~/p/src/a.txt");
        type_keys(&mut desk, "p");
        assert_eq!(note(&desk), "nothing's cut or copied: x or y does");
    }

    #[test]
    fn colon_explore_opens_an_explorer_at_a_directory_or_goes_to_the_one_there() {
        use clackatoo_protocol::OpenFiles;
        let mut desk = desk(one_tab(pane(3)));
        normal_mode(&mut desk);
        command(&mut desk, "Explore ~/src");
        assert_eq!(
            answer_all(&mut desk),
            [Request::SplitTab {
                tab: TabId(1),
                side: Side::Left,
                terminal: PaneTerminal::Files(OpenFiles {
                    dir: Some("~/src".into())
                }),
                share: Some(375),
            }]
        );
        let mut desk = super::tests::desk(one_tab(side_by_side(vec![files_pane(2, 1), pane(3)])));
        normal_mode(&mut desk);
        ctrl(&mut desk, "l");
        assert_eq!(shown_pane(&desk), PaneId(3));
        command(&mut desk, "Ex");
        assert_eq!(shown_pane(&desk), PaneId(2));
        // Keys in it are its own: q closes it.
        type_keys(&mut desk, "q");
        assert!(answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(2) }));
    }

    #[test]
    fn q_in_an_explorer_stops_recording_rather_than_closing_it() {
        let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
        let mut desk = super::tests::desk(one_tab(layout));
        load(&mut desk, 7, "/p/b.txt", "b\n");
        ctrl(&mut desk, "l");
        type_keys(&mut desk, "qa");
        assert_eq!(desk.recording(), Some('a'));
        ctrl(&mut desk, "h");
        answer_all(&mut desk);
        type_keys(&mut desk, "q");
        assert_eq!(desk.recording(), None);
        assert!(!answer_all(&mut desk).contains(&Request::ClosePane { pane: PaneId(2) }));
    }
}
