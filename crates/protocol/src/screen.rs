//! Snapshots of what a terminal is showing.

use clackatoo_term::{Row, Size, Style, Terminal};
use serde::{Deserialize, Serialize};

/// What a terminal is showing at one moment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// The terminal's size.
    pub size: Size,
    /// The cursor.
    pub cursor: CursorState,
    /// The window title the program set, or empty.
    pub title: String,
    /// The screen's lines, top to bottom. There is always one per row.
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

/// One line of the screen, as runs of identically styled text.
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
}

impl Screen {
    /// A snapshot of `terminal`'s visible screen.
    #[must_use]
    pub fn capture(terminal: &Terminal) -> Self {
        let cursor = terminal.cursor();
        Self {
            size: terminal.size(),
            cursor: CursorState {
                line: to_u16(cursor.line),
                col: to_u16(cursor.col),
                visible: cursor.visible,
            },
            title: terminal.title().to_owned(),
            lines: terminal.grid().rows().map(Line::from_row).collect(),
        }
    }
}

impl Line {
    fn from_row(row: &Row) -> Self {
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
                Some(run) if run.style == style => row.push_text(col, &mut run.text),
                _ => {
                    let mut text = String::new();
                    row.push_text(col, &mut text);
                    runs.push(Run { style, text });
                }
            }
        }
        // A wide character's spacer pushes no text, so it can't open a run
        // alone: its style always matches the wide character before it.
        Self {
            runs,
            wrapped: row.is_wrapped(),
        }
    }

    /// The line's text without styles.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

/// Converts a screen coordinate, which [`Size::MAX`] bounds.
fn to_u16(value: usize) -> u16 {
    u16::try_from(value).expect("screen coordinates are bounded by Size::MAX")
}

#[cfg(test)]
mod tests {
    use clackatoo_term::{Color, Config};

    use super::*;

    fn capture(cols: u16, rows: u16, output: &str) -> Screen {
        let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        Screen::capture(&terminal)
    }

    #[test]
    fn lines_are_runs_of_styled_text() {
        let screen = capture(10, 2, "ab\x1b[31mcd\x1b[0me");
        let red = Style {
            fg: Color::Indexed(1),
            ..Style::DEFAULT
        };
        assert_eq!(
            screen.lines[0].runs,
            [
                Run {
                    style: Style::DEFAULT,
                    text: "ab".into()
                },
                Run {
                    style: red,
                    text: "cd".into()
                },
                Run {
                    style: Style::DEFAULT,
                    text: "e".into()
                },
            ]
        );
        assert!(screen.lines[1].runs.is_empty());
    }

    #[test]
    fn styled_trailing_blanks_are_kept() {
        let screen = capture(4, 1, "\x1b[41m\x1b[K");
        assert_eq!(screen.lines[0].text(), "    ");
    }

    #[test]
    fn wide_characters_and_graphemes_appear_once() {
        let screen = capture(6, 1, "字e\u{301}");
        assert_eq!(screen.lines[0].text(), "字e\u{301}");
    }

    #[test]
    fn cursor_title_and_wrapping() {
        let screen = capture(3, 2, "\x1b]2;hi\x07abcd\x1b[?25l");
        assert_eq!(screen.title, "hi");
        assert_eq!(
            screen.cursor,
            CursorState {
                line: 1,
                col: 1,
                visible: false
            }
        );
        assert!(screen.lines[0].wrapped);
        assert_eq!(screen.lines.len(), 2);
    }
}
