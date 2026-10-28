//! What every picker has in common: the finder, the picker of a machine to
//! connect to, and the start page `:Agent` opens. Each is a query typed
//! over a list of rows, one of them picked, drawn as the finder is (see
//! [`crate::finder::view`]). [`Picker`] holds what's typed, the row picked
//! and the first row shown, and does the keys they share:
//!
//! | Key | Does |
//! |---|---|
//! | Typing, Backspace | Changes what's typed |
//! | Ctrl+U, Ctrl+W | Deletes what's typed, or its last word (see [`cmdline::simple`]) |
//! | Down, Ctrl+N; Up, Ctrl+P | The next or previous row |
//! | Page Down, Page Up | A page further or back |
//! | Home, End | The first row or the last |
//!
//! and the wheel, which moves the row picked as the arrows do, a click,
//! which picks the row clicked, and a paste, which types its text on one
//! line, as a list's bar takes it (see [`crate::vim::list::paste_into`]).
//!
//! What's typed is only ever added to or deleted from the end: a picker
//! has no caret to move, so Home and End are the list's, as they are in a
//! list pane.
//!
//! How many rows there are is each picker's own, as some have rows besides
//! those matching what's typed (the start page's new chat, the machine
//! typed to connect to), so it's given to each call that needs it. So are
//! Enter, Esc and the keys a picker has of its own, which it reads before
//! passing the rest here.

use std::ops::Range;

use winit::keyboard::Key;

use crate::cmdline::{self, simple};
use crate::input::KeyPress;
use crate::shortcuts::bindings::Hint;
use crate::vim::{Stroke, plain_strokes};

/// What a key did to a picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keyed {
    /// What's typed changed, so the list is to be matched against it
    /// again. The first row is picked, and shown first.
    Typed,
    /// The row picked moved.
    Moved,
    /// Nothing changed: the row picked was already as far as it goes, or
    /// there was nothing to delete.
    Nothing,
}

/// A key a picker does for every picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shared {
    /// Moves the row picked by so many rows.
    Go(isize),
    /// Moves the row picked a page down, or up if not.
    Page {
        down: bool,
    },
    First,
    Last,
    /// Edits what's typed, as [`simple::key`] does.
    Edit,
}

impl Shared {
    /// The shared key `stroke` is, if it's one, and what it does, as the
    /// picker's help says it, things typed being said to be `typing`.
    fn read(stroke: Stroke, typing: &'static str) -> Option<(Self, Hint)> {
        Some(match stroke {
            Stroke::Down
            | Stroke::Char {
                ch: 'n',
                ctrl: true,
            } => (Self::Go(1), Hint::Action("Next result")),
            Stroke::Up
            | Stroke::Char {
                ch: 'p',
                ctrl: true,
            } => (Self::Go(-1), Hint::Action("Previous result")),
            Stroke::PageDown => (Self::Page { down: true }, Hint::Action("Next page")),
            Stroke::PageUp => (Self::Page { down: false }, Hint::Action("Previous page")),
            Stroke::Home => (Self::First, Hint::Action("First result")),
            Stroke::End => (Self::Last, Hint::Action("Last result")),
            Stroke::Char { ctrl: false, .. } => (Self::Edit, Hint::Text(typing)),
            // Its own words for the rest, but never Esc's or Enter's, nor
            // Backspace's on nothing typed, which close a prompt: here
            // Backspace on nothing does nothing, and the others are the
            // picker's.
            Stroke::Backspace
            | Stroke::Char {
                ch: 'u' | 'w',
                ctrl: true,
            } => (Self::Edit, simple::hint(stroke, false)),
            _ => return None,
        })
    }
}

/// What's typed into a picker, the row picked and the first row shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picker {
    query: String,
    /// The row picked.
    selected: usize,
    /// The first row shown.
    top: usize,
    /// How many rows the list showed last, for paging.
    rows: usize,
}

