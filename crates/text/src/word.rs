//! Shared word policies. Vim motions preserve script classes and language
//! punctuation; mouse paths, prose, prompts and diff tokens deliberately
//! use their own named policies rather than accidentally drifting copies.
mod classes;
pub use classes::word_classes;
use unicode_segmentation::UnicodeSegmentation;

/// The characters words are made of besides letters, digits and `_`, as a
/// file's language says, as Vim's `iskeyword` adds them: `-` in CSS, so
/// `w` goes over `margin-top` whole. ASCII punctuation only, kept as one
/// bit each, so asking about a character costs no more than asking whether
/// it's `_`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Words(u128);

impl Words {
    /// None: words are letters, digits and `_`, as in plain text and in
    /// terminals.
    pub const NONE: Self = Self(0);

    /// The ASCII punctuation in `chars`, as a language pack gives it. Any
    /// other character is passed over: letters and digits are in words
    /// already.
    #[must_use]
    pub fn new(chars: &str) -> Self {
        Self(
            (chars.chars())
                .filter(char::is_ascii_punctuation)
                .fold(0, |bits, ch| bits | 1 << u32::from(ch)),
        )
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    /// Whether `ch` is one of them.
    #[must_use]
    pub fn has(self, ch: char) -> bool {
        let ch = u32::from(ch);
        ch < 128 && self.0 >> ch & 1 == 1
    }

    /// Whether `ch` is part of a word: a letter, a digit, `_` or one of
    /// them, or past ASCII, of a class of words, as Vim's `vim_iswordc()`
    /// has it (see [`classes`]).
    #[must_use]
    pub fn is_word(self, ch: char) -> bool {
        if ch.is_ascii() {
            ch.is_ascii_alphanumeric() || ch == '_' || self.has(ch)
        } else {
            classes::of(ch) >= 2
        }
    }

    /// Them, in the order of their codes.
    pub fn chars(self) -> impl Iterator<Item = char> {
        (0..128_u8).map(char::from).filter(move |&ch| self.has(ch))
    }
}

/// What kind of character a cell holds, for word motions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Whitespace between words.
    Blank,
    /// Letters, digits and `_`, and the characters the file's language
    /// adds to them (see [`Words`]), and most others past Latin-1.
    Word,
    /// Any other printing character.
    Punctuation,
    /// A script's own kind of word past Latin-1, as Vim's `utf_class()`
    /// sorts them: CJK ideographs, kana, hangul, emoji (see [`classes`]).
    Script(u32),
}

impl Class {
    /// Preserve the individual word class.
    #[must_use]
    pub fn of(class: Self) -> Self {
        class
    }

    /// For `W`, `B` and `E`: anything but blanks is one kind.
    #[must_use]
    pub fn big(class: Self) -> Self {
        match class {
            Self::Blank => Self::Blank,
            Self::Word | Self::Punctuation | Self::Script(_) => Self::Word,
        }
    }
}

/// The class of `ch`, with `words` part of words: ASCII as Vim's
/// `iskeyword` has it, with what the language adds; past it, as Vim's
/// classes have it (see [`classes`]).
#[must_use]
pub fn class_of(ch: char, words: Words) -> Class {
    if ch == ' ' || ch == '\t' {
        return Class::Blank;
    }
    if ch.is_ascii() {
        return if words.is_word(ch) {
            Class::Word
        } else {
            Class::Punctuation
        };
    }
    match classes::of(ch) {
        0 => Class::Blank,
        1 => Class::Punctuation,
        2 => Class::Word,
        class => Class::Script(class),
    }
}

/// The word boundaries appropriate to an interaction.
#[derive(Clone, Copy, Debug)]
pub enum WordClass {
    /// Vim motions, objects and editor double clicks.
    Vim(Words),
    /// Terminal double clicks include punctuation in paths and URLs.
    Path,
    /// Ctrl+W in simple prompts removes an alphanumeric run.
    Prompt,
    /// Command-line word movement also includes underscores.
    CommandLine,
    /// Diff tokens keep punctuation and newlines separate.
    Merge,
    /// Prose mouse selection follows Unicode word boundaries.
    Prose,
}

impl WordClass {
    /// Classify a character for this policy. Prose ranges additionally use
    /// Unicode's contextual word boundaries in `range`.
    #[must_use]
    pub fn class(self, ch: char) -> Class {
        if let Self::Vim(words) = self {
            return class_of(ch, words);
        }
        let word = ch.is_alphanumeric()
            || match self {
                Self::Path => "_-./~:@%+=#?&$".contains(ch),
                Self::CommandLine | Self::Merge => ch == '_',
                _ => false,
            };
        if word {
            Class::Word
        } else if ch.is_whitespace() && !(matches!(self, Self::Merge) && ch == '\n') {
            Class::Blank
        } else {
            Class::Punctuation
        }
    }

    /// The word containing a UTF-8 byte, as an exclusive range.
    #[must_use]
    pub fn range(self, text: &str, byte: usize) -> std::ops::Range<usize> {
        let byte = byte.min(text.len());
        let byte = text.floor_char_boundary(byte);
        if matches!(self, Self::Prose) {
            return text
                .split_word_bound_indices()
                .find_map(|(start, word)| {
                    (start <= byte && byte < start + word.len())
                        .then_some(start..start + word.len())
                })
                .unwrap_or(byte..byte);
        }
        let Some(ch) = text[byte..].chars().next() else {
            return byte..byte;
        };
        let class = self.class(ch);
        let mut start = byte;
        for (at, ch) in text[..byte].char_indices().rev() {
            if self.class(ch) != class {
                break;
            }
            start = at;
        }
        let mut end = byte;
        for (at, ch) in text[byte..].char_indices() {
            if self.class(ch) != class {
                break;
            }
            end = byte + at + ch.len_utf8();
        }
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policies_preserve_the_intent_of_each_interaction() {
        let vim = WordClass::Vim(Words::NONE);
        assert_eq!(vim.range("中文alpha", 0), 0..6);
        assert_eq!(vim.range("margin-top", 0), 0..6);
        assert_eq!(
            WordClass::Vim(Words::new("-")).range("margin-top", 0),
            0..10
        );
        assert_eq!(WordClass::Path.range("open ~/src/main.rs:12 now", 9), 5..21);
        assert_eq!(WordClass::Prompt.class('_'), Class::Punctuation);
        assert_eq!(WordClass::CommandLine.class('_'), Class::Word);
        assert_eq!(WordClass::Merge.class('\n'), Class::Punctuation);
        assert_eq!(WordClass::Merge.class('\t'), Class::Blank);
        assert_eq!(WordClass::Prose.range("don't stop", 3), 0..5);
        assert_eq!(WordClass::Prose.range("café noir", 3), 0..5);
    }
}
