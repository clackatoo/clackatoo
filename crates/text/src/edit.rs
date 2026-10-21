//! Edits: changes to a text, and how two made at once are reconciled.

use ropey::Rope;

/// An edit of more pieces than this, and more than one for each
/// [`PIECE_BYTES`] of the text, is made by writing the text out afresh, as
/// `:%s` can make hundreds of thousands: each piece made in the rope is a
/// search down its tree, where writing it out costs as much as the text
/// is long.
const REWRITE_PIECES: usize = 64;
const PIECE_BYTES: usize = 64;

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
    serde(try_from = "Vec<Op>", into = "Vec<Op>")
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
    /// The edit's pieces cover more bytes than any text can hold.
    #[error("the edit is longer than any text")]
    TooLong,
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
        edit.retain(at)
            .delete(removed)
            .insert(inserted)
            .retain(after);
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
        // Past the end, as a place a window sent can be.
        new.saturating_add(place.saturating_sub(old))
    }

    /// Where `place` is after the edit, as [`map`](Self::map) has it, but
    /// a place in text replaced, taken away with other text put in its
    /// place, keeps its offset into what's put in, as far as that goes: so
    /// a mark on a character whose case is changed stays on it, where
    /// `map` would take it past.
    #[must_use]
    pub fn map_keeping(&self, place: usize, bias: Bias) -> usize {
        let (mut old, mut new) = (0, 0);
        // The text just put in where the next deletion starts, if any.
        let mut put: Option<&str> = None;
        for op in &self.ops {
            match op {
                Op::Retain(len) => {
                    if place < old + len {
                        return new + (place - old);
                    }
                    old += len;
                    new += len;
                    put = None;
                }
                Op::Insert(text) => {
                    if place == old && bias == Bias::Before {
                        return new;
                    }
                    new += text.len();
                    put = Some(text);
                }
                Op::Delete(text) => {
                    if place < old + text.len() {
                        let Some(put) = put else {
                            return new;
                        };
                        // Back to a character's start, in what's put in.
                        let mut offset = (place - old).min(put.len());
                        while !put.is_char_boundary(offset) {
                            offset -= 1;
                        }
                        return new - put.len() + offset;
                    }
                    old += text.len();
                    put = None;
                }
            }
        }
        new.saturating_add(place.saturating_sub(old))
    }

    /// Where bytes `start` to `end` of the text before the edit are in the
    /// text after it, if it changes none of them and puts nothing in at
    /// either end: a name with anything typed beside it isn't that name
    /// any more. `None` if it does.
    #[must_use]
    pub fn map_untouched(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let touched =
            |(at, removed, _): (usize, &str, &str)| at <= end && at + removed.len() >= start;
        if self.changes().any(touched) {
            return None;
        }
        Some((self.map(start, Bias::After), self.map(end, Bias::Before)))
    }

    /// The lines it moves, if all it does is take lines away at one place
    /// and put the same back at another, as `:m` moves them: so places in
    /// them can go with them, where [`map`](Self::map) would leave them
    /// where they were. Lines moved to or from the end of a text with no
    /// line break after it take the break from the line before them, or
    /// give theirs to it, which isn't moved.
    ///
    /// Only lines with more than blanks in them are moved: `=` taking an
    /// indent off one line and putting as much on another, or a formatter
    /// taking a blank line out here and putting one in there, moves
    /// nothing. Whether the text taken and put are whole lines, from a
    /// line's start, is for [`Moved::is_lines`] to say, from the text the
    /// edit makes.
    #[must_use]
    pub fn moved(&self) -> Option<Moved> {
        let mut changes = self.changes();
        let (first, second) = (changes.next()?, changes.next()?);
        if changes.next().is_some() {
            return None;
        }
        let ((taken_at, taken), (put_at, put)) = match (first, second) {
            ((taken_at, taken, ""), (put_at, "", put))
            | ((put_at, "", put), (taken_at, taken, "")) => ((taken_at, taken), (put_at, put)),
            _ => return None,
        };
        if taken.chars().all(char::is_whitespace) {
            return None;
        }
        // How far into what's taken, and into what's put, the lines moved
        // start: past a break that isn't theirs.
        let (taken_skip, put_skip, was_last, is_last) = if taken == put && taken.ends_with('\n') {
            (0, 0, false, false)
        } else if (taken.strip_prefix('\n'))
            .is_some_and(|lines| put.strip_suffix('\n') == Some(lines))
        {
            // Up from the end: the break taken is the line before's,
            // which their last line's own becomes.
            (1, 0, true, false)
        } else if (taken.strip_suffix('\n'))
            .is_some_and(|lines| put.strip_prefix('\n') == Some(lines))
        {
            // Down to the end: the last line's break goes before them, and
            // what was after their own last is the text's end.
            (0, 1, false, true)
        } else {
            return None;
        };
        Some(Moved {
            from: taken_at + taken_skip,
            to: self.map(put_at, Bias::Before) + put_skip,
            len: taken.len() - taken_skip,
            gap: self.map(taken_at, Bias::Before),
            was_last,
            is_last,
        })
    }

    /// Each change it makes, as where it is in the text before the edit,
    /// the text taken away there and the text put in its place.
    pub fn changes(&self) -> impl Iterator<Item = (usize, &str, &str)> {
        self.walk()
    }

    /// Moves places through it as [`map_untouched`](Self::map_untouched)
    /// moves one, `start` and `end` the same, but going through the edit
    /// once for them all, if they're asked for in order: so moving each of
    /// thousands of places through an edit of thousands of changes, as
    /// `:%s` makes, costs as much as both, not their product.
    #[must_use]
    pub fn untouched(&self) -> Untouched<'_> {
        Untouched {
            changes: self.walk().peekable(),
            moved: 0,
        }
    }

    /// [`changes`](Self::changes), as a type that can be kept.
    fn walk(&self) -> Changes<'_> {
        Changes {
            ops: self.ops.iter().peekable(),
            at: 0,
        }
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
        if self.ops.len() > REWRITE_PIECES
            && self.ops.len().saturating_mul(PIECE_BYTES) > rope.len_bytes()
        {
            let made = self.apply_to(&rope.to_string())?;
            *rope = Rope::from_str(&made);
            return Ok(());
        }
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
                    let (gone, after) =
                        (rest.split_at_checked(deleted.len())).ok_or(EditError::Boundary)?;
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

