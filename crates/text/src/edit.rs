//! Edits: changes to a text, and how two made at once are reconciled.

use ropey::Rope;

/// One piece of an [`Edit`], which goes through the text from its start.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Op {
    /// Keep this many bytes as they are.
    Retain(usize),
    /// Put this text here.
    Insert(String),
    /// Take this text away. The text itself is kept, not just its length,
    /// so an edit can be undone without the text it was made to.
    Delete(String),
}

/// A change to a whole text: what to keep, insert and delete, from its
/// start to its end.
///
/// An edit is made for a text of one length, its base, and makes a text of
/// another, its target. It's always in its simplest form: no empty pieces,
/// no two pieces of a kind in a row, and an insertion before a deletion at
/// the same place, so equal changes are equal edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(from = "Vec<Op>", into = "Vec<Op>")
)]
pub struct Edit {
    ops: Vec<Op>,
    base_len: usize,
    target_len: usize,
}

/// Why an edit can't be made, or reconciled with another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// The edit is for a text of another length.
    #[error("the edit is for a text of {expected} bytes, not {actual}")]
    Length {
        /// The length the edit is for.
        expected: usize,
        /// The length it was given.
        actual: usize,
    },
    /// The edit deletes text that isn't there.
    #[error("the edit deletes text that isn't there")]
    Mismatch,
    /// The edit would split a character's bytes.
    #[error("the edit splits a character")]
    Boundary,
}

/// Which side of text inserted at a place the place goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    /// Before it, as a mark at the start of the text it's in.
    Before,
    /// After it, as the cursor of the person typing it.
    After,
}

impl Edit {
    /// An edit of nothing, which pieces are added to.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An edit of a text `len` bytes long that changes nothing.
    #[must_use]
    pub fn keep(len: usize) -> Self {
        let mut edit = Self::new();
        edit.retain(len);
        edit
    }

    /// An edit of a text `len` bytes long that replaces `removed`, at
    /// `at`, with `inserted`.
    ///
    /// # Panics
    ///
    /// If `removed` runs past the end of the text.
    #[must_use]
    pub fn replace(len: usize, at: usize, removed: &str, inserted: &str) -> Self {
        let after = (len.checked_sub(at))
            .and_then(|rest| rest.checked_sub(removed.len()))
            .expect("the text replaced is in the text");
        let mut edit = Self::new();
        edit.retain(at).delete(removed).insert(inserted).retain(after);
        edit
    }

    /// Keeps the next `len` bytes.
    pub fn retain(&mut self, len: usize) -> &mut Self {
        if len == 0 {
            return self;
        }
        self.base_len += len;
        self.target_len += len;
        if let Some(Op::Retain(last)) = self.ops.last_mut() {
            *last += len;
        } else {
            self.ops.push(Op::Retain(len));
        }
        self
    }

    /// Inserts `text` here.
    pub fn insert(&mut self, text: &str) -> &mut Self {
        if text.is_empty() {
            return self;
        }
        self.target_len += text.len();
        match self.ops.as_mut_slice() {
            [.., Op::Insert(last)] | [.., Op::Insert(last), Op::Delete(_)] => last.push_str(text),
            [.., Op::Delete(_)] => {
                let at = self.ops.len() - 1;
                self.ops.insert(at, Op::Insert(text.to_owned()));
            }
            _ => self.ops.push(Op::Insert(text.to_owned())),
        }
        self
    }

    /// Deletes the next `text.len()` bytes, which are `text`.
    pub fn delete(&mut self, text: &str) -> &mut Self {
        if text.is_empty() {
            return self;
        }
        self.base_len += text.len();
        if let Some(Op::Delete(last)) = self.ops.last_mut() {
            last.push_str(text);
        } else {
            self.ops.push(Op::Delete(text.to_owned()));
        }
        self
    }

