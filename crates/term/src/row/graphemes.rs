//! The codepoints that follow a cell's first: combining accents, variation
//! selectors, and the rest of joined emoji like 👩‍💻.

use std::ops::Range;

/// The most codepoints a cell may hold beyond its first.
///
/// Real graphemes stay far below this (long emoji ZWJ sequences have about
/// ten). The cap keeps a stream of combining characters from growing one
/// cell without limit.
pub(crate) const MAX_EXTRA: usize = 32;

/// Codepoints stored without a heap allocation. Enough for an accent or two,
/// or a two- or three-part ZWJ emoji.
const INLINE: usize = 5;

/// A row's graphemes, sorted by column. Text arrives left to right, so a new
/// grapheme almost always goes on the end.
#[derive(Clone, Debug, Default)]
pub(crate) struct Graphemes {
    entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
struct Entry {
    col: u16,
    extra: Extra,
}

/// The codepoints after a cell's first, inline while they fit.
#[derive(Clone, Debug)]
enum Extra {
    Inline { len: u8, chars: [char; INLINE] },
    Heap(Vec<char>),
}

impl Extra {
    fn new(ch: char) -> Self {
        let mut chars = ['\0'; INLINE];
        chars[0] = ch;
        Self::Inline { len: 1, chars }
    }

    fn as_slice(&self) -> &[char] {
        match self {
            Self::Inline { len, chars } => &chars[..usize::from(*len)],
            Self::Heap(chars) => chars,
        }
    }

    fn push(&mut self, ch: char) {
        match self {
            Self::Inline { len, chars } if usize::from(*len) < INLINE => {
                chars[usize::from(*len)] = ch;
                *len += 1;
            }
            Self::Inline { chars, .. } => {
                let mut heap = chars.to_vec();
                heap.push(ch);
                *self = Self::Heap(heap);
            }
            Self::Heap(chars) => chars.push(ch),
        }
    }
}

impl Graphemes {
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The extra codepoints of the cell at `col`, if it has any.
    pub(crate) fn get(&self, col: usize) -> Option<&[char]> {
        self.entry(col)
            .map(|entry| self.entries[entry].extra.as_slice())
    }

    /// Appends `ch` to the cell at `col`, unless it already holds
    /// [`MAX_EXTRA`] codepoints. Returns whether the cell had none before, so
    /// the caller can flag it.
    pub(crate) fn append(&mut self, col: usize, ch: char) -> bool {
        let at = match self.search(col) {
            Ok(entry) => {
                let extra = &mut self.entries[entry].extra;
                if extra.as_slice().len() < MAX_EXTRA {
                    extra.push(ch);
                }
                return false;
            }
            Err(at) => at,
        };
        let col = u16::try_from(col).expect("rows are at most Size::MAX wide");
        let entry = Entry {
            col,
            extra: Extra::new(ch),
        };
        self.entries.insert(at, entry);
        true
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
