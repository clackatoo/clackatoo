//! Highlighting: running a language's highlight query over part of a tree,
//! and flattening what it captures into [`Span`]s.

use std::ops::Range;

use clackatoo_text::{Kind, Rope, Span};
use tree_sitter::{Language, Node, Query, QueryCursor, StreamingIterator, TextProvider, Tree};

/// A language's grammar and highlight query, loaded.
#[derive(Debug)]
pub(crate) struct Grammar {
    pub(crate) language: Language,
    query: Query,
    /// The kind each of the query's captures is, by its index.
    kinds: Vec<Option<Kind>>,
}

impl Grammar {
    pub(crate) fn new(
        language: Language,
        highlights: &str,
    ) -> Result<Self, tree_sitter::QueryError> {
        let query = Query::new(&language, highlights)?;
        let kinds = (query.capture_names().iter())
            .map(|name| Kind::of_capture(name))
            .collect();
        Ok(Self {
            language,
            query,
            kinds,
        })
    }

    /// The highlights of bytes `range` of `text`, whose tree is `tree`.
    ///
    /// Where captures nest, as an escape in a string, the inner one is
    /// what's seen. Where one piece of text is captured more than once,
    /// the first pattern in the query to capture it wins, as in
    /// tree-sitter's own highlighter.
    pub(crate) fn highlight(
        &self,
        cursor: &mut QueryCursor,
        tree: &Tree,
        text: &Rope,
        range: Range<usize>,
    ) -> Vec<Span> {
        cursor.set_byte_range(range.clone());
        // Captures come in order of where they start, but at one place, in
        // the order of their patterns, not outer before inner.
        let mut found = Vec::new();
        let mut captures = cursor.captures(&self.query, tree.root_node(), RopeText(text));
        while let Some((matched, index)) = captures.next() {
            let capture = matched.captures()[*index];
            let node = capture.node.byte_range();
            // Captures not coloured, such as ones a pattern only checks,
            // don't hide others of the same text.
            if let Some(kind) = self.kinds[capture.index as usize]
                && node.start.max(range.start) < node.end.min(range.end)
            {
                found.push((node.start, node.end, kind));
            }
        }
        // Outer before inner, and for one piece of text, its first capture
        // first: the sort is stable. They're cut to the range only after,
        // as cutting can make an outer capture and an inner one the same
        // text, and the inner one must still win.
        found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
        found.dedup_by(|later, first| (later.0, later.1) == (first.0, first.1));
        let mut flat = Flattener::new(range.start);
        for (start, end, kind) in found {
            flat.open(start.max(range.start), end.min(range.end), kind);
        }
        flat.finish()
    }
}

/// Nested captures made into spans that don't overlap: each byte is the
/// kind of the innermost capture it's in.
struct Flattener {
    spans: Vec<Span>,
    /// The captures that the last one is in, and it: where each ends.
    open: Vec<(usize, Kind)>,
    /// How far the spans have got.
    done: usize,
}

impl Flattener {
    fn new(start: usize) -> Self {
        Self {
            spans: Vec::new(),
            open: Vec::new(),
            done: start,
        }
    }

    /// Adds a capture of bytes `start` to `end`. Captures come in order of
    /// where they start, and outer before inner.
    fn open(&mut self, start: usize, end: usize, kind: Kind) {
        self.close_before(start);
        if let Some(&(_, outer)) = self.open.last() {
            self.push(start, outer);
        }
        self.done = self.done.max(start);
        // An inner capture never goes past its outer one.
        let end = self.open.last().map_or(end, |&(outer, _)| end.min(outer));
        self.open.push((end, kind));
    }

    /// Closes the captures that end by `place`.
    fn close_before(&mut self, place: usize) {
        while let Some(&(end, kind)) = self.open.last() {
            if end > place {
                break;
            }
            self.push(end, kind);
            self.open.pop();
        }
    }

    /// Spans from where they've got to `end`, of `kind`.
    fn push(&mut self, end: usize, kind: Kind) {
        if end <= self.done {
            return;
        }
        match self.spans.last_mut() {
            Some(last) if last.end == self.done && last.kind == kind => last.end = end,
            _ => self.spans.push(Span {
                start: self.done,
                end,
                kind,
            }),
        }
        self.done = end;
    }

    fn finish(mut self) -> Vec<Span> {
        self.close_before(usize::MAX);
        self.spans
    }
}

/// A rope's text, as a query reads it to check its predicates, such as
/// `#eq?`.
struct RopeText<'a>(&'a Rope);

type Chunks<'a> = std::iter::Map<ropey::iter::Chunks<'a>, fn(&'a str) -> &'a [u8]>;

impl<'a> TextProvider<&'a [u8]> for RopeText<'a> {
    type I = Chunks<'a>;

    fn text(&mut self, node: Node<'_>) -> Self::I {
        let range = node.byte_range();
        let end = range.end.min(self.0.len_bytes());
        let start = range.start.min(end);
        (self.0.byte_slice(start..end).chunks()).map(str::as_bytes as fn(&'a str) -> &'a [u8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize, kind: Kind) -> Span {
        Span { start, end, kind }
    }

    #[test]
    fn nested_captures_flatten_to_the_innermost() {
        let mut flat = Flattener::new(0);
        // A string, 0..10, with an escape at 4..6, then a keyword after.
        flat.open(0, 10, Kind::String);
        flat.open(4, 6, Kind::Escape);
        flat.open(12, 14, Kind::Keyword);
        assert_eq!(
            flat.finish(),
            [
                span(0, 4, Kind::String),
                span(4, 6, Kind::Escape),
                span(6, 10, Kind::String),
                span(12, 14, Kind::Keyword),
            ]
        );
    }

    #[test]
    fn an_inner_capture_of_the_same_kind_joins_its_outer_one() {
        let mut flat = Flattener::new(0);
        flat.open(0, 10, Kind::String);
        flat.open(0, 3, Kind::String);
        flat.open(3, 12, Kind::Comment);
        assert_eq!(
            flat.finish(),
            [span(0, 3, Kind::String), span(3, 10, Kind::Comment)]
        );
    }
}
