//! The codepoints that follow a cell's first: combining accents, variation
//! selectors, and the rest of joined emoji like 👩‍💻.

use std::ops::Range;

/// The most codepoints a cell may hold beyond its first.
///
/// Real graphemes stay far below this (long emoji ZWJ sequences have about
/// ten). The cap keeps a stream of combining characters from growing one
/// cell without limit. Past it, more are dropped, as anything that lays
/// text out in cells the way rows do must know.
pub const MAX_EXTRA: usize = 32;

/// Codepoints stored in the entry itself. Enough for an accent or two, or a
/// two- or three-part ZWJ emoji; longer graphemes go to the overflow.
const INLINE: usize = 5;

/// A row's graphemes, sorted by column. Text arrives left to right, so a new
/// grapheme almost always goes on the end.
///
/// Entries are plain data, so clearing a row's graphemes, which happens every
/// time a row is recycled, costs nothing per entry. The rare grapheme too long
/// to keep inline lives in `overflow`, which the row shares.
#[derive(Clone, Debug, Default)]
pub(crate) struct Graphemes {
    entries: Vec<Entry>,
    /// Codepoints of graphemes longer than [`INLINE`]. Space that graphemes
    /// no longer use is reclaimed by [`compact`](Graphemes::compact).
    overflow: Vec<char>,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    col: u16,
    len: u8,
    storage: Storage,
}

#[derive(Clone, Copy, Debug)]
enum Storage {
    Inline([char; INLINE]),
    /// Where the codepoints start in [`Graphemes::overflow`].
    Overflow(u32),
}

impl Entry {
    fn new(col: u16, ch: char) -> Self {
        let mut chars = ['\0'; INLINE];
        chars[0] = ch;
        Self {
            col,
            len: 1,
            storage: Storage::Inline(chars),
        }
    }

    fn len(&self) -> usize {
        usize::from(self.len)
    }

    fn chars<'a>(&'a self, overflow: &'a [char]) -> &'a [char] {
        match &self.storage {
            Storage::Inline(chars) => &chars[..self.len()],
            Storage::Overflow(start) => {
                let start = overflow_index(*start);
                &overflow[start..start + self.len()]
            }
        }
    }
}

fn overflow_index(start: u32) -> usize {
    usize::try_from(start).expect("u32 fits in usize")
}

impl Graphemes {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The extra codepoints of the cell at `col`, if it has any.
    pub(crate) fn get(&self, col: usize) -> Option<&[char]> {
        self.entry(col)
            .map(|entry| self.entries[entry].chars(&self.overflow))
    }

    /// Appends `ch` to the cell at `col`, unless it already holds
    /// [`MAX_EXTRA`] codepoints. Returns whether the cell had none before, so
    /// the caller can flag it.
    pub(crate) fn append(&mut self, col: usize, ch: char) -> bool {
        match self.search(col) {
            Ok(entry) => {
                if self.entries[entry].len() < MAX_EXTRA {
                    self.push(entry, ch);
                }
                false
            }
            Err(at) => {
                let col = u16::try_from(col).expect("rows are at most Size::MAX wide");
                self.entries.insert(at, Entry::new(col, ch));
                true
            }
        }
    }

    /// Adds `ch` to the end of an entry's codepoints.
    fn push(&mut self, index: usize, ch: char) {
        let entry = self.entries[index];
        let len = entry.len();
        match entry.storage {
            Storage::Inline(mut chars) if len < INLINE => {
                chars[len] = ch;
                self.entries[index].storage = Storage::Inline(chars);
            }
            // At the end of the overflow, the codepoints can grow in place.
            Storage::Overflow(start) if overflow_index(start) + len == self.overflow.len() => {
                self.overflow.push(ch);
            }
            // Otherwise they move to the end, leaving the old copy unused.
            _ => {
                self.compact_if_wasteful();
                let entry = self.entries[index];
                let start = self.overflow.len();
                let moved: [char; MAX_EXTRA] = {
                    let mut buffer = ['\0'; MAX_EXTRA];
                    buffer[..len].copy_from_slice(entry.chars(&self.overflow));
                    buffer
                };
                self.overflow.extend_from_slice(&moved[..len]);
                self.overflow.push(ch);
                let start = u32::try_from(start).expect("the overflow is compacted long before 4G");
                self.entries[index].storage = Storage::Overflow(start);
            }
        }
        self.entries[index].len += 1;
    }

    /// Reclaims the overflow's unused space once it is mostly waste, so a
    /// program rewriting long graphemes in place can't grow it forever.
    fn compact_if_wasteful(&mut self) {
        if self.overflow.len() < 256 {
            return;
        }
        let used: usize = self
            .entries
            .iter()
            .filter(|entry| matches!(entry.storage, Storage::Overflow(_)))
            .map(Entry::len)
            .sum();
        if self.overflow.len() > 2 * used {
            self.compact();
        }
    }

