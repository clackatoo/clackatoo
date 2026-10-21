//! Keys as the grammar reads them: [`Stroke`], and [`strokes`], which
//! turns a key pressed into them.

use winit::keyboard::{Key, KeyCode, NamedKey};

use crate::input::KeyPress;
use crate::keymap::{Base, Chord, Named};

/// One key, as an editor reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stroke {
    /// A character, typed with Ctrl if `ctrl`.
    Char {
        ch: char,
        ctrl: bool,
    },
    Escape,
    Enter,
    Tab,
    /// Shift+Tab, which goes back a place in a snippet, and is Tab
    /// otherwise.
    BackTab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

impl Stroke {
    pub(crate) fn char(ch: char) -> Self {
        Self::Char { ch, ctrl: false }
    }

    pub(crate) fn ctrl(ch: char) -> Self {
        Self::Char { ch, ctrl: true }
    }

    pub(crate) fn plain(self) -> Option<char> {
        match self {
            Self::Char { ch, ctrl: false } => Some(ch),
            _ => None,
        }
    }

    /// The keys `text` writes down, for tests: a character for itself,
    /// and a key that types none by its name in angle brackets, `<Esc>`,
    /// `<CR>` or `<C-r>`, with `<lt>` for `<`. A `<` that starts no name
    /// is itself. Line breaks are Enter, and the other control characters
    /// the keys that type them.
    #[cfg(test)]
    pub(crate) fn read(text: &str) -> Vec<Self> {
        let mut strokes = Vec::with_capacity(text.len());
        let mut rest = text;
        while let Some(ch) = rest.chars().next() {
            if ch == '<'
                && let Some(end) = rest[1..].find('>').map(|end| end + 1)
                && let Some(stroke) = Self::named(&rest[1..end])
            {
                strokes.push(stroke);
                rest = &rest[end + 1..];
                continue;
            }
            strokes.push(match ch {
                '\n' | '\r' => Self::Enter,
                '\t' => Self::Tab,
                '\u{1b}' => Self::Escape,
                '\u{8}' | '\u{7f}' => Self::Backspace,
                // Ctrl+A to Ctrl+Z.
                '\u{1}'..='\u{1a}' => Self::ctrl(char::from(ch as u8 + b'a' - 1)),
                _ => Self::char(ch),
            });
            rest = &rest[ch.len_utf8()..];
        }
        strokes
    }

    /// The key called `name`, between angle brackets, if it's one.
    #[cfg(test)]
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "Esc" => Self::Escape,
            "CR" => Self::Enter,
            "Tab" => Self::Tab,
            "S-Tab" => Self::BackTab,
            "BS" => Self::Backspace,
            "Del" => Self::Delete,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "lt" => Self::char('<'),
            "Space" => Self::char(' '),
            "C-Space" => Self::ctrl(' '),
            _ => {
                let mut key = name.strip_prefix("C-")?.chars();
                let ch = key.next()?;
                if key.next().is_some() {
                    return None;
                }
                Self::ctrl(ch)
            }
        })
    }

    /// The key that types the stroke, as a macro records it: for what a
    /// completion menu put in, which is recorded in place of the key that
    /// took it.
    pub(crate) fn chord(self) -> Chord {
        let named = |named| Chord::with(Base::Named(named), false, false, false);
        match self {
            Self::Char { ch, ctrl } => Chord::with(Base::Char(ch), ctrl, false, false),
            Self::Escape => named(Named::Escape),
            Self::Enter => named(Named::Enter),
            Self::Tab => named(Named::Tab),
            Self::BackTab => Chord::with(Base::Named(Named::Tab), false, false, true),
            Self::Backspace => named(Named::Backspace),
            Self::Delete => named(Named::Delete),
            Self::Left => named(Named::Left),
            Self::Right => named(Named::Right),
            Self::Up => named(Named::Up),
            Self::Down => named(Named::Down),
            Self::Home => named(Named::Home),
            Self::End => named(Named::End),
            Self::PageUp => named(Named::PageUp),
            Self::PageDown => named(Named::PageDown),
        }
    }
}

