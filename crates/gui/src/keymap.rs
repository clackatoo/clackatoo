//! Key bindings: sequences of keys, written as in Vim (`<C-b>"`), bound in
//! one or more modes to something to do.
//!
//! A key that starts a longer binding waits for the rest. The sequence goes
//! on until it matches a binding, or a key matches none, which drops the
//! keys typed so far, that key included. Nothing times out: Ctrl+B waits
//! for its second key as long as it takes, as in tmux. So a binding whose
//! keys start a longer one's is never reached.
//!
//! In the notation, a character is itself, and `<…>` holds a key with
//! modifiers (`C-` Ctrl, `A-` Alt, `S-` Shift), or a named key such as
//! `<Esc>` or `<F5>`, ignoring case. Shift is part of a character: `<C-O>`
//! is Ctrl+Shift+O, and `<C-S-o>` means the same. `<lt>` is `<`.

use std::collections::BTreeMap;
use std::fmt;

use winit::keyboard::{Key, KeyCode, NamedKey};

use crate::input::{KeptPress, KeyPress, Modifiers};

/// The modes a binding can apply in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum KeyMode {
    Normal,
    Insert,
    Visual,
    /// Typing in a pane's prompt, such as a search.
    Prompt,
}

impl KeyMode {
    const ALL: [Self; 4] = [Self::Normal, Self::Insert, Self::Visual, Self::Prompt];

