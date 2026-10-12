//! Fuzzy matching and ranking: which of a list of things what's typed
//! matches, best first, as fzf ranks them, with nucleo's matcher (Helix's).
//!
//! What's typed is fzf's syntax: words each match on their own, `'word`
//! exactly, `^word` at the start, `word$` at the end, and `!word` leaves
//! out what matches. Things are scored as they come, and only those that
//! matched before are scored again when the query only grows, as typing
//! does, so a big project's files stay quick to filter as they stream in.
//! Sorting waits until the list is drawn, once however many came.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::options::Case;

/// Above this many things to score, the work is shared between threads.
const SHARED_ABOVE: usize = 16_384;

/// Things to match, their text packed one after another.
#[derive(Debug, Default)]
pub(crate) struct Items {
    text: String,
    /// Where each ends in `text`.
    ends: Vec<u32>,
}

impl Items {
    pub(crate) fn push(&mut self, item: &str) {
        self.text.push_str(item);
        let end = u32::try_from(self.text.len()).expect("the finder lists far less than 4 GB");
        self.ends.push(end);
    }

    pub(crate) fn len(&self) -> usize {
        self.ends.len()
    }

    pub(crate) fn get(&self, index: usize) -> &str {
        let start = index
            .checked_sub(1)
            .map_or(0, |before| self.ends[before] as usize);
        &self.text[start..self.ends[index] as usize]
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.ends.clear();
    }
}

/// What a query matches of some [`Items`], and in what order.
#[derive(Debug)]
pub(crate) struct Ranking {
    matcher: Matcher,
    pattern: Pattern,
    query: String,
    case: CaseMatching,
    /// Whether shorter things come first among those scored the same, as
    /// for paths, or else the first to come.
    by_length: bool,
    /// The things matched, as keys that sort best first (see [`key`]).
    matched: Vec<u64>,
    /// How many of the things have been scored.
    scored: usize,
    sorted: bool,
    /// Room for a thing's characters, if it isn't ASCII.
    chars: Vec<char>,
}

impl Ranking {
    /// Matches nothing yet. Case is ignored as the options say.
    pub(crate) fn new(case: Case, by_length: bool) -> Self {
        // Case is matched exactly with nothing typed only without
        // `ignorecase`, and with a capital typed only with `smartcase`.
        let case = if case.exact("") {
            CaseMatching::Respect
        } else if case.exact("A") {
            CaseMatching::Smart
        } else {
            CaseMatching::Ignore
        };
        Self {
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            pattern: Pattern::default(),
            query: String::new(),
            case,
            by_length,
            matched: Vec::new(),
            scored: 0,
            sorted: true,
            chars: Vec::new(),
        }
    }

    /// Matches `query` instead.
    pub(crate) fn set_query(&mut self, query: &str, items: &Items) {
        if query == self.query {
            return;
        }
        // Words added to a query, or letters added to its last, only ever
        // match less, so what didn't match before still doesn't: unless a
        // word leaves out what matches, which matches more as it grows, or
        // a `$` or `\` typed before means something else once followed.
        let narrower = query.starts_with(&self.query) && !query.contains(['!', '$', '\\']);
        query.clone_into(&mut self.query);
        self.pattern.reparse(query, self.case, Normalization::Smart);
        if narrower {
            let before = std::mem::take(&mut self.matched);
            let indices: Vec<usize> = before.iter().map(|&key| index(key)).collect();
            self.matched = self.score(items, &indices);
        } else {
            self.matched.clear();
            self.scored = 0;
            self.add(items);
            return;
        }
        self.sorted = false;
    }

    /// Scores the things added to `items` since last asked.
    pub(crate) fn add(&mut self, items: &Items) {
        if self.scored == items.len() {
            return;
        }
        let new: Vec<usize> = (self.scored..items.len()).collect();
        let matched = self.score(items, &new);
        self.matched.extend(matched);
        self.scored = items.len();
        self.sorted = false;
    }

    /// Forgets every thing, for a new list.
    pub(crate) fn clear(&mut self) {
        self.matched.clear();
        self.scored = 0;
        self.sorted = true;
    }

    /// How many things match.
    pub(crate) fn len(&self) -> usize {
        self.matched.len()
    }