    /// Rewrites the overflow with only the codepoints entries still use.
    fn compact(&mut self) {
        let mut compacted = Vec::with_capacity(self.overflow.len() / 2);
        for entry in &mut self.entries {
            if let Storage::Overflow(start) = entry.storage {
                let new_start = u32::try_from(compacted.len()).expect("smaller than before");
                let start = overflow_index(start);
                compacted.extend_from_slice(&self.overflow[start..start + entry.len()]);
                entry.storage = Storage::Overflow(new_start);
            }
        }
        self.overflow = compacted;
    }

    pub(crate) fn remove(&mut self, col: usize) {
        if let Ok(entry) = self.search(col) {
            self.entries.remove(entry);
        }
    }

    pub(crate) fn remove_range(&mut self, cols: &Range<usize>) {
        let start = self.search(cols.start).unwrap_or_else(|at| at);
        let end = self.search(cols.end).unwrap_or_else(|at| at);
        self.entries.drain(start..end);
    }

    /// Moves graphemes at or after `col` right by `count`, dropping any that
    /// end up at or beyond `width`.
    pub(crate) fn shift_right(&mut self, col: usize, count: usize, width: usize) {
        self.entries.retain_mut(|entry| {
            let c = usize::from(entry.col);
            if c < col {
                return true;
            }
            if c + count >= width {
                return false;
            }
            entry.col = u16::try_from(c + count).expect("the new column is within the row");
            true
        });
    }

    /// Drops graphemes in `col..col + count`, and moves those after them left
    /// by `count`.
    pub(crate) fn shift_left(&mut self, col: usize, count: usize) {
        self.entries.retain_mut(|entry| {
            let c = usize::from(entry.col);
            if c < col {
                true
            } else if c < col + count {
                false
            } else {
                entry.col = u16::try_from(c - count).expect("the new column is within the row");
                true
            }
        });
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.overflow.clear();
    }

    /// The columns that have graphemes, for invariant checks.
    #[cfg(test)]
    pub(crate) fn cols(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries.iter().map(|entry| usize::from(entry.col))
    }

    fn entry(&self, col: usize) -> Option<usize> {
        self.search(col).ok()
    }

    /// Where `col`'s entry is, or where it would go.
    fn search(&self, col: usize) -> Result<usize, usize> {
        // The common cases, the last grapheme or one after all the others,
        // skip the search.
        match self.entries.last() {
            None => Err(0),
            Some(last) if usize::from(last.col) == col => Ok(self.entries.len() - 1),
            Some(last) if usize::from(last.col) < col => Err(self.entries.len()),
            Some(_) => self
                .entries
                .binary_search_by_key(&col, |entry| usize::from(entry.col)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphemes_spill_to_the_heap_and_stop_at_the_cap() {
        let mut graphemes = Graphemes::default();
        assert!(graphemes.append(3, 'a'));
        for i in 1..100 {
            assert!(!graphemes.append(3, char::from(b'a' + (i % 26))));
        }
        let extra = graphemes.get(3).unwrap();
        assert_eq!(extra.len(), MAX_EXTRA);
        assert_eq!(&extra[..3], ['a', 'b', 'c']);
        assert_eq!(graphemes.get(2), None);
    }

    #[test]
    fn rewriting_long_graphemes_in_place_does_not_grow_the_overflow() {
        let letter = |i: u8, round: u16| char::from(b'a' + i + u8::try_from(round % 7).unwrap());
        let mut graphemes = Graphemes::default();
        for round in 0..10_000 {
            graphemes.remove(0);
            for i in 0..12 {
                graphemes.append(0, letter(i, round));
            }
            graphemes.append(1, 'z');
            graphemes.append(1, 'y');
        }
        assert!(
            graphemes.overflow.len() < 512,
            "overflow is {}",
            graphemes.overflow.len()
        );
        let expected: Vec<char> = (0..12).map(|i| letter(i, 9999)).collect();
        assert_eq!(graphemes.get(0), Some(&expected[..]));
    }

    #[test]
    fn graphemes_stay_sorted_however_they_arrive() {
        let mut graphemes = Graphemes::default();
        for col in [5, 1, 9, 3, 7] {
            graphemes.append(col, 'x');
        }
        graphemes.remove(9);
        graphemes.remove_range(&(4..6));
        graphemes.append(5, 'y');
        graphemes.remove_range(&(0..2));
        assert_eq!(graphemes.cols().collect::<Vec<_>>(), [3, 5, 7]);
        graphemes.append(1, 'x');
        assert_eq!(graphemes.cols().collect::<Vec<_>>(), [1, 3, 5, 7]);
        assert_eq!(graphemes.get(3), Some(&['x'][..]));
        assert_eq!(graphemes.get(4), None);
    }

    #[test]
    fn shifting_moves_and_drops_graphemes() {
        let mut graphemes = Graphemes::default();
        for col in [1, 3, 5] {
            graphemes.append(col, 'x');
        }
        graphemes.shift_right(2, 2, 7);
        assert_eq!(graphemes.cols().collect::<Vec<_>>(), [1, 5]);

        graphemes.shift_left(0, 2);
        assert_eq!(graphemes.cols().collect::<Vec<_>>(), [3]);
    }
}
