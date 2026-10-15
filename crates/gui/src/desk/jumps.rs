//! Where you've been, to go back and forth through, like Vim's jump list:
//! Ctrl+O and Ctrl+I in normal mode within the tab shown, and Ctrl+Shift+O
//! and Ctrl+Shift+I across every tab and workspace, in any mode.
//!
//! A jump records the place it left: the pane, and in an editor, the file
//! and where the cursor was in it, or the folder it showed and the entry
//! the cursor was on. Moving the focus to another pane is a jump, and so
//! are an editor's long moves, as Vim's are: `gg`, `G`, searches, `]d` and
//! `[d`, `gd` and its kin, and `:e`, and going into a folder, up from one,
//! or from a folder's listing to a file. Going back from the
//! newest end first records where you are, so Ctrl+I comes back to it.
//! Going back and then jumping somewhere new forgets the way forward. A
//! jump from the line the newest place is on replaces it, so going back
//! never stops twice on one line. Places in panes that no longer exist are
//! skipped.
//!
//! A place in a file moves with the text while the window has the file:
//! edits made above it, by anyone, take it down with them. Once no pane of
//! the window shows the file, the window hears of no edits to it, and the
//! place keeps its line and column, which are kept within the text when
//! the file comes back.
//!
//! The list is the window's own: each window has its own history.

use clackatoo_protocol::PaneId;
use clackatoo_text::{Edit, Rope};

use super::Desk;
pub(super) use crate::editor::FilePlace;

/// How many places are remembered.
const MAX: usize = 100;

/// A place jumped from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Jump {
    pub(super) pane: PaneId,
    /// Where the cursor was, if the pane was editing a file.
    pub(super) file: Option<FilePlace>,
    /// Where the cursor was, if the pane was showing a folder's listing.
    pub(super) folder: Option<FolderPlace>,
}

/// Where the cursor was in a folder's listing: the folder, and the path of
/// the entry on the cursor's line, if it was on one, or `..` if it was on
/// the way up. The entry is kept,
/// not the line, as a folder listed again may have more lines, or fewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FolderPlace {
    pub(super) dir: String,
    pub(super) entry: Option<String>,
}

impl FolderPlace {
    /// The file or directory at `from` has moved to `to`: the folder, or
    /// the entry, goes with it if it's in it.
    fn moved(&mut self, from: &str, to: &str) {
        let moved = |path: &mut String| {
            let rest =
                (path.strip_prefix(from)).filter(|rest| rest.is_empty() || rest.starts_with('/'));
            if let Some(rest) = rest {
                *path = format!("{to}{rest}");
            }
        };
        moved(&mut self.dir);
        if let Some(entry) = &mut self.entry {
            moved(entry);
        }
    }
}

impl Jump {
    /// Whether `self` and `other` are on the same line of the same pane,
    /// or the same entry of the same folder, or are the same pane, with
    /// neither.
    pub(super) fn same(&self, other: &Self) -> bool {
        let files = match (&self.file, &other.file) {
            (None, None) => true,
            (Some(mine), Some(theirs)) => mine.path == theirs.path && mine.line == theirs.line,
            _ => false,
        };
        self.pane == other.pane && files && self.folder == other.folder
    }
}

#[derive(Debug, Default)]
pub(super) struct Jumps {
    /// Oldest first.
    list: Vec<Jump>,
    /// Where in the list you are: the place gone back or forward to, or
    /// the list's length when at the newest end, somewhere not in it.
    at: usize,
}

impl Jumps {
    /// Records a jump from `from`. A place gone back to stays, as the way
    /// back from `from`, unless `from` is on its line.
    pub(super) fn left(&mut self, from: Jump) {
        self.list.truncate(self.at.saturating_add(1));
        self.push(from);
        if self.list.len() > MAX {
            self.list.drain(..self.list.len() - MAX);
        }
        self.at = self.list.len();
    }

    /// Adds `place` at the newest end, in place of a place on its line.
    fn push(&mut self, place: Jump) {
        if self.list.last().is_some_and(|last| last.same(&place)) {
            self.list.pop();
        }
        self.list.push(place);
    }

    /// Goes to the `count`th place back, or forward, that `wanted`
    /// accepts, from `here`, where you are now, passing places on
    /// `here`'s line. With fewer than `count` that way, it goes nowhere,
    /// as Vim's `3<C-o>` doesn't. Going back from the newest end first
    /// records `here`, to come forward to again. That can leave one place
    /// more than [`MAX`] until the next jump, so that it never loses the
    /// place it goes to.
    pub(super) fn step(
        &mut self,
        back: bool,
        count: usize,
        here: Jump,
        wanted: impl Fn(&Jump) -> bool,
    ) -> Option<Jump> {
        let wanted = |place: &&Jump| !place.same(&here) && wanted(place);
        let nth = count.max(1) - 1;
        let found = if back {
            let end = self.at.min(self.list.len());
            let (found, _) = (self.list[..end].iter().enumerate())
                .rev()
                .filter(|(_, place)| wanted(place))
                .nth(nth)?;
            if self.at >= self.list.len() {
                // Only a place on `here`'s line goes, and `found` isn't one.
                self.push(here);
                self.at = self.list.len() - 1;
            }
            Some(found)
        } else {
            (self.list.iter().enumerate())
                .skip(self.at + 1)
                .filter(|(_, place)| wanted(place))
                .nth(nth)
                .map(|(index, _)| index)
        }?;
        self.at = found;
        Some(self.list[found].clone())
    }