    /// The letter a mode is written as.
    fn letter(self) -> char {
        match self {
            Self::Normal => 'n',
            Self::Insert => 'i',
            Self::Visual => 'v',
            Self::Prompt => 'c',
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// The modes written as their letters, such as `"niv"`.
pub(crate) fn parse_modes(letters: &str) -> Result<Vec<KeyMode>, String> {
    let mut modes = Vec::new();
    for letter in letters.chars() {
        let mode = (KeyMode::ALL.into_iter())
            .find(|mode| mode.letter() == letter)
            .ok_or_else(|| format!("there's no mode `{letter}`: the modes are n, i, v and c"))?;
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    if modes.is_empty() {
        return Err("a binding needs a mode: n, i, v or c".into());
    }
    Ok(modes)
}

/// A named key a binding can have.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Named {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Space,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
}

/// Function key `n`, from F1 to F12.
fn function_key(n: u8) -> NamedKey {
    const KEYS: [NamedKey; 12] = [
        NamedKey::F1,
        NamedKey::F2,
        NamedKey::F3,
        NamedKey::F4,
        NamedKey::F5,
        NamedKey::F6,
        NamedKey::F7,
        NamedKey::F8,
        NamedKey::F9,
        NamedKey::F10,
        NamedKey::F11,
        NamedKey::F12,
    ];
    KEYS[usize::from(n.clamp(1, 12)) - 1]
}

/// The named keys by name, the first of each being how it's written.
const NAMES: [(&str, Named); 18] = [
    ("Esc", Named::Escape),
    ("Escape", Named::Escape),
    ("CR", Named::Enter),
    ("Enter", Named::Enter),
    ("Return", Named::Enter),
    ("Tab", Named::Tab),
    ("BS", Named::Backspace),
    ("Del", Named::Delete),
    ("Insert", Named::Insert),
    ("Space", Named::Space),
    ("Up", Named::Up),
    ("Down", Named::Down),
    ("Left", Named::Left),
    ("Right", Named::Right),
    ("Home", Named::Home),
    ("End", Named::End),
    ("PageUp", Named::PageUp),
    ("PageDown", Named::PageDown),
];

/// What's pressed, besides the modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Base {
    /// A character, with Shift applied.
    Char(char),
    Named(Named),
}

/// One key, with the modifiers held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Chord {
    pub(crate) base: Base,
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
    /// Only for named keys: a character has Shift in it.
    pub(crate) shift: bool,
}

impl Chord {
    /// A character with no modifiers.
    #[cfg(test)]
    pub(crate) const fn char(ch: char) -> Self {
        Self {
            base: Base::Char(ch),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// A press of the key, as if from the keyboard, to type it.
    pub(crate) fn kept(self) -> KeptPress {
        let modifiers = Modifiers {
            shift: self.shift || matches!(self.base, Base::Char(ch) if ch.is_uppercase()),
            ctrl: self.ctrl,
            alt: self.alt,
        };
        let plain = !self.ctrl && !self.alt;
        let (key, text) = match self.base {
            Base::Char(ch) => (
                Key::Character(ch.to_string().into()),
                plain.then(|| ch.to_string()),
            ),
            Base::Named(named) => {
                let (named, text) = match named {
                    Named::Escape => (NamedKey::Escape, None),
                    Named::Enter => (NamedKey::Enter, Some("\r")),
                    Named::Tab => (NamedKey::Tab, Some("\t")),
                    Named::Backspace => (NamedKey::Backspace, None),
                    Named::Delete => (NamedKey::Delete, None),
                    Named::Insert => (NamedKey::Insert, None),
                    Named::Space => (NamedKey::Space, Some(" ")),
                    Named::Up => (NamedKey::ArrowUp, None),
                    Named::Down => (NamedKey::ArrowDown, None),
                    Named::Left => (NamedKey::ArrowLeft, None),
                    Named::Right => (NamedKey::ArrowRight, None),
                    Named::Home => (NamedKey::Home, None),
                    Named::End => (NamedKey::End, None),
                    Named::PageUp => (NamedKey::PageUp, None),
                    Named::PageDown => (NamedKey::PageDown, None),
                    Named::F(n) => (function_key(n), None),
                };
                (Key::Named(named), text.filter(|_| plain).map(str::to_owned))
            }
        };
        // Ctrl+; is found by where it is on the keyboard.
        let code = matches!(self.base, Base::Char(';')).then_some(KeyCode::Semicolon);
        KeptPress::new(key, text, code, modifiers)
    }

    /// The key `press` is, if a binding can have it. A letter is upper
    /// case with Shift and lower case without, whatever Caps Lock says.
    pub(crate) fn of(press: &KeyPress<'_>) -> Option<Self> {
        let modifiers = press.modifiers;
        let base = match press.key {
            Key::Character(text) => {
                let mut chars = text.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    return None;
                };
                Base::Char(case(ch, modifiers.shift))
            }
            Key::Named(named) => Base::Named(match named {
                NamedKey::Escape => Named::Escape,
                NamedKey::Enter => Named::Enter,
                NamedKey::Tab => Named::Tab,
                NamedKey::Backspace => Named::Backspace,
                NamedKey::Delete => Named::Delete,
                NamedKey::Insert => Named::Insert,
                NamedKey::Space => Named::Space,
                NamedKey::ArrowUp => Named::Up,
                NamedKey::ArrowDown => Named::Down,
                NamedKey::ArrowLeft => Named::Left,
                NamedKey::ArrowRight => Named::Right,
                NamedKey::Home => Named::Home,
                NamedKey::End => Named::End,
                NamedKey::PageUp => Named::PageUp,
                NamedKey::PageDown => Named::PageDown,
                NamedKey::F1 => Named::F(1),
                NamedKey::F2 => Named::F(2),
                NamedKey::F3 => Named::F(3),
                NamedKey::F4 => Named::F(4),
                NamedKey::F5 => Named::F(5),
                NamedKey::F6 => Named::F(6),
                NamedKey::F7 => Named::F(7),
                NamedKey::F8 => Named::F(8),
                NamedKey::F9 => Named::F(9),
                NamedKey::F10 => Named::F(10),
                NamedKey::F11 => Named::F(11),
                NamedKey::F12 => Named::F(12),
                _ => return None,
            }),
            _ => return None,
        };
        Some(Self::with(
            base,
            modifiers.ctrl,
            modifiers.alt,
            modifiers.shift,
        ))
    }

    /// How the key is said to people, as a panel of keys titles itself:
    /// `Ctrl+B`, `Space`, `p`.
    pub(crate) fn spoken(&self) -> String {
        let mut said = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if held {
                said.push_str(name);
            }
        }
        match self.base {
            Base::Char(ch) if self.ctrl || self.alt => said.extend(ch.to_uppercase()),
            Base::Char(ch) => said.push(ch),
            Base::Named(Named::F(number)) => {
                said.push('F');
                said.push_str(&number.to_string());
            }
            Base::Named(named) => said.push_str(match named {
                Named::Escape => "Esc",
                Named::Enter => "Enter",
                Named::Backspace => "Backspace",
                Named::Delete => "Delete",
                named => {
                    let (name, _) = NAMES
                        .iter()
                        .find(|(_, each)| *each == named)
                        .expect("every named key has a name");
                    name
                }
            }),
        }
        said
    }

    /// The key written short, as Vim's `showcmd` writes keys waiting:
    /// `^B` for Ctrl+B, `␣` for Space, a character as itself, and the rest
    /// as they're bound.
    pub(crate) fn short(&self) -> String {
        match self.base {
            Base::Char(ch) if self.ctrl && !self.alt && ch.is_ascii_alphabetic() => {
                format!("^{}", ch.to_ascii_uppercase())
            }
            Base::Named(Named::Space) if !(self.ctrl || self.alt || self.shift) => "␣".to_owned(),
            _ => self.to_string(),
        }
    }

    /// A character typed with no modifiers, as text a key typed is
    /// recorded in a macro. A space is the space key.
    pub(crate) fn typed(ch: char) -> Self {
        Self::with(Base::Char(ch), false, false, false)
    }