impl Default for Picker {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl Picker {
    /// A picker with `query` typed, its first row picked.
    pub(crate) fn new(query: String) -> Self {
        Self {
            query,
            selected: 0,
            top: 0,
            rows: 1,
        }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The row picked.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Picks row `row`, which the list scrolls to when it's next shown.
    pub(crate) fn set_selected(&mut self, row: usize) {
        self.selected = row;
    }

    /// Picks the first row and shows it first, as when the list starts
    /// again.
    pub(crate) fn restart(&mut self) {
        self.selected = 0;
        self.top = 0;
    }

    /// Keeps the row picked among the `len` there are now, as fewer may
    /// have come.
    pub(crate) fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    /// Does `stroke`, in a list of `len` rows, if it's one every picker
    /// has; `None` if it isn't, for the picker to do itself.
    pub(crate) fn key(&mut self, stroke: Stroke, len: usize) -> Option<Keyed> {
        let (shared, _) = Shared::read(stroke, "")?;
        Some(match shared {
            Shared::Go(by) => self.go(by, len),
            Shared::Page { down } => self.page(down, len),
            Shared::First => self.go_to(0),
            Shared::Last => self.go_to(len.saturating_sub(1)),
            Shared::Edit => {
                // Ctrl+U, Ctrl+W and Backspace only ever delete, and
                // typing only adds, so a query as long as before is the
                // same query.
                let before = self.query.len();
                simple::key(&mut self.query, stroke);
                if self.query.len() == before {
                    return Some(Keyed::Nothing);
                }
                self.typed()
            }
        })
    }

    /// What [`key`](Self::key) does with `stroke`, as the picker's help
    /// says it, things typed being said to be `typing`; [`Hint::None`] if
    /// it's none of its.
    pub(crate) fn hint(stroke: Stroke, typing: &'static str) -> Hint {
        Shared::read(stroke, typing).map_or(Hint::None, |(_, hint)| hint)
    }

    /// Types `text`, its line breaks as spaces, and nothing else that
    /// isn't printed, as a paste does.
    pub(crate) fn paste(&mut self, text: &str) -> Keyed {
        let before = self.query.len();
        crate::vim::list::paste_into(&mut self.query, text);
        if self.query.len() == before {
            return Keyed::Nothing;
        }
        self.typed()
    }

    /// Deletes all that's typed, which picks the first row.
    pub(crate) fn clear(&mut self) -> Keyed {
        if self.query.is_empty() {
            return Keyed::Nothing;
        }
        self.query.clear();
        self.typed()
    }

    /// What's typed changed.
    fn typed(&mut self) -> Keyed {
        self.restart();
        Keyed::Typed
    }

    /// Moves the row picked by `by` rows, in a list of `len`, stopping at
    /// its ends.
    pub(crate) fn go(&mut self, by: isize, len: usize) -> Keyed {
        let last = len.saturating_sub(1);
        self.go_to(self.selected.saturating_add_signed(by).min(last))
    }

    /// Moves the row picked a page down, or up if not `down`.
    fn page(&mut self, down: bool, len: usize) -> Keyed {
        let page = isize::try_from(self.rows.max(1)).unwrap_or(isize::MAX);
        self.go(if down { page } else { -page }, len)
    }

    fn go_to(&mut self, row: usize) -> Keyed {
        if row == self.selected {
            return Keyed::Nothing;
        }
        self.selected = row;
        Keyed::Moved
    }

    /// Picks the row shown `row` rows down the list, as a click does, if
    /// it's one of the `len` there are; `false` if it's past them.
    pub(crate) fn click(&mut self, row: usize, len: usize) -> bool {
        let picked = self.top + row;
        if picked >= len {
            return false;
        }
        self.selected = picked;
        true
    }

    /// The rows of the `len` there are to show in a list `rows` tall, at
    /// least one: from the first shown before, scrolled as little as
    /// shows the row picked.
    pub(crate) fn shown(&mut self, rows: usize, len: usize) -> Range<usize> {
        let rows = rows.max(1);
        self.rows = rows;
        self.clamp(len);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + rows {
            self.top = self.selected + 1 - rows;
        }
        self.top..len.min(self.top + rows)
    }

    /// Where the row picked is among those [`shown`](Self::shown) last,
    /// if there are any.
    pub(crate) fn shown_selected(&self, len: usize) -> Option<usize> {
        (len > 0).then(|| self.selected - self.top)
    }
}

/// What a key does to a picker, as the picker says it to the window.
pub(crate) trait Outcome: Sized {
    /// Nothing to show.
    const NOTHING: Self;
    /// Something shown changed.
    const CHANGED: Self;