/// Pieces read from elsewhere, such as from a window, put in their
/// simplest form. Refused if they cover more bytes than any text can hold:
/// those lengths would wrap round, and the edit could then pass for one
/// made to a real text.
impl TryFrom<Vec<Op>> for Edit {
    type Error = EditError;

    fn try_from(ops: Vec<Op>) -> Result<Self, EditError> {
        let (mut base, mut target) = (0_usize, 0_usize);
        for op in &ops {
            let (before, after) = match op {
                Op::Retain(len) => (*len, *len),
                Op::Insert(text) => (0, text.len()),
                Op::Delete(text) => (text.len(), 0),
            };
            base = base.checked_add(before).ok_or(EditError::TooLong)?;
            target = target.checked_add(after).ok_or(EditError::TooLong)?;
        }
        let mut edit = Self::new();
        for op in &ops {
            match op {
                Op::Retain(len) => edit.retain(*len),
                Op::Insert(text) => edit.insert(text),
                Op::Delete(text) => edit.delete(text),
            };
        }
        Ok(edit)
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

/// The changes an edit makes, in order: see [`Edit::changes`].
#[derive(Debug)]
struct Changes<'a> {
    ops: std::iter::Peekable<std::slice::Iter<'a, Op>>,
    /// Where the next is looked for from, in the text before the edit.
    at: usize,
}

impl<'a> Iterator for Changes<'a> {
    type Item = (usize, &'a str, &'a str);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.ops.next()? {
                Op::Retain(len) => self.at += len,
                Op::Insert(inserted) => {
                    let removed = match self.ops.next_if(|op| matches!(op, Op::Delete(_))) {
                        Some(Op::Delete(removed)) => removed.as_str(),
                        _ => "",
                    };
                    let change = (self.at, removed, inserted.as_str());
                    self.at += removed.len();
                    return Some(change);
                }
                Op::Delete(removed) => {
                    let change = (self.at, removed.as_str(), "");
                    self.at += removed.len();
                    return Some(change);
                }
            }
        }
    }
}