    /// `base` with modifiers, Shift going into a character.
    pub(crate) fn with(base: Base, ctrl: bool, alt: bool, shift: bool) -> Self {
        // A space typed is the space key.
        let base = match base {
            Base::Char(' ') => Base::Named(Named::Space),
            base => base,
        };
        match base {
            Base::Char(ch) => Self {
                base: Base::Char(if shift { case(ch, true) } else { ch }),
                ctrl,
                alt,
                shift: false,
            },
            Base::Named(_) => Self {
                base,
                ctrl,
                alt,
                shift,
            },
        }
    }
}

/// `ch` in upper case with Shift, lower case without, if it's a letter
/// with cases.
fn case(ch: char, shift: bool) -> char {
    fn only(ch: char, mut changed: impl Iterator<Item = char>) -> char {
        match (changed.next(), changed.next()) {
            (Some(one), None) => one,
            _ => ch,
        }
    }
    if shift {
        only(ch, ch.to_uppercase())
    } else {
        only(ch, ch.to_lowercase())
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let plain = !(self.ctrl || self.alt || self.shift);
        match self.base {
            Base::Char('<') if plain => return f.write_str("<lt>"),
            Base::Char(ch) if plain => return write!(f, "{ch}"),
            _ => {}
        }
        f.write_str("<")?;
        for (held, letter) in [(self.ctrl, "C-"), (self.alt, "A-"), (self.shift, "S-")] {
            if held {
                f.write_str(letter)?;
            }
        }
        match self.base {
            Base::Char('<') => f.write_str("lt")?,
            Base::Char(ch) => write!(f, "{ch}")?,
            Base::Named(Named::F(number)) => write!(f, "F{number}")?,
            Base::Named(named) => {
                let (name, _) = NAMES
                    .iter()
                    .find(|(_, each)| *each == named)
                    .expect("every named key has a name");
                f.write_str(name)?;
            }
        }
        f.write_str(">")
    }
}

/// A sequence of keys, written as they're bound.
pub(crate) fn display(keys: &[Chord]) -> String {
    keys.iter().map(ToString::to_string).collect()
}

/// The keys `notation` writes, such as `<C-b>"`.
pub(crate) fn parse_keys(notation: &str) -> Result<Vec<Chord>, String> {
    let mut keys = Vec::new();
    let mut rest = notation;
    while let Some(ch) = rest.chars().next() {
        // `<` with no `>` after it, or another `<` first, is itself.
        if ch == '<'
            && let Some(end) = rest.find('>')
            && end > 1
            && !rest[1..end].contains('<')
        {
            keys.push(
                parse_angled(&rest[1..end])
                    .map_err(|err| format!("`{}` in `{notation}`: {err}", &rest[..=end]))?,
            );
            rest = &rest[end + 1..];
        } else {
            keys.push(Chord::with(Base::Char(ch), false, false, false));
            rest = &rest[ch.len_utf8()..];
        }
    }
    if keys.is_empty() {
        return Err("a binding needs at least one key".into());
    }
    Ok(keys)
}

/// `keys`, a macro, written down as `"ap` puts it: as bindings are
/// written, but with a space as itself.
pub(crate) fn write_macro(keys: &[Chord]) -> String {
    use fmt::Write as _;
    let mut text = String::with_capacity(keys.len());
    for key in keys {
        if *key == Chord::typed(' ') {
            text.push(' ');
        } else {
            write!(text, "{key}").expect("a String takes all that's written");
        }
    }
    text
}

/// The keys `text`, a register, holds, as `@` types them. Never fails, so
/// any text can be typed: a `<` that starts no key's name is itself, and
/// line breaks and the other control characters are the keys that type
/// them, so a line yanked from a file types as it reads.
pub(crate) fn read_macro(text: &str) -> Vec<Chord> {
    let named = |named| Chord::with(Base::Named(named), false, false, false);
    let mut keys = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        if ch == '<'
            && let Some(end) = rest.find('>')
            && end > 1
            && !rest[1..end].contains('<')
            && let Ok(key) = parse_angled(&rest[1..end])
        {
            keys.push(key);
            rest = &rest[end + 1..];
            continue;
        }
        keys.push(match ch {
            '\n' | '\r' => named(Named::Enter),
            '\t' => named(Named::Tab),
            '\u{1b}' => named(Named::Escape),
            '\u{8}' | '\u{7f}' => named(Named::Backspace),
            // Ctrl+A to Ctrl+Z.
            '\u{1}'..='\u{1a}' => {
                let letter = char::from(ch as u8 + b'a' - 1);
                Chord::with(Base::Char(letter), true, false, false)
            }
            _ => Chord::typed(ch),
        });
        rest = &rest[ch.len_utf8()..];
    }
    keys
}