    /// Moves the places in the file at `path` through `edits`, made to it
    /// in turn, which leave its text as `text`. Text put in where a place
    /// is goes before it, so a line opened above a place's line takes the
    /// place down with its line.
    pub(super) fn edited(
        &mut self,
        path: &str,
        edits: &[impl std::borrow::Borrow<Edit>],
        text: &Rope,
    ) {
        if edits.is_empty() {
            return;
        }
        for place in self.places_in(path) {
            place.edited(edits, text);
        }
    }

    /// The text of the file at `path` is new, as when it's opened again:
    /// its places go to their lines and columns in it.
    pub(super) fn replaced(&mut self, path: &str, text: &Rope) {
        for place in self.places_in(path) {
            place.find_byte(text);
        }
    }

    /// The file or directory at `from` has moved to `to`: places in it,
    /// or anything in it, go with it.
    pub(super) fn moved(&mut self, from: &str, to: &str) {
        for jump in &mut self.list {
            if let Some(place) = &mut jump.file {
                place.moved(from, to);
            }
            if let Some(place) = &mut jump.folder {
                place.moved(from, to);
            }
        }
    }

    fn places_in<'a>(&'a mut self, path: &'a str) -> impl Iterator<Item = &'a mut FilePlace> {
        (self.list.iter_mut())
            .filter_map(|jump| jump.file.as_mut())
            .filter(move |place| place.path == path)
    }
}

impl Desk {
    /// Goes back through the places you've been, or forward again: within
    /// the tab shown, or anywhere.
    pub(super) fn jump(&mut self, back: bool, within_tab: bool, count: usize) {
        let Some(shown) = self.shown else { return };
        let Some(here) = self.place_of(shown.pane, None) else {
            return;
        };
        let workspaces = &self.workspaces;
        let wanted = |place: &Jump| {
            (workspaces.find_pane(place.pane))
                .is_some_and(|(_, tab, _)| !within_tab || tab.id == shown.tab)
        };
        let Some(place) = self.jumps.step(back, count, here, wanted) else {
            return;
        };
        self.move_focus(place.pane, false);
        if let Some(file) = &place.file {
            self.show_file_place(place.pane, file);
        }
        if let Some(folder) = &place.folder {
            self.show_folder_place(place.pane, folder);
        }
    }

    /// Records a jump from where `pane` is: in an editor, from where its
    /// cursor is, or from `byte` of its file if given, which is the file's
    /// `'` mark then.
    pub(super) fn record_jump(&mut self, pane: PaneId, byte: Option<usize>) {
        if let Some(place) = self.place_of(pane, byte) {
            self.record_place(place);
        }
    }

