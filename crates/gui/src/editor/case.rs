//! Changing case, as `~`, `g~`, `gu`, `gU` and `g?` do, and `\u`, `\U`,
//! `\l` and `\L` in a replacement.
//!
//! Each character becomes one character, as in Vim, so a line keeps as
//! many characters, though not always as many bytes: `ß` goes up to `ẞ`,
//! not `SS`, and `İ` down to `i`. A character whose capital, or small
//! letter, is more than one character, as `ŉ` is, stays as it is, except
//! for Greek with a subscript iota, which goes to its one-character
//! capital, as Vim's does.

use crate::vim::Recase;

impl Recase {
    /// `ch`, with its case changed.
    pub(crate) fn char(self, ch: char) -> char {
        match self {
            Self::Lower => lower(ch),
            Self::Upper => upper(ch),
            Self::Toggle => {
                if ch.is_lowercase() {
                    upper(ch)
                } else if ch.is_uppercase() || is_title(ch) {
                    lower(ch)
                } else {
                    ch
                }
            }
            Self::Rot13 => match ch {
                'a'..='z' => char::from((ch as u8 - b'a' + 13) % 26 + b'a'),
                'A'..='Z' => char::from((ch as u8 - b'A' + 13) % 26 + b'A'),
                _ => ch,
            },
        }
    }

    /// `text`, with its case changed, as many characters as it was.
    pub(crate) fn text(self, text: &str) -> String {
        let mut changed = String::with_capacity(text.len());
        changed.extend(text.chars().map(|ch| self.char(ch)));
        changed
    }
}

/// `ch` as a capital, if that's one character.
pub(crate) fn upper(ch: char) -> char {
    if ch.is_ascii() {
        return ch.to_ascii_uppercase();
    }
    match ch {
        'ß' => return 'ẞ',
        // Greek with a subscript iota: its capital with one, which Unicode
        // keeps apart from the two letters it becomes in full.
        '\u{1F80}'..='\u{1F87}' | '\u{1F90}'..='\u{1F97}' | '\u{1FA0}'..='\u{1FA7}' => {
            return char::from_u32(ch as u32 + 8).unwrap_or(ch);
        }
        '\u{1FB3}' => return '\u{1FBC}',
        '\u{1FC3}' => return '\u{1FCC}',
        '\u{1FF3}' => return '\u{1FFC}',
        _ => {}
    }
    one(ch.to_uppercase()).unwrap_or(ch)
}

/// `ch` as a small letter, if that's one character, or for `İ`, the `i`
/// it's written with.
pub(crate) fn lower(ch: char) -> char {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase();
    }
    let mut lowered = ch.to_lowercase();
    let first = lowered.next().unwrap_or(ch);
    // Only `İ` is more than one, `i` and a combining dot.
    if lowered.next().is_some() && ch != 'İ' {
        return ch;
    }
    first
}

/// Whether `ch` is a title-case letter, as `ǅ` is, which Vim's `~` takes
/// down, as it does capitals.
fn is_title(ch: char) -> bool {
    !ch.is_lowercase() && !ch.is_uppercase() && lower(ch) != ch
}

/// The one character `chars` holds, if it's one.
fn one(mut chars: impl Iterator<Item = char>) -> Option<char> {
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_character_stays_one_as_in_vim() {
        // As Neovim does: `gUU`, `guu` and `g~~` on these.
        assert_eq!(
            Recase::Upper.text("straße İ ǰ ﬀ ᾳ ᾀ ẞ x"),
            "STRAẞE İ ǰ ﬀ ᾼ ᾈ ẞ X"
        );
        assert_eq!(
            Recase::Lower.text("STRASSE İ ẞ ΣΑΣ ᾈ ᾼ X"),
            "strasse i ß σασ ᾀ ᾳ x"
        );
        assert_eq!(
            Recase::Toggle.text("straße İ ǰ ﬀ ᾳ ẞ xY"),
            "STRAẞE i ǰ ﬀ ᾼ ß Xy"
        );
        assert_eq!(Recase::Rot13.text("Hello, wörld"), "Uryyb, jöeyq");
        assert_eq!(Recase::Toggle.text("1234 ǅ"), "1234 ǆ");
    }
}