    /// The thing ranked `rank`, from 0 for the best, as its index in the
    /// items.
    pub(crate) fn get(&mut self, rank: usize) -> Option<usize> {
        self.sort();
        self.matched.get(rank).map(|&key| index(key))
    }

    /// Which of `item`'s characters the query matched, in order.
    pub(crate) fn matched_chars(&mut self, item: &str) -> Vec<u32> {
        let mut found = Vec::new();
        let haystack = utf32(item, &mut self.chars);
        self.pattern
            .indices(haystack, &mut self.matcher, &mut found);
        found.sort_unstable();
        found.dedup();
        found
    }

    fn sort(&mut self) {
        if !self.sorted {
            self.matched.sort_unstable();
            self.sorted = true;
        }
    }

    /// Scores the things at `indices` in `items`, returning the keys of
    /// those that match, on several threads if there are many.
    fn score(&mut self, items: &Items, indices: &[usize]) -> Vec<u64> {
        if self.pattern.atoms.is_empty() {
            return (indices.iter())
                .map(|&index| self.key(0, items.get(index), index))
                .collect();
        }
        if indices.len() <= SHARED_ABOVE {
            let (pattern, matcher, chars) = (&self.pattern, &mut self.matcher, &mut self.chars);
            return score_all(pattern, matcher, chars, items, indices, self.by_length);
        }
        let threads = std::thread::available_parallelism().map_or(1, usize::from);
        let share = indices.len().div_ceil(threads);
        let (pattern, by_length) = (&self.pattern, self.by_length);
        std::thread::scope(|scope| {
            let shares: Vec<_> = (indices.chunks(share))
                .map(|share| {
                    scope.spawn(move || {
                        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
                        let mut chars = Vec::new();
                        score_all(pattern, &mut matcher, &mut chars, items, share, by_length)
                    })
                })
                .collect();
            (shares.into_iter())
                .flat_map(|share| share.join().expect("scoring doesn't panic"))
                .collect()
        })
    }

    fn key(&self, score: u32, item: &str, index: usize) -> u64 {
        key(score, item, index, self.by_length)
    }
}

/// Scores the things at `indices`, returning the keys of those that match.
fn score_all(
    pattern: &Pattern,
    matcher: &mut Matcher,
    chars: &mut Vec<char>,
    items: &Items,
    indices: &[usize],
    by_length: bool,
) -> Vec<u64> {
    (indices.iter())
        .filter_map(|&index| {
            let item = items.get(index);
            let score = pattern.score(utf32(item, chars), matcher)?;
            Some(key(score, item, index, by_length))
        })
        .collect()
}

/// `item` as nucleo reads it: its bytes if it's ASCII, or else its
/// characters, put in `chars`, so matched places are characters either way.
fn utf32<'a>(item: &'a str, chars: &'a mut Vec<char>) -> Utf32Str<'a> {
    if item.is_ascii() {
        return Utf32Str::Ascii(item.as_bytes());
    }
    chars.clear();
    chars.extend(item.chars());
    Utf32Str::Unicode(chars)
}

/// A thing's sort key: the best score first, then, by length, the shortest,
/// then the first to come. Its index is in the low 32 bits.
fn key(score: u32, item: &str, index: usize, by_length: bool) -> u64 {
    const SCORE_MAX: u32 = (1 << 24) - 1;
    let worse = u64::from(SCORE_MAX - score.min(SCORE_MAX));
    let length = if by_length {
        u64::try_from(item.len().min(255)).unwrap_or(255)
    } else {
        0
    };
    let index = u64::try_from(index).expect("indices fit in 32 bits");
    (worse << 40) | (length << 32) | index
}