/// What's inside `<…>`.
fn parse_angled(inside: &str) -> Result<Chord, String> {
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = inside;
    // A modifier is a letter and a dash, before at least one more
    // character: `<C-->` is Ctrl and a dash.
    while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
        match rest.as_bytes()[0].to_ascii_uppercase() {
            b'C' => ctrl = true,
            b'A' | b'M' => alt = true,
            b'S' => shift = true,
            _ => break,
        }
        rest = &rest[2..];
    }
    let mut chars = rest.chars();
    let base = match (chars.next(), chars.next()) {
        (Some(ch), None) => Base::Char(ch),
        _ if rest.eq_ignore_ascii_case("lt") => Base::Char('<'),
        _ => {
            let function = (rest.strip_prefix(['F', 'f']))
                .and_then(|number| number.parse::<u8>().ok())
                .filter(|number| (1..=12).contains(number));
            match function {
                Some(number) => Base::Named(Named::F(number)),
                None => Base::Named(
                    NAMES
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(rest))
                        .map(|&(_, named)| named)
                        .ok_or_else(|| format!("there's no key called `{rest}`"))?,
                ),
            }
        }
    };
    Ok(Chord::with(base, ctrl, alt, shift))
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pressed<T> {
    /// It finished a binding's keys.
    Bound(T),
    /// It started or continued a binding's keys, which wait for the rest.
    Waiting,
    /// It ended keys that matched no binding, which are typed as if no
    /// binding had them (see [`Desk::key`](crate::desk::Desk::key)).
    Dropped,
    /// No binding has it, so it goes on.
    Unbound,
}

/// A binding.
#[derive(Clone, Debug)]
struct Entry<T> {
    action: T,
    /// A word for what it does, for the hint shown while keys wait.
    label: Option<String>,
    /// When it was bound, so hints list keys in the order they were.
    order: u64,
}

/// A key the keys waiting can go on to: the binding it finishes, if it
/// finishes one, and those it starts the rest of, each with its label and
/// the key that comes after this one, in the order they were bound.
#[derive(Debug)]
pub(crate) struct Next<'a, T> {
    pub(crate) key: Chord,
    pub(crate) bound: Option<(&'a T, Option<&'a str>)>,
    pub(crate) further: Vec<(Chord, &'a T, Option<&'a str>)>,
}

/// Bindings in each mode, and the keys waiting for the rest of one.
#[derive(Clone, Debug)]
pub(crate) struct Keymap<T> {
    modes: [BTreeMap<Vec<Chord>, Entry<T>>; 4],
    /// The names of the bindings keys start, in each mode, for the panel
    /// of keys, as which-key.nvim's groups: `<Space>g` "git".
    groups: [BTreeMap<Vec<Chord>, String>; 4],
    waiting: Vec<Chord>,
    /// The mode the keys waiting were started in.
    waiting_mode: KeyMode,
    /// When the keys waiting started to.
    since: Option<std::time::Instant>,
    bound: u64,
}

impl<T> Default for Keymap<T> {
    fn default() -> Self {
        Self {
            modes: Default::default(),
            groups: Default::default(),
            waiting: Vec::new(),
            waiting_mode: KeyMode::Normal,
            since: None,
            bound: 0,
        }
    }
}

impl<T: Clone> Keymap<T> {
    /// Live bindings in this mode, narrowed to the pending prefix. A short
    /// binding hidden by a longer one is not executable by this resolver.
    pub(crate) fn available(&self, mode: KeyMode) -> Vec<(&[Chord], &T, Option<&str>)> {
        let mode = if self.waiting.is_empty() {
            mode
        } else {
            self.waiting_mode
        };
        let entries = &self.modes[mode.index()];
        entries
            .iter()
            .filter(|(keys, _)| {
                keys.starts_with(&self.waiting)
                    && !entries
                        .keys()
                        .any(|other| other.len() > keys.len() && other.starts_with(keys))
            })
            .map(|(keys, entry)| (keys.as_slice(), &entry.action, entry.label.as_deref()))
            .collect()
    }

    pub(crate) fn intercepts(&self, mode: KeyMode, keys: &[Chord]) -> bool {
        self.modes[mode.index()]
            .keys()
            .any(|bound| keys.starts_with(bound) || bound.starts_with(keys))
    }

    /// Binds `keys` in each of `modes`, replacing what they were bound to.
    pub(crate) fn bind(
        &mut self,
        modes: &[KeyMode],
        keys: &[Chord],
        action: T,
        label: Option<&str>,
    ) {
        self.bound += 1;
        for mode in modes {
            self.modes[mode.index()].insert(
                keys.to_vec(),
                Entry {
                    action: action.clone(),
                    label: label.map(str::to_owned),
                    order: self.bound,
                },
            );
        }
        self.waiting.clear();
    }

    /// Names the bindings `keys` start in each of `modes`, for the panel
    /// of keys, or with no name, stops naming them. Keys that finish a
    /// binding of their own are said by its label instead.
    pub(crate) fn name_group(&mut self, modes: &[KeyMode], keys: &[Chord], name: Option<&str>) {
        for mode in modes {
            let groups = &mut self.groups[mode.index()];
            match name {
                Some(name) => groups.insert(keys.to_vec(), name.to_owned()),
                None => groups.remove(keys),
            };
        }
    }