    /// Its pieces, in order.
    #[must_use]
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// The length of the text it's made to.
    #[must_use]
    pub fn base_len(&self) -> usize {
        self.base_len
    }

    /// The length of the text it makes.
    #[must_use]
    pub fn target_len(&self) -> usize {
        self.target_len
    }

    /// Whether it changes nothing.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.ops.iter().all(|op| matches!(op, Op::Retain(_)))
    }

    /// The edit that undoes it: made to the text it makes, it makes the
    /// text it was made to.
    #[must_use]
    pub fn invert(&self) -> Self {
        let mut inverse = Self::new();
        for op in &self.ops {
            match op {
                Op::Retain(len) => inverse.retain(*len),
                Op::Insert(text) => inverse.delete(text),
                Op::Delete(text) => inverse.insert(text),
            };
        }
        inverse
    }

    /// Where `place`, a byte offset into the text before the edit, is in
    /// the text after it. A place in deleted text goes to where the
    /// deletion was, and a place where text is inserted goes before or
    /// after it, by `bias`.
    #[must_use]
    pub fn map(&self, place: usize, bias: Bias) -> usize {
        let (mut old, mut new) = (0, 0);
        for op in &self.ops {
            match op {
                Op::Retain(len) => {
                    if place < old + len {
                        return new + (place - old);
                    }
                    old += len;
                    new += len;
                }
                Op::Insert(text) => {
                    if place == old && bias == Bias::Before {
                        return new;
                    }
                    new += text.len();
                }
                Op::Delete(text) => {
                    if place < old + text.len() {
                        return new;
                    }
                    old += text.len();
                }
            }
        }
        new + place.saturating_sub(old)
    }

    /// Each change it makes, as where it is in the text before the edit,
    /// the text taken away there and the text put in its place.
    pub fn changes(&self) -> impl Iterator<Item = (usize, &str, &str)> {
        let mut at = 0;
        let mut ops = self.ops.iter().peekable();
        std::iter::from_fn(move || {
            loop {
                match ops.next()? {
                    Op::Retain(len) => at += len,
                    Op::Insert(inserted) => {
                        let removed = match ops.next_if(|op| matches!(op, Op::Delete(_))) {
                            Some(Op::Delete(removed)) => removed.as_str(),
                            _ => "",
                        };
                        let change = (at, removed, inserted.as_str());
                        at += removed.len();
                        return Some(change);
                    }
                    Op::Delete(removed) => {
                        let change = (at, removed.as_str(), "");
                        at += removed.len();
                        return Some(change);
                    }
                }
            }
        })
    }

    /// Checks it can be made to `rope`, without changing it.
    ///
    /// # Errors
    ///
    /// If `rope` is another length, the edit deletes text that isn't
    /// there, or it would split a character.
    pub fn check(&self, rope: &Rope) -> Result<(), EditError> {
        if rope.len_bytes() != self.base_len {
            return Err(EditError::Length {
                expected: self.base_len,
                actual: rope.len_bytes(),
            });
        }
        let boundary = |at: usize| rope.char_to_byte(rope.byte_to_char(at)) == at;
        let mut at = 0;
        for op in &self.ops {
            if !boundary(at) {
                return Err(EditError::Boundary);
            }
            match op {
                Op::Retain(len) => at += len,
                Op::Insert(_) => {}
                Op::Delete(text) => {
                    let end = at + text.len();
                    if !boundary(end) {
                        return Err(EditError::Boundary);
                    }
                    if rope.byte_slice(at..end) != text.as_str() {
                        return Err(EditError::Mismatch);
                    }
                    at = end;
                }
            }
        }
        Ok(())
    }

    /// Makes the edit to `rope`. Leaves it as it was if it can't.
    ///
    /// # Errors
    ///
    /// As [`check`](Self::check).
    pub fn apply(&self, rope: &mut Rope) -> Result<(), EditError> {
        self.check(rope)?;
        // The byte the next piece starts at, in the text as edited so far.
        let mut at = 0;
        for op in &self.ops {
            match op {
                Op::Retain(len) => at += len,
                Op::Insert(text) => {
                    rope.insert(rope.byte_to_char(at), text);
                    at += text.len();
                }
                Op::Delete(text) => {
                    let start = rope.byte_to_char(at);
                    let end = rope.byte_to_char(at + text.len());
                    rope.remove(start..end);
                }
            }
        }
        Ok(())
    }

    /// Makes the edit to `text`, returning the text it makes.
    ///
    /// # Errors
    ///
    /// As [`check`](Self::check).
    pub fn apply_to(&self, text: &str) -> Result<String, EditError> {
        if text.len() != self.base_len {
            return Err(EditError::Length {
                expected: self.base_len,
                actual: text.len(),
            });
        }
        let mut made = String::with_capacity(self.target_len);
        let mut rest = text;
        for op in &self.ops {
            match op {
                Op::Retain(len) => {
                    let (kept, after) = rest.split_at_checked(*len).ok_or(EditError::Boundary)?;
                    made.push_str(kept);
                    rest = after;
                }
                Op::Insert(inserted) => made.push_str(inserted),
                Op::Delete(deleted) => {
                    let (gone, after) = (rest.split_at_checked(deleted.len()))
                        .ok_or(EditError::Boundary)?;
                    if gone != deleted {
                        return Err(EditError::Mismatch);
                    }
                    rest = after;
                }
            }
        }
        Ok(made)
    }
}

