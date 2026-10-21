//! What keys make: a command, as every pane that takes it reads it.
//!
//! A command is generic over `O`, the commands of a pane's own (see
//! [`super::Dialect`]): the editor's language server keys, say, or a
//! terminal's `gx`.

use crate::motion::Motion;
use crate::objects::Object;

/// Whether `ch` names a register, after `"`: `a` to `z`, or `A` to `Z` to
/// add to them, `0` to `9`, `"` for the unnamed, `-` for small deletes,
/// `_` for none, and `+` and `*` for the clipboard and the primary
/// selection.
pub(crate) fn is_register(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '"' | '-' | '_' | '+' | '*')
}

/// Whether `ch` names a mark that can be set, after `m`: `a` to `z` in
/// the file, `A` to `Z` anywhere, and `'` or `` ` `` for the place before
/// the last jump.
pub(super) fn is_settable_mark(ch: char) -> bool {
    ch.is_ascii_alphabetic() || matches!(ch, '\'' | '`')
}

/// Whether `ch` names a mark to go to, after `'` or `` ` ``: one that can
/// be set, or `.` for the last change.
pub(super) fn is_mark(ch: char) -> bool {
    is_settable_mark(ch) || ch == '.'
}

/// A change of case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recase {
    /// `gu`: to small letters.
    Lower,
    /// `gU`: to capitals.
    Upper,
    /// `g~` and `~`: small letters to capitals, and capitals to small.
    Toggle,
    /// `g?`: each ASCII letter thirteen on, round the alphabet.
    Rot13,
}

/// What an operator does to the text it's given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operator {
    /// `d`: delete it, keeping it to put back.
    Delete,
    /// `c`: delete it and type in its place.
    Change,
    /// `y`: copy it.
    Yank,
    /// `>`: indent its lines, whole, by `shiftwidth`.
    Indent,
    /// `<`: take as much off their indent.
    Dedent,
    /// `g~`, `gu`, `gU` and `g?`: change its case.
    Recase(Recase),
    /// `=`: indent its lines as their language's grammar says.
    Reindent,
}

/// What an operator acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// What the motion moves over, `count` times.
    Motion(Motion),
    /// A text object around the cursor.
    Object { object: Object, inner: bool },
    /// Whole lines, from the cursor's: `dd`, `cc` and `yy`.
    Lines,
    /// What's selected, in visual mode.
    Selection,
    /// To a mark: from line to line after `'`, or exactly, as an exclusive
    /// motion, after `` ` ``.
    Mark { mark: char, exact: bool },
}

/// Where insert mode starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// `i`: before the cursor.
    Before,
    /// `a`: after it.
    After,
    /// `I`: before the line's first character that isn't blank.
    LineStart,
    /// `A`: at the line's end.
    LineEnd,
    /// `o`: on a new line below.
    Below,
    /// `O`: on a new line above.
    Above,
}

/// Where to put the view, by the cursor's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewAt {
    /// `zt`: the cursor's line at the top.
    Top,
    /// `zz`: in the middle.
    Middle,
    /// `zb`: at the bottom.
    Bottom,
}

/// How far a scroll goes, down if `down`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scroll {
    /// Ctrl+E and Ctrl+Y: the view a line, keeping the cursor in it.
    Line { down: bool },
    /// Ctrl+D and Ctrl+U: the view and the cursor half the view.
    HalfPage { down: bool },
    /// Ctrl+F and Page Down, Page Up: the view most of it, keeping the
    /// cursor in it.
    Page { down: bool },
}

/// How far the scrolling keys go, the same in every pane, as Vim's do: the
/// half page Ctrl+D and Ctrl+U go is half the view, rounded down, until a
/// count before one sets it, as Vim's `scroll` option keeps it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Paging {
    half: Option<u64>,
}

impl Paging {
    /// How many screen rows `scroll`, with `count` typed, goes in a view
    /// `view` rows tall: a line a time for Ctrl+E and Ctrl+Y, the half page
    /// for Ctrl+D and Ctrl+U, and the view less two, keeping two rows of
    /// what was in view, a page a time for Ctrl+F, Page Down and Page Up.
    pub(crate) fn rows(&mut self, scroll: Scroll, count: Option<usize>, view: u64) -> u64 {
        let count = count.map(|count| count as u64);
        match scroll {
            Scroll::Line { .. } => count.unwrap_or(1),
            Scroll::HalfPage { .. } => {
                if count.is_some() {
                    self.half = count;
                }
                self.half.unwrap_or(view / 2).max(1)
            }
            Scroll::Page { .. } => {
                (count.unwrap_or(1)).saturating_mul(view.saturating_sub(2).max(1))
            }
        }
    }