    /// How much it changed, for the outcome of a key that makes several
    /// strokes to be the most any made, as a grep searched for once all
    /// is typed; `None` if it's an end, as closing, which the strokes
    /// after it don't get to change.
    fn weight(&self) -> Option<u8>;
}

impl Keyed {
    /// What the picker says to the window of what the core did: `typed`
    /// says it once what's typed changed.
    pub(crate) fn outcome<O: Outcome>(self, typed: impl FnOnce() -> O) -> O {
        match self {
            Self::Nothing => O::NOTHING,
            Self::Moved => O::CHANGED,
            Self::Typed => typed(),
        }
    }
}

/// Does each stroke `press` makes in a picker with `stroke`: the most any
/// changed, or the first that ended it (see [`Outcome::weight`]).
pub(crate) fn press<O: Outcome>(press: &KeyPress<'_>, mut stroke: impl FnMut(Stroke) -> O) -> O {
    let mut outcome = O::NOTHING;
    for made in strokes(press) {
        let next = stroke(made);
        match next.weight() {
            None => return next,
            Some(weight) if weight > outcome.weight().unwrap_or(0) => outcome = next,
            Some(_) => {}
        }
    }
    outcome
}

/// The strokes `press` makes in a picker: what it types, as in a list's
/// bar, and its keys, Ctrl and a letter as the letter's small one, so Caps
/// Lock changes nothing. With Shift, Ctrl and a letter are none of a
/// picker's: Ctrl+Shift+C and the like are the terminal's.
pub(crate) fn strokes(press: &KeyPress<'_>) -> Vec<Stroke> {
    let modifiers = press.modifiers;
    if modifiers.ctrl && modifiers.shift && matches!(press.key, Key::Character(_)) {
        return Vec::new();
    }
    (plain_strokes(press, true).into_iter())
        .map(|stroke| match stroke {
            Stroke::Char { ch, ctrl: true } => Stroke::ctrl(ch.to_ascii_lowercase()),
            stroke => stroke,
        })
        .collect()
}

/// The query as a picker draws it, with its caret at the end.
pub(crate) fn drawn(query: &str) -> String {
    format!("{query}{}", cmdline::CARET)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Picker {
        let mut picker = Picker::new(String::new());
        for ch in text.chars() {
            picker.key(Stroke::char(ch), 0);
        }
        picker
    }

    #[test]
    fn ctrl_w_deletes_the_last_word_as_every_prompt_does() {
        let mut picker = typed("src/main.rs");
        assert_eq!(picker.key(Stroke::ctrl('w'), 0), Some(Keyed::Typed));
        assert_eq!(picker.query(), "src/main.");
        picker.key(Stroke::ctrl('w'), 0);
        assert_eq!(picker.query(), "src/");
        let mut spaced = typed("foo bar  ");
        spaced.key(Stroke::ctrl('w'), 0);
        assert_eq!(spaced.query(), "foo ");
    }

    #[test]
    fn ctrl_u_and_backspace_delete_and_on_nothing_do_nothing() {
        let mut picker = typed("ab");
        assert_eq!(picker.key(Stroke::Backspace, 0), Some(Keyed::Typed));
        assert_eq!(picker.query(), "a");
        assert_eq!(picker.key(Stroke::ctrl('u'), 0), Some(Keyed::Typed));
        assert_eq!(picker.query(), "");
        for stroke in [Stroke::Backspace, Stroke::ctrl('u'), Stroke::ctrl('w')] {
            assert_eq!(picker.key(stroke, 0), Some(Keyed::Nothing), "{stroke:?}");
        }
        // Ctrl+W on what has no word left deletes what's left.
        let mut punctuated = typed("--");
        assert_eq!(punctuated.key(Stroke::ctrl('w'), 0), Some(Keyed::Typed));
        assert_eq!(punctuated.query(), "");
    }

    #[test]
    fn esc_enter_and_other_keys_are_the_pickers_own() {
        let mut picker = typed("a");
        for stroke in [
            Stroke::Escape,
            Stroke::Enter,
            Stroke::Tab,
            Stroke::Left,
            Stroke::ctrl('c'),
            Stroke::ctrl('v'),
        ] {
            assert_eq!(picker.key(stroke, 5), None, "{stroke:?}");
            assert_eq!(Picker::hint(stroke, "Search"), Hint::None, "{stroke:?}");
        }
        assert_eq!(picker.query(), "a");
    }

    #[test]
    fn a_paste_types_one_line_whatever_breaks_it() {
        let mut picker = typed("x");
        assert_eq!(picker.paste("a\r\nb\tc\u{7}"), Keyed::Typed);
        assert_eq!(picker.query(), "xa b c");
        assert_eq!(picker.paste("\u{7}"), Keyed::Nothing);
    }

    #[test]
    fn typing_picks_the_first_row_and_shows_it_first() {
        let mut picker = Picker::new(String::new());
        picker.shown(3, 10);
        picker.key(Stroke::End, 10);
        assert_eq!(picker.shown(3, 10), 7..10);
        picker.key(Stroke::char('a'), 10);
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.shown(3, 10), 0..3);
    }

