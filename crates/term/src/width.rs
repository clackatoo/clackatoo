//! How many columns a character takes.
//!
//! The answers come from the `unicode-width` crate, whose lookups walk a
//! few levels of tables. The first two planes, where nearly all text and
//! emoji live, are looked up once each and kept in 16 KiB tables instead,
//! built the first time a terminal prints a character from that plane.

use std::sync::OnceLock;

use unicode_segmentation::GraphemeCursor;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::row::MAX_EXTRA;

/// Code points in one Unicode plane.
const PLANE_LEN: usize = 0x1_0000;

/// What the table stores for a character with no width, such as a control
/// character. Real widths are 0, 1 or 2.
const NO_WIDTH: u8 = 3;

/// Two bits per code point, four to a byte: the Basic Multilingual Plane,
/// and the Supplementary Multilingual Plane, home of the emoji.
static PLANES: [OnceLock<Box<[u8]>>; 2] = [OnceLock::new(), OnceLock::new()];

/// Whether `ch` shows nothing itself but changes how what's around it
/// shows: zero-width characters, the marks that turn text round (U+061C,
/// U+200E and U+200F, the embeddings, overrides and isolates), and the
/// line and paragraph separators, U+2028 and U+2029, which break a line
/// where no control character shows it. Text a program or an agent wrote,
/// shown as clackatoo's own or beside it, goes without them, so it can't
/// hide or turn round what's around it.
#[must_use]
pub fn invisible(ch: char) -> bool {
    matches!(
        u32::from(ch),
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
            | 0xFEFF | 0xFFA0 | 0xFFF9..=0xFFFB | 0xE0000..=0xE0FFF
    )
}

/// Whether `ch` is the zero-width joiner or non-joiner, which emoji
/// sequences (👩‍👩‍👧) and scripts such as Persian need between two
/// characters, and which could hide what's around them anywhere else.
#[must_use]
pub fn joiner(ch: char) -> bool {
    matches!(ch, '\u{200C}' | '\u{200D}')
}

/// The most joiners in a row [`Joiners`] keeps: emoji and scripts need
/// one at a time.
pub const MOST_JOINERS: usize = 3;

/// Which joiners (see [`joiner`]) in a text to keep, the one rule for
/// everything clackatoo shows of others' text: one stays only between
/// two characters past ASCII that show as themselves, past any joiners
/// beside it, and only as one of at most [`MOST_JOINERS`] in a row. What
/// "shows as itself" means is the caller's: what it shows as a code, or
/// leaves out, can't be joined to.
///
/// Every character of the text goes through it in order: a joiner to
/// [`keeps`](Self::keeps), any other to [`passed`](Self::passed).
#[derive(Clone, Copy, Debug, Default)]
pub struct Joiners {
    /// Whether the last character that isn't a joiner shows as itself and
    /// is past ASCII, so a joiner after it may join it to the next.
    joinable: bool,
    /// How many joiners have come in a row since.
    run: usize,
}

impl Joiners {
    /// Takes in `ch`, the next character, which isn't a joiner, and
    /// whether it `shows` as itself.
    pub fn passed(&mut self, ch: char, shows: bool) {
        self.run = 0;
        self.joinable = shows && !ch.is_ascii();
    }

    /// Takes in the next character, a joiner, with `rest`, what follows
    /// it: whether to keep it, as what's before it and the first of
    /// `rest` that isn't a joiner are past ASCII and show as themselves,
    /// as `shows` says, and it's one of at most [`MOST_JOINERS`] in a row.
    pub fn keeps(
        &mut self,
        rest: impl IntoIterator<Item = char>,
        shows: impl Fn(char) -> bool,
    ) -> bool {
        self.run += 1;
        self.joinable
            && self.run <= MOST_JOINERS
            && (rest.into_iter())
                .find(|&next| !joiner(next))
                .is_some_and(|next| !next.is_ascii() && shows(next))
    }
}

/// `text`, a name of others' choosing, as a tab's name or a title, as
/// it's shown: without what [`invisible`] says goes unseen or turns text
/// round, but for the joiners that join two characters that show (see
/// [`Joiners`]); at most `most` characters of it, joiners not counted.
/// Control characters are the caller's, to leave out or put as spaces
/// first: they're kept, but never joined to.
#[must_use]
pub fn visible(text: impl IntoIterator<Item = char>, most: usize) -> String {
    // What shows, up to `most`, and the joiners between: no more of those
    // in a row than could be kept, so a run of them costs nothing.
    let (mut kept, mut count, mut run) = (Vec::new(), 0, 0);
    for ch in (text.into_iter()).filter(|&ch| joiner(ch) || !invisible(ch)) {
        if joiner(ch) {
            run += 1;
            if run > MOST_JOINERS {
                continue;
            }
        } else if count == most {
            break;
        } else {
            (count, run) = (count + 1, 0);
        }
        kept.push(ch);
    }
    let seen = |ch: char| !ch.is_control();
    let mut joiners = Joiners::default();
    let mut shown = String::with_capacity(kept.len());
    for (at, &ch) in kept.iter().enumerate() {
        if !joiner(ch) {
            joiners.passed(ch, seen(ch));
            shown.push(ch);
        } else if joiners.keeps(kept[at + 1..].iter().copied(), seen) {
            shown.push(ch);
        }
    }
    shown
}

