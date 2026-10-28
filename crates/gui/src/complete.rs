//! What every completion menu has in common: a word being typed in a
//! text, and a menu of what it could be, by it, that narrows as more is
//! typed. The editor's insert-mode completion (see
//! [`crate::editor::complete`]) and a chat's `/` menu (see
//! [`crate::chat::slash`]) are both made of it.
//!
//! [`Ranking`] matches what's typed against each item fuzzily, as fzf
//! does, preferring what starts with it, and keeps the order it was given
//! for what matches as well. [`Choice`] is which of those shown is picked,
//! whether a key moved to it, and the first in view: picking goes round
//! the ends, and the view scrolls as little as shows what's picked.
//!
//! Unlike a picker's list (see [`crate::picker`]), what's typed isn't the
//! menu's: it's the text's, and the menu only reads it. So the keys are
//! each pane's to give: Up and Down, or Ctrl+N and Ctrl+P, to
//! [`Choice::step`]; and Tab, Enter and Esc to what the pane does with
//! the item picked.

use std::ops::Range;

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32String};

/// Completion items ranked against what's typed: those that match it,
/// best first, by how well they match, then as the language server sorts
/// them. Public for its benchmark.
#[derive(Debug)]
pub struct Ranking {
    matcher: Matcher,
    /// Each item's text to match, as the matcher reads it.
    haystacks: Vec<Utf32String>,
    /// Each item's place in the server's order: those it would have picked
    /// first, then by their sort text.
    order: Vec<u32>,
    /// What was typed when last ranked.
    query: String,
    /// The items that match it, best first, by index.
    shown: Vec<u32>,
}

impl Ranking {
    /// Ranks items, each given as the text to match it by, the text to sort
    /// it by, and whether the server would have it picked first; all of
    /// them shown, in the server's order, until [`narrow`](Self::narrow)
    /// says what's typed.
    pub fn new<'a>(items: impl IntoIterator<Item = (&'a str, &'a str, bool)>) -> Self {
        let mut haystacks = Vec::new();
        let mut keys: Vec<(bool, &str)> = Vec::new();
        for (filter, sort, preselect) in items {
            haystacks.push(Utf32String::from(filter));
            keys.push((!preselect, sort));
        }
        let mut by_order: Vec<u32> = (0..index(keys.len())).collect();
        by_order.sort_by(|&a, &b| keys[a as usize].cmp(&keys[b as usize]));
        let mut order = vec![0; keys.len()];
        for (place, &item) in (0..).zip(&by_order) {
            order[item as usize] = place;
        }
        let mut config = Config::DEFAULT;
        // What's typed is mostly the start of what's wanted.
        config.prefer_prefix = true;
        Self {
            matcher: Matcher::new(config),
            haystacks,
            order,
            query: String::new(),
            shown: by_order,
        }
    }

    /// Ranks the items against `query`, what's typed. When it's what was
    /// typed before with more after, only those that matched that are
    /// looked at, as nothing else can match it.
    pub fn narrow(&mut self, query: &str) {
        if query == self.query {
            return;
        }
        let looked_at: Vec<u32> = if !self.query.is_empty() && query.starts_with(&self.query) {
            std::mem::take(&mut self.shown)
        } else {
            (0..index(self.haystacks.len())).collect()
        };
        query.clone_into(&mut self.query);
        if query.is_empty() {
            let mut all = looked_at;
            all.sort_by_key(|&index| self.order[index as usize]);
            self.shown = all;
            return;
        }
        let atom = atom(query);
        let typed: Vec<char> = query.chars().collect();
        let mut scored: Vec<(bool, u16, u32, u32)> = Vec::with_capacity(looked_at.len());
        for index in looked_at {
            let haystack = self.haystacks[index as usize].slice(..);
            if let Some(score) = atom.score(haystack, &mut self.matcher) {
                // What starts with exactly what's typed, case and all,
                // comes before what only matches it.
                let prefix = haystack.len() >= typed.len()
                    && haystack.chars().zip(&typed).all(|(ch, typed)| ch == *typed);
                scored.push((prefix, score, self.order[index as usize], index));
            }
        }
        // Best first, then as the server sorts them.
        scored.sort_unstable_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)).then(a.2.cmp(&b.2)));
        self.shown = scored.into_iter().map(|(.., index)| index).collect();
    }

    /// Shows the items in the order they were given, rather than as their
    /// sort texts sort them, until something's typed.
    pub(crate) fn keep_given_order(&mut self) {
        self.shown.sort_unstable();
    }

    /// The items matching what's typed, best first, by index.
    #[must_use]
    pub fn shown(&self) -> &[u32] {
        &self.shown
    }

    /// The characters of `text` that match what's typed, by index, as
    /// they're highlighted in the menu.
    pub(crate) fn matched(&mut self, text: &str) -> Vec<u32> {
        let mut indices = Vec::new();
        if self.query.is_empty() {
            return indices;
        }
        let mut chars = Vec::new();
        let haystack = nucleo_matcher::Utf32Str::new(text, &mut chars);
        atom(&self.query).indices(haystack, &mut self.matcher, &mut indices);
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

/// `count`, as an item's index: there are never more than fit.
fn index(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `query` as the matcher reads it: fuzzily, ignoring case unless it has
/// a capital.
fn atom(query: &str) -> Atom {
    Atom::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Fuzzy,
        false,
    )
}

/// Which of the items a menu shows is picked, as a place in those shown,
/// whether it was moved to with a key, and the first in view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Choice {
    selected: usize,
    moved: bool,
    top: usize,
}

