//! Pieces of code: the functions, classes and arguments a language's
//! grammar finds in a text, which text objects such as `af` pick out and
//! motions such as `]f` go to.
//!
//! Only the daemon has the syntax tree they're found in, so a window asks
//! it for them, and moves what it's told along with edits made since, as
//! it moves diagnostics.

use crate::edit::{Bias, Edit};

/// A kind of piece of code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Construct {
    /// A function, method or closure: `af`, and its body, `if`.
    Function,
    /// A class, or what a language has instead, such as a struct, an
    /// impl, a trait, an enum or an interface: `ac`, and its body, `ic`.
    Class,
    /// One argument of a call, or parameter of a function: `ia`, and with
    /// the comma that separates it from the next or the one before, `aa`.
    Argument,
}

impl Construct {
    /// What it's called, for messages such as "no function here".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Argument => "argument",
        }
    }
}

/// One piece of code found: bytes `start` to `end` of the text, and its
/// insides, if it has any, such as a function's body without its braces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Piece {
    /// Its first byte.
    pub start: usize,
    /// The byte after its last.
    pub end: usize,
    /// Its insides: their first byte, and the byte after their last.
    pub inner: Option<(usize, usize)>,
}

impl Piece {
    /// Moves it along with `edit`, made to the text it was found in. Text
    /// put in at either edge stays outside it.
    pub fn map(&mut self, edit: &Edit) {
        let range = |(start, end): (usize, usize)| {
            let start = edit.map(start, Bias::After);
            (start, edit.map(end, Bias::Before).max(start))
        };
        (self.start, self.end) = range((self.start, self.end));
        self.inner = self.inner.map(range);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_piece_moves_with_edits_and_keeps_text_at_its_edges_out() {
        let mut piece = Piece {
            start: 4,
            end: 10,
            inner: Some((6, 9)),
        };
        // "ab" put in at its start, and a byte taken out of its insides.
        let mut edit = Edit::new();
        edit.retain(4).insert("ab").retain(3).delete("x").retain(6);
        piece.map(&edit);
        assert_eq!(
            piece,
            Piece {
                start: 6,
                end: 11,
                inner: Some((8, 10)),
            }
        );
        // Text put in at its end stays out.
        let mut edit = Edit::new();
        edit.retain(11).insert("cd").retain(4);
        piece.map(&edit);
        assert_eq!((piece.start, piece.end), (6, 11));
    }
}