/// Lines an edit moves whole from one place to another, as
/// [`Edit::moved`] finds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moved {
    /// Where they start in the text before the edit...
    pub from: usize,
    /// ...where they start in the text after it...
    pub to: usize,
    /// ...and how many bytes of them there were before.
    pub len: usize,
    /// Where they were taken from, in the text after the edit.
    gap: usize,
    /// They were the text's last lines, with no break after them...
    was_last: bool,
    /// ...or they are.
    is_last: bool,
}

impl Moved {
    /// Whether they're whole lines, given `after`, the text the edit
    /// makes: taken from a line's start, and put at one, or each from or
    /// to the text's end, with no break after them.
    #[must_use]
    pub fn is_lines(&self, after: &Rope) -> bool {
        let len = after.len_bytes();
        let starts_line = |at: usize| at == 0 || (at <= len && after.byte(at - 1) == b'\n');
        let taken = if self.was_last {
            self.gap == len
        } else {
            starts_line(self.gap)
        };
        // Put at the end, the break before them is theirs to put, and
        // they're the rest.
        let put = !self.is_last || self.to + self.len - 1 == len;
        starts_line(self.to) && taken && put
    }

    /// Where `place`, a byte offset into the text before the edit, goes
    /// if it's in the text moved: as far into it as it was.
    #[must_use]
    pub fn place(&self, place: usize) -> Option<usize> {
        (place.checked_sub(self.from))
            .filter(|&offset| offset < self.len)
            .map(|offset| self.to + offset)
    }
}

/// Places moved through an edit one after another, in order, as
/// [`Edit::untouched`] makes it.
#[derive(Debug)]
pub struct Untouched<'a> {
    changes: std::iter::Peekable<Changes<'a>>,
    /// How far the changes passed so far moved what's after them.
    moved: isize,
}

