//! Syntax highlights: what each piece of a text is, as its language's
//! grammar reads it, so it can be coloured.
//!
//! The daemon parses a buffer and works out its highlights; it and every
//! window showing the buffer keep them, and move them along with each edit
//! until the parse that follows the edit sends new ones for what changed.

use std::ops::Range;

use crate::edit::{Bias, Edit, Op};

/// What a piece of text is, as a grammar's highlight query names it. Each
/// is coloured as one; a query's names are many more, and each goes to the
/// kind its name starts with (see [`Kind::of_capture`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Kind {
    /// A comment.
    Comment,
    /// A keyword, such as `fn` or `if`.
    Keyword,
    /// A string or a character.
    String,
    /// A special string, such as a regular expression or a path.
    StringSpecial,
    /// An escape in a string, such as `\n`.
    Escape,
    /// A number.
    Number,
    /// A constant, such as `true` or `None`, or a name that is one.
    Constant,
    /// A function or method, where it's defined or called.
    Function,
    /// A macro.
    Macro,
    /// A type.
    Type,
    /// A constructor, such as an enum's variant.
    Constructor,
    /// A module or namespace.
    Module,
    /// A variable.
    Variable,
    /// A variable the language gives, such as `self`.
    VariableBuiltin,
    /// A function's parameter.
    Parameter,
    /// A field or property.
    Property,
    /// An attribute or annotation, such as Rust's `#[derive]`.
    Attribute,
    /// A label, or a lifetime.
    Label,
    /// An operator.
    Operator,
    /// Punctuation: brackets and delimiters.
    Punctuation,
    /// Punctuation that does something, such as `${` in a template string.
    PunctuationSpecial,
    /// A markup tag, as in HTML.
    Tag,
}

impl Kind {
    /// The kind a highlight query's capture name is, by the longest of
    /// the names below it starts with: `function.method.call` is a
    /// function. `None` for names that aren't coloured, such as ones
    /// starting `_`, which queries use for their own purposes.
    #[must_use]
    pub fn of_capture(name: &str) -> Option<Self> {
        // A dotted name before the name it starts with, so `string.escape`
        // isn't taken as `string`.
        const NAMES: &[(&str, Kind)] = &[
            ("punctuation.special", Kind::PunctuationSpecial),
            ("variable.parameter", Kind::Parameter),
            ("variable.builtin", Kind::VariableBuiltin),
            ("variable.member", Kind::Property),
            ("constant.builtin", Kind::Constant),
            ("function.macro", Kind::Macro),
            ("string.special", Kind::StringSpecial),
            ("string.escape", Kind::Escape),
            ("string.regexp", Kind::StringSpecial),
            ("string.regex", Kind::StringSpecial),
            ("type.builtin", Kind::Type),
            ("punctuation", Kind::Punctuation),
            ("constructor", Kind::Constructor),
            ("keyframes", Kind::Keyword),
            ("conditional", Kind::Keyword),
            ("exception", Kind::Keyword),
            ("character", Kind::String),
            ("attribute", Kind::Attribute),
            ("namespace", Kind::Module),
            ("parameter", Kind::Parameter),
            ("delimiter", Kind::Punctuation),
            ("property", Kind::Property),
            ("function", Kind::Function),
            ("variable", Kind::Variable),
            ("operator", Kind::Operator),
            ("supports", Kind::Keyword),
            ("constant", Kind::Constant),
            ("boolean", Kind::Constant),
            ("comment", Kind::Comment),
            ("keyword", Kind::Keyword),
            ("include", Kind::Keyword),
            ("charset", Kind::Keyword),
            ("preproc", Kind::Macro),
            ("method", Kind::Function),
            ("number", Kind::Number),
            ("string", Kind::String),
            ("escape", Kind::Escape),
            ("module", Kind::Module),
            ("import", Kind::Keyword),
            ("repeat", Kind::Keyword),
            ("float", Kind::Number),
            ("media", Kind::Keyword),
            ("label", Kind::Label),
            ("macro", Kind::Macro),
            ("field", Kind::Property),
            ("type", Kind::Type),
            ("tag", Kind::Tag),
        ];
        NAMES.iter().find_map(|&(prefix, kind)| {
            let rest = name.strip_prefix(prefix)?;
            (rest.is_empty() || rest.starts_with('.')).then_some(kind)
        })
    }
}

/// A piece of text of one [`Kind`]: bytes `start` to `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Span {
    /// Its first byte.
    pub start: usize,
    /// The byte after its last.
    pub end: usize,
    /// What it is.
    pub kind: Kind,
}

/// A text's highlights: [`Span`]s in order, none overlapping and none
/// empty. Text in none is coloured as plain text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Highlights {
    spans: Vec<Span>,
}

