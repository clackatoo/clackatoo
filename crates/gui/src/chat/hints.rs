//! What a chat draws, as hints mode reads it (see [`crate::hints`]): the
//! text of each block shown, as far as it's in view, as its search reads
//! it, and where its bytes are drawn.

use std::ops::Range;

use parley::{Affinity, Cursor, Selection};

use super::ChatView;
use super::paint::control;
use crate::hints::Drawn;

/// The text a chat draws in view: each block's, by its place among those
/// shown, and the first byte of it in view.
pub(crate) struct InView<'a> {
    view: &'a ChatView,
    runs: Vec<(usize, usize, &'a str)>,
}

impl ChatView {
    /// What it draws in view, as it was last laid out.
    pub(crate) fn in_view(&self) -> InView<'_> {
        let mut runs = Vec::new();
        for (index, shown) in self.shown.iter().enumerate() {
            if control(shown.key.piece) {
                continue;
            }
            let Some(text) = self.text(shown.key) else {
                continue;
            };
            let Some(bytes) = self.bytes_in_view(shown, text.layout) else {
                continue;
            };
            if let Some(seen) = text.text.get(bytes.clone()) {
                runs.push((index, bytes.start, seen));
            }
        }
        InView { view: self, runs }
    }
}

impl Drawn for InView<'_> {
    fn texts(&self) -> Vec<&str> {
        self.runs.iter().map(|&(_, _, text)| text).collect()
    }

    #[allow(clippy::cast_possible_truncation, reason = "pixels")]
    fn rects(&self, index: usize, range: Range<usize>) -> Vec<[f32; 4]> {
        let Some(&(shown, start, _)) = self.runs.get(index) else {
            return Vec::new();
        };
        let shown = &self.view.shown[shown];
        let Some(text) = self.view.text(shown.key) else {
            return Vec::new();
        };
        let picked = Selection::new(
            Cursor::from_byte_index(text.layout, start + range.start, Affinity::Downstream),
            Cursor::from_byte_index(text.layout, start + range.end, Affinity::Upstream),
        );
        (picked.geometry(text.layout).into_iter())
            .map(|(bounds, _)| {
                [
                    shown.at[0] + bounds.x0 as f32,
                    shown.at[1] + bounds.y0 as f32,
                    (bounds.x1 - bounds.x0) as f32,
                    (bounds.y1 - bounds.y0) as f32,
                ]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::fixtures::{fonts, paint, talk};
    use crate::hints::{Kind, hints};

    #[test]
    fn a_chats_text_in_view_is_read_for_hints_where_its_drawn() {
        let mut fonts = fonts();
        let node = talk(&[(
            "where is it?",
            "see https://x.example/c and 7e84c2e9, in src/lib.rs:4",
        )]);
        let mut chat = ChatView::new(String::new(), node);
        paint(&mut chat, &mut fonts);
        let found = hints(&chat.in_view(), None);
        let kinds: Vec<(&str, Kind)> = (found.iter())
            .map(|hint| (hint.text.as_str(), hint.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("src/lib.rs:4", Kind::Path),
                ("7e84c2e9", Kind::Hash),
                ("https://x.example/c", Kind::Url),
            ]
        );
        // Each where the chat draws it: left to right along its line.
        let lefts: Vec<f32> = found.iter().map(|hint| hint.rects[0][0]).collect();
        assert!(lefts[0] > lefts[1] && lefts[1] > lefts[2], "{lefts:?}");
        assert!(found.iter().all(|hint| hint.rects[0][2] > 0.0));
    }
}
