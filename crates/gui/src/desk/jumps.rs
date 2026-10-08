//! Where the focus has been, to go back and forth through, like Vim's jump
//! list or a browser's history: Ctrl+O and Ctrl+I in normal mode within
//! the tab shown, and Ctrl+Shift+O and Ctrl+Shift+I across every tab and
//! workspace, in any mode.
//!
//! Each move of the focus is recorded. Going back and then moving somewhere
//! new forgets the way forward. Places that no longer exist are skipped.
//! The list is the window's own: each window has its own history.

use super::Shown;

/// How many places are remembered.
const MAX: usize = 100;

#[derive(Debug, Default)]
pub(super) struct Jumps {
    /// Oldest first.
    list: Vec<Shown>,
    /// Where in the list the focus is.
    at: usize,
}

impl Jumps {
    /// Records the focus moving from `from` to `to`.
    pub(super) fn moved(&mut self, from: Option<Shown>, to: Shown) {
        self.list.truncate(self.at + 1);
        if let Some(from) = from
            && self.list.last() != Some(&from)
        {
            self.list.push(from);
        }
        if self.list.last() != Some(&to) {
            self.list.push(to);
        }
        if self.list.len() > MAX {
            self.list.drain(..self.list.len() - MAX);
        }
        self.at = self.list.len() - 1;
    }

    /// The nearest place back (or forward) that `wanted` accepts, and
    /// where it is in the list.
    pub(super) fn step(
        &self,
        back: bool,
        wanted: impl Fn(&Shown) -> bool,
    ) -> Option<(usize, Shown)> {
        let found = if back {
            self.list[..self.at.min(self.list.len())]
                .iter()
                .rposition(&wanted)
        } else {
            self.list
                .iter()
                .skip(self.at + 1)
                .position(wanted)
                .map(|offset| self.at + 1 + offset)
        };
        found.map(|index| (index, self.list[index]))
    }

    /// Goes to the place at `index`, from [`step`](Self::step).
    pub(super) fn go(&mut self, index: usize) {
        self.at = index;
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{PaneId, TabId, WorkspaceId};

    use super::*;

    fn at(tab: u32, pane: u32) -> Shown {
        Shown {
            workspace: WorkspaceId(1),
            tab: TabId(tab),
            pane: PaneId(pane),
        }
    }

    #[test]
    fn back_forward_and_a_new_move_forgets_the_way_forward() {
        let mut jumps = Jumps::default();
        jumps.moved(Some(at(1, 1)), at(1, 2));
        jumps.moved(Some(at(1, 2)), at(2, 3));
        let any = |_: &Shown| true;
        let (index, back) = jumps.step(true, any).unwrap();
        assert_eq!(back, at(1, 2));
        jumps.go(index);
        // Within tab 1 only: back past tab 2's pane to pane 1.
        let (index, back) = jumps.step(true, |shown| shown.tab == TabId(1)).unwrap();
        assert_eq!(back, at(1, 1));
        jumps.go(index);
        assert_eq!(jumps.step(true, any), None);
        assert_eq!(
            jumps.step(false, any).map(|(_, shown)| shown),
            Some(at(1, 2))
        );
        // Somewhere new from here: pane 2 and tab 2's are forgotten.
        jumps.moved(Some(at(1, 1)), at(1, 4));
        assert_eq!(jumps.step(false, any), None);
        assert_eq!(
            jumps.step(true, any).map(|(_, shown)| shown),
            Some(at(1, 1))
        );
    }
}