pub(crate) fn strokes(press: &KeyPress<'_>, typing: bool) -> Vec<Stroke> {
    if enters_normal_mode(press) {
        return vec![Stroke::Escape];
    }
    let modifiers = press.modifiers;
    if modifiers.alt {
        return Vec::new();
    }
    let named = match press.key {
        Key::Named(NamedKey::Escape) => Some(Stroke::Escape),
        Key::Named(NamedKey::Enter) => Some(Stroke::Enter),
        Key::Named(NamedKey::Tab) if modifiers.shift => Some(Stroke::BackTab),
        Key::Named(NamedKey::Tab) => Some(Stroke::Tab),
        Key::Named(NamedKey::Backspace) => Some(Stroke::Backspace),
        Key::Named(NamedKey::Delete) => Some(Stroke::Delete),
        Key::Named(NamedKey::ArrowLeft) => Some(Stroke::Left),
        Key::Named(NamedKey::ArrowRight) => Some(Stroke::Right),
        Key::Named(NamedKey::ArrowUp) => Some(Stroke::Up),
        Key::Named(NamedKey::ArrowDown) => Some(Stroke::Down),
        Key::Named(NamedKey::Home) => Some(Stroke::Home),
        Key::Named(NamedKey::End) => Some(Stroke::End),
        Key::Named(NamedKey::PageUp) => Some(Stroke::PageUp),
        Key::Named(NamedKey::PageDown) => Some(Stroke::PageDown),
        Key::Named(NamedKey::Space) if !modifiers.ctrl => Some(Stroke::char(' ')),
        Key::Named(NamedKey::Space) => Some(Stroke::ctrl(' ')),
        _ => None,
    };
    if let Some(named) = named {
        return vec![named];
    }
    let Key::Character(key) = press.key else {
        return Vec::new();
    };
    if modifiers.ctrl {
        return key.chars().next().map(Stroke::ctrl).into_iter().collect();
    }
    let text = if typing {
        press.text.unwrap_or(key)
    } else {
        key
    };
    text.chars()
        .filter(|ch| !ch.is_control())
        .map(Stroke::char)
        .collect()
}

/// Whether `press` is Ctrl+\\.
pub(crate) fn is_ctrl_backslash(press: &KeyPress<'_>) -> bool {
    press.modifiers.ctrl
        && !press.modifiers.alt
        && matches!(press.key, Key::Character(text) if text == "\\")
}

/// Whether `press` is one of the keys that switch to normal mode from
/// anywhere: Ctrl+;, Shift+Esc or Ctrl+\\.
pub(crate) fn enters_normal_mode(press: &KeyPress<'_>) -> bool {
    let modifiers = press.modifiers;
    let shift_escape = modifiers.shift && *press.key == Key::Named(NamedKey::Escape);
    // By its place on the keyboard, so it's the same keys on layouts
    // where `;` takes Shift.
    let ctrl_semicolon = modifiers.ctrl && !modifiers.alt && press.code == Some(KeyCode::Semicolon);
    shift_escape || ctrl_semicolon || is_ctrl_backslash(press)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strokes_are_the_keys_that_type_them_as_vim_writes_them() {
        let strokes = [
            Stroke::char('i'),
            Stroke::char('<'),
            Stroke::char(' '),
            Stroke::Enter,
            Stroke::ctrl('w'),
            Stroke::ctrl(' '),
            Stroke::Left,
            Stroke::Escape,
        ];
        let chords: Vec<Chord> = strokes.iter().map(|stroke| stroke.chord()).collect();
        let written = crate::keymap::write_macro(&chords);
        assert_eq!(written, "i<lt> <CR><C-w><C-Space><Left><Esc>");
        assert_eq!(Stroke::read(&written), strokes);
    }
}
