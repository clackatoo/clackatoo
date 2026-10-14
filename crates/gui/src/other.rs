//! A pane of a kind this window doesn't know: one a newer daemon has,
//! which it sends as [`Content::Other`](clackatoo_protocol::Content::Other),
//! naming the kind. The pane says so, and how to update; the rest of the
//! window goes on as usual.

use clackatoo_protocol::{Attributes, PaneKind, Size, Style};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;

/// A pane saying it needs a newer clackatoo.
#[derive(Debug)]
pub(crate) struct OtherPane {
    kind: PaneKind,
    title: String,
    width: usize,
    rows: Vec<Row>,
}

impl OtherPane {
    pub(crate) fn new(kind: PaneKind) -> Self {
        Self {
            kind,
            title: format!("{kind} (needs a newer clackatoo)"),
            width: 0,
            rows: Vec::new(),
        }
    }

    /// What kind of pane it is, as the daemon names it.
    pub(crate) fn kind(&self) -> PaneKind {
        self.kind
    }

    /// What the window's title says.
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    /// Lays out its words to `size`, if it changed.
    pub(crate) fn resize(&mut self, size: Size) {
        let width = usize::from(size.cols());
        if width == self.width && !self.rows.is_empty() {
            return;
        }
        self.width = width;
        let bold = Style {
            attributes: Attributes::BOLD,
            ..Style::DEFAULT
        };
        let mut first = RowBuilder::new(width);
        first.push(&said(self.kind), &bold, true);
        let mut second = RowBuilder::new(width);
        second.push(
            "The daemon is from a newer release of clackatoo than this window. `:update` \
             updates this window's.",
            &colors::line_number(false),
            true,
        );
        self.rows = vec![first.finish(false), second.finish(false)];
    }

    /// The rows to draw.
    pub(crate) fn scene(&self) -> Vec<&Row> {
        self.rows.iter().collect()
    }
}

/// What a pane of `kind` says.
pub(crate) fn said(kind: PaneKind) -> String {
    format!("This pane needs a newer clackatoo ({kind})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_names_the_kind_and_how_to_update() {
        let mut pane = OtherPane::new(PaneKind::new("notebook"));
        pane.resize(Size::new(120, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(
            rows[0].trim_end(),
            "This pane needs a newer clackatoo (notebook)"
        );
        assert!(rows[1].contains(":update"), "{rows:?}");
        assert_eq!(pane.title(), "notebook (needs a newer clackatoo)");
    }
}