/// The rules a terminal lays its cells out by, beyond each character's
/// own width ([`of`]): which cells a character joining them makes wide.
/// Fixed for a terminal's life, as its rows are rebuilt from their text
/// by them (see [`RowBuilder`](crate::RowBuilder)), so whatever lays a
/// terminal's rows out again, a window or the next build, must use the
/// same. The default is the rules before there were any: no cell widens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Widths {
    /// A character with an emoji form that variation selector 16
    /// (U+FE0F) follows takes two columns, as kitty and Ghostty draw it:
    /// ❤ is one column, ❤️ two.
    pub vs16: bool,
    /// Rows are rebuilt by grapheme clusters, as mode 2027 prints them: a
    /// character Unicode's segmentation (UAX #29) says goes on with the
    /// cell before it joins it, unless its run starts apart (see
    /// [`RowBuilder::push`](crate::RowBuilder::push)), as what's printed
    /// with the mode off does; and a cell of a pair of regional
    /// indicators (a flag), or an emoji with a skin tone, takes two
    /// columns. The terminal can then honour mode 2027.
    pub graphemes: bool,
}

impl Widths {
    /// The rules before there were any: what a terminal restored from a
    /// build before them was laid out by.
    pub const LEGACY: Self = Self {
        vs16: false,
        graphemes: false,
    };

    /// Whether a cell holding `first`, a character one column wide, then
    /// `rest`, takes two columns by these rules.
    pub(crate) fn widens(self, first: char, rest: &[char]) -> bool {
        match rest.first() {
            Some('\u{FE0F}') if self.vs16 => emoji_form(first),
            Some(&next) if self.graphemes => {
                (regional_indicator(first) && regional_indicator(next)) || skin_tone(next)
            }
            _ => false,
        }
    }
}

/// Whether `ch` is a regional indicator, half a flag.
fn regional_indicator(ch: char) -> bool {
    matches!(ch, '\u{1F1E6}'..='\u{1F1FF}')
}

/// Whether `ch` is one of the five skin tones that colour the emoji
/// before them.
fn skin_tone(ch: char) -> bool {
    matches!(ch, '\u{1F3FB}'..='\u{1F3FF}')
}

/// Whether `next` goes on with the grapheme cluster a cell holding
/// `first`, then `rest`, has begun, as Unicode's segmentation (UAX #29)
/// says, so it belongs in that cell, where clusters are kept whole: a
/// spacing mark, a skin tone, a flag's second half, a Hangul syllable's
/// next jamo. A cell starts a cluster, so the cell is all the context the
/// rules need.
pub(crate) fn continues_cluster(first: char, rest: &[char], next: char) -> bool {
    let last = rest.last().copied().unwrap_or(first);
    // What nearly all text is, said quickly: ASCII after ASCII, and an
    // ideograph or kana after another, never join.
    if (last.is_ascii() && next.is_ascii()) || (ideograph(last) && ideograph(next)) {
        return false;
    }
    let mut text = [0; 4 * (MAX_EXTRA + 2)];
    let mut len = 0;
    for ch in std::iter::once(first).chain(rest.iter().copied().take(MAX_EXTRA)) {
        len += ch.encode_utf8(&mut text[len..]).len();
    }
    let at = len;
    len += next.encode_utf8(&mut text[len..]).len();
    let Ok(text) = std::str::from_utf8(&text[..len]) else {
        return false;
    };
    let mut cursor = GraphemeCursor::new(at, len, true);
    cursor.is_boundary(text, 0) == Ok(false)
}

/// Whether `ch` is a CJK ideograph or kana: never a cluster's extension,
/// and never extended.
fn ideograph(ch: char) -> bool {
    matches!(ch, '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}')
}

/// Whether `ch` has an emoji form that variation selector 16 asks for,
/// as `unicode-width` reads the pair, taking two columns: ❤, ☺, ✔, but
/// not a letter.
fn emoji_form(ch: char) -> bool {
    if of(ch) != Some(1) {
        return false;
    }
    let mut pair = [0; 8];
    let len = ch.encode_utf8(&mut pair).len();
    '\u{FE0F}'.encode_utf8(&mut pair[len..]);
    std::str::from_utf8(&pair[..len + 3]).is_ok_and(|pair| pair.width() == 2)
}

/// The columns `ch` takes: 0 for combining characters, 2 for wide ones, and
/// `None` for controls.
///
/// This is `unicode-width`'s answer, except that it is never more than 2.
/// `unicode-width` gives U+17D8 KHMER SIGN BEYYAL a width of 3, which a
/// grid of one- and two-column cells can't show; it is drawn wide instead.
#[must_use]
pub fn of(ch: char) -> Option<usize> {
    let code = u32::from(ch) as usize;
    let Some(table) = PLANES.get(code / PLANE_LEN) else {
        return clamped(ch);
    };
    let table = table.get_or_init(|| build(code / PLANE_LEN));
    let code = code % PLANE_LEN;
    match (table[code / 4] >> (code % 4 * 2)) & 3 {
        NO_WIDTH => None,
        width => Some(usize::from(width)),
    }
}