impl Untouched<'_> {
    /// Where `place` goes, if no change touches it, at it or beside it, as
    /// [`Edit::map_untouched`] has it. Places must be asked for in order,
    /// none before the last.
    pub fn place(&mut self, place: usize) -> Option<usize> {
        let signed = |len: usize| isize::try_from(len).unwrap_or(isize::MAX);
        while let Some(&(start, removed, inserted)) = self.changes.peek()
            && start + removed.len() < place
        {
            self.moved += signed(inserted.len()) - signed(removed.len());
            self.changes.next();
        }
        match self.changes.peek() {
            Some(&(start, removed, _)) if start <= place && place <= start + removed.len() => None,
            _ => Some(place.saturating_add_signed(self.moved)),
        }
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
    fn map_keeping_keeps_places_in_replaced_text_and_is_map_elsewhere() {
        // `bc` to `BC`, as a case changed: the places on them stay.
        let edit = Edit::replace(5, 1, "bc", "BC");
        assert_eq!(edit.map(1, Bias::After), 3);
        assert_eq!(edit.map_keeping(1, Bias::After), 1);
        assert_eq!(edit.map_keeping(2, Bias::Before), 2);
        assert_eq!(edit.map_keeping(3, Bias::After), 3);
        assert_eq!(edit.map_keeping(4, Bias::After), 4);
        // Into shorter text, as far as it goes, on a character's start.
        let edit = Edit::replace(6, 0, "abcd", "é");
        assert_eq!(edit.map_keeping(3, Bias::After), 2);
        assert_eq!(edit.map_keeping(1, Bias::After), 0);
        assert_eq!(edit.map_keeping(4, Bias::After), 2);
        // Text only taken away, or only put in, as `map` has it.
        let edit = Edit::replace(5, 1, "bc", "");
        assert_eq!(edit.map_keeping(2, Bias::After), 1);
        let edit = Edit::replace(5, 1, "", "xy");
        assert_eq!(edit.map_keeping(1, Bias::After), 3);
        assert_eq!(edit.map_keeping(1, Bias::Before), 1);
    }

    #[test]
    fn text_taken_away_and_put_back_elsewhere_is_moved_and_places_in_it_go_with_it() {
        // `a\nb\n` down below `c\n`, and back up.
        let mut down = Edit::new();
        down.delete("a\nb\n").retain(2).insert("a\nb\n").retain(2);
        assert_eq!(down.apply_to("a\nb\nc\nd\n").unwrap(), "c\na\nb\nd\n");
        let lines = |edit: &Edit, before: &str| {
            let after = Rope::from_str(&edit.apply_to(before).unwrap());
            edit.moved().filter(|moved| moved.is_lines(&after))
        };
        let moved = lines(&down, "a\nb\nc\nd\n").unwrap();
        assert_eq!((moved.from, moved.to, moved.len), (0, 2, 4));
        assert_eq!(moved.place(2), Some(4));
        assert_eq!(moved.place(4), None, "c's line isn't moved");
        let up = lines(&down.invert(), "c\na\nb\nd\n").unwrap();
        assert_eq!((up.from, up.to, up.len), (2, 0, 4));
        // To the end of a text with no break after it, the break goes
        // before them, and back up, it stays with the line before.
        let mut down = Edit::new();
        down.delete("a\n").retain(1).insert("\na");
        assert_eq!(down.apply_to("a\nb").unwrap(), "b\na");
        let moved = lines(&down, "a\nb").unwrap();
        assert_eq!((moved.from, moved.to, moved.len), (0, 2, 2));
        assert_eq!(moved.place(1), Some(3), "a's break is the text's end");
        let up = lines(&down.invert(), "b\na").unwrap();
        assert_eq!((up.from, up.to, up.len), (2, 0, 1));
        assert_eq!(up.place(1), None, "b's break is b's");
        // Text that isn't whole lines isn't lines moved: here from a
        // line's middle, and there to one.
        let mut within = Edit::new();
        within.retain(1).delete("a\n").retain(2).insert("a\n");
        assert_eq!(lines(&within, "xa\nb\n"), None);
        let mut into = Edit::new();
        into.delete("a\n").retain(1).insert("a\n").retain(2);
        assert_eq!(lines(&into, "a\nbc\n"), None);
        // Nor are blanks: an indent taken off one line and put on
        // another, as `=` does, or a blank line taken out and put in.
        let mut indent = Edit::new();
        indent.delete("    ").retain(6).insert("    ").retain(2);
        assert_eq!(
            indent.apply_to("    a\nb {\nc\n").unwrap(),
            "a\nb {\n    c\n"
        );
        assert_eq!(indent.moved(), None);
        let mut blank = Edit::new();
        blank.retain(2).delete("\n").retain(2).insert("\n");
        assert_eq!(blank.moved(), None);
        // Anything else isn't a move.
        assert_eq!(Edit::replace(4, 1, "b", "").moved(), None);
        assert_eq!(Edit::replace(4, 1, "", "x\n").moved(), None);
        let mut other = Edit::new();
        other.delete("a\n").retain(2).insert("x\n");
        assert_eq!(other.moved(), None);
        let mut three = Edit::new();
        three
            .delete("a\n")
            .retain(1)
            .insert("a\n")
            .retain(1)
            .insert("z");
        assert_eq!(three.moved(), None);
    }

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
        assert_eq!(Edit::try_from(edit.ops().to_vec()), Ok(edit));
    }

    #[test]
    fn pieces_longer_than_any_text_are_refused() {
        for ops in [
            vec![
                Op::Retain(usize::MAX),
                Op::Insert("x".into()),
                Op::Retain(3),
            ],
            vec![Op::Delete("ab".into()), Op::Retain(usize::MAX - 1)],
            vec![Op::Insert("ab".into()), Op::Retain(usize::MAX - 1)],
        ] {
            assert_eq!(Edit::try_from(ops), Err(EditError::TooLong));
        }
        let longest = Edit::try_from(vec![Op::Retain(usize::MAX)]).unwrap();
        assert_eq!(longest.base_len(), usize::MAX);
    }

    #[test]
    fn text_is_moved_only_if_nothing_touches_it() {
        // "let name = 1;": `name` is bytes 4 to 8.
        let moved = |edit: Edit| edit.map_untouched(4, 8);
        assert_eq!(moved(Edit::replace(13, 0, "", "  ")), Some((6, 10)));
        assert_eq!(moved(Edit::replace(13, 12, ";", "")), Some((4, 8)));
        // In it, or beside it at either end, it's no longer the same.
        assert_eq!(moved(Edit::replace(13, 5, "a", "o")), None);
        assert_eq!(moved(Edit::replace(13, 8, "", "s")), None);
        assert_eq!(moved(Edit::replace(13, 3, " ", "")), None);
        assert_eq!(moved(Edit::keep(13)), Some((4, 8)));
        // Taken away with what's around it.
        assert_eq!(moved(Edit::replace(13, 2, "t name =", "")), None);
        assert_eq!(moved(Edit::replace(13, 4, "name", "")), None);
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
    fn an_edit_of_many_pieces_is_made_as_one_of_few() {
        let text = "ab ".repeat(1000);
        let mut edit = Edit::new();
        for _ in 0..1000 {
            edit.delete("a").insert("x").retain(2);
        }
        let mut rope = Rope::from_str(&text);
        edit.apply(&mut rope).unwrap();
        assert_eq!(rope.to_string(), "xb ".repeat(1000));
        // And a bad one is refused, leaving the text alone.
        let mut rope = Rope::from_str(&"zb ".repeat(1000));
        assert_eq!(edit.apply(&mut rope), Err(EditError::Mismatch));
        assert_eq!(rope.to_string(), "zb ".repeat(1000));
    }

    #[test]
    fn a_bad_edit_leaves_the_text_alone() {
        let mut rope = Rope::from_str("héllo");
        let wrong = Edit::replace(6, 0, "j", "");
        assert_eq!(wrong.apply(&mut rope), Err(EditError::Mismatch));
        let split = Edit::replace(6, 2, "", "x");
        assert_eq!(split.apply(&mut rope), Err(EditError::Boundary));
        let short = Edit::keep(3);
        assert!(matches!(
            short.apply(&mut rope),
            Err(EditError::Length { .. })
        ));
        assert_eq!(rope.to_string(), "héllo");
    }

    #[test]
    fn places_follow_the_text_around_them() {
        let edit = Edit::replace(10, 4, "ab", "xyz");
        assert_eq!(edit.map(2, Bias::After), 2);
        assert_eq!(edit.map(4, Bias::Before), 4);
        assert_eq!(
            edit.map(4, Bias::After),
            7,
            "a deletion follows the insertion"
        );
        assert_eq!(edit.map(5, Bias::After), 7, "in the deleted text");
        assert_eq!(edit.map(6, Bias::After), 7);
        assert_eq!(edit.map(10, Bias::After), 11);
        let mut typed = Edit::new();
        typed.retain(3).insert("!");
        assert_eq!(typed.map(3, Bias::After), 4, "the cursor of the typist");
        assert_eq!(typed.map(3, Bias::Before), 3);
        assert_eq!(
            typed.map(usize::MAX, Bias::After),
            usize::MAX,
            "past the end"
        );
    }

    #[test]
    fn changes_are_listed_where_they_were() {
        let mut edit = Edit::new();
        edit.retain(1)
            .delete("b")
            .retain(2)
            .insert("x")
            .delete("e")
            .retain(1)
            .insert("!");
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
            (edit_of(text.clone()), edit_of(text)).prop_map(|((text, a), (_, b))| (text, a, b))
        })
    }

    fn edit_and_next() -> impl Strategy<Value = (String, Edit, Edit)> {
        text().prop_flat_map(edit_of).prop_flat_map(|(text, a)| {
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
        fn places_moved_in_order_go_where_each_alone_would(
            (text, a) in text().prop_flat_map(edit_of)
        ) {
            let mut untouched = a.untouched();
            for place in 0..=text.len() {
                let alone = a.map_untouched(place, place).map(|(at, _)| at);
                prop_assert_eq!(untouched.place(place), alone, "at {}", place);
            }
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