    #[test]
    fn moves_stop_at_the_ends_and_say_so() {
        let mut picker = Picker::new(String::new());
        picker.shown(4, 10);
        assert_eq!(picker.key(Stroke::Up, 10), Some(Keyed::Nothing));
        assert_eq!(picker.key(Stroke::ctrl('n'), 10), Some(Keyed::Moved));
        assert_eq!(picker.key(Stroke::Down, 10), Some(Keyed::Moved));
        assert_eq!(picker.selected(), 2);
        assert_eq!(picker.key(Stroke::ctrl('p'), 10), Some(Keyed::Moved));
        assert_eq!(picker.selected(), 1);
        // A page is as many rows as were shown last.
        assert_eq!(picker.key(Stroke::PageDown, 10), Some(Keyed::Moved));
        assert_eq!(picker.selected(), 5);
        picker.key(Stroke::PageDown, 10);
        picker.key(Stroke::PageDown, 10);
        assert_eq!(picker.selected(), 9);
        assert_eq!(picker.key(Stroke::Down, 10), Some(Keyed::Nothing));
        assert_eq!(picker.key(Stroke::End, 10), Some(Keyed::Nothing));
        assert_eq!(picker.key(Stroke::Home, 10), Some(Keyed::Moved));
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.key(Stroke::PageUp, 10), Some(Keyed::Nothing));
        // Nothing to pick: nothing moves.
        let mut empty = Picker::new(String::new());
        for stroke in [Stroke::Down, Stroke::End, Stroke::PageDown] {
            assert_eq!(empty.key(stroke, 0), Some(Keyed::Nothing), "{stroke:?}");
        }
    }

    #[test]
    fn the_list_scrolls_as_little_as_shows_the_row_picked() {
        let mut picker = Picker::new(String::new());
        assert_eq!(picker.shown(3, 10), 0..3);
        picker.go(4, 10);
        assert_eq!(picker.shown(3, 10), 2..5);
        assert_eq!(picker.shown_selected(10), Some(2));
        picker.go(-3, 10);
        assert_eq!(picker.shown(3, 10), 1..4);
        assert_eq!(picker.shown_selected(10), Some(0));
        // Fewer rows than shown before: the row picked is the last.
        assert_eq!(picker.shown(3, 1), 0..1);
        assert_eq!(picker.selected(), 0);
        assert_eq!(picker.shown(3, 0), 0..0);
        assert_eq!(picker.shown_selected(0), None);
        // A list no rows tall still shows one.
        picker.go(5, 10);
        assert_eq!(picker.shown(0, 10), 5..6);
    }

    #[test]
    fn a_click_picks_the_row_shown_there_if_there_is_one() {
        let mut picker = Picker::new(String::new());
        picker.go(5, 10);
        picker.shown(3, 10);
        assert!(picker.click(1, 10));
        assert_eq!(picker.selected(), 4);
        assert!(!picker.click(3, 6));
        assert_eq!(picker.selected(), 4);
    }

    #[test]
    fn ctrl_shift_and_a_letter_are_no_pickers_and_caps_lock_changes_nothing() {
        use crate::input::Modifiers;
        let key = Key::Character("C".into());
        let press = |modifiers| KeyPress {
            key: &key,
            text: None,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers,
        };
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let shifted = Modifiers {
            shift: true,
            ..ctrl
        };
        assert_eq!(strokes(&press(shifted)), Vec::new());
        // Caps Lock on, Ctrl and a letter are still that letter's keys.
        assert_eq!(strokes(&press(ctrl)), vec![Stroke::ctrl('c')]);
    }
}