    /// Forgets the half page a count set, as the view's size changed, as
    /// Vim's `scroll` is set afresh then.
    pub(crate) fn resized(&mut self) {
        self.half = None;
    }
}

/// A command, how many times to do it, and the register it takes text
/// from or keeps it in, if one's named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Command<O> {
    pub(crate) count: Option<usize>,
    pub(crate) action: Action<O>,
    pub(crate) register: Option<char>,
}

impl<O> Command<O> {
    /// The count, or 1.
    pub(crate) fn times(&self) -> usize {
        self.count.unwrap_or(1)
    }
}

/// What a command does, as every pane that takes it does it; or `Own`, a
/// command of the pane's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action<O> {
    Move(Motion),
    Operate(Operator, Target),
    /// `x`, or `X` if `before`.
    DeleteChars {
        before: bool,
    },
    /// `D`, or `C` if `change`.
    DeleteToEnd {
        change: bool,
    },
    /// `s`.
    Substitute,
    /// `p`, or `P` if `before`.
    Put {
        before: bool,
    },
    /// `J`.
    Join,
    /// `r` and the character.
    Replace(char),
    Insert(Entry),
    Undo,
    Redo,
    /// `.`.
    Repeat,
    /// `v`, or `V` if `lines`.
    Visual {
        lines: bool,
    },
    /// Ctrl+V: select a block of columns, as Vim's does.
    BlockVisual,
    /// `/`, or `?` if `backward`.
    Search {
        backward: bool,
    },
    /// `n`, or `N` if `reverse`.
    SearchAgain {
        reverse: bool,
    },
    /// `*`, or `#` if `backward`: the word under the cursor, as a whole
    /// word.
    SearchWord {
        backward: bool,
    },
    /// `;`, or `,` if `reverse`.
    FindAgain {
        reverse: bool,
    },
    Scroll(Scroll),
    View(ViewAt),
    /// `o` in visual mode: to the other end of the selection.
    OtherEnd,
    /// `O` in visual mode: in a block, to the other corner on the same
    /// line; otherwise, as `o`.
    OtherCorner,
    /// `gv`: selects again what was selected last, as it was selected; in
    /// visual mode, swapping it with what's selected now.
    Reselect,
    /// `i` or `a` and an object, in visual mode: select it.
    Select {
        object: Object,
        inner: bool,
    },
    /// `m` and a mark: set it at the cursor.
    SetMark(char),
    /// `` ` `` and a mark: go to it, or with `'`, to its line's first
    /// character that isn't blank. `''` and ``` `` ``` go back to before
    /// the last jump.
    Mark {
        mark: char,
        exact: bool,
    },
    /// `~`, with `tildeop` off: turn over the case of the character under
    /// the cursor, and as many after it as the count, moving on past them.
    Tilde,
    /// A command of the pane's own.
    Own(O),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_goes_as_far_in_every_pane_as_vims_does() {
        let mut paging = Paging::default();
        let half = Scroll::HalfPage { down: true };
        let page = Scroll::Page { down: false };
        // Half of eleven rows, rounded down, as Vim's `scroll` starts.
        assert_eq!(paging.rows(half, None, 11), 5);
        // A count sets it, until the view's size changes.
        assert_eq!(paging.rows(half, Some(3), 11), 3);
        assert_eq!(paging.rows(half, None, 11), 3);
        paging.resized();
        assert_eq!(paging.rows(half, None, 11), 5);
        // A page is the view less two, a count that many.
        assert_eq!(paging.rows(page, None, 10), 8);
        assert_eq!(paging.rows(page, Some(2), 10), 16);
        assert_eq!(paging.rows(Scroll::Line { down: true }, Some(4), 10), 4);
        // Never nothing, however small the view.
        assert_eq!(paging.rows(half, None, 1), 1);
        assert_eq!(paging.rows(page, None, 1), 1);
    }
}
