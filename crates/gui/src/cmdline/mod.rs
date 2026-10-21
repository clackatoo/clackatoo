//! A line typed into one of the window's own prompts: the command line
//! after `:`, a search after `/` or `?` in a terminal or an editor, or a
//! new name. Each is edited as Vim's command line is:
//!
//! | Key | Does |
//! |---|---|
//! | Left, Right, Home, End | Move the caret |
//! | Ctrl or Shift and Left, Right | Move it a WORD, anything between blanks |
//! | Backspace, Delete | Delete the character before the caret, or under it |
//! | Ctrl+U | Deletes everything before the caret |
//! | Ctrl+W, Ctrl+Backspace | Deletes the word before the caret |
//! | Ctrl+R and a register's name | Puts in what the register holds (see [`Line::wants_register`]) |
//! | Up, Down | The lines typed before, oldest last, that start with what's typed (see [`recall`]) |
//! | Tab, Shift+Tab | Complete the word before the caret, on the command line (see [`complete`]) |
//!
//! Backspace on an empty line closes it, as in Vim; whoever owns the line
//! sees that as [`Keyed::Emptied`].
//!
//! A line can start with all its text selected, as a new name starts with
//! the old one (see [`Line::selected`]): the first key typed, pasted or
//! put from a register replaces it, Backspace, Delete and Ctrl+W clear
//! it, and any other key that edits drops the selection first, so the
//! arrows, Home and End edit the old text as usual.

pub(crate) mod complete;
pub(crate) mod nearest;
pub(crate) mod recall;

use crate::input::KeyPress;
use crate::vim::{Stroke, prompt_strokes};

/// The caret, drawn in the line's text where it is.
pub(crate) const CARET: char = '▏';

/// What a key did to a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keyed {
    /// It edited the line, or moved its caret.
    Edited,
    /// Backspace, on a line with nothing in it: the prompt closes.
    Emptied,
    /// It isn't a key that edits, such as Enter, Esc, Tab, Up or Down,
    /// which are the prompt's own.
    Other,
}

/// A key that edits a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edit {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Ctrl or Shift and Left: back a WORD, as Vim's command line goes.
    WordLeft,
    /// Ctrl or Shift and Right: on a WORD.
    WordRight,
    /// Ctrl+U.
    ClearBefore,
    /// Ctrl+W.
    DeleteWord,
    /// Ctrl+R: a register's name comes next.
    Register,
}

impl Edit {
    /// The edit Ctrl and `ch` make.
    fn ctrl(ch: char) -> Option<Self> {
        Some(match ch {
            'u' => Self::ClearBefore,
            'w' => Self::DeleteWord,
            'r' => Self::Register,
            _ => return None,
        })
    }
}

/// A line being typed, and where its caret is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Line {
    text: String,
    /// The byte the caret is before.
    caret: usize,
    /// Ctrl+R was pressed: the next key names a register.
    register: bool,
    /// Where Up and Down have gone in the lines typed before.
    walk: Option<recall::Walk>,
    /// All the text is selected, for what's typed first to replace it.
    selected: bool,
}

