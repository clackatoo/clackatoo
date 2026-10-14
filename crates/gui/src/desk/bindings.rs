//! The window's own keys, as bindings in the [`Keymap`]: what's bound
//! before any config, and what each does.
//!
//! | Ctrl+B, then, in any mode | Does |
//! |---|---|
//! | `1`–`9` | Tab 1 to 9 |
//! | `c` | A new tab |
//! | `[` `]` | The tab before, the next tab |
//! | `,` | Renames the tab |
//! | `p` and `1`–`9` | Pane 1 to 9, numbered on screen meanwhile |
//! | `h` `j` `k` `l` | The pane left, below, above, right |
//! | `\|` `-` | Splits the pane, to the right or below |
//! | `x` | Closes the pane |
//! | `o` `i` | Back and forward through the jump list, within the tab (see [`super::jumps`]) |
//! | `a` | Opens a chat with an agent, to the right |
//! | `w` | Picks a workspace, or makes one, in the palette |
//! | `r` | Picks a machine to open a new window on, as `:connect` does |
//! | Ctrl+B | Sends Ctrl+B to the program |
//!
//! Esc, or any other key, cancels. While Ctrl+B waits, the bottom bar says
//! what it takes.
//!
//! In normal and visual mode, Space starts the finder's keys, as a leader
//! key does in Neovim: `<Space>pf` the project's files, `<Space>pgf` a grep
//! of them, `<Space>?` the files open, `<Space>ds` what's wrong in the
//! file, `<Space>bs` the symbols in the focused editor's file, and
//! `<Space>ps` the project's (see [`crate::finder`]). And in an editor,
//! the keys for the hunk at the cursor, as gitsigns.nvim suggests them:
//! `<Space>hp` shows it, `<Space>hr` puts its lines back as they are in
//! git's index, and `<Space>hs` stages it. `<Space>e` opens the tab's
//! file explorer down its left, goes to it, or closes it if it has the
//! focus (see [`super::explorers`]). `<Space>gr` opens a review of what
//! agents changed, `<Space>ga` keeps the agent's change at the cursor and
//! `<Space>gx` rejects it, and `<Space>c` comments on the cursor's line
//! for the agent (see [`super::reviews`]).
//!
//! In normal and visual mode, `:` opens the command line, Ctrl with `h`,
//! `j`, `k` or `l` moves to the pane that way, Alt with them moves the
//! pane's edge, and Ctrl+O and Ctrl+I go back and forward through the
//! jump list, within the tab: panes, and places in files. Ctrl+Shift+O
//! and Ctrl+Shift+I go back and forward through all of it, in any mode, a
//! pane's prompt included.

use clackatoo_protocol::{Side, WorkspaceId};

use super::{Desk, Input, Purpose};
use crate::command::{Command, Kind, Place, ReviewWhich};
use crate::editor::HunkAction;
use crate::finder;
use crate::keymap::{KeyMode, Keymap, parse_keys, parse_modes};
use crate::layout::Direction;

/// How many workspaces the picker lists.
const CHOICES: usize = 8;

/// What a key can be bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Binding {
    /// One of the window's own actions.
    Window(Action),
    /// A function of the config's, by its number.
    Lua(u64),
    /// Keys to type into the focused pane, as if pressed, past the
    /// bindings: `client.keys.map("n", "gi", "gri")`.
    Keys(Vec<crate::keymap::Chord>),
}

/// The window's own actions, which its keys are bound to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// The tab with this number, from 1.
    Tab(usize),
    NewTab,
    /// The next tab, or with `false`, the one before.
    NextTab(bool),
    RenameTab,
    /// The pane with this number in the tab, from 1.
    Pane(usize),
    /// The pane beside the focused one, that way.
    Focus(Direction),
    /// Moves the focused pane's edge a cell that way.
    MoveEdge(Direction),
    /// A new pane beside the focused one.
    Split(Side, Kind),
    Close,
    /// Back through the jump list, or forward, within the tab or anywhere.
    Jump {
        back: bool,
        within_tab: bool,
    },
    /// The palette, to pick a workspace or make one.
    PickWorkspace,
    /// The command line.
    CommandLine,
    /// The picker of a machine to open a new window on, as `:connect`.
    Connect,
    /// The finder, for this.
    Find(finder::Kind),
    /// This, to the hunk at the focused editor's cursor.
    Hunk(HunkAction),
    /// The tab's file explorer: opened down its left, gone to, or closed
    /// if it has the focus.
    Explore,
    /// Reviewing what agents changed (see [`super::reviews`]).
    Review(ReviewKey),
    /// Sends Ctrl+B to the focused pane's program.
    SendCtrlB,
    /// Turns inlay hints off, or on, as `:set ih!` does.
    ToggleInlayHints,
}

