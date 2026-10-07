//! Finding a style in a row's palette without searching it one by one.

use crate::cell::StyleIndex;
use crate::style::{Color, Style};

/// Palettes up to this size are searched one entry at a time, which beats
/// hashing for the few styles a typical row uses.
pub(crate) const LINEAR_SEARCH_MAX: usize = 16;

/// A hash table from style to palette index, for rows with more styles than
/// [`LINEAR_SEARCH_MAX`]: the worst case is an app giving every cell its own
/// style. Open addressing, with linear probing.
///
/// It stores only indices; the styles themselves stay in the palette. It is
/// at most half full, so probes stay short, and it never deletes: when the
/// palette compacts, the table is rebuilt.
#[derive(Clone, Debug, Default)]
pub(crate) struct StyleLookup {
    /// [`StyleIndex`] values, with 0 (the default style, which the palette
    /// never holds) marking an empty slot. Empty when the table isn't built.
    slots: Vec<u16>,
}

impl StyleLookup {
    pub(crate) fn is_built(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Builds the table for `palette`, sized to stay at most half full until
    /// the palette reaches `max_len`.
    pub(crate) fn build(&mut self, palette: &[Style], max_len: usize) {
        self.slots.clear();
        self.slots.resize((2 * max_len).next_power_of_two(), 0);
        for (position, style) in palette.iter().enumerate() {
            self.insert(style, super::palette_index(position));
        }
    }

    /// Forgets the table, keeping its memory to build the next one in.
    pub(crate) fn invalidate(&mut self) {
        self.slots.clear();
    }

    /// The index of `style` in `palette`, which the table must be built for.
    pub(crate) fn find(&self, palette: &[Style], style: &Style) -> Option<StyleIndex> {
        let mask = self.slots.len() - 1;
        let mut slot = hash(style) & mask;
        loop {
            match self.slots[slot] {
                0 => return None,
                index if palette[usize::from(index) - 1] == *style => {
                    return Some(StyleIndex(index));
                }
                _ => slot = (slot + 1) & mask,
            }
        }
    }

    /// Records that `style` is at `index`. It must not be in the table yet.
    pub(crate) fn insert(&mut self, style: &Style, index: StyleIndex) {
        let mask = self.slots.len() - 1;
        let mut slot = hash(style) & mask;
        while self.slots[slot] != 0 {
            slot = (slot + 1) & mask;
        }
        self.slots[slot] = index.0;
    }
}

/// A fast hash of every field of a style. Not for untrusted keys in general,
/// but a program can only make its own row slow by colliding styles.
fn hash(style: &Style) -> usize {
    fn color(color: Color) -> u64 {
        match color {
            Color::Default => 0,
            Color::Indexed(index) => 1 << 24 | u64::from(index),
            Color::Rgb(r, g, b) => 2 << 24 | u64::from(r) << 16 | u64::from(g) << 8 | u64::from(b),
        }
    }
    let colors = color(style.fg) | color(style.bg) << 32;
    let rest = color(style.underline_color)
        | u64::from(style.attributes.bits()) << 32
        | (style.underline as u64) << 48;
    let mixed = (colors ^ rest.rotate_left(29)).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    // The high bits are the best mixed; fold them down to where the mask
    // looks. Truncating to usize is intended.
    #[allow(clippy::cast_possible_truncation)]
    let folded = (mixed ^ (mixed >> 32)) as usize;
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(n: u8) -> Style {
        Style {
            fg: Color::Indexed(n),
            bg: Color::Rgb(n, 0, 255 - n),
            ..Style::DEFAULT
        }
    }

    #[test]
    fn finds_every_style_and_nothing_else() {
        let palette: Vec<Style> = (0..200).map(style).collect();
        let mut lookup = StyleLookup::default();
        lookup.build(&palette, 240);
        for (position, style) in palette.iter().enumerate() {
            assert_eq!(
                lookup.find(&palette, style),
                Some(super::super::palette_index(position))
            );
        }
        assert_eq!(lookup.find(&palette, &style(250)), None);
    }
}
