//! The menu of code actions `gra` shows by the cursor: what the language
//! server could do to the text there, such as fix what's wrong with it, or
//! rewrite it, in the order the daemon sends them, quick fixes first.
//!
//! It's a menu by the cursor, not the finder: the actions are about the
//! place the eye is on, there are seldom more than a handful, and each is
//! a title, with no file to preview or open beside. The first nine are
//! numbered, and a number picks one at once.

use clackatoo_protocol::CodeAction;

use super::panel::{self, Line, Placed, Run};
use crate::render::{Ink, mono_cells, mono_width};

/// The most actions the menu shows at once. More are scrolled to.
const MENU_HEIGHT: usize = 10;

/// The widest the menu goes, in cells: what's shown beside an action's
/// title is cut to fit.
const MENU_WIDTH: usize = 72;

/// The cells an action's number takes, before its title, with the gap
/// after.
const NUMBER_WIDTH: usize = 3;

/// The code actions answered to a query, to pick from.
#[derive(Debug)]
pub(crate) struct ActionMenu {
    /// The number of the query they answered, to pick one by.
    pub(crate) list: u64,
    actions: Vec<CodeAction>,
    /// The action picked, and the first in view.
    selected: usize,
    top: usize,
    /// Its lines in view, and where it goes, as last settled.
    lines: Vec<Line>,
    placed: Option<Placed>,
}

/// What a key did to the menu.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MenuKey {
    /// It moved what's picked.
    Moved,
    /// It picked the action at this index.
    Picked(u32),
    /// It closed the menu.
    Closed,
}

impl ActionMenu {
    /// The actions answered to the code actions query numbered `list`,
    /// the first picked. `None` if there are none.
    pub(crate) fn new(list: u64, actions: Vec<CodeAction>) -> Option<Self> {
        (!actions.is_empty()).then(|| Self {
            list,
            actions,
            selected: 0,
            top: 0,
            lines: Vec::new(),
            placed: None,
        })
    }

    /// The action at `index`.
    pub(crate) fn action(&self, index: u32) -> Option<&CodeAction> {
        self.actions.get(index as usize)
    }

    /// Moves the pick to the next action, or the one before if not
    /// `down`, going round the ends.
    pub(crate) fn pick(&mut self, down: bool) {
        let count = self.actions.len();
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + MENU_HEIGHT {
            self.top = self.selected + 1 - MENU_HEIGHT;
        }
    }

    /// The index of the action picked.
    pub(crate) fn picked(&self) -> u32 {
        u32::try_from(self.selected).unwrap_or(u32::MAX)
    }

    /// What `key` does to the menu. `None` for a number past its last
    /// action, which does nothing.
    pub(crate) fn key(&mut self, key: MenuStroke) -> Option<MenuKey> {
        match key {
            MenuStroke::Down => self.pick(true),
            MenuStroke::Up => self.pick(false),
            MenuStroke::Pick => return Some(MenuKey::Picked(self.picked())),
            MenuStroke::Number(n) => {
                let index = usize::from(n).checked_sub(1)?;
                if index >= self.actions.len() {
                    return None;
                }
                return Some(MenuKey::Picked(u32::try_from(index).ok()?));
            }
            MenuStroke::Close => return Some(MenuKey::Closed),
        }
        Some(MenuKey::Moved)
    }

    /// Works out the menu's lines and where it goes, in a pane `rows` by
    /// `cols`, the cursor being at `cursor`. Nothing's shown while the
    /// cursor's out of view.
    pub(crate) fn settle(&mut self, (rows, cols): (usize, usize), cursor: Option<(usize, usize)>) {
        self.placed = None;
        let Some(cursor) = cursor else {
            return;
        };
        let count = self.actions.len();
        let rows_wanted = count.min(MENU_HEIGHT);
        let width = (self.actions.iter())
            .map(|action| NUMBER_WIDTH + mono_width(&action.title) + extra_width(action))
            .max()
            .unwrap_or(0)
            .min(MENU_WIDTH);
        let Some(placed) = panel::place((rows, cols), cursor, (width, rows_wanted)) else {
            return;
        };
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + placed.height {
            self.top = self.selected + 1 - placed.height;
        }
        self.top = self.top.min(count.saturating_sub(placed.height));
        let in_view = self.top..(self.top + placed.height).min(count);
        self.lines = in_view
            .map(|index| self.line(index, placed.width))
            .collect();
        self.placed = Some(placed);
    }

