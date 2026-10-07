//! Views: what a pane shows, sent to clients and kept current with patches.
//!
//! A view is a tree of nodes, which clients render without knowing what
//! kind of pane it is. Today the only node is a [`GridNode`], the cells of
//! a terminal, so every view is a single grid. Other nodes (text, lists,
//! images…) will join it as panes other than terminals need them.

use clackatoo_term::{InputModes, Row, RowBuilder, Size, Style, Terminal};
use serde::{Deserialize, Serialize};

/// What a pane shows at one moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// The pane's title: for a terminal, the one its program set, or empty.
    pub title: String,
    /// The view's content.
    pub root: Node,
}

/// One node of a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Node {
    /// A grid of character cells.
    Grid(GridNode),
}

/// Names a node within a view, so patches can say which one they change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The view's root node.
    pub const ROOT: Self = Self(0);
}

/// A grid of character cells: what a terminal shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridNode {
    /// The grid's size.
    pub size: Size,
    /// The cursor.
    pub cursor: CursorState,
    /// How the program wants keys and pastes encoded.
    pub modes: InputModes,
    /// The grid's lines, top to bottom. There is always one per row.
    pub lines: Vec<Line>,
}

/// Where the cursor is, and whether it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Screen line, from 0 at the top.
    pub line: u16,
    /// Column, from 0 at the left.
    pub col: u16,
    /// Whether the program wants the cursor drawn.
    pub visible: bool,
}

/// One line of a grid, as runs of identically styled text.
///
/// Trailing blank cells in the default style are left out, so an empty line
/// has no runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The line's text, split wherever the style changes.
    pub runs: Vec<Run>,
    /// Whether the line's text continues on the next line because it reached
    /// the right edge, rather than ending with a newline.
    pub wrapped: bool,
}

/// A stretch of text in one style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// How the text is drawn.
    pub style: Style,
    /// The text. A double-width character is one `char` here but covers
    /// two columns.
    pub text: String,
    /// The run's first character starts a new cell, though the run before
    /// ends in a zero width joiner, which would otherwise join them.
    pub separate: bool,
}

/// A change to a view a client already has.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewUpdate {
    /// The whole view: the first one a client gets, and whenever a patch
    /// can't describe the change, such as a resize.
    Snapshot(View),
    /// Changes to the view last sent.
    Patch(Vec<Patch>),
}

/// One change to a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Patch {
    /// The title changed.
    Title(String),
    /// A grid changed.
    Grid {
        /// The grid node.
        node: NodeId,
        /// The cursor, changed or not.
        cursor: CursorState,
        /// The input modes, changed or not.
        modes: InputModes,
        /// The lines that changed, each with its line number.
        lines: Vec<(u16, Line)>,
    },
}

impl View {
    /// What `terminal` is showing.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        Self {
            title: terminal.title().to_owned(),
            root: Node::Grid(GridNode::capture(terminal)),
        }
    }

    /// The root grid, if the view is one.
    #[must_use]
    pub fn grid(&self) -> Option<&GridNode> {
        match &self.root {
            Node::Grid(grid) => Some(grid),
        }
    }

    /// Applies `update`. Changes to lines or nodes the view doesn't have are
    /// ignored, so a confused daemon can't crash a client.
    pub fn apply(&mut self, update: ViewUpdate) {
        let patches = match update {
            ViewUpdate::Snapshot(view) => {
                *self = view;
                return;
            }
            ViewUpdate::Patch(patches) => patches,
        };
        for patch in patches {
            match (patch, &mut self.root) {
                (Patch::Title(title), _) => self.title = title,
                (
                    Patch::Grid {
                        node: NodeId::ROOT,
                        cursor,
                        modes,
                        lines,
                    },
                    Node::Grid(grid),
                ) => {
                    grid.cursor = cursor;
                    grid.modes = modes;
                    for (index, line) in lines {
                        if let Some(old) = grid.lines.get_mut(usize::from(index)) {
                            *old = line;
                        }
                    }
                }
                (Patch::Grid { .. }, _) => {}
            }
        }
    }
}

impl GridNode {
    /// What `terminal`'s screen shows.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        Self {
            size: terminal.size(),
            cursor: CursorState::of(terminal),
            modes: terminal.input_modes(),
            lines: terminal.grid().rows().map(Line::from_row).collect(),
        }
    }
}

impl CursorState {
    /// `terminal`'s cursor.
    #[must_use]
    pub fn of(terminal: &Terminal) -> Self {
        let cursor = terminal.cursor();
        Self {
            line: to_u16(cursor.line),
            col: to_u16(cursor.col),
            visible: cursor.visible,
        }
    }
}

impl Line {
    /// `row`'s text and styles.
    #[must_use]
    pub fn from_row(row: &Row) -> Self {
        let cells = row.cells();
        let is_blank = |col: usize| {
            let cell = cells[col];
            cell.ch() == ' ' && cell.flags().is_empty() && *row.style(cell) == Style::DEFAULT
        };
        let end = (0..cells.len())
            .rposition(|col| !is_blank(col))
            .map_or(0, |last| last + 1);

        let mut runs: Vec<Run> = Vec::new();
        for (col, &cell) in cells.iter().enumerate().take(end) {
            let style = *row.style(cell);
            match runs.last_mut() {
                // A cell after one ending in a ZWJ starts its own run, marked
                // separate, or rebuilding the row would join the two.
                Some(run) if run.style == style && !run.text.ends_with(ZWJ) => {
                    row.push_text(col, &mut run.text);
                }
                last => {
                    let separate = last.is_some_and(|run| run.text.ends_with(ZWJ));
                    let mut text = String::new();
                    row.push_text(col, &mut text);
                    // A wide character's spacer pushes no text. Its style
                    // matches the character before it, so it only lands
                    // here after a ZWJ, and then must not open a run.
                    if !text.is_empty() {
                        runs.push(Run {
                            style,
                            text,
                            separate,
                        });
                    }
                }
            }
        }
        Self {
            runs,
            wrapped: row.is_wrapped(),
        }
    }