    /// Takes away the binding of `keys` in each of `modes`. Returns
    /// whether there was one in any.
    pub(crate) fn unbind(&mut self, modes: &[KeyMode], keys: &[Chord]) -> bool {
        let mut any = false;
        for mode in modes {
            any |= self.modes[mode.index()].remove(keys).is_some();
        }
        self.waiting.clear();
        any
    }

    /// Handles `chord`, pressed in `mode`.
    pub(crate) fn press(&mut self, mode: KeyMode, chord: Chord) -> Pressed<T> {
        if self.waiting.is_empty() {
            self.waiting_mode = mode;
        }
        let map = &self.modes[self.waiting_mode.index()];
        let had_waiting = !self.waiting.is_empty();
        self.waiting.push(chord);
        let exact = map.get(&self.waiting).map(|entry| entry.action.clone());
        let longer = (map.range(self.waiting.clone()..))
            .find(|(keys, _)| **keys != self.waiting)
            .is_some_and(|(keys, _)| keys.starts_with(&self.waiting));
        match (exact, longer) {
            (_, true) => {
                if !had_waiting {
                    self.since = Some(std::time::Instant::now());
                }
                Pressed::Waiting
            }
            (Some(action), false) => {
                self.waiting.clear();
                Pressed::Bound(action)
            }
            (None, false) => {
                self.waiting.clear();
                if had_waiting {
                    Pressed::Dropped
                } else {
                    Pressed::Unbound
                }
            }
        }
    }

    /// Stops waiting for the rest of a binding's keys. Returns whether it
    /// was waiting.
    pub(crate) fn cancel(&mut self) -> bool {
        let waiting = !self.waiting.is_empty();
        self.waiting.clear();
        waiting
    }

    /// The keys waiting for the rest of a binding.
    pub(crate) fn waiting(&self) -> &[Chord] {
        &self.waiting
    }

    /// Every binding, in every mode, by its keys, with its label.
    #[cfg(test)]
    pub(crate) fn labels(&self) -> Vec<(&[Chord], Option<&str>)> {
        (self.modes.iter())
            .flat_map(|mode| mode.iter())
            .map(|(keys, entry)| (keys.as_slice(), entry.label.as_deref()))
            .collect()
    }

    /// When the keys waiting started to, if any are.
    pub(crate) fn waiting_since(&self) -> Option<std::time::Instant> {
        self.since.filter(|_| !self.waiting.is_empty())
    }

    /// The keys waiting, written short, as the bottom bar marks them by
    /// the mode: `^B`.
    pub(crate) fn waiting_short(&self) -> Option<String> {
        (!self.waiting.is_empty()).then(|| self.waiting.iter().map(Chord::short).collect())
    }

    /// The keys the keys waiting can go on to, each once, in the order
    /// the first binding each reaches was bound.
    pub(crate) fn next(&self) -> Vec<Next<'_, T>> {
        let map = &self.modes[self.waiting_mode.index()];
        let mut next: Vec<(u64, Next<'_, T>)> = Vec::new();
        for (keys, entry) in map.range(self.waiting.clone()..) {
            if !keys.starts_with(&self.waiting) {
                break;
            }
            let Some(&key) = keys.get(self.waiting.len()) else {
                continue;
            };
            let at = if let Some(at) = next.iter().position(|(_, each)| each.key == key) {
                at
            } else {
                let found = Next {
                    key,
                    bound: None,
                    further: Vec::new(),
                };
                next.push((entry.order, found));
                next.len() - 1
            };
            let (first, found) = &mut next[at];
            *first = (*first).min(entry.order);
            let label = entry.label.as_deref();
            match keys.get(self.waiting.len() + 1) {
                None => found.bound = Some((&entry.action, label)),
                Some(&after) => found.further.push((after, &entry.action, label)),
            }
        }
        next.sort_by_key(|(order, _)| *order);
        next.into_iter().map(|(_, next)| next).collect()
    }

    /// What a panel lists while keys wait, as which-key.nvim does: the
    /// keys waiting, and a row for each key that can come next, saying
    /// what it does. That's the label of the binding it finishes; a
    /// binding with no label is said by `unlabelled`. A key that starts
    /// others says `+` and the name of their group if they're named, or
    /// the label they share, or else how many keys can come after it.
    /// Keys that finish bindings labelled the same
    /// share a row, 1 to 9 written `1-9`; every other key has a row of its
    /// own.
    pub(crate) fn which_keys(&self, unlabelled: impl Fn(&T) -> String) -> Option<WhichKeys> {
        if self.waiting.is_empty() {
            return None;
        }
        // Each row's keys, what they do, and whether they share a label.
        let mut rows: Vec<(Vec<String>, String, bool)> = Vec::new();
        let groups = &self.groups[self.waiting_mode.index()];
        let mut keys = self.waiting.clone();
        for next in self.next() {
            keys.push(next.key);
            let group = groups.get(&keys);
            keys.pop();
            let (does, shares) = match (next.bound, next.further.as_slice()) {
                (Some((_, Some(label))), _) => (label.to_owned(), true),
                (Some((action, None)), _) => (unlabelled(action), false),
                (None, _) if let Some(group) = group => (format!("+{group}"), false),
                (None, further) => (Self::starts(further, &unlabelled), false),
            };
            let joins = shares
                .then(|| (rows.iter_mut()).find(|(_, each, shares)| *shares && *each == does))
                .flatten();
            match joins {
                Some((keys, _, _)) => keys.push(next.key.short()),
                None => rows.push((vec![next.key.short()], does, shares)),
            }
        }
        Some(WhichKeys {
            title: (self.waiting.iter().map(Chord::spoken))
                .collect::<Vec<_>>()
                .join(" "),
            rows: (rows.into_iter())
                .map(|(keys, does, _)| (digits_together(&keys).join(" "), does))
                .collect(),
        })
    }