impl From<Vec<Op>> for Edit {
    fn from(ops: Vec<Op>) -> Self {
        let mut edit = Self::new();
        for op in &ops {
            match op {
                Op::Retain(len) => edit.retain(*len),
                Op::Insert(text) => edit.insert(text),
                Op::Delete(text) => edit.delete(text),
            };
        }
        edit
    }
}

impl From<Edit> for Vec<Op> {
    fn from(edit: Edit) -> Self {
        edit.ops
    }
}

/// What's left of a piece of an edit, as two edits are gone through side
/// by side.
#[derive(Clone, Copy, Debug)]
enum Piece<'a> {
    Retain(usize),
    Insert(&'a str),
    Delete(&'a str),
}

/// Goes through an edit's pieces, a part of one at a time if need be.
struct Pieces<'a> {
    ops: std::slice::Iter<'a, Op>,
    head: Option<Piece<'a>>,
}

impl<'a> Pieces<'a> {
    fn new(edit: &'a Edit) -> Self {
        let mut pieces = Self {
            ops: edit.ops.iter(),
            head: None,
        };
        pieces.advance();
        pieces
    }

    fn advance(&mut self) {
        self.head = self.ops.next().map(|op| match op {
            Op::Retain(len) => Piece::Retain(*len),
            Op::Insert(text) => Piece::Insert(text),
            Op::Delete(text) => Piece::Delete(text),
        });
    }

    fn peek(&self) -> Option<Piece<'a>> {
        self.head
    }

    /// The length of the text the next piece covers, or for an insertion,
    /// puts in.
    fn len(&self) -> usize {
        match self.head {
            Some(Piece::Retain(len)) => len,
            Some(Piece::Insert(text) | Piece::Delete(text)) => text.len(),
            None => 0,
        }
    }

    /// Takes the first `len` bytes of the next piece.
    fn take(&mut self, len: usize) -> Result<Piece<'a>, EditError> {
        let (taken, rest) = match self.head {
            Some(Piece::Retain(all)) => (Piece::Retain(len), Piece::Retain(all - len)),
            Some(Piece::Insert(text)) => {
                let (taken, rest) = text.split_at_checked(len).ok_or(EditError::Boundary)?;
                (Piece::Insert(taken), Piece::Insert(rest))
            }
            Some(Piece::Delete(text)) => {
                let (taken, rest) = text.split_at_checked(len).ok_or(EditError::Boundary)?;
                (Piece::Delete(taken), Piece::Delete(rest))
            }
            None => unreachable!("taking from a piece that's there"),
        };
        if len == self.len() {
            self.advance();
        } else {
            self.head = Some(rest);
        }
        Ok(taken)
    }
}