    /// Records a jump from `place`, taken before the move it was left by,
    /// once that's seen to go ahead.
    pub(super) fn record_place(&mut self, place: Jump) {
        if let Some(file) = &place.file {
            self.jumped_from(file);
        }
        self.jumps.left(place);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(pane: u32) -> Jump {
        Jump {
            pane: PaneId(pane),
            file: None,
            folder: None,
        }
    }

    fn pane_of(jump: Option<Jump>) -> Option<u32> {
        jump.map(|jump| jump.pane.0)
    }

    #[test]
    fn a_place_gone_back_to_stays_when_you_jump_from_elsewhere() {
        let mut jumps = Jumps::default();
        let any = |_: &Jump| true;
        jumps.left(at(1));
        jumps.left(at(2));
        assert_eq!(pane_of(jumps.step(true, 1, at(3), any)), Some(2));
        // Off pane 2 to pane 4, and a jump from there.
        jumps.left(at(4));
        assert_eq!(pane_of(jumps.step(true, 1, at(5), any)), Some(4));
        assert_eq!(pane_of(jumps.step(true, 1, at(4), any)), Some(2));
        assert_eq!(pane_of(jumps.step(true, 1, at(2), any)), Some(1));
    }

    #[test]
    fn going_back_from_a_full_list_keeps_the_place_it_goes_to() {
        let mut jumps = Jumps::default();
        jumps.left(at(1));
        for index in 1..MAX {
            jumps.left(at(2 + u32::from(index % 2 == 0)));
        }
        assert_eq!(jumps.list.len(), MAX);
        let only_1 = |jump: &Jump| jump.pane == PaneId(1);
        assert_eq!(pane_of(jumps.step(true, 1, at(4), only_1)), Some(1));
        let any = |_: &Jump| true;
        assert_eq!(pane_of(jumps.step(false, 1, at(1), any)), Some(2));
        // A jump from the newest place keeps the list to its length.
        for _ in 0..MAX {
            jumps.step(false, 1, at(1), any);
        }
        jumps.left(at(5));
        assert_eq!(jumps.list.len(), MAX);
    }

    #[test]
    fn back_forward_and_a_new_jump_forgets_the_way_forward() {
        let mut jumps = Jumps::default();
        let any = |_: &Jump| true;
        jumps.left(at(1));
        jumps.left(at(2));
        // Back from pane 3, which is recorded to come forward to.
        assert_eq!(pane_of(jumps.step(true, 1, at(3), any)), Some(2));
        // Only to panes 1 and 2, as if pane 3 were in another tab.
        let not_3 = |jump: &Jump| jump.pane != PaneId(3);
        assert_eq!(pane_of(jumps.step(true, 1, at(2), not_3)), Some(1));
        assert_eq!(jumps.step(true, 1, at(1), any), None);
        assert_eq!(pane_of(jumps.step(false, 1, at(1), not_3)), Some(2));
        assert_eq!(jumps.step(false, 1, at(2), not_3), None);
        assert_eq!(pane_of(jumps.step(false, 1, at(2), any)), Some(3));
        assert_eq!(jumps.step(false, 1, at(3), any), None);
        // Back twice, then somewhere new: panes 2 and 3 are forgotten.
        jumps.step(true, 1, at(3), any);
        jumps.step(true, 1, at(2), any);
        jumps.left(at(1));
        assert_eq!(jumps.step(false, 1, at(4), any), None);
        assert_eq!(pane_of(jumps.step(true, 1, at(4), any)), Some(1));
    }

    /// As checked against Neovim: from line 40, after jumps from lines 1,
    /// 10, 20 and 30, `3<C-o>` goes to 10, `5<C-o>` nowhere, and then
    /// `2<C-i>` to 30 and `9<C-i>` nowhere.
    #[test]
    fn a_count_steps_that_many_places_or_none() {
        let mut jumps = Jumps::default();
        let any = |_: &Jump| true;
        for pane in [1, 10, 20, 30] {
            jumps.left(at(pane));
        }
        assert_eq!(jumps.step(true, 5, at(40), any), None);
        assert_eq!(jumps.list.len(), 4, "nothing recorded");
        assert_eq!(pane_of(jumps.step(true, 3, at(40), any)), Some(10));
        assert_eq!(jumps.step(false, 9, at(10), any), None);
        assert_eq!(pane_of(jumps.step(false, 2, at(10), any)), Some(30));
        // Places passed over count only if wanted.
        let not_20 = |jump: &Jump| jump.pane != PaneId(20);
        assert_eq!(pane_of(jumps.step(true, 2, at(30), not_20)), Some(1));
        assert_eq!(pane_of(jumps.step(false, 2, at(1), not_20)), Some(30));
    }

    #[test]
    fn places_in_folders_are_their_entries_and_move_with_them() {
        let in_folder = |dir: &str, entry: Option<&str>| Jump {
            folder: Some(FolderPlace {
                dir: dir.to_owned(),
                entry: entry.map(str::to_owned),
            }),
            ..at(1)
        };
        let mut jumps = Jumps::default();
        jumps.left(in_folder("/p/src", Some("/p/src/a.rs")));
        // On another entry, it's another place; on the same, it's the same.
        jumps.left(in_folder("/p/src", Some("/p/src/b.rs")));
        jumps.left(in_folder("/p/src", Some("/p/src/b.rs")));
        assert_eq!(jumps.list.len(), 2);
        assert!(!in_folder("/p", None).same(&at(1)));
        // A folder moved takes its places, and its entries', with it.
        jumps.moved("/p/src", "/p/lib");
        assert_eq!(jumps.list[1], in_folder("/p/lib", Some("/p/lib/b.rs")));
        jumps.moved("/p/lib/a.rs", "/p/lib/c.rs");
        assert_eq!(jumps.list[0], in_folder("/p/lib", Some("/p/lib/c.rs")));
    }

    #[test]
    fn places_follow_edits_and_are_kept_within_new_text() {
        let text = Rope::from_str("one\ntwo\nthree\n");
        let mut jumps = Jumps::default();
        let mut jump = at(1);
        // On "three"'s "r".
        jump.file = Some(FilePlace::at("/f", &text, 10));
        jumps.left(jump.clone());
        let place = |jumps: &Jumps| {
            let file = jumps.list[0].file.as_ref().unwrap();
            (file.line, file.col)
        };
        assert_eq!(place(&jumps), (2, 2));
        // A line put in above takes it down.
        let edit = Edit::replace(14, 0, "", "zero\n");
        let text = Rope::from_str("zero\none\ntwo\nthree\n");
        jumps.edited("/f", std::slice::from_ref(&edit), &text);
        jumps.edited("/other", std::slice::from_ref(&edit), &text);
        assert_eq!(place(&jumps), (3, 2));
        // Shorter text: as far as it goes, at its last line's end.
        jumps.replaced("/f", &Rope::from_str("zero\nx\n"));
        assert_eq!(place(&jumps), (1, 1));
        jumps.replaced("/f", &Rope::from_str(""));
        assert_eq!(place(&jumps), (0, 0));
    }
}
