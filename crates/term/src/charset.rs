//! Character sets: the 1970s way of drawing boxes.
//!
//! Before Unicode, terminals had a few alternative character sets that
//! could be swapped in. The one still in use is DEC Special Graphics, which
//! remaps lowercase ASCII to line-drawing characters. ncurses uses it for
//! borders on `xterm-256color`, so htop's boxes depend on it.

/// A character set that can be designated into G0 or G1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Charset {
    #[default]
    Ascii,
    DecSpecialGraphics,
}

impl Charset {
    /// The charset an `ESC ( <final>` designation names, if we support it.
    pub(crate) fn from_designator(byte: u8) -> Option<Self> {
        match byte {
            b'B' => Some(Self::Ascii),
            b'0' => Some(Self::DecSpecialGraphics),
            _ => None,
        }
    }

    fn map(self, ch: char) -> char {
        match self {
            Self::Ascii => ch,
            Self::DecSpecialGraphics => dec_special_graphics(ch),
        }
    }
}

/// Which slot shifted-in text draws from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Slot {
    #[default]
    G0,
    G1,
}

/// The designated character sets and which one is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Charsets {
    g0: Charset,
    g1: Charset,
    active: Slot,
}

impl Charsets {
    pub(crate) fn from_parts(g0: Charset, g1: Charset, active: Slot) -> Self {
        Self { g0, g1, active }
    }

    /// G0, G1 and the active slot.
    pub(crate) fn parts(self) -> (Charset, Charset, Slot) {
        (self.g0, self.g1, self.active)
    }

    pub(crate) fn designate(&mut self, slot: Slot, charset: Charset) {
        match slot {
            Slot::G0 => self.g0 = charset,
            Slot::G1 => self.g1 = charset,
        }
    }

    /// Makes `slot` the active set (SI selects G0, SO selects G1).
    pub(crate) fn shift(&mut self, slot: Slot) {
        self.active = slot;
    }

    /// `ch` as the active character set draws it.
    /// Whether ASCII prints as itself, which it does unless the active set
    /// is DEC Special Graphics.
    pub(crate) fn maps_ascii_verbatim(self) -> bool {
        let active = match self.active {
            Slot::G0 => self.g0,
            Slot::G1 => self.g1,
        };
        active == Charset::Ascii
    }

    pub(crate) fn map(self, ch: char) -> char {
        match self.active {
            Slot::G0 => self.g0.map(ch),
            Slot::G1 => self.g1.map(ch),
        }
    }
}

fn dec_special_graphics(ch: char) -> char {
    match ch {
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}
