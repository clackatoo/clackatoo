//! Ctrl+B and a key: the window's own commands, in any mode, as in tmux.
//!
//! | Ctrl+B, then | Does |
//! |---|---|
//! | `1`–`9` | Tab 1 to 9 |
//! | `c` | A new tab |
//! | `]` `[` | The next tab, the one before |
//! | `,` | Renames the tab |
//! | `p` and `1`–`9` | Pane 1 to 9, numbered on screen meanwhile |
//! | `h` `j` `k` `l` | The pane left, below, above, right |
//! | `\|` `-` | Splits the pane, to the right or below |
//! | `a` | Opens a chat with an agent, to the right |
//! | `x` | Closes the pane |
//! | `w` | Picks a workspace, or makes one, in the palette |
//! | `o` `i` | Back and forward through the tab's panes (see [`super::jumps`]) |
//! | Ctrl+B | Sends Ctrl+B to the program |
//!
//! Esc, or any other key, cancels. While Ctrl+B waits, the bottom bar says
//! what it takes.

use clackatoo_protocol::{Side, WorkspaceId};
use winit::keyboard::{Key, NamedKey};

use super::{Desk, Input, Purpose};
use crate::command::{Command, Kind, Place};
use crate::input::KeyPress;
use crate::layout::Direction;

/// How many workspaces the picker lists.
const CHOICES: usize = 8;

/// What Ctrl+B is waiting for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prefix {
    /// The key after Ctrl+B.
    Waiting,
    /// A pane's number, after Ctrl+B p.
    Pane,
}

/// Whether `press` is Ctrl+B.
pub(super) fn is_ctrl_b(press: &KeyPress<'_>) -> bool {
    let modifiers = press.modifiers;
    modifiers.ctrl
        && !modifiers.alt
        && !modifiers.shift
        && matches!(press.key, Key::Character(text) if text.eq_ignore_ascii_case("b"))
}

impl Desk {
    /// Whether the panes are numbered on screen, while Ctrl+B p waits for
    /// one.
    pub(crate) fn numbering_panes(&self) -> bool {
        self.prefix == Some(Prefix::Pane)
    }

    /// Stops waiting after Ctrl+B, as when the mouse is clicked. Returns
    /// whether it was waiting.
    pub(crate) fn cancel_prefix(&mut self) -> bool {
        self.prefix.take().is_some()
    }

    /// What the bottom bar says while Ctrl+B waits.
    pub(super) fn prefix_note(&self) -> Option<&'static str> {
        Some(match self.prefix? {
            Prefix::Waiting => "tab 1-9 c [ ] ,  pane p hjkl | - x  o i  a agent  w workspace",
            Prefix::Pane => "pane number",
        })
    }

    /// Handles the key after Ctrl+B, or after Ctrl+B p. Returns whether the
    /// window must be redrawn.
    pub(super) fn prefix_key(&mut self, prefix: Prefix, press: &KeyPress<'_>) -> bool {
        let character = match press.key {
            Key::Character(text) if !press.modifiers.ctrl => text.chars().next(),
            _ => None,
        };
        let digit = character
            .and_then(|ch| ch.to_digit(10))
            .and_then(|digit| usize::try_from(digit).ok())
            .filter(|&digit| digit > 0);
        if prefix == Prefix::Pane {
            if let Some(number) = digit {
                self.show_pane_numbered(number);
            }
            return true;
        }
        if is_ctrl_b(press) {
            if let Some(pane) = self.focused_pane() {
                pane.send_bytes(vec![0x02]);
            }
            return true;
        }
        if *press.key == Key::Named(NamedKey::Escape) {
            return true;
        }
        let beside = |side, kind| Command::Open {
            place: Place::Beside(side),
            kind,
            daemon: None,
        };
        match (digit, character) {
            (Some(number), _) => self.run(Command::Tab(number)),
            (_, Some('c')) => self.new_tab(),
            (_, Some(bracket @ (']' | '['))) => self.next_tab(bracket == ']'),
            (_, Some(',')) => {
                if let Some(shown) = self.shown {
                    self.rename_tab(shown.tab);
                }
            }
            (_, Some('p')) => self.prefix = Some(Prefix::Pane),
            (_, Some(letter @ ('h' | 'j' | 'k' | 'l'))) => self.focus_towards(match letter {
                'h' => Direction::Left,
                'j' => Direction::Down,
                'k' => Direction::Up,
                _ => Direction::Right,
            }),
            (_, Some('|')) => self.run(beside(Side::Right, Kind::Terminal)),
            (_, Some('-')) => self.run(beside(Side::Below, Kind::Terminal)),
            (_, Some('a')) => self.run(beside(Side::Right, Kind::Agent)),
            (_, Some('x')) => self.run(Command::Close),
            (_, Some('w')) => {
                self.input = Some(Input {
                    purpose: Purpose::Workspace,
                    text: String::new(),
                    selected: 0,
                });
            }
            (_, Some(key @ ('o' | 'i'))) => self.jump(key == 'o', true),
            _ => {}
        }
        true
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