    /// The line as a row `width` cells wide, filled by the emulator's rules.
    #[must_use]
    pub fn to_row(&self, width: usize) -> Row {
        let mut builder = RowBuilder::new(width);
        for run in &self.runs {
            builder.push(&run.text, &run.style, run.separate);
        }
        builder.finish(self.wrapped)
    }

    /// The line's text without styles.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

const ZWJ: char = '\u{200D}';

/// Converts a screen coordinate, which [`Size::MAX`] bounds.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).expect("screen coordinates are bounded by Size::MAX")
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{Color, Config};

    use super::*;

    fn terminal(cols: u16, rows: u16, output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn capture(cols: u16, rows: u16, output: &str) -> GridNode {
        GridNode::capture(&terminal(cols, rows, output))
    }

    fn run(style: Style, text: &str) -> Run {
        Run {
            style,
            text: text.into(),
            separate: false,
        }
    }

    #[test]
    fn lines_are_runs_of_styled_text() {
        let grid = capture(10, 2, "ab\x1b[31mcd\x1b[0me");
        let red = Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        };
        assert_eq!(
            grid.lines[0].runs,
            [
                run(Style::DEFAULT, "ab"),
                run(red, "cd"),
                run(Style::DEFAULT, "e")
            ]
        );
        assert!(grid.lines[1].runs.is_empty());
    }

    #[test]
    fn styled_trailing_blanks_are_kept() {
        let grid = capture(4, 1, "\x1b[41m\x1b[K");
        assert_eq!(grid.lines[0].text(), "    ");
    }

    #[test]
    fn wide_characters_and_graphemes_appear_once() {
        let grid = capture(6, 1, "字e\u{301}");
        assert_eq!(grid.lines[0].text(), "字e\u{301}");
    }

    #[test]
    fn cursor_title_modes_and_wrapping() {
        let view = View::capture(&terminal(3, 2, "\x1b]2;hi\x07\x1b[?2004habcd\x1b[?25l"));
        assert_eq!(view.title, "hi");
        let grid = view.grid().unwrap();
        assert_eq!(
            grid.cursor,
            CursorState {
                line: 1,
                col: 1,
                visible: false
            }
        );
        assert!(grid.modes.bracketed_paste);
        assert!(grid.lines[0].wrapped);
        assert_eq!(grid.lines.len(), 2);
    }

    #[test]
    fn a_cell_placed_after_a_zwj_stays_separate() {
        // "b" is printed first, then "a" and a ZWJ before it, so the two sit
        // side by side without the printer having joined them.
        let t = terminal(4, 1, "\x1b[2Gb\x1b[1Ga\u{200D}");
        let line = Line::from_row(t.grid().row(0));
        assert!(line.runs[1].separate);
        assert_eq!(line.to_row(4).cells()[1].ch(), 'b');
    }

    /// Each cell's text, flags and style, plus whether the row wraps.
    fn cells(row: &Row) -> (Vec<(String, clackatoo_term::CellFlags, Style)>, bool) {
        let cells = (0..row.width())
            .map(|col| {
                let mut text = String::new();
                row.push_text(col, &mut text);
                let cell = row.cells()[col];
                (text, cell.flags(), *row.style(cell))
            })
            .collect();
        (cells, row.is_wrapped())
    }

    proptest::proptest! {
        #[test]
        fn lines_rebuild_into_the_same_rows(
            fragments in proptest::collection::vec(
                proptest::sample::select(vec![
                    "ab", "字", "e\u{301}", "👩\u{200D}💻", "\u{200D}", "\u{301}",
                    "\x1b[31m", "\x1b[1;44m", "\x1b[0m", "\x1b[K", "\x1b[3G", "\x1b[1G",
                    "\x1b[D", "\r\n", " ",
                ]),
                0..30,
            ),
            cols in 1u16..10,
        ) {
            let t = terminal(cols, 3, &fragments.concat());
            for row in t.grid().rows() {
                let rebuilt = Line::from_row(row).to_row(row.width());
                proptest::prop_assert_eq!(cells(&rebuilt), cells(row));
            }
        }
    }

    #[test]
    fn patches_update_lines_cursor_and_title() {
        let mut t = terminal(4, 2, "ab");
        let mut view = View::capture(&t);
        t.advance(b"\r\ncd\x1b]2;x\x07");
        view.apply(ViewUpdate::Patch(vec![
            Patch::Title("x".into()),
            Patch::Grid {
                node: NodeId::ROOT,
                cursor: CursorState::of(&t),
                modes: t.input_modes(),
                lines: vec![(1, Line::from_row(t.grid().row(1))), (9, Line::default())],
            },
        ]));
        assert_eq!(view, View::capture(&t));
    }
}
