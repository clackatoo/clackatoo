//! One buffer's text and its syntax tree, kept up to date edit by edit, so
//! each parse after the first only reads again what an edit changed.

use std::ops::Range;

use clackatoo_text::{Edit, Rope};
use tree_sitter::{InputEdit, Parser, Point, Tree};

/// A text, and its tree as of the last parse.
#[derive(Debug)]
pub(crate) struct Document {
    text: Rope,
    /// The tree, edited to fit the text: `None` before the first parse.
    tree: Option<Tree>,
    /// Where the text has changed since the last parse, in the text now.
    touched: Option<Range<usize>>,
}

impl Document {
    pub(crate) fn new(text: Rope) -> Self {
        let len = text.len_bytes();
        Self {
            text,
            tree: None,
            touched: Some(0..len),
        }
    }

    pub(crate) fn text(&self) -> &Rope {
        &self.text
    }

    pub(crate) fn tree(&self) -> Option<&Tree> {
        self.tree.as_ref()
    }

    /// Takes `edit`, which made `text` from the text it had.
    pub(crate) fn edit(&mut self, edit: &Edit, text: Rope) {
        if let Some(tree) = &mut self.tree {
            for input in input_edits(&self.text, edit) {
                tree.edit(&input);
            }
        }
        let touched = self.touched.take().map(|range| {
            edit.map(range.start, clackatoo_text::Bias::Before)
                ..edit.map(range.end, clackatoo_text::Bias::After)
        });
        let changed = changed_range(edit);
        self.touched = match (touched, changed) {
            (Some(a), Some(b)) => Some(a.start.min(b.start)..a.end.max(b.end)),
            (a, b) => a.or(b),
        };
        self.text = text;
    }

    /// Parses the text again, if it's changed, and says what's to be
    /// highlighted again: where the text changed or the tree did. `None` if
    /// nothing has.
    pub(crate) fn parse(&mut self, parser: &mut Parser) -> Option<Range<usize>> {
        let touched = self.touched.take()?;
        let len = self.text.len_bytes();
        let text = &self.text;
        let mut read = |byte: usize, _: Point| -> &[u8] {
            if byte >= len {
                return &[];
            }
            let (chunk, start, _, _) = text.chunk_at_byte(byte);
            &chunk.as_bytes()[byte - start..]
        };
        let Some(tree) = parser.parse_with_options(&mut read, self.tree.as_ref(), None) else {
            self.touched = Some(touched);
            return None;
        };
        let mut range = touched;
        if let Some(old) = &self.tree {
            for changed in old.changed_ranges(&tree) {
                range.start = range.start.min(changed.start_byte);
                range.end = range.end.max(changed.end_byte);
            }
        }
        self.tree = Some(tree);
        Some(range)
    }
}

/// Where `edit` changes the text, in the text it makes: from the first
/// change to the end of the last. `None` if it changes nothing.
fn changed_range(edit: &Edit) -> Option<Range<usize>> {
    let mut grown: isize = 0;
    let mut range: Option<Range<usize>> = None;
    for (at, removed, inserted) in edit.changes() {
        let start = at.saturating_add_signed(grown);
        let end = start + inserted.len();
        range = Some(range.map_or(start..end, |range| range.start..end));
        grown += isize::try_from(inserted.len()).unwrap_or(0)
            - isize::try_from(removed.len()).unwrap_or(0);
    }
    range
}

/// `edit`, as tree-sitter is told of it: one change at a time, from the
/// last to the first, so each is where it was in `before`.
fn input_edits(before: &Rope, edit: &Edit) -> Vec<InputEdit> {
    let changes: Vec<_> = edit.changes().collect();
    changes
        .into_iter()
        .rev()
        .map(|(at, removed, inserted)| {
            let start_position = point(before, at);
            InputEdit {
                start_byte: at,
                old_end_byte: at + removed.len(),
                new_end_byte: at + inserted.len(),
                start_position,
                old_end_position: point(before, at + removed.len()),
                new_end_position: after(start_position, inserted),
            }
        })
        .collect()
}

/// Where byte `byte` of `text` is: its line, and its byte in the line.
fn point(text: &Rope, byte: usize) -> Point {
    let row = text.byte_to_line(byte);
    Point::new(row, byte - text.line_to_byte(row))
}

/// Where the end of `text` is, put in at `start`.
fn after(start: Point, text: &str) -> Point {
    match text.rfind('\n') {
        Some(last) => Point::new(
            start.row + text.matches('\n').count(),
            text.len() - last - 1,
        ),
        None => Point::new(start.row, start.column + text.len()),
    }
}
