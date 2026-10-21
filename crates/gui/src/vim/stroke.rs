//! Keys as the grammar reads them: [`Stroke`], and [`strokes`], which
//! turns a key pressed into them.

use winit::keyboard::{Key, KeyCode, NamedKey};

use crate::input::KeyPress;
use crate::keymap::{Base, Chord, Named};

/// One key, as the grammar reads it.
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
    /// Ctrl+Left or Shift+Left: a word back, as Vim's `<C-Left>`.
    WordBack,
    /// Ctrl+Right or Shift+Right: a word on.
    WordForward,
    /// Ctrl+Home: the text's first line.
    TextStart,
    /// Ctrl+End: its last.
    TextEnd,
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

    /// The keys `text` writes down, for tests, as `@` types a register
    /// holding it (see [`crate::keymap::read_macro`]): a character for
    /// itself, and a key that types none by its name in angle brackets,
    /// `<Esc>`, `<CR>` or `<C-r>`, with `<lt>` for `<`. A `<` that starts
    /// no key's name is itself, and line breaks and the other control
    /// characters are the keys that type them.
    #[cfg(test)]
    pub(crate) fn read(text: &str) -> Vec<Self> {
        (crate::keymap::read_macro(text).into_iter())
            .flat_map(|chord| strokes(&chord.kept().press(), false))
            .collect()
    }

    /// The stroke as the bar shows it in a command half typed, as Vim's
    /// `showcmd` does: a character as itself, Ctrl and one as `^W`, and
    /// another key by its name, as `<Tab>`.
    pub(crate) fn shown(self) -> String {
        match self {
            Self::Char { ch, ctrl: false } => ch.to_string(),
            Self::Char { ch, ctrl: true } => format!("^{}", ch.to_ascii_uppercase()),
            stroke => stroke.chord().to_string(),
        }
    }

    /// The key that types the stroke, as a macro records it: for what a
    /// completion menu put in, which is recorded in place of the key that
    /// took it.
    pub(crate) fn chord(self) -> Chord {
        let named = |named| Chord::with(Base::Named(named), false, false, false);
        let ctrl = |named| Chord::with(Base::Named(named), true, false, false);
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
            Self::WordBack => ctrl(Named::Left),
            Self::WordForward => ctrl(Named::Right),
            Self::TextStart => ctrl(Named::Home),
            Self::TextEnd => ctrl(Named::End),
        }
    }
}

/// The strokes `press` makes, for a pane that reads Vim's keys, in insert
/// mode or a prompt if `typing`: Ctrl or Shift with an arrow, Home or End
/// as Vim reads them, Ctrl+Left as `b` and Shift+Up as Page Up, and
/// otherwise as [`plain_strokes`] makes them.
pub(crate) fn strokes(press: &KeyPress<'_>, typing: bool) -> Vec<Stroke> {
    let modifiers = press.modifiers;
    if (modifiers.ctrl || modifiers.shift) && !modifiers.alt {
        let vim = match press.key {
            Key::Named(NamedKey::ArrowLeft) => Some(Stroke::WordBack),
            Key::Named(NamedKey::ArrowRight) => Some(Stroke::WordForward),
            Key::Named(NamedKey::ArrowUp) if modifiers.shift => Some(Stroke::PageUp),
            Key::Named(NamedKey::ArrowDown) if modifiers.shift => Some(Stroke::PageDown),
            Key::Named(NamedKey::Home) if modifiers.ctrl => Some(Stroke::TextStart),
            Key::Named(NamedKey::End) if modifiers.ctrl => Some(Stroke::TextEnd),
            _ => None,
        };
        if let Some(stroke) = vim {
            return vec![stroke];
        }
    }
    plain_strokes(press, typing)
}

/// The strokes `press` makes, for a pane that doesn't read Vim's keys, in
/// a prompt if `typing`: Ctrl and Shift with a named key are the key alone,
/// Alt and a key is none, and Ctrl+;, Shift+Esc and Ctrl+\ are Esc.
pub(crate) fn plain_strokes(press: &KeyPress<'_>, typing: bool) -> Vec<Stroke> {
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
