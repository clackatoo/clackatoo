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
//! | `b` `t` | Sends the tab to the background, as `:bg` does (see [`super::background`]) |
//! | `b` `p` | Sends the focused pane to the background, in a tab of its own, as `:bgpane` does |
//! | `b` `l` | Lists the background tabs, as `:Background` does |
//! | Ctrl+B | Sends Ctrl+B to the program |
//!
//! Esc, or any other key, cancels. While Ctrl+B waits, the bottom bar
//! marks it `^B` by the mode, and a panel above the bar lists the keys it
//! takes and what each does, as which-key.nvim does. The panel is made from
//! the bindings themselves: each says what it does in its label, so a key
//! bound again says so, and keys of the config's own are listed too, by
//! their `desc`. A key that starts several says their group's name, as
//! `<Space>p` says "project", which the config can name too
//! (`client.keys.group`). The
//! panes are numbered meanwhile if there are two or more, and always after
//! `p`. Other keys that start bindings, as Space does, list theirs only
//! once `whichkeydelay` has passed, so keys typed quickly never flash the
//! panel; the bar marks them at once.
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

use std::time::{Duration, Instant};

use clackatoo_protocol::{Side, TerminalId, WorkspaceId};

use super::{Desk, Input, Purpose};
use crate::command::{Command, Kind, Place, ReviewWhich};
use crate::editor::HunkAction;
use crate::finder;
use crate::keymap::{KeyMode, Keymap, WhichKeys, parse_keys, parse_modes};
use crate::layout::Direction;
use crate::vim::{KeyState, Mode};

