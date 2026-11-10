//! kitty's Unicode placeholders: a program prints U+10EEEE in cells where
//! an image is to show, each cell naming which image by its text's colour,
//! which placement by its underline's, and which part by the combining
//! marks after it. The cells are text, so they scroll, split, wrap and go
//! into history as text does; a window draws the image's part in place of
//! each (see [`placeholders`]), and the placement's size and image come
//! from the terminal's virtual placements ([`Virtual`](super::Virtual)).
//!
//! An image's ID is the text's colour: an indexed colour for IDs below
//! 256, a 24-bit colour for the rest, and a third mark for the ID's top
//! byte. The row and the column are a mark each, from [`DIACRITICS`]. A
//! cell may leave out its marks: then it's taken as the one after the
//! cell to its left, as kitty takes it.

use crate::row::Row;
use crate::style::Color;

/// The character a placeholder cell holds.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// The combining marks rows and columns are numbered by, in order: the
/// mark at index `n` says `n`. kitty's `rowcolumn-diacritics.txt`.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// What a placeholder cell says it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placeholder {
    /// The image's ID, as the program gave it.
    pub image: u32,
    /// The placement's ID, or 0 for whichever of the image's.
    pub placement: u32,
    /// Which row of the placement's cells it is.
    pub row: u16,
    /// Which column.
    pub col: u16,
}

/// The number a combining mark says, if it's one of [`DIACRITICS`].
fn number(mark: char) -> Option<u16> {
    let index = DIACRITICS.binary_search(&u32::from(mark)).ok()?;
    u16::try_from(index).ok()
}

/// The ID a colour says, without its top byte.
pub(crate) fn colour_id(colour: Color) -> u32 {
    match colour {
        Color::Default => 0,
        Color::Indexed(index) => u32::from(index),
        Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
    }
}

/// Whether `row` may hold placeholders: one of its cells is
/// [`PLACEHOLDER`]. Cheap enough to ask of every row drawn.
#[must_use]
pub fn has_placeholders(row: &Row) -> bool {
    row.cells().iter().any(|cell| cell.ch() == PLACEHOLDER)
}

/// The placeholder cells of `row`, each with its column, as kitty reads
/// them: what a cell leaves out is taken from the cell to its left, if
/// that's a placeholder of the same colours. A cell naming image 0 is
/// left out.
#[must_use]
pub fn placeholders(row: &Row) -> Vec<(usize, Placeholder)> {
    let mut found = Vec::new();
    // The cell to the left: its colours, what it said, and its ID's top
    // byte.
    let mut left: Option<(Color, Color, Placeholder, u32)> = None;
    for (col, &cell) in row.cells().iter().enumerate() {
        if cell.ch() != PLACEHOLDER {
            left = None;
            continue;
        }
        let style = row.style(cell);
        let marks = row.grapheme(col).unwrap_or(&[]);
        let mut numbers = marks.iter().map(|&mark| number(mark));
        let row_mark = numbers.next().flatten();
        let col_mark = numbers.next().flatten();
        let top_mark = numbers.next().flatten().map(u32::from);
        let same = left
            .filter(|&(fg, underline, ..)| fg == style.fg && underline == style.underline_color);
        let (row_at, col_at, top) = match (row_mark, col_mark, same) {
            (Some(r), Some(c), Some((.., before, top_before)))
                if before.row == r && before.col.checked_add(1) == Some(c) =>
            {
                (r, c, top_mark.unwrap_or(top_before))
            }
            (Some(r), Some(c), _) => (r, c, top_mark.unwrap_or(0)),
            (Some(r), None, Some((.., before, top_before))) if before.row == r => {
                (r, before.col.saturating_add(1), top_before)
            }
            (None, None, Some((.., before, top_before))) => {
                (before.row, before.col.saturating_add(1), top_before)
            }
            (Some(r), None, _) => (r, 0, 0),
            (None, _, _) => (0, 0, 0),
        };
        let said = Placeholder {
            image: colour_id(style.fg) | (top & 0xFF) << 24,
            placement: colour_id(style.underline_color),
            row: row_at,
            col: col_at,
        };
        left = Some((style.fg, style.underline_color, said, top));
        if said.image != 0 {
            found.push((col, said));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marks_are_numbered_in_order() {
        assert_eq!(number('\u{0305}'), Some(0));
        assert_eq!(number('\u{030D}'), Some(1));
        assert_eq!(number('\u{1D244}'), Some(296));
        assert_eq!(number('a'), None);
        assert!(DIACRITICS.is_sorted());
    }
}
