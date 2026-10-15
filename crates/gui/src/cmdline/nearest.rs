//! The name nearest a word that names nothing, so an error can ask "did
//! you mean …?" of a slip, as `:Fils` for `:Files` or `:set numbr` for
//! `:set number`. The names themselves are for Tab to list, so an error
//! never does.

/// How many characters may differ, at most, for a name to be offered to a
/// word `chars` characters long: two, or for a word of one or two, one, as
/// two would offer `wq` for any two letters.
fn most_apart(chars: usize) -> usize {
    if chars <= 2 { 1 } else { 2 }
}

/// Of `names`, the one nearest `word`, as few characters apart as can be
/// and no more than two (see [`most_apart`]): the first listed of those
/// as near. Case is ignored, so `files` finds `Files`. `None` when none is
/// near enough, or `word` is empty.
pub(crate) fn nearest<'a>(word: &str, names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    if word.is_empty() {
        return None;
    }
    let word: Vec<char> = word.chars().flat_map(char::to_lowercase).collect();
    let most = most_apart(word.len());
    let mut best: Option<(usize, &str)> = None;
    let mut rows = Rows::default();
    for name in names {
        let apart = rows.apart(&word, name, most);
        if apart <= most && best.is_none_or(|(before, _)| apart < before) {
            best = Some((apart, name));
        }
    }
    best.map(|(_, name)| name)
}

/// Room for the rows [`Rows::apart`] works with, kept from one name to the
/// next.
#[derive(Default)]
struct Rows {
    name: Vec<char>,
    before_last: Vec<usize>,
    last: Vec<usize>,
    this: Vec<usize>,
}

impl Rows {
    /// How many characters `word`, already in lower case, and `name` are
    /// apart: the fewest put in, taken out, changed, or two side by side
    /// swapped, to make one the other (the optimal string alignment
    /// distance, Damerau–Levenshtein's simpler kin). Or, without working
    /// it out, `usize::MAX` when their lengths alone are more than `most`
    /// apart, so a long name costs nothing.
    fn apart(&mut self, word: &[char], name: &str, most: usize) -> usize {
        self.name.clear();
        self.name.extend(name.chars().flat_map(char::to_lowercase));
        let name = &self.name;
        if name.len().abs_diff(word.len()) > most {
            return usize::MAX;
        }
        let width = name.len() + 1;
        self.before_last.clear();
        self.before_last.resize(width, 0);
        self.last.clear();
        self.last.extend(0..width);
        for (i, &a) in word.iter().enumerate() {
            self.this.clear();
            self.this.push(i + 1);
            for (j, &b) in name.iter().enumerate() {
                let changed = usize::from(a != b);
                let mut cost = (self.last[j] + changed)
                    .min(self.last[j + 1] + 1)
                    .min(self.this[j] + 1);
                if i > 0 && j > 0 && a == name[j - 1] && word[i - 1] == b {
                    cost = cost.min(self.before_last[j - 1] + 1);
                }
                self.this.push(cost);
            }
            std::mem::swap(&mut self.before_last, &mut self.last);
            std::mem::swap(&mut self.last, &mut self.this);
        }
        self.last[name.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slip_finds_the_name_it_was_for_and_nonsense_finds_none() {
        let names = ["set", "Files", "Grep", "number", "nonumber", "wq"];
        assert_eq!(nearest("Fils", names), Some("Files"));
        assert_eq!(nearest("files", names), Some("Files"), "case is ignored");
        assert_eq!(nearest("nubmer", names), Some("number"), "a swap is one");
        assert_eq!(nearest("numbre", names), Some("number"));
        assert_eq!(nearest("st", names), Some("set"));
        assert_eq!(nearest("nonsense", names), None);
        assert_eq!(nearest("", names), None);
        // Two letters may differ by one only, or any two would find `wq`.
        assert_eq!(nearest("zz", names), None);
        assert_eq!(nearest("wz", names), Some("wq"));
    }

    #[test]
    fn the_nearest_wins_and_then_the_first_listed() {
        assert_eq!(nearest("grap", ["grab", "grep", "Grep"]), Some("grab"));
        assert_eq!(nearest("numbe", ["nonumber", "number"]), Some("number"));
    }

    #[test]
    fn a_name_too_long_or_short_by_itself_isnt_worked_out() {
        let mut rows = Rows::default();
        let word = ['a', 'b'];
        assert_eq!(rows.apart(&word, "abcd", 2), 2);
        assert_eq!(rows.apart(&word, "abcde", 2), usize::MAX);
        assert_eq!(rows.apart(&['a', 'b', 'c', 'd', 'e'], "ab", 2), usize::MAX);
        assert_eq!(nearest("wq", ["w".repeat(100_000).as_str()]), None);
    }

    #[test]
    fn characters_are_counted_not_bytes() {
        assert_eq!(nearest("café", ["cafe"]), Some("cafe"));
        assert_eq!(nearest("éé", ["e"]), None);
    }
}