/// Edits `a` and `b`, made at once to the same text, as each must become
/// to follow the other: `a` made after `b`, and `b` after `a`. Making `a`
/// then the second gives the same text as `b` then the first. Where both
/// insert at the same place, `a`'s text goes first.
///
/// # Errors
///
/// If `a` and `b` are for texts of different lengths, or split a
/// character between them.
pub fn transform(a: &Edit, b: &Edit) -> Result<(Edit, Edit), EditError> {
    if a.base_len != b.base_len {
        return Err(EditError::Length {
            expected: a.base_len,
            actual: b.base_len,
        });
    }
    let (mut a_after, mut b_after) = (Edit::new(), Edit::new());
    let (mut a_pieces, mut b_pieces) = (Pieces::new(a), Pieces::new(b));
    loop {
        match (a_pieces.peek(), b_pieces.peek()) {
            (None, None) => return Ok((a_after, b_after)),
            (Some(Piece::Insert(text)), _) => {
                a_after.insert(text);
                b_after.retain(text.len());
                a_pieces.advance();
            }
            (_, Some(Piece::Insert(text))) => {
                a_after.retain(text.len());
                b_after.insert(text);
                b_pieces.advance();
            }
            (None, Some(_)) | (Some(_), None) => {
                unreachable!("edits of the same length end together")
            }
            (Some(_), Some(_)) => {
                let len = a_pieces.len().min(b_pieces.len());
                match (a_pieces.take(len)?, b_pieces.take(len)?) {
                    (Piece::Retain(_), Piece::Retain(_)) => {
                        a_after.retain(len);
                        b_after.retain(len);
                    }
                    // Both deleted it, so neither need.
                    (Piece::Delete(_), Piece::Delete(_)) => {}
                    (Piece::Delete(text), Piece::Retain(_)) => {
                        a_after.delete(text);
                    }
                    (Piece::Retain(_), Piece::Delete(text)) => {
                        b_after.delete(text);
                    }
                    (Piece::Insert(_), _) | (_, Piece::Insert(_)) => {
                        unreachable!("insertions are taken whole")
                    }
                }
            }
        }
    }
}