    /// What a key that starts the bindings `further` says it does: `+` and
    /// the label they share, or else how many keys can come after it.
    fn starts(further: &[(Chord, &T, Option<&str>)], unlabelled: &impl Fn(&T) -> String) -> String {
        let said = |(_, action, label): &(Chord, &T, Option<&str>)| {
            label.map_or_else(|| unlabelled(action), str::to_owned)
        };
        let first = further.first().map(said);
        if let Some(first) = first
            && further.iter().all(|each| said(each) == first)
        {
            return format!("+{first}");
        }
        let mut after: Vec<Chord> = further.iter().map(|(key, _, _)| *key).collect();
        after.sort_unstable();
        after.dedup();
        format!("+{} keys", after.len())
    }
}

/// What a panel of keys shows while keys wait for the rest of a binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WhichKeys {
    /// The keys waiting, said in full, as the panel's title: `Ctrl+B`.
    pub(crate) title: String,
    /// Each key that can come next, or several that do the same, and what
    /// it does.
    pub(crate) rows: Vec<(String, String)>,
}

/// `keys`, with 1 to 9 written `1-9` if they're all there.
fn digits_together(keys: &[String]) -> Vec<String> {
    let digits: Vec<String> = (1..=9).map(|digit| digit.to_string()).collect();
    if !digits.iter().all(|digit| keys.contains(digit)) {
        return keys.to_vec();
    }
    let mut out = Vec::new();
    for key in keys {
        if key == "1" {
            out.push("1-9".to_owned());
        } else if !digits.contains(key) {
            out.push(key.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use winit::keyboard::KeyLocation;

    use super::*;
    use crate::input::Modifiers;

    fn keys(notation: &str) -> Vec<Chord> {
        parse_keys(notation).unwrap()
    }

    // By value, so call sites can build the key inline.
    #[allow(clippy::needless_pass_by_value)]
    fn press(key: Key, ctrl: bool, shift: bool) -> Option<Chord> {
        Chord::of(&KeyPress {
            key: &key,
            text: None,
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers {
                shift,
                ctrl,
                alt: false,
            },
        })
    }

    #[test]
    fn keys_are_written_as_in_vim() {
        let ctrl_b = Chord {
            ctrl: true,
            ..Chord::char('b')
        };
        assert_eq!(keys("<C-b>\""), [ctrl_b, Chord::char('"')]);
        assert_eq!(keys("<c-B>"), keys("<C-S-b>"), "Shift is in the letter");
        assert_eq!(keys("<lt><Space> "), keys("<<space><Space>"));
        assert_eq!(
            keys("<S-Tab><F12><C-->"),
            [
                Chord {
                    shift: true,
                    ..Chord::with(Base::Named(Named::Tab), false, false, false)
                },
                Chord::with(Base::Named(Named::F(12)), false, false, false),
                Chord {
                    ctrl: true,
                    ..Chord::char('-')
                },
            ]
        );
        // A lone `<` is itself.
        assert_eq!(keys("<"), [Chord::char('<')]);
        assert!(parse_keys("<Hyper-x>").is_err());
        assert!(parse_keys("").is_err());
        // And they're written back the same way.
        for notation in [
            "<C-b>\"", "<lt>", "<C-O>", "<S-Tab>", "<A-h>", "<Space>", "gg", "<F5>",
        ] {
            assert_eq!(display(&keys(notation)), notation);
        }
        assert_eq!(parse_modes("niv").unwrap().len(), 3);
        assert!(parse_modes("nx").is_err());
    }

    #[test]
    fn macros_are_written_as_keys_and_any_text_reads_as_keys() {
        let macro_keys = keys(":w<CR>i a<lt><Esc><C-l><Space>x<S-Tab><A-h>");
        let written = write_macro(&macro_keys);
        assert_eq!(written, ":w<CR>i a<lt><Esc><C-l> x<S-Tab><A-h>");
        assert_eq!(read_macro(&written), macro_keys);
        // A `<` that names no key is itself, and so is one before another
        // `<`; line breaks and control characters are the keys typing them.
        let mut expected: Vec<Chord> = "a<b <C-ww>x<<".chars().map(Chord::typed).collect();
        expected.extend(keys("<CR><Tab><Esc><C-r>"));
        assert_eq!(read_macro("a<b <C-ww>x<<\n\t\u{1b}\u{12}"), expected);
        assert!(read_macro("").is_empty());
    }

    #[test]
    fn a_press_is_the_key_written() {
        let ch = |text: &str| Key::Character(text.into());
        assert_eq!(press(ch("b"), true, false), Some(keys("<C-b>")[0]));
        // Caps Lock makes the letter upper case without Shift.
        assert_eq!(press(ch("B"), true, false), Some(keys("<C-b>")[0]));
        assert_eq!(press(ch("O"), true, true), Some(keys("<C-O>")[0]));
        assert_eq!(press(ch("|"), false, true), Some(keys("|")[0]));
        assert_eq!(
            press(Key::Named(NamedKey::Tab), false, true),
            Some(keys("<S-Tab>")[0])
        );
        assert_eq!(
            press(Key::Named(NamedKey::Space), false, false),
            Some(keys(" ")[0])
        );
    }

    fn type_in(
        map: &mut Keymap<&'static str>,
        mode: KeyMode,
        notation: &str,
    ) -> Vec<Pressed<&'static str>> {
        keys(notation)
            .into_iter()
            .map(|key| map.press(mode, key))
            .collect()
    }

    #[test]
    fn keys_wait_for_the_rest_of_a_binding_or_are_dropped() {
        let mut map = Keymap::default();
        let niv = parse_modes("niv").unwrap();
        map.bind(&niv, &keys("<C-b>c"), "new tab", Some("tab"));
        map.bind(&niv, &keys("<C-b>p1"), "pane 1", Some("pane"));
        map.bind(&[KeyMode::Normal], &keys(":"), "command", None);
        assert_eq!(
            type_in(&mut map, KeyMode::Insert, "<C-b>c"),
            [Pressed::Waiting, Pressed::Bound("new tab")]
        );
        assert_eq!(
            type_in(&mut map, KeyMode::Insert, "<C-b>p1"),
            [Pressed::Waiting, Pressed::Waiting, Pressed::Bound("pane 1")]
        );
        assert_eq!(
            type_in(&mut map, KeyMode::Insert, "<C-b>zx"),
            [Pressed::Waiting, Pressed::Dropped, Pressed::Unbound]
        );
        assert_eq!(type_in(&mut map, KeyMode::Insert, ":"), [Pressed::Unbound]);
        assert_eq!(
            type_in(&mut map, KeyMode::Normal, ":"),
            [Pressed::Bound("command")]
        );

        map.press(KeyMode::Normal, keys("<C-b>")[0]);
        let rows = |map: &Keymap<&str>| map.which_keys(|_| String::new()).map(|which| which.rows);
        assert_eq!(
            rows(&map),
            Some(vec![
                ("c".into(), "tab".into()),
                ("p".into(), "+pane".into())
            ])
        );
        assert!(map.cancel());
        assert_eq!(rows(&map), None);

        assert!(map.unbind(&[KeyMode::Insert], &keys("<C-b>c")));
        assert_eq!(
            type_in(&mut map, KeyMode::Insert, "<C-b>c"),
            [Pressed::Waiting, Pressed::Dropped]
        );
        assert_eq!(
            type_in(&mut map, KeyMode::Normal, "<C-b>c"),
            [Pressed::Waiting, Pressed::Bound("new tab")],
            "only where it was taken away"
        );
    }

    #[test]
    fn which_keys_says_what_each_next_key_does_from_the_bindings() {
        let mut map = Keymap::default();
        let n = [KeyMode::Normal];
        for digit in 1..=9 {
            map.bind(
                &n,
                &keys(&format!("<C-b>{digit}")),
                "tab",
                Some("go to tab"),
            );
        }
        map.bind(&n, &keys("<C-b>c"), "new", Some("new tab"));
        for digit in 1..=9 {
            map.bind(
                &n,
                &keys(&format!("<C-b>p{digit}")),
                "pane",
                Some("pick a pane"),
            );
        }
        map.bind(&n, &keys("<C-b>gr"), "review", Some("review"));
        map.bind(&n, &keys("<C-b>gx"), "reject", Some("reject"));
        map.bind(&n, &keys("<C-b>hp"), "show", Some("show hunk"));
        map.bind(&n, &keys("<C-b>hs"), "stage", Some("stage hunk"));
        map.bind(&n, &keys("<C-b>hsa"), "stage all", Some("stage all"));
        map.bind(&n, &keys("<C-b>ds"), "problems", Some("problems"));
        map.bind(&n, &keys("<C-b>z"), "zoom", Some("new tab"));
        map.bind(&n, &keys("<C-b><C-b>"), "send", None);
        map.bind(&n, &keys("<C-b>m"), "mine", None);
        map.bind(&n, &keys("<C-b>n"), "mine", None);
        map.press(KeyMode::Normal, keys("<C-b>")[0]);
        let which = map.which_keys(|_| "from your config".into()).unwrap();
        assert_eq!(
            (which.title.as_str(), map.waiting_short().as_deref()),
            ("Ctrl+B", Some("^B"))
        );
        assert!(map.waiting_since().is_some());
        let rows: Vec<(&str, &str)> = (which.rows.iter())
            .map(|(keys, does)| (keys.as_str(), does.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("1-9", "go to tab"),
                ("c z", "new tab"),
                ("p", "+pick a pane"),
                ("g", "+2 keys"),
                // `p` and `s` come after `h`: two keys, of three bindings.
                ("h", "+2 keys"),
                ("d", "+problems"),
                // Unlabelled, each is listed: they may do anything.
                ("^B", "from your config"),
                ("m", "from your config"),
                ("n", "from your config"),
            ]
        );
        map.press(KeyMode::Normal, keys("p")[0]);
        let which = map.which_keys(|_| String::new()).unwrap();
        assert_eq!(
            (which.title.as_str(), map.waiting_short().as_deref()),
            ("Ctrl+B p", Some("^Bp"))
        );
        assert_eq!(which.rows, [("1-9".to_owned(), "pick a pane".to_owned())]);
    }

    #[test]
    fn a_group_names_the_keys_that_start_it_in_its_modes() {
        let mut map = Keymap::default();
        let nv = parse_modes("nv").unwrap();
        map.bind(&nv, &keys("<Space>gs"), "stage", Some("stage"));
        map.bind(&nv, &keys("<Space>gr"), "reset", Some("reset"));
        map.bind(&nv, &keys("<Space>f"), "find", Some("find"));
        map.name_group(&[KeyMode::Normal], &keys("<Space>g"), Some("git"));
        // A group of a key that finishes a binding names nothing.
        map.name_group(&nv, &keys("<Space>f"), Some("files"));
        let rows = |map: &mut Keymap<&str>, mode| {
            map.press(mode, keys("<Space>")[0]);
            let rows = map.which_keys(|_| String::new()).unwrap().rows;
            map.cancel();
            rows
        };
        let row = |keys: &str, does: &str| (keys.to_owned(), does.to_owned());
        assert_eq!(
            rows(&mut map, KeyMode::Normal),
            [row("g", "+git"), row("f", "find")]
        );
        assert_eq!(
            rows(&mut map, KeyMode::Visual),
            [row("g", "+2 keys"), row("f", "find")],
            "only in the modes it was named in"
        );
        map.name_group(&[KeyMode::Normal], &keys("<Space>g"), None);
        assert_eq!(rows(&mut map, KeyMode::Normal)[0], row("g", "+2 keys"));
    }

    #[test]
    fn keys_are_said_in_full_or_short() {
        let said = |notation: &str| {
            let key = keys(notation)[0];
            (key.spoken(), key.short())
        };
        assert_eq!(said("<C-b>"), ("Ctrl+B".into(), "^B".into()));
        assert_eq!(said("<Space>"), ("Space".into(), "␣".into()));
        assert_eq!(said("p"), ("p".into(), "p".into()));
        assert_eq!(said("<CR>"), ("Enter".into(), "<CR>".into()));
        assert_eq!(said("<A-x>"), ("Alt+X".into(), "<A-x>".into()));
    }
}

#[cfg(test)]
mod help_tests {
    use super::*;

    #[test]
    fn shortcut_help_tracks_modes_replacement_unbinding_and_pending_prefixes() {
        let keys = |text| parse_keys(text).unwrap();
        let mut map = Keymap::default();
        map.bind(&[KeyMode::Normal], &keys("ga"), 1, Some("old"));
        map.bind(&[KeyMode::Normal], &keys("ga"), 2, Some("replacement"));
        map.bind(&[KeyMode::Insert], &keys("ga"), 3, Some("insert"));
        map.bind(&[KeyMode::Normal], &keys("gx"), 4, None);
        map.bind(&[KeyMode::Normal], &keys("g"), 5, Some("unreachable"));
        map.bind(&[KeyMode::Normal], &keys("z"), 6, Some("elsewhere"));
        assert_eq!(map.available(KeyMode::Insert).len(), 1);
        assert_eq!(map.available(KeyMode::Normal).len(), 3);
        map.unbind(&[KeyMode::Normal], &keys("gx"));
        assert_eq!(map.available(KeyMode::Normal).len(), 2);
        map.press(KeyMode::Normal, keys("g")[0]);
        let before = map.waiting().to_vec();
        let available = map.available(KeyMode::Insert);
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].2, Some("replacement"));
        assert_eq!(map.waiting(), before);
    }
}
