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
    /// Shift+Left: `b` in normal mode, as Vim's `<S-Left>`.
    ShiftLeft,
    /// Shift+Right: `w`.
    ShiftRight,
    /// Ctrl+Left: `B`, as Vim's `<C-Left>`.
    CtrlLeft,
    /// Ctrl+Right: `W`.
    CtrlRight,
    /// Ctrl+Home: `gg`.
    CtrlHome,
    /// Ctrl+End: the last line's last character.
    CtrlEnd,
    /// Shift+Up, in normal mode: Page Up, as Vim's `<S-Up>`.
    ShiftUp,
    /// Shift+Down, in normal mode: Page Down.
    ShiftDown,
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

    /// The character it types as what `f` or `r` takes: a plain one, or a
    /// tab.
    pub(crate) fn typed(self) -> Option<char> {
        match self {
            Self::Tab => Some('\t'),
            stroke => stroke.plain(),
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
        let shift = |named| Chord::with(Base::Named(named), false, false, true);
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
            Self::ShiftLeft => shift(Named::Left),
            Self::ShiftRight => shift(Named::Right),
            Self::CtrlLeft => ctrl(Named::Left),
            Self::CtrlRight => ctrl(Named::Right),
            Self::CtrlHome => ctrl(Named::Home),
            Self::CtrlEnd => ctrl(Named::End),
            Self::ShiftUp => shift(Named::Up),
            Self::ShiftDown => shift(Named::Down),
        }
    }
}

/// The strokes `press` makes, for a pane that reads Vim's keys, in insert
/// mode or a prompt if `typing`: Ctrl or Shift with Left or Right, and
/// Ctrl with Home or End, as strokes of their own, which each mode reads as
/// Vim's does; Shift with Up or Down too, but only in normal mode; and
/// otherwise as [`plain_strokes`] makes them.
pub(crate) fn strokes(press: &KeyPress<'_>, typing: bool) -> Vec<Stroke> {
    let modifiers = press.modifiers;
    let (ctrl, shift) = (modifiers.ctrl, modifiers.shift);
    if (ctrl || shift) && !modifiers.alt {
        let vim = match press.key {
            Key::Named(NamedKey::ArrowLeft) if ctrl => Some(Stroke::CtrlLeft),
            Key::Named(NamedKey::ArrowRight) if ctrl => Some(Stroke::CtrlRight),
            Key::Named(NamedKey::ArrowLeft) => Some(Stroke::ShiftLeft),
            Key::Named(NamedKey::ArrowRight) => Some(Stroke::ShiftRight),
            Key::Named(NamedKey::ArrowUp) if shift && !typing => Some(Stroke::ShiftUp),
            Key::Named(NamedKey::ArrowDown) if shift && !typing => Some(Stroke::ShiftDown),
            Key::Named(NamedKey::Home) if ctrl => Some(Stroke::CtrlHome),
            Key::Named(NamedKey::End) if ctrl => Some(Stroke::CtrlEnd),
            _ => None,
        };
        if let Some(stroke) = vim {
            return vec![stroke];
        }
    }
    plain_strokes(press, typing)
}

/// The strokes `press` makes typed into a prompt, as [`strokes`] makes
/// them, but for two keys more useful there: Ctrl+Backspace is Ctrl+W,
/// deleting a word, and Alt and a key types the key's character, if it
/// types one, so a layout that holds Alt for a character still types it.
pub(crate) fn prompt_strokes(press: &KeyPress<'_>) -> Vec<Stroke> {
    let modifiers = press.modifiers;
    if modifiers.ctrl && *press.key == Key::Named(NamedKey::Backspace) {
        return vec![Stroke::ctrl('w')];
    }
    if modifiers.alt && !modifiers.ctrl && !enters_normal_mode(press) {
        let text = press.text.or(match press.key {
            Key::Character(text) => Some(text.as_str()),
            Key::Named(NamedKey::Space) => Some(" "),
            _ => None,
        });
        let typed: Vec<Stroke> = (text.unwrap_or_default().chars())
            .filter(|ch| !ch.is_control())
            .map(Stroke::char)
            .collect();
        if !typed.is_empty() {
            return typed;
        }
        // Alt and a named key, as Left or Backspace, is the key alone.
        if let Key::Named(_) = press.key {
            let without_alt = KeyPress {
                modifiers: crate::input::Modifiers {
                    alt: false,
                    ..modifiers
                },
                ..*press
            };
            return strokes(&without_alt, true);
        }
    }
    strokes(press, true)
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
    // Typed, what a key types after dead keys and compose comes first: `'`
    // then Space types `'`, and a key the layout can't name types its text.
    let typed = |text: Option<&str>| -> Vec<Stroke> {
        (text.unwrap_or_default().chars())
            .filter(|ch| !ch.is_control())
            .map(Stroke::char)
            .collect()
    };
    if typing
        && !modifiers.ctrl
        && matches!(
            press.key,
            Key::Named(NamedKey::Space) | Key::Unidentified(_)
        )
    {
        let text = typed(press.text);
        if !text.is_empty() {
            return text;
        }
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

    /// A press of `key`, typing `text`, with `modifiers`.
    fn pressed(key: &Key, text: Option<&str>, modifiers: crate::input::Modifiers) -> Vec<Stroke> {
        let press = KeyPress {
            key,
            text,
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers,
        };
        strokes(&press, true)
    }

    #[test]
    fn what_a_dead_key_and_space_type_is_typed() {
        use winit::keyboard::NativeKey;
        let plain = crate::input::Modifiers::default();
        let space = Key::Named(NamedKey::Space);
        // `'` then Space, with compose, is Space typing `'`.
        assert_eq!(pressed(&space, Some("'"), plain), [Stroke::char('\'')]);
        assert_eq!(pressed(&space, Some(" "), plain), [Stroke::char(' ')]);
        // In normal mode, Space is Space.
        let press = KeyPress {
            key: &space,
            text: Some("'"),
            location: winit::keyboard::KeyLocation::Standard,
            code: None,
            modifiers: plain,
        };
        assert_eq!(strokes(&press, false), [Stroke::char(' ')]);
        // A key the layout doesn't name types its text.
        let unnamed = Key::Unidentified(NativeKey::Unidentified);
        assert_eq!(pressed(&unnamed, Some("é"), plain), [Stroke::char('é')]);
    }

    #[test]
    fn alt_and_a_named_key_in_a_prompt_is_the_key() {
        let alt = crate::input::Modifiers {
            alt: true,
            ..crate::input::Modifiers::default()
        };
        let key = |named| {
            let key = Key::Named(named);
            prompt_strokes(&KeyPress {
                key: &key,
                text: None,
                location: winit::keyboard::KeyLocation::Standard,
                code: None,
                modifiers: alt,
            })
        };
        assert_eq!(key(NamedKey::ArrowLeft), [Stroke::Left]);
        assert_eq!(key(NamedKey::Backspace), [Stroke::Backspace]);
        assert_eq!(key(NamedKey::End), [Stroke::End]);
    }
}
