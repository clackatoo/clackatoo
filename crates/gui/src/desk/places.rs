//! Restores the same locations for jump history and named marks.
use super::{Desk, jumps::Jump, view::PaneView};
use crate::vim::place::Place;
use clackatoo_protocol::PaneId;
impl Desk {
    pub(super) fn other_place(&self, pane: PaneId) -> Option<Place> {
        Some(match self.pane_view(pane)? {
            PaneView::Terminal(terminal, view) => Place::Line {
                pane,
                terminal,
                at: view.place()?,
            },
            PaneView::Chat(terminal, view) => Place::Chat {
                pane,
                terminal,
                at: view.view().place()?,
            },
            PaneView::Explorer(_, view) => Place::Item {
                pane,
                at: view.saved_place(None)?,
            },
            PaneView::Board(_, view) => Place::Item {
                pane,
                at: view.place(None)?,
            },
            PaneView::Review(_, view) => Place::Item {
                pane,
                at: view.saved_place(None)?,
            },
            _ => return None,
        })
    }
    pub(super) fn valid_place(&self, jump: &Jump) -> bool {
        let Some(view) = self.pane_view(jump.pane) else {
            return false;
        };
        match (&jump.place, view) {
            (Place::File(file), PaneView::Editor(..)) => {
                file.machine == self.pane_machine(jump.pane)
            }
            (Place::Folder { machine, .. }, _) => *machine == self.pane_machine(jump.pane),
            (Place::Line { terminal, at, .. }, PaneView::Terminal(id, view)) => {
                *terminal == id && view.valid_place(at)
            }
            (Place::Chat { terminal, at, .. }, PaneView::Chat(id, view)) => {
                *terminal == id && view.view().valid_place(at)
            }
            (Place::Item { at, .. }, PaneView::Board(_, view)) => view.valid_place(at).is_some(),
            (Place::Item { at, .. }, PaneView::Review(_, view)) => view.valid_place(at).is_some(),
            (Place::Item { at, .. }, PaneView::Explorer(_, view)) => view.valid_place(at),
            (Place::Pane(_), _) => true,
            _ => false,
        }
    }
    pub(super) fn show_place(&mut self, jump: &Jump, exact: bool) -> bool {
        if !self.valid_place(jump) {
            return false;
        }
        self.move_focus(jump.pane, false);
        match &jump.place {
            Place::File(file) => {
                let mut file = file.clone();
                if !exact {
                    file.col = 0;
                }
                return self.show_file_place(jump.pane, &file);
            }
            Place::Folder { at, .. } => self.show_folder_place(jump.pane, at),
            Place::Line { terminal, at, .. } => {
                return self
                    .panes
                    .get_mut(terminal)
                    .is_some_and(|view| view.show_place(at, exact));
            }
            Place::Chat { terminal, at, .. } => {
                return self
                    .chats
                    .get_mut(terminal)
                    .is_some_and(|view| view.view_mut().show_place(*at, exact));
            }
            Place::Item { at, .. } => {
                if let Some(view) = self.boards.panes.get_mut(&jump.pane) {
                    return view.show_place(at);
                }
                if let Some(view) = self.reviews.panes.get_mut(&jump.pane) {
                    return view.show_place(at);
                }
                if let Some(view) = self.explorers.panes.get_mut(&jump.pane) {
                    return view.show_place(at);
                }
                return false;
            }
            Place::Pane(_) => {}
        }
        true
    }
    pub(super) fn set_place_mark(&mut self, mark: char) {
        let Some(shown) = self.shown else { return };
        let Some(place) = self.place_of(shown.pane, None) else {
            return;
        };
        if matches!(place.place, Place::Pane(_)) {
            self.refuse("there is no position to mark here".into());
            return;
        }
        let mark = if mark == '`' { '\'' } else { mark };
        if mark == '\'' {
            self.record_place(place);
        } else {
            self.shared_mut().marks.set_place(mark, place.place);
        }
    }
    pub(super) fn go_to_place_mark(&mut self, mark: char, exact: bool) {
        let Some(shown) = self.shown else { return };
        let Some(here) = self.place_of(shown.pane, None) else {
            return;
        };
        let mark = if mark == '`' { '\'' } else { mark };
        let Some(place) = self.shared().marks.get_place(mark, &here.place).cloned() else {
            self.refuse(format!("mark {mark} isn't set"));
            return;
        };
        let pane = match &place {
            Place::File(file) => {
                let matching = |pane: &&PaneId| self.pane_machine(**pane) == file.machine;
                let exact = self
                    .editors
                    .panes
                    .keys()
                    .filter(matching)
                    .find(|pane| self.file_in(**pane) == Some(file.path.as_str()));
                exact
                    .or_else(|| self.editors.panes.keys().find(matching))
                    .copied()
            }
            other => other.pane(),
        };
        let Some(pane) = pane else {
            self.refuse(format!("mark {mark}'s pane is closed"));
            return;
        };
        if !self.show_place(&Jump { pane, place }, exact) {
            self.refuse(format!("mark {mark}'s position is no longer available"));
            return;
        }
        self.record_place(here);
    }
    /// Collect before another key can run, including after an asynchronous
    /// search settles, so a macro sees its new jump history immediately.
    pub(super) fn collect_place_jumps(&mut self) {
        let Some(pane) = self.shown.map(|shown| shown.pane) else {
            return;
        };
        let place = if let Some(terminal) = self
            .workspaces
            .find_pane(pane)
            .and_then(|(_, _, p)| p.content.terminal())
        {
            if let Some(chat) = self.chats.get_mut(&terminal) {
                chat.view_mut()
                    .take_left_place()
                    .map(|at| Place::Chat { pane, terminal, at })
            } else {
                self.panes
                    .get_mut(&terminal)
                    .and_then(crate::pane::Pane::take_left_place)
                    .map(|at| Place::Line { pane, terminal, at })
            }
        } else if let Some(view) = self.boards.panes.get_mut(&pane) {
            view.take_left_row()
                .and_then(|row| view.place(Some(row)))
                .map(|at| Place::Item { pane, at })
        } else if let Some(view) = self.reviews.panes.get_mut(&pane) {
            view.take_left_row()
                .and_then(|row| view.saved_place(Some(row)))
                .map(|at| Place::Item { pane, at })
        } else if let Some(view) = self.explorers.panes.get_mut(&pane) {
            view.take_left_row()
                .and_then(|row| view.saved_place(Some(row)))
                .map(|at| Place::Item { pane, at })
        } else {
            None
        };
        if let Some(place) = place {
            self.record_place(Jump { pane, place });
        }
    }
}