impl Highlights {
    /// No highlights.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every span, in order.
    #[must_use]
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// The spans with any of bytes `start` to `end` in them, in order.
    #[must_use]
    pub fn within(&self, start: usize, end: usize) -> &[Span] {
        let first = self.spans.partition_point(|span| span.end <= start);
        let after = first + self.spans[first..].partition_point(|span| span.start < end);
        &self.spans[first..after]
    }

    /// Forgets them all, as when the text is replaced.
    pub fn clear(&mut self) {
        self.spans.clear();
    }

    /// Makes bytes `start` to `end` highlighted as `spans` say, and
    /// nothing else there: what a parse found there. A span that was there
    /// and goes past either end keeps the part outside. Spans given are
    /// cut to fit, and must be in order.
    pub fn replace(&mut self, start: usize, end: usize, spans: impl IntoIterator<Item = Span>) {
        let first = self.spans.partition_point(|span| span.end <= start);
        let after = first + self.spans[first..].partition_point(|span| span.start < end);
        let mut kept = Vec::new();
        if let Some(&before) = self.spans[first..after].first()
            && before.start < start
        {
            kept.push(Span {
                end: start,
                ..before
            });
        }
        kept.extend(spans.into_iter().filter_map(|span| {
            let span = Span {
                start: span.start.max(start),
                end: span.end.min(end),
                ..span
            };
            (span.start < span.end).then_some(span)
        }));
        if let Some(&beyond) = self.spans[first..after].last()
            && beyond.end > end
        {
            kept.push(Span {
                start: end,
                ..beyond
            });
        }
        self.spans.splice(first..after, kept);
    }

    /// [`replace`](Self::replace)s with what a parse found for bytes
    /// `range` of the text before `edits` were made to it, moving that
    /// along with them first. Returns where it went, which
    /// [`within`](Self::within) gives the spans of.
    pub fn replace_since<'a>(
        &mut self,
        range: Range<usize>,
        spans: impl IntoIterator<Item = Span>,
        edits: impl IntoIterator<Item = &'a Edit>,
    ) -> Range<usize> {
        let (mut start, mut end) = (range.start, range.end);
        let mut found = Self::new();
        found.replace(start, end, spans);
        for edit in edits {
            found.map(edit);
            start = edit.map(start, Bias::After);
            end = edit.map(end, Bias::Before).max(start);
        }
        self.replace(start, end, found.spans);
        start..end
    }

    /// Moves them along with `edit`, made to the text they're for. Text
    /// put in at either end of a span isn't in it, and a span whose text
    /// is all deleted goes.
    pub fn map(&mut self, edit: &Edit) {
        // Spans that end before the first change don't move.
        let Some(Op::Retain(unchanged)) = edit.ops().first() else {
            return self.map_from(0, edit);
        };
        let first = self.spans.partition_point(|span| span.end <= *unchanged);
        self.map_from(first, edit);
    }

    fn map_from(&mut self, first: usize, edit: &Edit) {
        let mut places = Places::new(edit);
        let mut kept = first;
        for index in first..self.spans.len() {
            let span = self.spans[index];
            let start = places.map(span.start, Bias::After);
            let end = places.map(span.end, Bias::Before);
            if start < end {
                self.spans[kept] = Span { start, end, ..span };
                kept += 1;
            }
        }
        self.spans.truncate(kept);
    }
}

/// Places moved through an edit, as [`Edit::map`] moves them, but asked
/// for in order, so going through the edit once for them all.
struct Places<'a> {
    ops: &'a [Op],
    /// The piece the last place asked for was in...
    index: usize,
    /// ...and where it starts, in the text before the edit and after.
    old: usize,
    new: usize,
}

impl<'a> Places<'a> {
    fn new(edit: &'a Edit) -> Self {
        Self {
            ops: edit.ops(),
            index: 0,
            old: 0,
            new: 0,
        }
    }