impl Choice {
    /// The place picked, in those shown.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Whether what's picked was moved to with a key, rather than being
    /// the first.
    pub(crate) fn moved(&self) -> bool {
        self.moved
    }

    /// The first in view.
    pub(crate) fn top(&self) -> usize {
        self.top
    }

    /// Picks the first, as a menu whose items changed does.
    pub(crate) fn restart(&mut self) {
        *self = Self::default();
    }

    /// Moves the pick to the next of `count` shown, or the one before if
    /// not `down`, going round the ends, in a menu `height` tall.
    pub(crate) fn step(&mut self, down: bool, count: usize, height: usize) {
        if count == 0 {
            return;
        }
        self.selected = if down {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.moved = true;
        self.reveal(height);
    }

    /// Picks the place `place`, as if moved to, in a menu `height` tall.
    pub(crate) fn go_to(&mut self, place: usize, height: usize) {
        let height = height.max(1);
        self.selected = place;
        self.moved = true;
        if self.selected < self.top || self.selected >= self.top + height {
            self.top = self.selected.saturating_sub(height - 1);
        }
    }

    /// The first in view moves as little as shows what's picked.
    fn reveal(&mut self, height: usize) {
        let height = height.max(1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + height {
            self.top = self.selected + 1 - height;
        }
    }

    /// Those in view of `count` shown, in a menu `height` tall: from the
    /// first in view, kept as full as there are items for, and showing
    /// what's picked.
    pub(crate) fn in_view(&mut self, count: usize, height: usize) -> Range<usize> {
        self.selected = self.selected.min(count.saturating_sub(1));
        self.top = self.top.min(count.saturating_sub(height));
        self.reveal(height);
        self.top..(self.top + height).min(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_goes_round_the_ends_and_the_view_follows() {
        let mut choice = Choice::default();
        assert_eq!(choice.in_view(5, 3), 0..3);
        choice.step(false, 5, 3);
        assert_eq!((choice.selected(), choice.moved()), (4, true));
        assert_eq!(choice.in_view(5, 3), 2..5);
        choice.step(true, 5, 3);
        assert_eq!(choice.selected(), 0);
        assert_eq!(choice.in_view(5, 3), 0..3);
        // Fewer shown keeps the pick among them.
        choice.go_to(4, 3);
        assert_eq!(choice.in_view(2, 3), 0..2);
        assert_eq!(choice.selected(), 1);
        choice.restart();
        assert!(!choice.moved());
    }

    #[test]
    fn what_starts_with_what_is_typed_comes_first_then_the_given_order() {
        let mut ranking = Ranking::new(
            ["review", "compact", "clear", "context"]
                .into_iter()
                .zip(["0", "1", "2", "3"])
                .map(|(name, sort)| (name, sort, false)),
        );
        assert_eq!(ranking.shown(), [0, 1, 2, 3]);
        ranking.narrow("co");
        assert_eq!(ranking.shown()[..2], [1, 3]);
    }
}