impl Line {
    /// A line holding `text`, with the caret at its end.
    pub(crate) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            caret: text.len(),
            text,
            ..Self::default()
        }
    }

    /// A line holding `text`, all of it selected, with the caret at its
    /// end: the first key typed replaces it, and the arrows keep it.
    pub(crate) fn selected(text: impl Into<String>) -> Self {
        let line = Self::new(text);
        Self {
            selected: !line.text.is_empty(),
            ..line
        }
    }

    /// The bytes of [`shown`](Self::shown) that are selected, if any are.
    pub(crate) fn selection(&self) -> Option<std::ops::Range<usize>> {
        self.selected.then_some(0..self.text.len())
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The text, taken, leaving the line empty.
    pub(crate) fn take(&mut self) -> String {
        self.caret = 0;
        self.walk = None;
        self.selected = false;
        std::mem::take(&mut self.text)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text before the caret.
    pub(crate) fn before_caret(&self) -> &str {
        &self.text[..self.caret]
    }

    /// The text as the bar shows it: the caret where it is, or a `"`
    /// there while Ctrl+R waits for a register's name, as Vim shows it.
    pub(crate) fn shown(&self) -> String {
        let mark = if self.register { '"' } else { CARET };
        let mut shown = String::with_capacity(self.text.len() + mark.len_utf8());
        shown.push_str(&self.text[..self.caret]);
        shown.push(mark);
        shown.push_str(&self.text[self.caret..]);
        shown
    }

    /// Whether Ctrl+R waits for a register's name: the next key is the
    /// owner's to read, which then [`put`](Self::put)s what the register
    /// holds, or [`forget_register`](Self::forget_register)s.
    pub(crate) fn wants_register(&self) -> bool {
        self.register
    }

    /// Stops waiting for a register's name.
    pub(crate) fn forget_register(&mut self) {
        self.register = false;
    }

    /// Puts `text` in at the caret, as typed or pasted: on one line, its
    /// line breaks as spaces, a trailing one dropped, and other control
    /// characters left out.
    pub(crate) fn put(&mut self, text: &str) {
        self.register = false;
        self.walk = None;
        self.replace_selection();
        let text = text.strip_suffix('\n').unwrap_or(text);
        for ch in text.chars() {
            let ch = if ch == '\n' { ' ' } else { ch };
            if !ch.is_control() {
                self.text.insert(self.caret, ch);
                self.caret += ch.len_utf8();
            }
        }
    }

    /// Replaces the text from byte `start` to the caret with `with`,
    /// leaving the caret after it, as completing a word does.
    pub(crate) fn replace_before(&mut self, start: usize, with: &str) {
        self.walk = None;
        self.selected = false;
        self.text.replace_range(start..self.caret, with);
        self.caret = start + with.len();
    }

    /// Makes the line `text`, with the caret at its end, keeping where Up
    /// and Down have gone.
    fn set(&mut self, text: &str) {
        self.selected = false;
        self.text.clear();
        self.text.push_str(text);
        self.caret = self.text.len();
    }

    /// Takes `press`, a key typed into the window's prompt, as the keys it
    /// makes (see [`stroke`](Self::stroke)): every prompt reads keys the
    /// one way.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> Keyed {
        let mut keyed = Keyed::Other;
        for stroke in prompt_strokes(press) {
            keyed = match (keyed, self.stroke(stroke)) {
                (Keyed::Emptied, _) | (_, Keyed::Emptied) => Keyed::Emptied,
                (Keyed::Edited, _) | (_, Keyed::Edited) => Keyed::Edited,
                _ => Keyed::Other,
            };
        }
        keyed
    }

    /// Takes `stroke`, a key typed into a prompt.
    pub(crate) fn stroke(&mut self, stroke: Stroke) -> Keyed {
        let edit = match stroke {
            Stroke::Char { ch, ctrl: true } => Edit::ctrl(ch),
            Stroke::Char { ch, ctrl: false } => {
                self.put(ch.encode_utf8(&mut [0; 4]));
                return Keyed::Edited;
            }
            Stroke::Backspace => Some(Edit::Backspace),
            Stroke::Delete => Some(Edit::Delete),
            Stroke::Left => Some(Edit::Left),
            Stroke::Right => Some(Edit::Right),
            Stroke::Home | Stroke::CtrlHome => Some(Edit::Home),
            Stroke::End | Stroke::CtrlEnd => Some(Edit::End),
            Stroke::CtrlLeft | Stroke::ShiftLeft => Some(Edit::WordLeft),
            Stroke::CtrlRight | Stroke::ShiftRight => Some(Edit::WordRight),
            _ => None,
        };
        edit.map_or(Keyed::Other, |edit| self.edit(edit))
    }

    fn edit(&mut self, edit: Edit) -> Keyed {
        if edit == Edit::Backspace && self.text.is_empty() {
            return Keyed::Emptied;
        }
        self.walk = None;
        if matches!(edit, Edit::Backspace | Edit::Delete | Edit::DeleteWord) && self.selected {
            self.replace_selection();
            return Keyed::Edited;
        }
        // Moving off it, it's text like any other; a register's text
        // replaces it once it's put.
        if edit != Edit::Register {
            self.selected = false;
        }
        let caret = self.caret;
        match edit {
            Edit::Backspace => {
                let start = self.char_before(caret);
                self.text.replace_range(start..caret, "");
                self.caret = start;
            }
            Edit::Delete => {
                let end = self.char_after(caret);
                self.text.replace_range(caret..end, "");
            }
            Edit::Left => self.caret = self.char_before(caret),
            Edit::Right => self.caret = self.char_after(caret),
            Edit::Home => self.caret = 0,
            Edit::End => self.caret = self.text.len(),
            Edit::WordLeft => self.caret = self.word_before(caret, true),
            Edit::WordRight => self.caret = self.word_after(caret, true),
            Edit::ClearBefore => {
                self.text.replace_range(..caret, "");
                self.caret = 0;
            }
            Edit::DeleteWord => {
                let start = self.word_before(caret, false);
                self.text.replace_range(start..caret, "");
                self.caret = start;
            }
            Edit::Register => self.register = true,
        }
        Keyed::Edited
    }

    /// Clears the text, if it's all selected, for what comes next to take
    /// its place.
    fn replace_selection(&mut self) {
        if std::mem::take(&mut self.selected) {
            self.text.clear();
            self.caret = 0;
        }
    }

    /// The byte the character before `at` starts at, or 0.
    fn char_before(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    /// The byte after the character at `at`, or the end.
    fn char_after(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |ch| at + ch.len_utf8())
    }

    /// Where the word before `at` starts, past blanks before it, as Vim's
    /// Ctrl+W finds it: a run of letters, digits and `_`, or of other
    /// characters that aren't blank.
    fn word_before(&self, at: usize, big: bool) -> usize {
        let mut chars = self.text[..at].char_indices().rev().peekable();
        while chars.next_if(|(_, ch)| ch.is_whitespace()).is_some() {}
        let Some(&(_, first)) = chars.peek() else {
            return 0;
        };
        let class = class(first, big);
        let mut start = at;
        while let Some((at, _)) =
            chars.next_if(|&(_, ch)| !ch.is_whitespace() && self::class(ch, big) == class)
        {
            start = at;
        }
        start.min(at)
    }

    /// Where the next word after `at` starts, or the end.
    fn word_after(&self, at: usize, big: bool) -> usize {
        let mut chars = self.text[at..].char_indices().peekable();
        if let Some(&(_, first)) = chars.peek() {
            let class = class(first, big);
            while chars
                .next_if(|&(_, ch)| !ch.is_whitespace() && self::class(ch, big) == class)
                .is_some()
            {}
        }
        while chars.next_if(|(_, ch)| ch.is_whitespace()).is_some() {}
        chars
            .peek()
            .map_or(self.text.len(), |&(offset, _)| at + offset)
    }
}