    /// Where `place` goes. Places must be asked for in order, and never
    /// one with [`Bias::Before`] after the same place with
    /// [`Bias::After`].
    fn map(&mut self, place: usize, bias: Bias) -> usize {
        while let Some(op) = self.ops.get(self.index) {
            match op {
                Op::Retain(len) => {
                    if place < self.old + len {
                        return self.new + (place - self.old);
                    }
                    self.old += len;
                    self.new += len;
                }
                Op::Insert(text) => {
                    if place == self.old && bias == Bias::Before {
                        return self.new;
                    }
                    self.new += text.len();
                }
                Op::Delete(text) => {
                    if place < self.old + text.len() {
                        return self.new;
                    }
                    self.old += text.len();
                }
            }
            self.index += 1;
        }
        self.new + place.saturating_sub(self.old)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn span(start: usize, end: usize, kind: Kind) -> Span {
        Span { start, end, kind }
    }

    #[test]
    fn capture_names_go_to_the_kind_they_start_with() {
        assert_eq!(Kind::of_capture("keyword"), Some(Kind::Keyword));
        assert_eq!(
            Kind::of_capture("function.method.call"),
            Some(Kind::Function)
        );
        assert_eq!(Kind::of_capture("string.escape"), Some(Kind::Escape));
        assert_eq!(
            Kind::of_capture("string.special.path"),
            Some(Kind::StringSpecial)
        );
        assert_eq!(Kind::of_capture("type.builtin"), Some(Kind::Type));
        assert_eq!(Kind::of_capture("typewriter"), None);
        assert_eq!(Kind::of_capture("_name"), None);
        assert_eq!(Kind::of_capture("spell"), None);
    }

    #[test]
    fn replacing_keeps_the_parts_of_spans_outside() {
        let mut highlights = Highlights::new();
        highlights.replace(
            0,
            20,
            [span(0, 6, Kind::String), span(8, 20, Kind::Comment)],
        );
        highlights.replace(4, 10, [span(2, 5, Kind::Keyword)]);
        assert_eq!(
            highlights.spans(),
            [
                span(0, 4, Kind::String),
                span(4, 5, Kind::Keyword),
                span(10, 20, Kind::Comment),
            ]
        );
        assert_eq!(highlights.within(5, 10), []);
        assert_eq!(highlights.within(4, 11), &highlights.spans()[1..]);
    }

    #[test]
    fn spans_move_with_edits() {
        // "fn main" with "pub " put before and "ai" deleted.
        let mut highlights = Highlights::new();
        highlights.replace(
            0,
            7,
            [span(0, 2, Kind::Keyword), span(3, 7, Kind::Function)],
        );
        let mut edit = Edit::new();
        edit.insert("pub ").retain(4).delete("ai").retain(1);
        highlights.map(&edit);
        assert_eq!(
            highlights.spans(),
            [span(4, 6, Kind::Keyword), span(7, 9, Kind::Function)]
        );
        // Text typed at a span's end isn't in it; a span deleted goes.
        let mut edit = Edit::new();
        edit.retain(6).insert("x").delete(" mn");
        highlights.map(&edit);
        assert_eq!(highlights.spans(), [span(4, 6, Kind::Keyword)]);
    }

    #[test]
    fn highlights_found_before_edits_are_moved_past_them() {
        let mut highlights = Highlights::new();
        highlights.replace(0, 9, [span(0, 2, Kind::Keyword), span(3, 9, Kind::String)]);
        // A parse of "fn \"abc\"" found the string a comment; since then,
        // "x" was typed at the start.
        let mut typed = Edit::new();
        typed.insert("x").retain(9);
        highlights.map(&typed);
        let range = highlights.replace_since(3..9, [span(3, 9, Kind::Comment)], [&typed]);
        assert_eq!(range, 4..10);
        assert_eq!(highlights.within(4, 10), [span(4, 10, Kind::Comment)]);
        assert_eq!(highlights.spans()[0], span(1, 3, Kind::Keyword));
    }

    fn kind() -> impl Strategy<Value = Kind> {
        prop_oneof![Just(Kind::Keyword), Just(Kind::String), Just(Kind::Comment)]
    }

    /// A text, its highlights, and an edit to it.
    fn highlighted_edit() -> impl Strategy<Value = (String, Vec<Span>, Edit)> {
        "[a-c ]{0,30}".prop_flat_map(|text| {
            let len = text.len();
            let spans =
                prop::collection::vec((0..=len, 1..5usize, kind()), 0..8).prop_map(move |raw| {
                    let mut spans: Vec<Span> = Vec::new();
                    for (start, width, kind) in raw {
                        let start = start.max(spans.last().map_or(0, |span| span.end));
                        let end = (start + width).min(len);
                        if start < end {
                            spans.push(span(start, end, kind));
                        }
                    }
                    spans
                });
            let edit = prop::collection::vec((0..=len, 0..4usize, "[x\n]{0,3}"), 0..4).prop_map({
                let text = text.clone();
                move |changes| {
                    let mut edit = Edit::new();
                    let mut at = 0;
                    let mut places: Vec<_> = changes;
                    places.sort_by_key(|&(place, ..)| place);
                    for (place, removing, inserted) in places {
                        if place < at {
                            continue;
                        }
                        let end = (place + removing).min(len);
                        edit.retain(place - at)
                            .insert(&inserted)
                            .delete(&text[place..end]);
                        at = end;
                    }
                    edit.retain(len - at);
                    edit
                }
            });
            (Just(text), spans, edit)
        })
    }

    proptest! {
        #[test]
        fn moving_spans_together_moves_each_as_edits_map_places(
            (_text, spans, edit) in highlighted_edit()
        ) {
            let mut highlights = Highlights::new();
            highlights.replace(0, usize::MAX, spans.clone());
            highlights.map(&edit);
            let expected: Vec<Span> = spans
                .into_iter()
                .map(|span| Span {
                    start: edit.map(span.start, Bias::After),
                    end: edit.map(span.end, Bias::Before),
                    ..span
                })
                .filter(|span| span.start < span.end)
                .collect();
            prop_assert_eq!(highlights.spans(), &expected[..]);
        }
    }
}
