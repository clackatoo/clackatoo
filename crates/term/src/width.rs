//! How many columns a character takes.
//!
//! The answers come from the `unicode-width` crate, whose lookups walk a
//! few levels of tables. The Basic Multilingual Plane, where nearly all text
//! lives, is looked up once and kept in a 16 KiB table instead, built the
//! first time a terminal prints anything beyond ASCII.

use std::sync::OnceLock;

use unicode_width::UnicodeWidthChar;

/// Code points in the Basic Multilingual Plane.
const BMP_LEN: usize = 0x1_0000;

/// What the table stores for a character with no width, such as a control
/// character. Real widths are 0, 1 or 2.
const NO_WIDTH: u8 = 3;

/// Two bits per BMP code point, four to a byte.
static BMP: OnceLock<Box<[u8]>> = OnceLock::new();

/// The columns `ch` takes: 0 for combining characters, 2 for wide ones, and
/// `None` for controls.
///
/// This is `unicode-width`'s answer, except that it is never more than 2.
/// `unicode-width` gives U+17D8 KHMER SIGN BEYYAL a width of 3, which a
/// grid of one- and two-column cells can't show; it is drawn wide instead.
pub(crate) fn of(ch: char) -> Option<usize> {
    let code = u32::from(ch) as usize;
    if code >= BMP_LEN {
        return clamped(ch);
    }
    let table = BMP.get_or_init(build);
    match (table[code / 4] >> (code % 4 * 2)) & 3 {
        NO_WIDTH => None,
        width => Some(usize::from(width)),
    }
}

fn build() -> Box<[u8]> {
    let mut table = vec![0; BMP_LEN / 4].into_boxed_slice();
    for code in 0..BMP_LEN {
        let width = u32::try_from(code)
            .ok()
            .and_then(char::from_u32)
            .and_then(clamped)
            .map_or(NO_WIDTH, |width| u8::try_from(width).expect("clamped to 2"));
        table[code / 4] |= width << (code % 4 * 2);
    }
    table
}

/// `unicode-width`'s answer, at most 2.
fn clamped(ch: char) -> Option<usize> {
    ch.width().map(|width| width.min(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agrees_with_unicode_width_everywhere() {
        for ch in (0..=u32::from(char::MAX)).filter_map(char::from_u32) {
            assert_eq!(of(ch), ch.width().map(|width| width.min(2)), "{ch:?}");
        }
        assert_eq!('\u{17D8}'.width(), Some(3), "unicode-width still says 3");
        assert_eq!(of('\u{17D8}'), Some(2));
    }
}
