//! A line typed into one of the window's own prompts: the command line
//! after `:`, a search after `/` or `?` in a terminal or an editor, or a
//! new name. Each is edited as Vim's command line is:
//!
//! | Key | Does |
//! |---|---|
//! | Left, Right, Home, End | Move the caret |
//! | Ctrl+Left, Ctrl+Right | Move it a word |
//! | Backspace, Delete | Delete the character before the caret, or under it |
//! | Ctrl+U | Deletes everything before the caret |
//! | Ctrl+W | Deletes the word before the caret |
//! | Ctrl+R and a register's name | Puts in what the register holds (see [`Line::wants_register`]) |
//! | Up, Down | The lines typed before, oldest last, that start with what's typed (see [`recall`]) |
//! | Tab, Shift+Tab | Complete the word before the caret, on the command line (see [`complete`]) |
//!
//! Backspace on an empty line closes it, as in Vim; whoever owns the line
//! sees that as [`Keyed::Emptied`].

pub(crate) mod complete;
pub(crate) mod recall;

use winit::keyboard::{Key, NamedKey};

use crate::editor::Stroke;
use crate::input::KeyPress;

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
    WordLeft,
    WordRight,
    /// Ctrl+U.
    ClearBefore,
    /// Ctrl+W.
    DeleteWord,
    /// Ctrl+R: a register's name comes next.
    Register,
}

impl Edit {
    /// The edit a named key makes, with Ctrl held if `ctrl`.
    fn named(key: NamedKey, ctrl: bool) -> Option<Self> {
        Some(match key {
            NamedKey::Backspace if ctrl => Self::DeleteWord,
            NamedKey::Backspace => Self::Backspace,
            NamedKey::Delete => Self::Delete,
            NamedKey::ArrowLeft if ctrl => Self::WordLeft,
            NamedKey::ArrowRight if ctrl => Self::WordRight,
            NamedKey::ArrowLeft => Self::Left,
            NamedKey::ArrowRight => Self::Right,
            NamedKey::Home => Self::Home,
            NamedKey::End => Self::End,
            _ => return None,
        })
    }

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

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The text, taken, leaving the line empty.
    pub(crate) fn take(&mut self) -> String {
        self.caret = 0;
        self.walk = None;
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
        self.text.replace_range(start..self.caret, with);
        self.caret = start + with.len();
    }

    /// Makes the line `text`, with the caret at its end, keeping where Up
    /// and Down have gone.
    fn set(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.caret = self.text.len();
    }

    /// Takes `press`, a key typed into the window's prompt.
    pub(crate) fn key(&mut self, press: &KeyPress<'_>) -> Keyed {
        let ctrl = press.modifiers.ctrl;
        let edit = match press.key {
            Key::Named(named) => Edit::named(*named, ctrl),
            Key::Character(text) if ctrl => text.chars().next().and_then(Edit::ctrl),
            _ => None,
        };
        if let Some(edit) = edit {
            return self.edit(edit);
        }
        if ctrl {
            return Keyed::Other;
        }
        let text = press.text.or(match press.key {
            Key::Character(text) => Some(text.as_str()),
            Key::Named(NamedKey::Space) => Some(" "),
            _ => None,
        });
        match text.filter(|text| !text.chars().all(char::is_control)) {
            Some(text) => {
                self.put(text);
                Keyed::Edited
            }
            None => Keyed::Other,
        }
    }

    /// Takes `stroke`, a key typed into an editor's prompt.
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
            Stroke::Home => Some(Edit::Home),
            Stroke::End => Some(Edit::End),
            _ => None,
        };
        edit.map_or(Keyed::Other, |edit| self.edit(edit))
    }

    fn edit(&mut self, edit: Edit) -> Keyed {
        if edit == Edit::Backspace && self.text.is_empty() {
            return Keyed::Emptied;
        }
        self.walk = None;
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
            Edit::WordLeft => self.caret = self.word_before(caret),
            Edit::WordRight => self.caret = self.word_after(caret),
            Edit::ClearBefore => {
                self.text.replace_range(..caret, "");
                self.caret = 0;
            }
            Edit::DeleteWord => {
                let start = self.word_before(caret);
                self.text.replace_range(start..caret, "");
                self.caret = start;
            }
            Edit::Register => self.register = true,
        }
        Keyed::Edited
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
    fn word_before(&self, at: usize) -> usize {
        let mut chars = self.text[..at].char_indices().rev().peekable();
        while chars.next_if(|(_, ch)| ch.is_whitespace()).is_some() {}
        let Some(&(_, first)) = chars.peek() else {
            return 0;
        };
        let class = class(first);
        let mut start = at;
        while let Some((at, _)) = chars.next_if(|&(_, ch)| self::class(ch) == class) {
            start = at;
        }
        start.min(at)
    }

    /// Where the next word after `at` starts, or the end.
    fn word_after(&self, at: usize) -> usize {
        let mut chars = self.text[at..].char_indices().peekable();
        if let Some(&(_, first)) = chars.peek() {
            let class = class(first);
            while chars
                .next_if(|&(_, ch)| !ch.is_whitespace() && self::class(ch) == class)
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
/// anything else.
fn class(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
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
    fn the_windows_keys_edit_too_ctrl_moving_by_words() {
        use winit::keyboard::KeyLocation;

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
}