/// The one edit that makes `a` and then `b`.
///
/// # Errors
///
/// If `b` isn't for the text `a` makes.
pub fn compose(a: &Edit, b: &Edit) -> Result<Edit, EditError> {
    if a.target_len != b.base_len {
        return Err(EditError::Length {
            expected: b.base_len,
            actual: a.target_len,
        });
    }
    let mut both = Edit::new();
    let (mut a_pieces, mut b_pieces) = (Pieces::new(a), Pieces::new(b));
    loop {
        match (a_pieces.peek(), b_pieces.peek()) {
            (None, None) => return Ok(both),
            (Some(Piece::Delete(text)), _) => {
                both.delete(text);
                a_pieces.advance();
            }
            (_, Some(Piece::Insert(text))) => {
                both.insert(text);
                b_pieces.advance();
            }
            (None, Some(_)) | (Some(_), None) => {
                unreachable!("a's target and b's base are the same length")
            }
            (Some(_), Some(_)) => {
                let len = a_pieces.len().min(b_pieces.len());
                match (a_pieces.take(len)?, b_pieces.take(len)?) {
                    (Piece::Retain(_), Piece::Retain(_)) => {
                        both.retain(len);
                    }
                    (Piece::Insert(text), Piece::Retain(_)) => {
                        both.insert(text);
                    }
                    (Piece::Retain(_), Piece::Delete(text)) => {
                        both.delete(text);
                    }
                    // b takes away what a put in.
                    (Piece::Insert(inserted), Piece::Delete(deleted)) => {
                        if inserted != deleted {
                            return Err(EditError::Mismatch);
                        }
                    }
                    (Piece::Delete(_), _) | (_, Piece::Insert(_)) => {
                        unreachable!("taken whole above")
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn edits_keep_their_simplest_form() {
        let mut edit = Edit::new();
        edit.retain(2)
            .retain(1)
            .delete("ab")
            .insert("x")
            .delete("c")
            .insert("y")
            .retain(0)
            .insert("");
        assert_eq!(
            edit.ops(),
            [
                Op::Retain(3),
                Op::Insert("xy".into()),
                Op::Delete("abc".into())
            ]
        );
        assert_eq!((edit.base_len(), edit.target_len()), (6, 5));
        assert_eq!(Edit::from(edit.ops().to_vec()), edit);
    }

    #[test]
    fn an_edit_is_made_to_a_rope_or_a_string_alike() {
        let edit = Edit::replace(11, 6, "world", "there");
        let mut rope = Rope::from_str("hello world");
        edit.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "hello there");
        assert_eq!(edit.apply_to("hello world").unwrap(), "hello there");
    }

    #[test]
    fn a_bad_edit_leaves_the_text_alone() {
        let mut rope = Rope::from_str("héllo");
        let wrong = Edit::replace(6, 0, "j", "");
        assert_eq!(wrong.apply(&mut rope), Err(EditError::Mismatch));
        let split = Edit::replace(6, 2, "", "x");
        assert_eq!(split.apply(&mut rope), Err(EditError::Boundary));
        let short = Edit::keep(3);
        assert!(matches!(short.apply(&mut rope), Err(EditError::Length { .. })));
        assert_eq!(rope.to_string(), "héllo");
    }

    #[test]
    fn places_follow_the_text_around_them() {
        let edit = Edit::replace(10, 4, "ab", "xyz");
        assert_eq!(edit.map(2, Bias::After), 2);
        assert_eq!(edit.map(4, Bias::Before), 4);
        assert_eq!(edit.map(4, Bias::After), 7, "a deletion follows the insertion");
        assert_eq!(edit.map(5, Bias::After), 7, "in the deleted text");
        assert_eq!(edit.map(6, Bias::After), 7);
        assert_eq!(edit.map(10, Bias::After), 11);
        let mut typed = Edit::new();
        typed.retain(3).insert("!");
        assert_eq!(typed.map(3, Bias::After), 4, "the cursor of the typist");
        assert_eq!(typed.map(3, Bias::Before), 3);
    }

    #[test]
    fn changes_are_listed_where_they_were() {
        let mut edit = Edit::new();
        edit.retain(1).delete("b").retain(2).insert("x").delete("e").retain(1).insert("!");
        let changes: Vec<_> = edit.changes().collect();
        assert_eq!(changes, [(1, "b", ""), (4, "e", "x"), (6, "", "!")]);
    }

    #[test]
    fn concurrent_insertions_at_one_place_put_the_first_edits_first() {
        let a = Edit::replace(4, 2, "", "A");
        let b = Edit::replace(4, 2, "", "B");
        let (a_after, b_after) = transform(&a, &b).unwrap();
        let ab = a_after.apply_to(&b.apply_to("abcd").unwrap()).unwrap();
        let ba = b_after.apply_to(&a.apply_to("abcd").unwrap()).unwrap();
        assert_eq!((ab.as_str(), ba.as_str()), ("abABcd", "abABcd"));
    }

    #[test]
    fn deleting_text_someone_else_changed_keeps_their_change() {
        // You delete "bcd"; meanwhile an agent puts "X" between c and d,
        // and deletes the c.
        let you = Edit::replace(5, 1, "bcd", "");
        let mut agent = Edit::new();
        agent.retain(2).delete("c").insert("X").retain(2);
        let (you_after, agent_after) = transform(&you, &agent).unwrap();
        let text = "abcde";
        let one = you_after.apply_to(&agent.apply_to(text).unwrap()).unwrap();
        let two = agent_after.apply_to(&you.apply_to(text).unwrap()).unwrap();
        assert_eq!((one.as_str(), two.as_str()), ("aXe", "aXe"));
    }

    /// Characters of every width, so edits are made across them.
    const CHARS: &[char] = &['a', 'b', '\n', 'é', '中', '🦀'];

    fn text() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(CHARS), 0..24)
            .prop_map(|chars| chars.into_iter().collect())
    }

    /// A random edit of `text`: of each character, keep it, delete it, or
    /// insert something before it.
    fn edit_of(text: String) -> impl Strategy<Value = (String, Edit)> {
        let len = text.chars().count();
        (
            prop::collection::vec(0..4u8, len + 1),
            prop::collection::vec(text_piece(), len + 1),
        )
            .prop_map(move |(choices, inserts)| {
                let mut edit = Edit::new();
                let mut chars = text.chars();
                for (choice, insert) in choices.into_iter().zip(inserts) {
                    if choice == 3 {
                        edit.insert(&insert);
                    }
                    let Some(char) = chars.next() else { break };
                    let mut buffer = [0; 4];
                    let char = char.encode_utf8(&mut buffer);
                    match choice {
                        0 => edit.delete(char),
                        _ => edit.retain(char.len()),
                    };
                }
                (text.clone(), edit)
            })
    }

    fn text_piece() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(CHARS), 1..4)
            .prop_map(|chars| chars.into_iter().collect())
    }

    fn two_edits() -> impl Strategy<Value = (String, Edit, Edit)> {
        text().prop_flat_map(|text| {
            (edit_of(text.clone()), edit_of(text))
                .prop_map(|((text, a), (_, b))| (text, a, b))
        })
    }

    fn edit_and_next() -> impl Strategy<Value = (String, Edit, Edit)> {
        text()
            .prop_flat_map(edit_of)
            .prop_flat_map(|(text, a)| {
                let made = a.apply_to(&text).unwrap();
                (Just(text), Just(a), edit_of(made).prop_map(|(_, b)| b))
            })
    }

    proptest! {
        #[test]
        fn concurrent_edits_converge((text, a, b) in two_edits()) {
            let (a_after, b_after) = transform(&a, &b).unwrap();
            let ab = a_after.apply_to(&b.apply_to(&text).unwrap()).unwrap();
            let ba = b_after.apply_to(&a.apply_to(&text).unwrap()).unwrap();
            prop_assert_eq!(&ab, &ba);
            let mut rope = Rope::from_str(&text);
            b.apply(&mut rope).unwrap();
            a_after.apply(&mut rope).unwrap();
            prop_assert_eq!(rope.to_string(), ab);
        }

        #[test]
        fn composing_makes_both((text, a, b) in edit_and_next()) {
            let both = compose(&a, &b).unwrap();
            let one_by_one = b.apply_to(&a.apply_to(&text).unwrap()).unwrap();
            prop_assert_eq!(both.apply_to(&text).unwrap(), one_by_one);
        }

        #[test]
        fn inverting_undoes((text, a) in text().prop_flat_map(edit_of)) {
            let made = a.apply_to(&text).unwrap();
            prop_assert_eq!(a.invert().apply_to(&made).unwrap(), text);
        }

        #[test]
        fn each_change_is_where_it_says((text, a) in text().prop_flat_map(edit_of)) {
            // Making the changes from the last back to the first, each at
            // the place it gives, makes the edit.
            let mut made = text.clone();
            let changes: Vec<_> = a.changes().collect();
            for (at, removed, inserted) in changes.into_iter().rev() {
                prop_assert_eq!(&made[at..at + removed.len()], removed);
                made.replace_range(at..at + removed.len(), inserted);
            }
            prop_assert_eq!(made, a.apply_to(&text).unwrap());
        }
    }
}
