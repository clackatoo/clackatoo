//! A single grid cell.

use bitflags::bitflags;

/// Which entry of its row's style palette a cell uses.
///
/// Index 0 is always [`Style::DEFAULT`](crate::Style::DEFAULT); other indices
/// only mean something within the row that owns the cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct StyleIndex(pub(crate) u16);

impl StyleIndex {
    pub(crate) const DEFAULT: Self = Self(0);
}

bitflags! {
    /// Facts about a cell beyond its character and style.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct CellFlags: u16 {
        /// The cell holds the first half of a double-width character. The
        /// cell to its right is always a [`WIDE_SPACER`](Self::WIDE_SPACER).
        const WIDE = 1 << 0;
        /// The cell is the second half of the double-width character to its
        /// left and has no text of its own.
        const WIDE_SPACER = 1 << 1;
        /// More codepoints follow the cell's character, such as combining
        /// accents or the rest of an emoji sequence. The row stores them.
        const GRAPHEME = 1 << 2;
    }
}

/// One cell of the grid: a character, its style and some flags.
///
/// Kept at 8 bytes, because the grid's memory use and cache behaviour come
/// down to cell size. Styles are stored once per row and referenced by
/// index, and codepoints beyond the first live in a side table on the row,
/// so neither inflates every cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Cell {
    ch: char,
    style: StyleIndex,
    flags: CellFlags,
}

const _: () = assert!(size_of::<Cell>() == 8);

impl Cell {
    /// An empty cell: a space in the default style.
    pub(crate) const BLANK: Self = Self::blank(StyleIndex::DEFAULT);

    pub(crate) const fn blank(style: StyleIndex) -> Self {
        Self::new(' ', style, CellFlags::empty())
    }

    pub(crate) const fn new(ch: char, style: StyleIndex, flags: CellFlags) -> Self {
        Self { ch, style, flags }
    }

    /// The whole cell as one number: character in the low 32 bits, then
    /// style, then flags. With the fields laid out in that order, this is a
    /// single load, so loops can test several fields with one comparison.
    pub(crate) const fn bits(self) -> u64 {
        self.ch as u64 | (self.style.0 as u64) << 32 | (self.flags.bits() as u64) << 48
    }

    /// The cell's character, or its first codepoint if it holds a longer
    /// grapheme. A [`CellFlags::WIDE_SPACER`] cell reports a space.
    #[must_use]
    pub const fn ch(self) -> char {
        self.ch
    }

    /// The cell's flags.
    #[must_use]
    pub const fn flags(self) -> CellFlags {
        self.flags
    }

    pub(crate) const fn style(self) -> StyleIndex {
        self.style
    }

    pub(crate) const fn with_style(self, style: StyleIndex) -> Self {
        Self { style, ..self }
    }

    /// Whether the cell holds a double-width character, which also covers
    /// the cell to its right.
    #[must_use]
    pub fn is_wide(self) -> bool {
        self.flags.contains(CellFlags::WIDE)
    }

    /// Whether the cell is the right half of a double-width character, and
    /// holds nothing of its own.
    #[must_use]
    pub fn is_wide_spacer(self) -> bool {
        self.flags.contains(CellFlags::WIDE_SPACER)
    }

    pub(crate) fn has_grapheme(self) -> bool {
        self.flags.contains(CellFlags::GRAPHEME)
    }

    pub(crate) fn add_flags(&mut self, flags: CellFlags) {
        self.flags |= flags;
    }
}