    /// The line showing action `index`, `width` cells wide: its number, if
    /// it has one, its title, and what sort of action it is, dimmed, or
    /// why it can't be done now.
    fn line(&self, index: usize, width: usize) -> Line {
        let action = &self.actions[index];
        let number = if index < 9 {
            format!("{:<NUMBER_WIDTH$}", index + 1)
        } else {
            " ".repeat(NUMBER_WIDTH)
        };
        let title_ink = if action.disabled.is_some() {
            Ink::Muted
        } else {
            Ink::Plain
        };
        let extra = match &action.disabled {
            Some(why) => format!("  {why}"),
            None if action.kind.is_empty() => String::new(),
            None => format!("  {}", action.kind),
        };
        let mut used = NUMBER_WIDTH;
        let mut runs = vec![Run {
            text: number,
            ink: Ink::Muted,
            code: false,
        }];
        for (text, ink) in [(action.title.as_str(), title_ink), (&extra, Ink::Muted)] {
            let (shown, cells) = cut(text, width.saturating_sub(used));
            used += cells;
            if !shown.is_empty() {
                runs.push(Run {
                    text: shown,
                    ink,
                    code: false,
                });
            }
        }
        Line {
            runs,
            block: false,
            picked: index == self.selected,
        }
    }

    /// The menu as it's to be drawn, if there's room for it.
    pub(crate) fn view(&self) -> Option<super::PanelView<'_>> {
        let placed = self.placed?;
        Some(super::PanelView {
            placed,
            lines: &self.lines,
            first: self.top,
            total: self.actions.len(),
        })
    }
}

/// A key the menu takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuStroke {
    Down,
    Up,
    Pick,
    /// A digit, 1 to 9.
    Number(u8),
    Close,
}

/// How many cells what's shown after an action's title takes.
fn extra_width(action: &CodeAction) -> usize {
    match &action.disabled {
        Some(why) => 2 + mono_width(why),
        None if action.kind.is_empty() => 0,
        None => 2 + mono_width(&action.kind),
    }
}

/// As much of `text` as fits in `room` cells, ending in `…` if it's cut,
/// and how many cells that takes.
fn cut(text: &str, room: usize) -> (String, usize) {
    if mono_width(text) <= room {
        return (text.to_owned(), mono_width(text));
    }
    let mut shown = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cells = mono_cells(ch);
        if used + cells + 1 > room {
            break;
        }
        used += cells;
        shown.push(ch);
    }
    if room > 0 {
        shown.push('…');
        used += 1;
    }
    (shown, used)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(title: &str, kind: &str) -> CodeAction {
        CodeAction {
            title: title.into(),
            kind: kind.into(),
            preferred: false,
            disabled: None,
        }
    }

    fn texts(menu: &ActionMenu) -> Vec<String> {
        menu.lines.iter().map(Line::text).collect()
    }

    #[test]
    fn the_menu_numbers_its_actions_and_goes_round_its_ends() {
        assert!(ActionMenu::new(1, Vec::new()).is_none());
        let mut actions: Vec<CodeAction> = (0..12)
            .map(|n| action(&format!("action {n}"), "refactor"))
            .collect();
        actions[1].disabled = Some("not now".into());
        let mut menu = ActionMenu::new(1, actions).unwrap();
        menu.settle((30, 80), Some((0, 0)));
        let shown = texts(&menu);
        assert_eq!(shown.len(), MENU_HEIGHT);
        assert_eq!(shown[0], "1  action 0  refactor");
        assert_eq!(shown[1], "2  action 1  not now");
        assert!(menu.lines[0].picked);
        // Up from the first goes round to the last, scrolling to it,
        // which has no number.
        assert_eq!(menu.key(MenuStroke::Up), Some(MenuKey::Moved));
        menu.settle((30, 80), Some((0, 0)));
        assert_eq!(menu.picked(), 11);
        assert_eq!(texts(&menu).last().unwrap(), "   action 11  refactor");
        assert_eq!(menu.key(MenuStroke::Pick), Some(MenuKey::Picked(11)));
        assert_eq!(menu.key(MenuStroke::Number(3)), Some(MenuKey::Picked(2)));
        assert_eq!(menu.key(MenuStroke::Close), Some(MenuKey::Closed));
        // A number past the last is nothing.
        let mut short = ActionMenu::new(2, vec![action("only", "")]).unwrap();
        assert_eq!(short.key(MenuStroke::Number(2)), None);
        // Cut to the pane.
        short.actions[0].title = "a title much too long for it".into();
        short.settle((10, 14), Some((0, 0)));
        assert_eq!(texts(&short), ["1  a title …"]);
    }
}
