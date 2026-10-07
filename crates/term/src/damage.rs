//! Which screen lines changed, so a viewer can redraw only those.

use std::ops::Range;

/// A set of screen lines that changed since it was last taken.
///
/// Changes are tracked by position on screen, not by row: when the screen
/// scrolls, every line it moved counts as changed, because each now shows
/// different content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Damage {
    /// One bit per line, from line 0 in the lowest bit of the first word.
    lines: Vec<u64>,
    /// Every line changed, however many there are.
    all: bool,
}

impl Damage {
    /// No lines changed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether no line changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.all && self.lines.iter().all(|&word| word == 0)
    }

    /// Whether every line changed, as after a resize or a switch between
    /// the primary and alternate screens.
    #[must_use]
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// The changed lines of a screen `height` lines tall, top first.
    pub fn lines(&self, height: usize) -> impl Iterator<Item = usize> + '_ {
        (0..height).filter(move |&line| self.all || self.contains(line))
    }

    /// Adds `other`'s lines to these.
    pub fn merge(&mut self, other: &Self) {
        self.all |= other.all;
        if self.all {
            return;
        }
        if self.lines.len() < other.lines.len() {
            self.lines.resize(other.lines.len(), 0);
        }
        for (word, &other) in self.lines.iter_mut().zip(&other.lines) {
            *word |= other;
        }
    }

    /// Forgets every change, keeping the memory.
    pub fn clear(&mut self) {
        self.lines.fill(0);
        self.all = false;
    }

    #[inline]
    pub(crate) fn mark(&mut self, line: usize) {
        if self.all {
            return;
        }
        let (word, bit) = (line / 64, line % 64);
        if word >= self.lines.len() {
            self.lines.resize(word + 1, 0);
        }
        self.lines[word] |= 1 << bit;
    }

    /// Marks `lines` a word of 64 at a time: scrolling a region marks every
    /// line in it, and does so often.
    pub(crate) fn mark_range(&mut self, lines: Range<usize>) {
        if self.all || lines.is_empty() {
            return;
        }
        let (first, last) = (lines.start / 64, (lines.end - 1) / 64);
        if last >= self.lines.len() {
            self.lines.resize(last + 1, 0);
        }
        for word in first..=last {
            let low = if word == first { lines.start % 64 } else { 0 };
            let high = if word == last {
                (lines.end - 1) % 64
            } else {
                63
            };
            self.lines[word] |= (u64::MAX >> (63 - high)) & (u64::MAX << low);
        }
    }

    pub(crate) fn mark_all(&mut self) {
        self.all = true;
    }

    fn contains(&self, line: usize) -> bool {
        self.lines
            .get(line / 64)
            .is_some_and(|word| word & (1 << (line % 64)) != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_merge_and_clear() {
        let mut damage = Damage::new();
        assert!(damage.is_empty());
        damage.mark(1);
        damage.mark(70);
        let mut merged = Damage::new();
        merged.mark(3);
        merged.merge(&damage);
        assert_eq!(merged.lines(100).collect::<Vec<_>>(), [1, 3, 70]);
        // Lines past the screen's height are left out.
        assert_eq!(merged.lines(50).collect::<Vec<_>>(), [1, 3]);
        merged.clear();
        assert!(merged.is_empty());
    }

    #[test]
    fn ranges_mark_exactly_their_lines() {
        for (start, end) in [(0, 1), (3, 64), (63, 65), (10, 200), (64, 128)] {
            let mut damage = Damage::new();
            damage.mark_range(start..end);
            assert_eq!(
                damage.lines(256).collect::<Vec<_>>(),
                (start..end).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn all_covers_every_line() {
        let mut damage = Damage::new();
        damage.mark_all();
        assert!(!damage.is_empty());
        assert_eq!(damage.lines(3).collect::<Vec<_>>(), [0, 1, 2]);
        let mut other = Damage::new();
        other.merge(&damage);
        assert!(other.is_all());
    }
}