/// The index of the thing `key` is for.
fn index(key: u64) -> usize {
    usize::try_from(key & u64::from(u32::MAX)).expect("indices fit in 32 bits")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(list: &[&str]) -> Items {
        let mut items = Items::default();
        for item in list {
            items.push(item);
        }
        items
    }

    fn ranked(ranking: &mut Ranking, items: &Items) -> Vec<String> {
        (0..ranking.len())
            .map(|rank| items.get(ranking.get(rank).unwrap()).to_owned())
            .collect()
    }

    #[test]
    fn the_best_match_comes_first_then_the_shortest() {
        let list = items(&[
            "crates/gui/src/finder/rank.rs",
            "crates/daemon/src/find.rs",
            "README",
            "crates/gui/src/desk.rs",
            "fonts/fira.ttf",
        ]);
        let mut ranking = Ranking::new(Case::default(), true);
        ranking.add(&list);
        // Nothing typed: everything, shortest first.
        assert_eq!(
            ranked(&mut ranking, &list)[..2],
            ["README", "fonts/fira.ttf"]
        );
        ranking.set_query("find", &list);
        assert_eq!(
            ranked(&mut ranking, &list),
            ["crates/daemon/src/find.rs", "crates/gui/src/finder/rank.rs"]
        );
        // fzf's syntax: words each match, and `!` leaves out.
        ranking.set_query("gui !desk", &list);
        assert_eq!(
            ranked(&mut ranking, &list),
            ["crates/gui/src/finder/rank.rs"]
        );
        ranking.set_query("rs$", &list);
        assert_eq!(ranking.len(), 3);
        assert_eq!(ranking.matched_chars("crates/gui/src/desk.rs"), [20, 21]);
    }

    #[test]
    fn case_is_matched_as_the_options_say() {
        let list = items(&["Cargo.toml", "cargo.lock"]);
        let mut ranking = Ranking::new(Case::default(), true);
        ranking.add(&list);
        ranking.set_query("cargo", &list);
        assert_eq!(ranking.len(), 2);
        // A capital, with `smartcase`, matches case.
        ranking.set_query("Cargo", &list);
        assert_eq!(ranked(&mut ranking, &list), ["Cargo.toml"]);
        let options = crate::options::Options {
            ignorecase: false,
            ..Default::default()
        };
        let mut exact = Ranking::new(Case::of(&options), true);
        exact.add(&list);
        exact.set_query("cargo", &list);
        assert_eq!(ranked(&mut exact, &list), ["cargo.lock"]);
    }

    #[test]
    fn things_coming_later_are_matched_and_a_narrower_query_keeps_up() {
        let mut list = items(&["src/main.rs"]);
        let mut ranking = Ranking::new(Case::default(), false);
        ranking.add(&list);
        ranking.set_query("ma", &list);
        list.push("docs/manual.md");
        list.push("Makefile");
        list.push("lib.rs");
        ranking.add(&list);
        // Not by length: the first to come, among those scored the same.
        assert_eq!(ranking.len(), 3);
        ranking.set_query("manu", &list);
        assert_eq!(ranked(&mut ranking, &list), ["docs/manual.md"]);
        // Wider again: everything is looked at.
        ranking.set_query("m", &list);
        assert_eq!(ranking.len(), 3);
        // And so for a word that leaves out, which grows wider.
        ranking.set_query("m !ma", &list);
        assert_eq!(ranking.len(), 0);
        ranking.set_query("m !mak", &list);
        assert_eq!(
            ranked(&mut ranking, &list),
            ["src/main.rs", "docs/manual.md"]
        );
    }

    #[test]
    fn many_things_are_scored_on_several_threads_the_same() {
        let mut list = Items::default();
        for index in 0..(SHARED_ABOVE * 3) {
            list.push(&format!("dir{}/file{index}.rs", index % 97));
        }
        let mut ranking = Ranking::new(Case::default(), true);
        ranking.add(&list);
        ranking.set_query("dir5/file1", &list);
        let shared = ranked(&mut ranking, &list);
        let mut alone = Ranking::new(Case::default(), true);
        let indices: Vec<usize> = (0..list.len()).collect();
        alone.pattern = ranking.pattern.clone();
        let mut keys = score_all(
            &alone.pattern,
            &mut alone.matcher,
            &mut alone.chars,
            &list,
            &indices,
            true,
        );
        keys.sort_unstable();
        let keys: Vec<String> = (keys.iter())
            .map(|&key| list.get(index(key)).to_owned())
            .collect();
        assert_eq!(shared, keys);
        assert_eq!(shared[0], "dir5/file102.rs");
    }

    #[test]
    fn matched_characters_are_counted_in_characters() {
        let mut ranking = Ranking::new(Case::default(), true);
        let list = items(&["é/ab"]);
        ranking.set_query("ab", &list);
        assert_eq!(ranking.matched_chars("é/ab"), [2, 3]);
    }
}
