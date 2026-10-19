//! A pane of a kind this window can't show: one a newer daemon has, which
//! it sends as [`Content::Other`](clackatoo_protocol::Content::Other),
//! naming the kind, or one on another machine that doesn't come through:
//! an editor, say, whose machine's clackatoo is too old to pass it on. A
//! pane from another machine in a workspace of the daemon's own shows as
//! one too until that machine says what it shows, as before it's reached;
//! so does one on a machine the daemon can't reach at all, which another
//! machine put in one of its workspaces; and so does one of the daemon's
//! own machine, in a workspace of a machine that's away, as it's parked
//! meanwhile. The pane says which, and what to do, if anything; the rest
//! of the window goes on as usual.

use clackatoo_protocol::{Attributes, PaneKind, Size, Style};
use clackatoo_term::{Row, RowBuilder};

use crate::colors;

/// Why a pane shows as one this window can't show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// It's of a kind newer than this window.
    Newer,
    /// It's on this machine, another, whose clackatoo is too old to pass
    /// it on.
    TooOld(String),
    /// It's on this machine, another, in a workspace of the daemon's own,
    /// and waits for that machine to say what it shows.
    Waiting(String),
    /// It's on this machine, another, which the daemon can't reach: one
    /// that put it in a workspace of a machine that can't reach it either.
    Unreachable(String),
    /// It's on the daemon's own machine, in a workspace of this machine,
    /// another, which is away: it's parked meanwhile, in a workspace of
    /// the daemon's own named after that machine.
    Parked(String),
}

/// A pane saying it needs a newer clackatoo, here or on the machine it's
/// on, or why else it can't be shown here.
#[derive(Debug)]
pub(crate) struct OtherPane {
    kind: PaneKind,
    why: Why,
    title: String,
    width: usize,
    rows: Vec<Row>,
}

impl OtherPane {
    /// A pane of `kind` that can't be shown, as `why` says.
    pub(crate) fn why(kind: PaneKind, why: Why) -> Self {
        let title = match &why {
            Why::Newer => format!("{kind} (needs a newer clackatoo)"),
            Why::TooOld(host) => said_elsewhere(kind, host),
            Why::Waiting(host) => said_waiting(kind, host),
            Why::Unreachable(host) => {
                format!("{}, not reachable from here", said_waiting(kind, host))
            }
            Why::Parked(host) => format!("{}, parked while ‘{host}’ is away", what(kind)),
        };
        Self {
            kind,
            why,
            title,
            width: 0,
            rows: Vec::new(),
        }
    }

    /// Why it can't be shown.
    pub(crate) fn reason(&self) -> &Why {
        &self.why
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
        let kind = self.kind;
        let (first_words, second_words) = match &self.why {
            Why::Waiting(host) => (
                said_waiting(kind, host),
                format!("It shows here once {host} is reached, and says what it shows."),
            ),
            Why::Unreachable(host) => (
                format!("{}, not reachable from here", said_waiting(kind, host)),
                format!(
                    "Another machine put it here, and this machine isn't connected to {host}: \
                     a window whose machine reaches {host} shows it. `:connect {host}` here \
                     would too."
                ),
            ),
            Why::Parked(host) => (
                format!(
                    "{} of this machine's, parked while ‘{host}’ is away",
                    what(kind)
                ),
                format!(
                    "It's in the workspace ‘from-{host}’ meanwhile, and comes back here once \
                     {host} is reached."
                ),
            ),
            Why::TooOld(host) => (
                said_elsewhere(kind, host),
                format!(
                    "clackatoo on {host} is from before it passed these on: `clackatoo update` \
                     there updates it."
                ),
            ),
            Why::Newer => (
                said(kind),
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

/// What a pane of `kind` is called in what one says: "a terminal".
fn what(kind: PaneKind) -> &'static str {
    match kind.as_str() {
        "terminal" => "a terminal",
        "editor" => "an editor",
        "explorer" => "an explorer",
        "board" => "a board",
        "review" => "a review",
        "browser" => "a chat's browser",
        _ => "a pane",
    }
}

/// What a pane of `kind` on `host`, another machine, says while it waits
/// for that machine: "a terminal on desk".
fn said_waiting(kind: PaneKind, host: &str) -> String {
    format!("{} on {host}", what(kind))
}

/// What a pane of `kind` on `host`, another machine, says: "an editor on
/// desk needs a newer clackatoo there", as the daemon says when asked
/// for one there.
pub(crate) fn said_elsewhere(kind: PaneKind, host: &str) -> String {
    let what = match kind.as_str() {
        "editor" => "an editor",
        "explorer" => "an explorer",
        "board" => "a board",
        "review" => "a review",
        "browser" => "a chat's browser",
        "terminal" => {
            return format!(
                "a terminal from yet another machine, in a workspace on {host}: not reachable \
                 from here"
            );
        }
        _ => "a pane",
    };
    format!("{what} on {host} needs a newer clackatoo there")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_names_the_kind_and_how_to_update() {
        let mut pane = OtherPane::why(PaneKind::new("notebook"), Why::Newer);
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
    fn one_on_another_machine_says_where_and_that_its_clackatoo_is_too_old() {
        let mut pane = OtherPane::why(PaneKind::new("editor"), Why::TooOld("desk".into()));
        pane.resize(Size::new(120, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(
            rows[0].trim_end(),
            "an editor on desk needs a newer clackatoo there"
        );
        assert!(rows[1].contains("clackatoo update"), "{rows:?}");
        assert!(!rows[1].contains(":update"), "{rows:?}");
        assert_eq!(
            pane.title(),
            "an editor on desk needs a newer clackatoo there"
        );
        assert_eq!(pane.reason(), &Why::TooOld("desk".into()));
        // A terminal from yet another machine that can't be reached says
        // so.
        let pane = OtherPane::why(PaneKind::new("terminal"), Why::TooOld("desk".into()));
        assert!(
            pane.title().ends_with("not reachable from here"),
            "{}",
            pane.title()
        );
    }

    #[test]
    fn one_on_a_machine_out_of_reach_says_so_and_what_would_show_it() {
        let mut pane = OtherPane::why(PaneKind::new("terminal"), Why::Unreachable("laptop".into()));
        pane.resize(Size::new(200, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(
            rows[0].trim_end(),
            "a terminal on laptop, not reachable from here"
        );
        assert!(rows[1].contains(":connect laptop"), "{rows:?}");
    }

    #[test]
    fn one_parked_says_where_it_is_meanwhile() {
        let mut pane = OtherPane::why(PaneKind::new("editor"), Why::Parked("desk".into()));
        pane.resize(Size::new(200, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(
            rows[0].trim_end(),
            "an editor of this machine's, parked while ‘desk’ is away"
        );
        assert!(rows[1].contains("‘from-desk’"), "{rows:?}");
    }

    #[test]
    fn one_waiting_for_its_machine_says_where_and_what_it_waits_for() {
        let mut pane = OtherPane::why(PaneKind::new("terminal"), Why::Waiting("desk".into()));
        pane.resize(Size::new(120, 10).unwrap());
        let rows: Vec<String> = pane.scene().iter().map(|row| row.text()).collect();
        assert_eq!(rows[0].trim_end(), "a terminal on desk");
        assert!(rows[1].contains("once desk is reached"), "{rows:?}");
        assert_eq!(pane.title(), "a terminal on desk");
    }
}