/// Which kind of word a character is part of: letters, digits and `_`, or
/// anything else; or, for a WORD (`big`), all one kind but blanks.
fn class(ch: char, big: bool) -> bool {
    big || ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line after `strokes`, as `Stroke::read` reads them.
    fn typed(strokes: &str) -> Line {
        let mut line = Line::default();
        for stroke in Stroke::read(strokes) {
            line.stroke(stroke);
        }
        line
    }

    #[test]
    fn keys_edit_at_the_caret() {
        let line = typed("set wrp<Left>a");
        assert_eq!(line.shown(), "set wra▏p");
        let line = typed("abc<Home>x<End>y<Left><Left><BS>");
        assert_eq!(line.shown(), "xa▏cy");
        let line = typed("abc<Home><Del>");
        assert_eq!(line.shown(), "▏bc");
        // Characters of several bytes are one each.
        let line = typed("é字<Left>x<BS><BS>");
        assert_eq!(line.shown(), "▏字");
    }

    #[test]
    fn ctrl_u_clears_before_the_caret_and_ctrl_w_a_word() {
        assert_eq!(typed("Ex<C-u>set wr").shown(), "set wr▏");
        assert_eq!(typed("e src/ma<Left><C-u>").shown(), "▏a");
        assert_eq!(typed("set wrap  <C-w>").shown(), "set ▏");
        assert_eq!(typed("e src/main.rs<C-w>").shown(), "e src/main.▏");
        assert_eq!(typed("e src/main.rs<C-w><C-w>").shown(), "e src/main▏");
        assert_eq!(
            typed("a b<Left><C-w>").shown(),
            "▏b",
            "the blanks and the word"
        );
        assert_eq!(typed("one<C-w><C-w>").shown(), "▏");
    }

    /// A line starting as `text`, all of it selected, after `strokes`.
    fn over_selected(text: &str, strokes: &str) -> Line {
        let mut line = Line::selected(text);
        for stroke in Stroke::read(strokes) {
            line.stroke(stroke);
        }
        line
    }

    #[test]
    fn a_selected_line_is_replaced_by_the_first_key_and_kept_by_the_arrows() {
        let line = Line::selected("count");
        assert_eq!(
            (line.shown(), line.selection()),
            ("count▏".into(), Some(0..5))
        );
        // What's typed first takes its place, and then adds to itself.
        let line = over_selected("count", "amount");
        assert_eq!((line.shown(), line.selection()), ("amount▏".into(), None));
        // Moving keeps it, to edit as any text.
        assert_eq!(over_selected("count", "<End>er").shown(), "counter▏");
        assert_eq!(over_selected("count", "<Home>re").shown(), "re▏count");
        assert_eq!(over_selected("count", "<Left>x").shown(), "counx▏t");
        // Ctrl+W clears the whole of it, not only its last word.
        assert_eq!(over_selected("two words", "<C-w>").shown(), "▏");
        assert_eq!(over_selected("two words", "<C-w>total").shown(), "total▏");
        // Backspace and Delete clear it, and Backspace again closes it.
        let mut line = Line::selected("count");
        assert_eq!(line.stroke(Stroke::Backspace), Keyed::Edited);
        assert_eq!((line.text(), line.selection()), ("", None));
        assert_eq!(line.stroke(Stroke::Backspace), Keyed::Emptied);
        assert_eq!(over_selected("count", "<Del>").text(), "");
        // A register's text, or a paste, takes its place too.
        let mut line = over_selected("count", "<C-r>");
        assert!(line.wants_register());
        line.put("total");
        assert_eq!(line.shown(), "total▏");
        // Nothing to select, nothing is.
        assert_eq!(Line::selected("").selection(), None);
    }

    #[test]
    fn backspace_on_an_empty_line_empties_it() {
        let mut line = typed("a");
        assert_eq!(line.stroke(Stroke::Backspace), Keyed::Edited);
        assert_eq!(line.stroke(Stroke::Backspace), Keyed::Emptied);
        // At the start of a line with text, it does nothing.
        let mut line = typed("a<Home>");
        assert_eq!(line.stroke(Stroke::Backspace), Keyed::Edited);
        assert_eq!(line.text(), "a");
    }

    #[test]
    fn ctrl_r_waits_for_a_register_and_puts_it_on_one_line() {
        let mut line = typed("e <C-r>");
        assert!(line.wants_register());
        assert_eq!(line.shown(), "e \"");
        line.put("one\ntwo\n");
        assert!(!line.wants_register());
        assert_eq!(line.shown(), "e one two▏");
        let mut line = typed("x<C-r>");
        line.forget_register();
        assert_eq!(line.shown(), "x▏");
    }

    #[test]
    fn the_windows_keys_edit_as_strokes_do_ctrl_and_shift_moving_by_words() {
        use winit::keyboard::{Key, KeyLocation, NamedKey};

        use crate::input::Modifiers;
        let press = |line: &mut Line, key: Key, ctrl: bool| {
            let text = match (&key, ctrl) {
                (Key::Character(text), false) => Some(text.to_string()),
                _ => None,
            };
            line.key(&KeyPress {
                key: &key,
                text: text.as_deref(),
                location: KeyLocation::Standard,
                code: None,
                modifiers: Modifiers {
                    ctrl,
                    ..Modifiers::default()
                },
            })
        };
        let mut line = Line::new("e a.b c.d");
        press(&mut line, Key::Named(NamedKey::ArrowLeft), true);
        assert_eq!(line.shown(), "e a.b ▏c.d", "by WORDs");
        let mut line = Line::new("set wrap number");
        press(&mut line, Key::Named(NamedKey::ArrowLeft), true);
        assert_eq!(line.shown(), "set wrap ▏number");
        press(&mut line, Key::Named(NamedKey::ArrowLeft), true);
        press(&mut line, Key::Named(NamedKey::ArrowRight), true);
        assert_eq!(line.shown(), "set wrap ▏number");
        assert_eq!(
            press(&mut line, Key::Character("x".into()), false),
            Keyed::Edited
        );
        assert_eq!(
            press(&mut line, Key::Named(NamedKey::Space), false),
            Keyed::Edited
        );
        assert_eq!(line.shown(), "set wrap x ▏number");
        // Ctrl+Backspace deletes a word, as Ctrl+W does.
        press(&mut line, Key::Named(NamedKey::Backspace), true);
        assert_eq!(line.shown(), "set wrap ▏number");
        press(&mut line, Key::Character("u".into()), true);
        assert_eq!(line.shown(), "▏number");
        // Ctrl and a key that edits nothing, and Enter, are the prompt's.
        assert_eq!(
            press(&mut line, Key::Character("a".into()), true),
            Keyed::Other
        );
        assert_eq!(
            press(&mut line, Key::Named(NamedKey::Enter), false),
            Keyed::Other
        );
        assert_eq!(line.text(), "number");
    }

    #[test]
    fn completing_replaces_the_word_before_the_caret() {
        let mut line = typed("set wr nu<Left><Left><Left>");
        line.replace_before(4, "wrap");
        assert_eq!(line.shown(), "set wrap▏ nu");
    }

    #[test]
    fn alt_and_a_key_types_its_character() {
        use winit::keyboard::{Key, KeyLocation};

        use crate::input::Modifiers;
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        let mut line = Line::new("a");
        // As a layout that holds Alt for `@` sends it.
        let key = Key::Character("@".into());
        let keyed = line.key(&KeyPress {
            key: &key,
            text: Some("@"),
            location: KeyLocation::Standard,
            code: None,
            modifiers: alt,
        });
        assert_eq!(keyed, Keyed::Edited);
        assert_eq!(line.shown(), "a@▏");
    }
}