/// What a key does in reviewing what agents changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReviewKey {
    /// The tab's review: opened, gone to, or closed if it has the focus.
    Open,
    /// Keep the agent's change at the cursor.
    Keep,
    /// Reject it.
    Reject,
    /// Comment on the cursor's line, for the agent.
    Comment,
}

/// The window's keymap, which starts with its own keys bound.
#[derive(Debug)]
pub(crate) struct Keys(Keymap<Binding>);

impl Default for Keys {
    fn default() -> Self {
        Self(defaults())
    }
}

impl std::ops::Deref for Keys {
    type Target = Keymap<Binding>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Keys {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The keymap the window starts with.
fn defaults() -> Keymap<Binding> {
    let mut map = Keymap::default();
    let mut bind = |modes: &str, keys: &str, action, label: Option<&str>| {
        let modes = parse_modes(modes).expect("the defaults' modes are valid");
        let keys = parse_keys(keys).expect("the defaults' keys are valid");
        map.bind(&modes, &keys, Binding::Window(action), label);
    };
    let tab = Some("tab");
    let pane = Some("pane");
    for number in 1..=9 {
        bind("niv", &format!("<C-b>{number}"), Action::Tab(number), tab);
    }
    bind("niv", "<C-b>c", Action::NewTab, tab);
    bind("niv", "<C-b>[", Action::NextTab(false), tab);
    bind("niv", "<C-b>]", Action::NextTab(true), tab);
    bind("niv", "<C-b>,", Action::RenameTab, tab);
    for number in 1..=9 {
        bind(
            "niv",
            &format!("<C-b>p{number}"),
            Action::Pane(number),
            pane,
        );
    }
    for (letter, direction) in DIRECTIONS {
        bind(
            "niv",
            &format!("<C-b>{letter}"),
            Action::Focus(direction),
            pane,
        );
    }
    bind(
        "niv",
        "<C-b>|",
        Action::Split(Side::Right, Kind::Terminal),
        pane,
    );
    bind(
        "niv",
        "<C-b>-",
        Action::Split(Side::Below, Kind::Terminal),
        pane,
    );
    bind("niv", "<C-b>x", Action::Close, pane);
    let jump = |back| Action::Jump {
        back,
        within_tab: true,
    };
    bind("niv", "<C-b>o", jump(true), Some("jump"));
    bind("niv", "<C-b>i", jump(false), Some("jump"));
    bind(
        "niv",
        "<C-b>a",
        Action::Split(Side::Right, Kind::Agent),
        Some("agent"),
    );
    bind("niv", "<C-b>w", Action::PickWorkspace, Some("workspace"));
    bind("niv", "<C-b>r", Action::Connect, Some("connect"));
    bind("niv", "<C-b><C-b>", Action::SendCtrlB, None);

    bind("nv", ":", Action::CommandLine, None);
    for (keys, kind) in FINDER_KEYS {
        bind("nv", keys, Action::Find(kind), Some("find"));
    }
    // As gitsigns.nvim suggests them.
    let hunk = Some("hunk");
    bind("nv", "<Space>hp", Action::Hunk(HunkAction::Preview), hunk);
    bind("nv", "<Space>hr", Action::Hunk(HunkAction::Reset), hunk);
    bind("nv", "<Space>hs", Action::Hunk(HunkAction::Stage), hunk);
    // As nvim-tree's and neo-tree's are often bound.
    bind("nv", "<Space>e", Action::Explore, Some("explorer"));
    // As kickstart.nvim's `<leader>th`.
    bind(
        "nv",
        "<Space>th",
        Action::ToggleInlayHints,
        Some("inlay hints"),
    );
    for (letter, direction) in DIRECTIONS {
        bind(
            "nv",
            &format!("<C-{letter}>"),
            Action::Focus(direction),
            None,
        );
        bind(
            "nv",
            &format!("<A-{letter}>"),
            Action::MoveEdge(direction),
            None,
        );
    }
    bind("nv", "<C-o>", jump(true), None);
    bind("nv", "<C-i>", jump(false), None);
    // The terminal's own keys, as Ctrl+Shift+C is, so programs lose
    // nothing.
    for (keys, back) in [("<C-O>", true), ("<C-I>", false)] {
        let action = Action::Jump {
            back,
            within_tab: false,
        };
        bind("nivc", keys, action, None);
    }
    bind_review_keys(&mut map);
    map
}

/// The finder's keys, as jack's Telescope keys are: `p` for the project,
/// as in `<Space>pf`, and `b` for the buffer, as in `<Space>bs`, its
/// symbols.
const FINDER_KEYS: [(&str, finder::Kind); 6] = [
    ("<Space>pf", finder::Kind::Files),
    ("<Space>pgf", finder::Kind::Grep),
    ("<Space>?", finder::Kind::Buffers),
    ("<Space>ds", finder::Kind::Diagnostics),
    ("<Space>bs", finder::Kind::Symbols),
    ("<Space>ps", finder::Kind::ProjectSymbols),
];

/// Binds the keys for reviewing what agents changed in `map`.
fn bind_review_keys(map: &mut Keymap<Binding>) {
    let modes = parse_modes("nv").expect("the defaults' modes are valid");
    for (keys, key, label) in REVIEW_KEYS {
        let keys = parse_keys(keys).expect("the defaults' keys are valid");
        map.bind(
            &modes,
            &keys,
            Binding::Window(Action::Review(key)),
            Some(label),
        );
    }
}

/// The keys for reviewing what agents changed, beside git's hunk keys:
/// `g` for the agents' changes, as `h` is for git's hunks, then `r`eview,
/// `a`ccept and `x`, Vim's delete; and `c` to comment.
const REVIEW_KEYS: [(&str, ReviewKey, &str); 4] = [
    ("<Space>gr", ReviewKey::Open, "review"),
    ("<Space>ga", ReviewKey::Keep, "review"),
    ("<Space>gx", ReviewKey::Reject, "review"),
    ("<Space>c", ReviewKey::Comment, "comment"),
];

/// Vim's letters for the four ways.
const DIRECTIONS: [(char, Direction); 4] = [
    ('h', Direction::Left),
    ('j', Direction::Down),
    ('k', Direction::Up),
    ('l', Direction::Right),
];

impl Desk {
    /// Whether the panes are numbered on screen, while keys wait for a
    /// pane's number, as after Ctrl+B p.
    pub(crate) fn numbering_panes(&self) -> bool {
        !self.keymap.waiting().is_empty()
            && (self.keymap.next_keys().iter())
                .any(|(_, binding, _)| matches!(binding, Binding::Window(Action::Pane(_))))
    }

    /// Stops waiting for the rest of a binding's keys, as when the mouse
    /// is clicked. Returns whether it was waiting.
    pub(crate) fn cancel_keys(&mut self) -> bool {
        self.forget_waiting();
        self.keymap.cancel()
    }

    /// Carries out one of the window's own actions.
    pub(super) fn act(&mut self, action: Action) {
        match action {
            Action::Tab(number) => self.run(Command::Tab(number)),
            Action::NewTab => self.new_tab(),
            Action::NextTab(forward) => self.next_tab(forward),
            Action::RenameTab => {
                if let Some(shown) = self.shown {
                    self.rename_tab(shown.tab);
                }
            }
            Action::Pane(number) => self.show_pane_numbered(number),
            Action::Focus(direction) => self.focus_towards(direction),
            Action::MoveEdge(direction) => self.move_edge(direction),
            Action::Split(side, kind) => self.run(Command::Open {
                place: Place::Beside(side),
                kind,
                daemon: None,
                agent: None,
            }),
            Action::Close => self.run(Command::Close),
            Action::Jump { back, within_tab } => self.jump(back, within_tab),
            Action::PickWorkspace => self.open_palette(Purpose::Workspace),
            Action::CommandLine => self.open_palette(Purpose::Command),
            Action::Connect => self.ask_connect(None),
            Action::Find(kind) => self.open_finder(kind, String::new()),
            Action::Hunk(action) => self.editor_hunk(action),
            Action::Explore => self.toggle_explorer(),
            Action::Review(key) => match key {
                ReviewKey::Open => self.toggle_review(),
                ReviewKey::Keep => self.review_act(true, false, ReviewWhich::Here),
                ReviewKey::Reject => self.review_act(false, false, ReviewWhich::Here),
                ReviewKey::Comment => self.comment_here(),
            },
            Action::SendCtrlB => {
                if let Some(pane) = self.focused_pane() {
                    pane.send_bytes(vec![0x02]);
                }
            }
            Action::ToggleInlayHints => {
                self.run_set("inlayhints!");
                let on = self.options.inlayhints;
                self.message = Some(
                    if on {
                        "inlay hints on"
                    } else {
                        "inlay hints off"
                    }
                    .into(),
                );
            }
        }
    }

    fn open_palette(&mut self, purpose: Purpose) {
        // From a selection in an editor, a command starts with its lines.
        let text = match &purpose {
            Purpose::Command => self.editor_command_line(),
            _ => String::new(),
        };
        self.input = Some(Input {
            purpose,
            text,
            selected: 0,
        });
    }

    /// Whether the focused pane has a command half typed, which the next
    /// key finishes.
    pub(super) fn pane_pending(&self) -> bool {
        if let Some((editor, _)) = self.focused_editor() {
            return editor.pending();
        }
        if let Some(explorer) = self.focused_explorer() {
            return explorer.pending();
        }
        if let Some(board) = self.focused_board() {
            return board.pending();
        }
        if let Some(review) = self.focused_review() {
            return review.pending();
        }
        (self.focused_terminal())
            .and_then(|terminal| self.panes.get(&terminal))
            .is_some_and(crate::pane::Pane::pending)
    }

    /// The mode bindings are looked up in: the focused pane's.
    pub(super) fn key_mode(&self) -> KeyMode {
        if let Some((editor, _)) = self.focused_editor() {
            if editor.prompting() {
                return KeyMode::Prompt;
            }
            return match editor.mode {
                crate::pane::Mode::Insert => KeyMode::Insert,
                crate::pane::Mode::Normal => KeyMode::Normal,
                crate::pane::Mode::Visual(_) => KeyMode::Visual,
            };
        }
        if let Some(explorer) = self.focused_explorer() {
            return if explorer.prompting() {
                KeyMode::Prompt
            } else {
                KeyMode::Normal
            };
        }
        if let Some(board) = self.focused_board() {
            return if board.prompting() {
                KeyMode::Prompt
            } else {
                KeyMode::Normal
            };
        }
        let Some(pane) = self
            .focused_terminal()
            .and_then(|terminal| self.panes.get(&terminal))
        else {
            return KeyMode::Normal;
        };
        if pane.prompting() {
            return KeyMode::Prompt;
        }
        match pane.mode {
            crate::pane::Mode::Insert => KeyMode::Insert,
            crate::pane::Mode::Normal => KeyMode::Normal,
            crate::pane::Mode::Visual(_) => KeyMode::Visual,
        }
    }

    /// Focuses pane `number` of the tab shown, counting from 1 in the
    /// order they're laid out: left to right and top to bottom.
    fn show_pane_numbered(&mut self, number: usize) {
        let pane = self
            .placement()
            .and_then(|placement| placement.panes.get(number - 1).map(|&(id, _)| id));
        match pane {
            Some(pane) => self.show_pane(pane),
            None => self.message = Some(format!("there's no pane {number} in this tab")),
        }
    }

    /// Shows the next tab of the workspace shown, or the one before, round
    /// from the last to the first.
    fn next_tab(&mut self, forward: bool) {
        let Some(shown) = self.shown else { return };
        let Some(workspace) = self.workspaces.get(shown.workspace) else {
            return;
        };
        let tabs = &workspace.tabs;
        let Some(index) = tabs.iter().position(|tab| tab.id == shown.tab) else {
            return;
        };
        let next = if forward {
            (index + 1) % tabs.len()
        } else {
            (index + tabs.len() - 1) % tabs.len()
        };
        let pane = tabs[next].focused;
        self.show_pane(pane);
    }

    /// The workspaces whose names hold `text`, ignoring case: those that
    /// start with it first, then the rest, each in the order they're in.
    pub(super) fn workspace_choices(&self, text: &str) -> Vec<(WorkspaceId, String)> {
        let wanted = text.trim().to_lowercase();
        let (mut starting, others): (Vec<_>, Vec<_>) = self
            .workspaces
            .list
            .iter()
            .filter(|workspace| workspace.name.to_lowercase().contains(&wanted))
            .map(|workspace| (workspace.id, workspace.name.clone()))
            .partition(|(_, name)| name.to_lowercase().starts_with(&wanted));
        starting.extend(others);
        starting.truncate(CHOICES);
        starting
    }

    /// Enter in the workspace picker: the workspace picked, or if none
    /// matches what's typed, a new one by that name.
    pub(super) fn pick_workspace(&mut self, text: &str, selected: usize) {
        let choices = self.workspace_choices(text);
        if let Some(&(workspace, _)) = choices.get(selected).or(choices.first()) {
            self.show_workspace(workspace);
        } else if !text.trim().is_empty() {
            self.run(Command::Workspace(Some(text.trim().to_owned())));
        }
    }
}
