//! A pane of a kind this window can't show: one a newer daemon has, which
//! it sends as [`Content::Other`](clackatoo_protocol::Content::Other),
//! naming the kind, or one on another machine, in a workspace `:connect`
//! brought in, of a kind the daemon doesn't relay yet, such as an editor.
//! The pane says so, and for the first, how to update; the rest of the
//! window goes on as usual.

use clackatoo_protocol::{Attributes, PaneKind, Size, Style};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;

/// A pane saying it needs a newer clackatoo, or that it's on another
/// machine, and not shown here yet.
#[derive(Debug)]
pub(crate) struct OtherPane {
    kind: PaneKind,
    /// The machine it's on, for one from another machine.
    host: Option<String>,
    title: String,
    width: usize,
    rows: Vec<Row>,
}

impl OtherPane {
    /// A pane of `kind`, newer than this window, or on `host`, another
    /// machine, if it's from one.
    pub(crate) fn new(kind: PaneKind, host: Option<String>) -> Self {
        let title = match &host {
            Some(host) => said_elsewhere(kind, host),
            None => format!("{kind} (needs a newer clackatoo)"),
        };
        Self {
            kind,
            host,
            title,
            width: 0,
            rows: Vec::new(),
        }
    }

    /// What kind of pane it is, as the daemon names it.
    pub(crate) fn kind(&self) -> PaneKind {
        self.kind
    }

    /// The machine it's on, if it's from another.
    pub(crate) fn host(&self) -> Option<&str> {
        self.host.as_deref()
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
        let (first_words, second_words) = match &self.host {
            Some(host) => (
                said_elsewhere(self.kind, host),
                "Only terminals and chats come through from another machine yet.".to_owned(),
            ),
            None => (
                said(self.kind),
                "The daemon is from a newer release of clackatoo than this window. `:update` \
                 updates this window's."
                    .to_owned(),
            ),
        };
        let quiet = colors::line_number(false);
        self.rows = (wrapped(&first_words, width)
            .iter()
            .map(|line| (line, &bold)))
        .chain(
            wrapped(&second_words, width)
                .iter()
                .map(|line| (line, &quiet)),
        )
        .map(|(line, style)| {
            let mut row = RowBuilder::new(width);
            row.push(line, style, true);
            row.finish(false)
        })
        .collect();
    }

    /// The rows to draw.
    pub(crate) fn scene(&self) -> Vec<&Row> {
        self.rows.iter().collect()
    }
}

/// `text` in lines of at most `width` cells, broken between words: a word
/// longer than that alone on its line, to be cut short.
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split(' ') {
        match lines.last_mut() {
            Some(line)
                if crate::render::mono_width(line) + 1 + crate::render::mono_width(word)
                    <= width =>
            {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// What a pane of `kind` says.
pub(crate) fn said(kind: PaneKind) -> String {
    format!("This pane needs a newer clackatoo ({kind})")
}

/// What a pane of `kind` on `host`, another machine, says: "an editor on
/// desk: coming soon".
pub(crate) fn said_elsewhere(kind: PaneKind, host: &str) -> String {
    let what = match kind.as_str() {
        "editor" => "an editor",
        "explorer" => "an explorer",
        "board" => "a board",
        "review" => "a review",
        "browser" => "a chat's browser",
        "terminal" => "a terminal from yet another machine",
        _ => "a pane",
    };
    format!("{what} on {host}: coming soon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_names_the_kind_and_how_to_update() {
        let mut pane = OtherPane::new(PaneKind::new("notebook"), None);
        pane.resize(Size::new(120, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(
            rows[0].trim_end(),
            "This pane needs a newer clackatoo (notebook)"
        );
        assert!(rows[1].contains(":update"), "{rows:?}");
        assert_eq!(pane.title(), "notebook (needs a newer clackatoo)");
    }

    #[test]
    fn one_on_another_machine_says_where_and_that_its_coming() {
        let mut pane = OtherPane::new(PaneKind::new("editor"), Some("desk".into()));
        pane.resize(Size::new(120, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(rows[0].trim_end(), "an editor on desk: coming soon");
        assert!(!rows[1].contains(":update"), "{rows:?}");
        assert_eq!(pane.title(), "an editor on desk: coming soon");
        assert_eq!(pane.host(), Some("desk"));
    }
}