fn build(plane: usize) -> Box<[u8]> {
    let mut table = vec![0; PLANE_LEN / 4].into_boxed_slice();
    for code in 0..PLANE_LEN {
        let width = u32::try_from(plane * PLANE_LEN + code)
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

    #[test]
    fn clusters_go_on_as_unicode_says() {
        // A spacing mark, a skin tone, a flag's second half, a jamo.
        assert!(continues_cluster('क', &[], '\u{093F}'));
        assert!(continues_cluster('☝', &[], '\u{1F3FD}'));
        assert!(continues_cluster('\u{1F1EC}', &[], '\u{1F1E7}'));
        assert!(!continues_cluster('\u{1F1EC}', &['\u{1F1E7}'], '\u{1F1EF}'));
        assert!(continues_cluster('\u{1100}', &[], '\u{1161}'));
        // A prefix, as the Arabic number sign, joins what follows, ASCII too.
        assert!(continues_cluster('\u{0600}', &[], '1'));
        for (before, next) in [
            ('a', 'b'),
            ('字', '字'),
            ('a', '字'),
            ('é', 'x'),
            ('👍', '👍'),
        ] {
            assert!(!continues_cluster(before, &[], next), "{before:?} {next:?}");
        }
        // A cell full of marks is still read whole.
        let marks = ['\u{301}'; MAX_EXTRA];
        assert!(continues_cluster('e', &marks, '\u{301}'));
    }

    #[test]
    fn flags_and_skin_tones_widen_by_clusters() {
        let clusters = Widths {
            vs16: false,
            graphemes: true,
        };
        assert!(clusters.widens('\u{1F1EC}', &['\u{1F1E7}']));
        assert!(clusters.widens('☝', &['\u{1F3FD}']));
        assert!(!clusters.widens('a', &['\u{301}']));
        assert!(!Widths::LEGACY.widens('\u{1F1EC}', &['\u{1F1E7}']));
    }

    #[test]
    fn vs16_widens_only_what_has_an_emoji_form() {
        let vs16 = Widths {
            vs16: true,
            graphemes: false,
        };
        for ch in ['❤', '☺', '✔', '☝'] {
            assert!(vs16.widens(ch, &['\u{FE0F}']), "{ch:?}");
            assert!(!Widths::LEGACY.widens(ch, &['\u{FE0F}']), "{ch:?}");
            assert!(!vs16.widens(ch, &['\u{301}', '\u{FE0F}']), "{ch:?}");
        }
        for ch in ['a', 'é', '中'] {
            assert!(!vs16.widens(ch, &['\u{FE0F}']), "{ch:?}");
        }
        assert!(!vs16.widens('❤', &['\u{FE0E}']), "VS15 asks for text");
    }

    #[test]
    fn names_keep_joiners_only_between_what_shows() {
        let all = usize::MAX;
        // Emoji sequences and Persian's non-joiner stay.
        let family = "👩\u{200d}👩\u{200d}👧";
        assert_eq!(visible(family.chars(), all), family);
        let persian = "می\u{200c}خواهم";
        assert_eq!(visible(persian.chars(), all), persian);
        // Next to ASCII, at either end, or too many in a row, they go,
        // with what turns text round or hides.
        assert_eq!(
            visible("a\u{200d}b \u{200d}👩 👩\u{200d}".chars(), all),
            "ab 👩 👩"
        );
        assert_eq!(
            visible("👩\u{200d}\u{200d}\u{200d}\u{200d}👧".chars(), all),
            "👩\u{200d}\u{200d}\u{200d}👧"
        );
        // What's left out isn't there to hide anything: a joiner either
        // side of it joins what shows.
        assert_eq!(
            visible("👩\u{200d}\u{202e}👩 x\u{200b}y".chars(), all),
            "👩\u{200d}👩 xy"
        );
        // A control character is never joined to.
        assert_eq!(visible("👩\u{200d}\u{85}".chars(), all), "👩\u{85}");
        assert_eq!(visible("ab\u{200b}cd".chars(), 3), "abc");
        // Joiners don't count towards the most shown.
        assert_eq!(
            visible("👩\u{200d}👩\u{200d}👧!".chars(), 3),
            "👩\u{200d}👩\u{200d}👧"
        );
    }

    #[test]
    fn a_joiner_is_kept_by_what_the_caller_says_shows() {
        // As agent text is: what shows as a code can't be joined to.
        let mut joiners = Joiners::default();
        joiners.passed('👩', true);
        assert!(joiners.keeps("👩".chars(), |_| true));
        assert!(!joiners.keeps("\u{202e}👩".chars(), |ch| ch != '\u{202e}'));
        joiners.passed('\u{202e}', false);
        assert!(!joiners.keeps("👩".chars(), |_| true));
    }
}