/// How many choices the palette lists at once: workspaces, or what Tab
/// could complete a word with.
pub(super) const CHOICES: usize = 8;

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
    /// Shows the focused editor's Markdown rendered, or its text again, as
    /// `:Markdown` does.
    ToggleMarkdown,
    /// Background tabs, as `:bg`, `:bgpane` and `:Background` do.
    Background(crate::command::BackgroundCommand),
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
#[allow(clippy::too_many_lines, reason = "a binding or two a line")]
fn defaults() -> Keymap<Binding> {
    let mut map = Keymap::default();
    let mut bind = |modes: &str, keys: &str, action, label: Option<&str>| {
        let modes = parse_modes(modes).expect("the defaults' modes are valid");
        let keys = parse_keys(keys).expect("the defaults' keys are valid");
        map.bind(&modes, &keys, Binding::Window(action), label);
    };
    for number in 1..=9 {
        let keys = format!("<C-b>{number}");
        bind("niv", &keys, Action::Tab(number), Some("go to tab"));
    }
    bind("niv", "<C-b>c", Action::NewTab, Some("new tab"));
    bind("niv", "<C-b>[", Action::NextTab(false), Some("tab before"));
    bind("niv", "<C-b>]", Action::NextTab(true), Some("next tab"));
    bind("niv", "<C-b>,", Action::RenameTab, Some("rename tab"));
    for number in 1..=9 {
        let keys = format!("<C-b>p{number}");
        bind("niv", &keys, Action::Pane(number), Some("pick a pane"));
    }
    for (letter, direction, label) in [
        ('h', Direction::Left, "pane left"),
        ('j', Direction::Down, "pane below"),
        ('k', Direction::Up, "pane above"),
        ('l', Direction::Right, "pane right"),
    ] {
        let keys = format!("<C-b>{letter}");
        bind("niv", &keys, Action::Focus(direction), Some(label));
    }
    let terminal = |side| Action::Split(side, Kind::Terminal);
    bind("niv", "<C-b>|", terminal(Side::Right), Some("split right"));
    bind("niv", "<C-b>-", terminal(Side::Below), Some("split below"));
    bind("niv", "<C-b>x", Action::Close, Some("close pane"));
    let jump = |back| Action::Jump {
        back,
        within_tab: true,
    };
    bind("niv", "<C-b>o", jump(true), Some("jump back"));
    bind("niv", "<C-b>i", jump(false), Some("jump forward"));
    let agent = Action::Split(Side::Right, Kind::Agent);
    bind("niv", "<C-b>a", agent, Some("agent chat"));
    bind("niv", "<C-b>w", Action::PickWorkspace, Some("workspaces"));
    bind("niv", "<C-b>r", Action::Connect, Some("connect"));
    for (keys, command, label) in BACKGROUND_KEYS {
        bind("niv", keys, Action::Background(command), Some(label));
    }
    bind("niv", "<C-b><C-b>", Action::SendCtrlB, Some("send Ctrl+B"));

    bind("nv", ":", Action::CommandLine, None);
    for (keys, kind, label) in FINDER_KEYS {
        bind("nv", keys, Action::Find(kind), Some(label));
    }
    // As gitsigns.nvim suggests them.
    let hunk = |action| Action::Hunk(action);
    bind(
        "nv",
        "<Space>hp",
        hunk(HunkAction::Preview),
        Some("show hunk"),
    );
    bind(
        "nv",
        "<Space>hr",
        hunk(HunkAction::Reset),
        Some("reset hunk"),
    );
    bind(
        "nv",
        "<Space>hs",
        hunk(HunkAction::Stage),
        Some("stage hunk"),
    );
    // As nvim-tree's and neo-tree's are often bound.
    bind("nv", "<Space>e", Action::Explore, Some("explorer"));
    // As kickstart.nvim's `<leader>th`.
    bind(
        "nv",
        "<Space>th",
        Action::ToggleInlayHints,
        Some("inlay hints"),
    );
    // Beside it: `m` for Markdown.
    bind(
        "nv",
        "<Space>tm",
        Action::ToggleMarkdown,
        Some("rendered Markdown"),
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
    name_groups(&mut map);
    map
}

/// The keys for background tabs, after Ctrl+B's `b`: the `t`ab, the
/// `p`ane, or the `l`ist.
const BACKGROUND_KEYS: [(&str, crate::command::BackgroundCommand, &str); 3] = [
    (
        "<C-b>bt",
        crate::command::BackgroundCommand::Tab,
        "tab to background",
    ),
    (
        "<C-b>bp",
        crate::command::BackgroundCommand::Pane,
        "pane to background",
    ),
    (
        "<C-b>bl",
        crate::command::BackgroundCommand::List,
        "background tabs",
    ),
];

/// Names the keys after Space that start several in `map`, as
/// which-key.nvim's groups are named, and Ctrl+B's `b`.
fn name_groups(map: &mut Keymap<Binding>) {
    let modes = parse_modes("niv").expect("the defaults' modes are valid");
    let keys = parse_keys("<C-b>b").expect("the defaults' keys are valid");
    map.name_group(&modes, &keys, Some("background"));
    let modes = parse_modes("nv").expect("the defaults' modes are valid");
    for (keys, name) in [
        ("<Space>p", "project"),
        ("<Space>h", "git hunks"),
        ("<Space>g", "agent changes"),
        ("<Space>t", "toggles"),
    ] {
        let keys = parse_keys(keys).expect("the defaults' keys are valid");
        map.name_group(&modes, &keys, Some(name));
    }
}

/// The finder's keys, as jack's Telescope keys are: `p` for the project,
/// as in `<Space>pf`, and `b` for the buffer, as in `<Space>bs`, its
/// symbols; and what each finds.
const FINDER_KEYS: [(&str, finder::Kind, &str); 6] = [
    ("<Space>pf", finder::Kind::Files, "find files"),
    ("<Space>pgf", finder::Kind::Grep, "grep files"),
    ("<Space>?", finder::Kind::Buffers, "open files"),
    ("<Space>ds", finder::Kind::Diagnostics, "problems"),
    ("<Space>bs", finder::Kind::Symbols, "file symbols"),
    ("<Space>ps", finder::Kind::ProjectSymbols, "project symbols"),
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
    ("<Space>ga", ReviewKey::Keep, "keep change"),
    ("<Space>gx", ReviewKey::Reject, "reject change"),
    ("<Space>c", ReviewKey::Comment, "comment"),
];

/// Vim's letters for the four ways.
const DIRECTIONS: [(char, Direction); 4] = [
    ('h', Direction::Left),
    ('j', Direction::Down),
    ('k', Direction::Up),
    ('l', Direction::Right),
];

/// The kinds of pane whose keys the desk asks about (see [`KeyState`]).
#[derive(Clone, Copy)]
enum WithKeys {
    Editor,
    Explorer,
    Board,
    Review,
    Terminal(TerminalId),
}

impl Desk {
    /// Whether the panes are numbered on screen: while keys wait for a
    /// pane's number, as after Ctrl+B p, or for keys that lead to one, as
    /// after Ctrl+B, if there are two or more panes to tell apart.
    pub(crate) fn numbering_panes(&self) -> bool {
        let pane = |binding: &Binding| matches!(binding, Binding::Window(Action::Pane(_)));
        if self.keymap.waiting().is_empty() {
            return false;
        }
        let next = self.keymap.next();
        let picks = |next: &crate::keymap::Next<'_, Binding>| {
            next.bound.is_some_and(|(binding, _)| pane(binding))
        };
        if !next.is_empty() && next.iter().all(picks) {
            return true;
        }
        let leads_to_one =
            (next.iter()).any(|next| next.further.iter().any(|(_, binding, _)| pane(binding)));
        leads_to_one
            && self
                .shown_tab()
                .is_some_and(|tab| tab.layout.panes().len() >= 2)
    }

    /// When the panel of keys is due, while keys wait for the rest of a
    /// binding: at once after a key held with Ctrl or Alt, as the window's
    /// own Ctrl+B, and otherwise once `whichkeydelay` has passed, so keys
    /// typed quickly, as `<Space>pf`, never flash it.
    pub(super) fn which_keys_due(&self) -> Option<Instant> {
        let since = self.keymap.waiting_since()?;
        let first = self.keymap.waiting().first()?;
        let delay = if first.ctrl || first.alt {
            0
        } else {
            self.options.whichkeydelay
        };
        Some(since + Duration::from_millis(u64::from(delay)))
    }

    /// The keys waiting for the rest of a binding, written short, as the
    /// bottom bar marks them by the mode, at once: `^B`.
    pub(crate) fn keys_waiting(&self) -> Option<String> {
        self.keymap.waiting_short()
    }

    /// What the panel of keys shows while keys wait for the rest of a
    /// binding, as after Ctrl+B or Space, once it's due (see
    /// [`Desk::which_keys_due`]).
    pub(crate) fn which_keys(&self) -> Option<WhichKeys> {
        if self.which_keys_due()? > Instant::now() {
            return None;
        }
        self.keymap.which_keys(|binding| match binding {
            Binding::Keys(keys) => format!("types {}", crate::keymap::display(keys)),
            // The window's own are all labelled, so it's the config's.
            Binding::Lua(_) | Binding::Window(_) => "from your config".to_owned(),
        })
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
                agent: None,
            }),
            Action::Close => self.run(Command::Close),
            // `3<C-o>` goes back three places, as in Vim.
            Action::Jump { back, within_tab } => {
                self.jump(back, within_tab, self.bound_count.unwrap_or(1));
            }
            Action::PickWorkspace => self.open_palette(Purpose::Workspace),
            Action::CommandLine => self.open_palette(Purpose::Command),
            Action::Connect => self.ask_connect(None, None),
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
                self.say(
                    if on {
                        "inlay hints on"
                    } else {
                        "inlay hints off"
                    }
                    .into(),
                );
            }
            Action::ToggleMarkdown => self.run(Command::Markdown),
            Action::Background(command) => self.run(Command::Background(command)),
        }
    }

    fn open_palette(&mut self, purpose: Purpose) {
        // From a selection in an editor, a command starts with its lines,
        // count or not; after a count, with as many from the cursor's, as
        // Vim's `5:` starts `:.,.+4`.
        let visual = (self.focused_editor())
            .is_some_and(|(editor, _)| matches!(editor.mode, Mode::Visual(_)));
        let text = match (&purpose, self.bound_count.take()) {
            (Purpose::Command, Some(count)) if self.focused_editor().is_some() && !visual => {
                match count {
                    1 => ".".to_owned(),
                    count => format!(".,.+{}", count - 1),
                }
            }
            (Purpose::Command, _) => self.editor_command_line(),
            _ => String::new(),
        };
        self.input = Some(Input::new(purpose, text));
    }

    /// Which kind of pane with keys the focused one is.
    fn focused_with_keys(&self) -> Option<WithKeys> {
        Some(if self.focused_editor().is_some() {
            WithKeys::Editor
        } else if self.focused_explorer().is_some() {
            WithKeys::Explorer
        } else if self.focused_board().is_some() {
            WithKeys::Board
        } else if self.focused_review().is_some() {
            WithKeys::Review
        } else {
            WithKeys::Terminal(self.focused_terminal()?)
        })
    }

    /// The focused pane's keys, if it reads any: an editor's, a
    /// terminal's or a chat's, an explorer's, a board's or a review's.
    pub(super) fn focused_key_state(&self) -> Option<&dyn KeyState> {
        Some(match self.focused_with_keys()? {
            WithKeys::Editor => self.focused_editor()?.0,
            WithKeys::Explorer => self.focused_explorer()?,
            WithKeys::Board => self.focused_board()?,
            WithKeys::Review => self.focused_review()?,
            WithKeys::Terminal(terminal) => self.panes.get(&terminal)?,
        })
    }

    /// The same, to change.
    fn focused_key_state_mut(&mut self) -> Option<&mut dyn KeyState> {
        Some(match self.focused_with_keys()? {
            WithKeys::Editor => self.focused_editor_mut()?,
            WithKeys::Explorer => self.focused_explorer_mut()?,
            WithKeys::Board => self.focused_board_mut()?,
            WithKeys::Review => self.focused_review_mut()?,
            WithKeys::Terminal(terminal) => self.panes.get_mut(&terminal)?,
        })
    }

    /// Whether the focused pane has a command half typed, which the next
    /// key finishes.
    pub(super) fn pane_pending(&self) -> bool {
        self.focused_key_state().is_some_and(KeyState::pending)
    }

    /// The count typed in the focused pane, if that's all that's waiting
    /// there: the next key is looked up in the bindings, as `5:` opens the
    /// command line.
    pub(super) fn pane_counting(&self) -> Option<usize> {
        self.focused_key_state()?.counting()
    }

    /// Takes a count typed in the focused pane, as one of the bindings
    /// took the key after it, for the binding to use.
    pub(super) fn take_pane_count(&mut self) -> Option<usize> {
        let count = self.pane_counting()?;
        self.focused_key_state_mut()?.forget_count();
        Some(count)
    }

    /// The mode bindings are looked up in: the focused pane's.
    pub(super) fn key_mode(&self) -> KeyMode {
        let Some(keys) = self.focused_key_state() else {
            return KeyMode::Normal;
        };
        if keys.prompting() {
            return KeyMode::Prompt;
        }
        match keys.mode() {
            Mode::Insert => KeyMode::Insert,
            Mode::Normal => KeyMode::Normal,
            Mode::Visual(_) => KeyMode::Visual,
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
            None => self.say(format!("there's no pane {number} in this tab")),
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
