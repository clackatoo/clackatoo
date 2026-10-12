//! Diagnostics: what a language server says is wrong with a text, or worth
//! knowing about it, such as an error, a warning or a hint.
//!
//! The daemon gets them from a language server, as places in the text; it
//! and every window showing the text keep them, and move them along with
//! each edit, as highlights are moved, until the server sends new ones.

use crate::edit::{Bias, Edit};

/// How much a diagnostic matters, most first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Severity {
    /// Something wrong, which stops it building or working.
    Error,
    /// Something that looks wrong, but works.
    Warning,
    /// Something worth knowing.
    Info,
    /// A suggestion, such as a name that could be shorter.
    Hint,
}

/// One diagnostic: bytes `start` to `end` of the text, and what's said
/// about them. A diagnostic can be about no text at all, as where a
/// semicolon is missing, when `start` is `end`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Diagnostic {
    /// The first byte it's about.
    pub start: usize,
    /// The byte after the last.
    pub end: usize,
    /// How much it matters.
    pub severity: Severity,
    /// What's said, which can take several lines.
    pub message: String,
    /// Who says so, such as `rustc` or `clippy`.
    pub source: Option<String>,
    /// Its name, such as `E0308`.
    pub code: Option<String>,
}

impl Diagnostic {
    /// What it says, as a bar or a list says it: how much it matters, and
    /// the first line of what it says, with its name and who says so, as
    /// `error: mismatched types [E0308 rustc]`.
    #[must_use]
    pub fn described(&self) -> String {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
            Severity::Hint => "hint",
        };
        let message = self.message.lines().next().unwrap_or_default();
        let by: Vec<&str> = [&self.code, &self.source]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if by.is_empty() {
            format!("{severity}: {message}")
        } else {
            format!("{severity}: {message} [{}]", by.join(" "))
        }
    }
}

/// A text's diagnostics, in order of where they start. Unlike highlights,
/// they can overlap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    list: Vec<Diagnostic>,
}

impl Diagnostics {
    /// No diagnostics.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// These diagnostics, put in order.
    #[must_use]
    pub fn of(mut list: Vec<Diagnostic>) -> Self {
        list.sort_by_key(|diagnostic| diagnostic.start);
        Self { list }
    }

    /// Every one, in order of where they start.
    #[must_use]
    pub fn all(&self) -> &[Diagnostic] {
        &self.list
    }

    /// Those about any of bytes `start` to `end`, or about no text at a
    /// place in them, in order of where they start.
    pub fn within(&self, start: usize, end: usize) -> impl Iterator<Item = &Diagnostic> {
        let after = self
            .list
            .partition_point(|diagnostic| diagnostic.start <= end);
        self.list[..after]
            .iter()
            .filter(move |diagnostic| diagnostic.end > start || diagnostic.start >= start)
    }

    /// Moves them along with `edit`, made to the text they're for. Text
    /// put in at either end of one isn't in it, and one whose text is all
    /// deleted stays where the text was, about no text.
    pub fn map(&mut self, edit: &Edit) {
        for diagnostic in &mut self.list {
            diagnostic.start = edit.map(diagnostic.start, Bias::After);
            diagnostic.end = edit.map(diagnostic.end, Bias::Before).max(diagnostic.start);
        }
        // Moving keeps them in order but for ones that started together.
        self.list.sort_by_key(|diagnostic| diagnostic.start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic(start: usize, end: usize, message: &str) -> Diagnostic {
        Diagnostic {
            start,
            end,
            severity: Severity::Error,
            message: message.to_owned(),
            source: None,
            code: None,
        }
    }

    fn messages<'a>(found: impl Iterator<Item = &'a Diagnostic>) -> Vec<&'a str> {
        found
            .map(|diagnostic| diagnostic.message.as_str())
            .collect()
    }

    #[test]
    fn those_within_a_range_are_found() {
        let diagnostics = Diagnostics::of(vec![
            diagnostic(10, 20, "b"),
            diagnostic(0, 5, "a"),
            diagnostic(12, 12, "here"),
            diagnostic(30, 40, "c"),
        ]);
        assert_eq!(messages(diagnostics.within(0, 4)), ["a"]);
        assert_eq!(messages(diagnostics.within(5, 9)), Vec::<&str>::new());
        assert_eq!(messages(diagnostics.within(15, 35)), ["b", "c"]);
        assert_eq!(messages(diagnostics.within(12, 12)), ["b", "here"]);
    }

    #[test]
    fn they_move_with_edits() {
        let mut diagnostics =
            Diagnostics::of(vec![diagnostic(4, 7, "word"), diagnostic(10, 10, "point")]);
        // "abc def ghi" with "XY" put in at 4, and "ghi" deleted.
        let mut edit = Edit::new();
        edit.retain(4).insert("XY").retain(4).delete("gh").retain(1);
        diagnostics.map(&edit);
        let places: Vec<(usize, usize)> = (diagnostics.all().iter())
            .map(|diagnostic| (diagnostic.start, diagnostic.end))
            .collect();
        assert_eq!(places, [(6, 9), (10, 10)]);
        // All its text deleted: it stays, about none.
        let mut edit = Edit::new();
        edit.retain(6).delete("def").retain(2);
        diagnostics.map(&edit);
        assert_eq!(
            (diagnostics.all()[0].start, diagnostics.all()[0].end),
            (6, 6)
        );
    }
}
