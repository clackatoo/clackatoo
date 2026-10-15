//! Marks of how a terminal's commands ended, at the end of their
//! prompt's line, as `blockmarks` says: drawn over the rows shown, never
//! part of the text.
//!
//! The daemon says which lines start a block's prompt, with the lines it
//! sends, and how blocks ended (see [`clackatoo_protocol::BlockMarks`]):
//! the window keeps the prompts of its screen's rows and of the history
//! it fetched, and how blocks ended, by their numbers.

use clackatoo_term::{Row, RowBuilder, Style};

use super::{Document, Pane};
use crate::colors;
use crate::options::BlockMarks;
use crate::render::mono_width;

/// The most rows a prompt's line is followed over to its end, for the
/// mark of how its command ended.
const MARKED_ROWS: u64 = 16;

impl Pane {
    /// The rows in a view `height` lines tall that end the line a
    /// finished command's prompt starts, each with what marks how the
    /// command ended at the end of its text, as `blockmarks` says: `✗`
    /// and its exit status for one that failed, `✓` for one that worked.
    /// Only where the shell said how it ended, and where the mark fits
    /// after the line's text: a line taken to the right edge, as by a
    /// prompt on the right, isn't marked. Never part of the text: nothing
    /// copied, searched or selected has it.
    pub(crate) fn block_marks(&self, height: usize) -> Vec<(usize, Row)> {
        if self.blockmarks == BlockMarks::Off {
            return Vec::new();
        }
        let Some(screen) = self
            .screen
            .as_ref()
            .filter(|screen| !screen.state.alternate)
        else {
            return Vec::new();
        };
        let top = self.view_top();
        let bottom = top + height as u64;
        let end = self.history_end();
        // As wide as the terminal's rows, and no wider than the pane.
        let width = usize::from(screen.size.cols().min(self.shown_size(screen).cols()));
        // A prompt's line may wrap onto rows below it, and start above
        // the view.
        let from = top.saturating_sub(MARKED_ROWS);
        let on_screen = (end..)
            .zip(&screen.prompts)
            .filter_map(|(line, prompt)| Some((line, (*prompt)?)))
            .filter(|&(line, _)| line >= from);
        let mut prompts = (self.history.prompts(from..end))
            .chain(on_screen)
            .peekable();
        let document = Document(self);
        let overlay = self.overlay.as_ref().map(|overlay| overlay.shown.line);
        let mut marked = Vec::new();
        while let Some((line, block)) = prompts.next() {
            if line >= bottom {
                break;
            }
            // A prompt drawn again before a command ran, after Enter on
            // an empty line or Ctrl+C, is the same block's: only its last
            // has the command, so only that's marked. The next prompt is
            // looked at past the view too.
            if prompts.peek().is_some_and(|&(_, next)| next == block) {
                continue;
            }
            let Some(exit) = screen.exits.get(block) else {
                continue;
            };
            let (text, style) = match exit {
                0 if self.blockmarks == BlockMarks::All => ("✓".to_owned(), colors::block_worked()),
                0 => continue,
                exit => (format!("✗ {exit}"), colors::block_failed()),
            };
            // The last row of the line, which ends where it isn't wrapped.
            let mut last = line;
            while last < line + MARKED_ROWS && document.line(last).is_some_and(Row::is_wrapped) {
                last += 1;
            }
            let Some(row) = document
                .line(last)
                .filter(|_| (top..bottom).contains(&last))
            else {
                continue;
            };
            if overlay == Some(last) {
                continue;
            }
            let col = row.text_end() + 2;
            if col + mono_width(&text) > width {
                continue;
            }
            let at = usize::try_from(last - top).expect("within the height");
            marked.push((at, with_text_at(row, col, &text, &style)));
        }
        marked
    }
}

/// `row` with `text` written in `style` from column `col`, past the end
/// of its text, the row made wider for it if need be: a history line may
/// be narrower than the pane it's shown in now.
fn with_text_at(row: &Row, col: usize, text: &str, style: &Style) -> Row {
    let width = row.width().max(col + mono_width(text));
    let mut builder = RowBuilder::new(width);
    let mut cell = String::new();
    for at in 0..col {
        cell.clear();
        let shown = row.cells().get(at).map(|&value| {
            row.push_text(at, &mut cell);
            row.style(value)
        });
        match shown {
            Some(cell_style) if !cell.is_empty() => builder.push(&cell, cell_style, true),
            Some(_) => {}
            None => builder.push(" ", &Style::DEFAULT, true),
        }
    }
    builder.push(text, style, true);
    // What's after the mark, a blank in its own colours.
    for at in builder.filled()..row.width() {
        cell.clear();
        row.push_text(at, &mut cell);
        if !cell.is_empty() {
            builder.push(&cell, row.style(row.cells()[at]), true);
        }
    }
    builder.finish(row.is_wrapped())
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        BlockMarks as Marks, ClientMessage, GridState, HistoryLine, HistoryLines, Line, NodeId,
        Patch, Request, Response, Size, TerminalId, View, ViewUpdate,
    };
    use clackatoo_term::{Config, Terminal};

    use super::*;
    use crate::input::Modifiers;
    use crate::options::Options;

    const PROMPT: &str = "\x1b]133;A\x07$ ";

    /// A command typed at a prompt, run, printing `output` and ending with
    /// `exit`, as shell integration marks it.
    fn ran(command: &str, output: &str, exit: i32) -> String {
        format!("{PROMPT}{command}\r\n\x1b]133;C\x07{output}\x1b]133;D;{exit}\x07")
    }

    fn terminal(cols: u16, rows: u16, output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn pane(terminal: &Terminal) -> Pane {
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(terminal)));
        pane.take_outbox();
        pane
    }

    fn marked(pane: &Pane, height: usize) -> Vec<(usize, String)> {
        (pane.block_marks(height).into_iter())
            .map(|(line, row)| (line, row.text()))
            .collect()
    }

    fn set(pane: &mut Pane, blockmarks: BlockMarks) {
        let options = Options {
            blockmarks,
            ..Options::default()
        };
        pane.set_options(&options);
    }

    /// Answers the pane's fetches of history as the daemon does.
    fn serve(pane: &mut Pane, terminal: &Terminal) {
        for message in pane.take_outbox() {
            let ClientMessage::Request(Request::History { end, count, .. }) = message else {
                continue;
            };
            let start = end.saturating_sub(u64::from(count));
            let lines: Vec<HistoryLine> = (terminal.primary().scrollback_lines(start..end))
                .map(|(_, row)| HistoryLine {
                    width: u16::try_from(row.width()).unwrap(),
                    line: Line::from_row(&row),
                })
                .collect();
            let count = u16::try_from(lines.len()).unwrap();
            pane.answered(Response::History(HistoryLines {
                first: start,
                marks: Marks::of_history(terminal, start, count),
                lines,
                links: Vec::new(),
            }));
        }
    }

    #[test]
    fn failed_commands_are_marked_after_their_prompts_text() {
        let output = [
            ran("false", "", 1),
            ran("true", "", 0),
            ran("ls /x", "ls: no\r\n", 2),
            PROMPT.to_owned(),
        ];
        let mut pane = pane(&terminal(20, 5, &output.concat()));
        let failed = [
            (0, "$ false  ✗ 1".to_owned()),
            (2, "$ ls /x  ✗ 2".to_owned()),
        ];
        assert_eq!(marked(&pane, 5), failed);
        // The rows the window keeps are as the program drew them.
        assert_eq!(pane.screen().unwrap().rows[0].text(), "$ false");

        set(&mut pane, BlockMarks::All);
        let all = marked(&pane, 5);
        assert_eq!(all.len(), 3, "the prompt waiting has no mark: {all:?}");
        assert_eq!(all[1], (1, "$ true  ✓".to_owned()));
        set(&mut pane, BlockMarks::Off);
        assert_eq!(marked(&pane, 5), []);
    }

    #[test]
    fn a_prompt_drawn_again_marks_only_its_last_row() {
        // Enter on an empty line: the same block's prompt, drawn again.
        let output = format!("{PROMPT}\r\n{}{PROMPT}", ran("false", "", 1));
        let pane = pane(&terminal(20, 5, &output));
        assert_eq!(marked(&pane, 5), [(1, "$ false  ✗ 1".to_owned())]);
        // Its last row below the view still keeps the first unmarked.
        assert_eq!(marked(&pane, 1), []);
    }

    #[test]
    fn a_mark_fits_in_the_pane_not_only_the_terminal() {
        let mut pane = pane(&terminal(20, 5, &(ran("false", "", 1) + PROMPT)));
        assert_eq!(marked(&pane, 5).len(), 1);
        // Another window decides the terminal's size; this pane shows ten
        // columns of it, too few for the mark.
        pane.resize(Size::new(10, 5).unwrap());
        assert_eq!(marked(&pane, 5), []);
    }

    #[test]
    fn a_command_finishing_is_marked_as_the_daemon_says() {
        let mut terminal = terminal(20, 4, &format!("{PROMPT}false\r\n\x1b]133;C\x07"));
        let mut pane = pane(&terminal);
        assert_eq!(marked(&pane, 4), [], "it's still running");

        // As the daemon patches it: the lines changed, and the blocks
        // finished since it last did.
        let patch = |pane: &mut Pane, terminal: &Terminal, settled| {
            let grid = terminal.grid();
            let (exits, settled) = Marks::finished_after(terminal, settled);
            pane.view_changed(&ViewUpdate::Patch(vec![Patch::Grid {
                node: NodeId::ROOT,
                state: GridState::of(terminal),
                lines: (0..4)
                    .map(|line| (line, Line::from_row(grid.row(usize::from(line)))))
                    .collect(),
                links: Vec::new(),
                marks: Marks {
                    prompts: Marks::prompts_of(
                        (0..4).map(|line| (line, grid.row(usize::from(line)))),
                    ),
                    exits,
                },
            }]));
            settled
        };
        let settled = Marks::settled(&terminal);
        terminal.advance(format!("\x1b]133;D;1\x07{PROMPT}").as_bytes());
        let settled = patch(&mut pane, &terminal, settled);
        assert_eq!(marked(&pane, 4), [(0, "$ false  ✗ 1".to_owned())]);

        // Rows used again start nothing.
        terminal.advance(b"\x1b[H\x1b[2Jfalse");
        patch(&mut pane, &terminal, settled);
        assert_eq!(marked(&pane, 4), []);
    }

    #[test]
    fn prompts_in_history_are_marked_as_it_comes() {
        let terminal = terminal(20, 3, &(ran("false", "1\r\n2\r\n3\r\n", 1) + PROMPT));
        let mut pane = pane(&terminal);
        assert!(pane.wheel(4, ([0, 0], [0.0, 0.0]), Modifiers::default()));
        serve(&mut pane, &terminal);
        assert_eq!(marked(&pane, 3), [(0, "$ false  ✗ 1".to_owned())]);
    }

    #[test]
    fn a_wrapped_line_is_marked_where_it_ends_and_only_where_it_fits() {
        // 19 cells on 12-cell rows: 7 on the second, with room after.
        let mut pane = pane(&terminal(12, 4, &ran("echo aaaaaaaaaaaa", "", 1)));
        assert_eq!(marked(&pane, 4), [(1, "aaaaaaa  ✗ 1".to_owned())]);
        // 8 on the second leaves none.
        pane = self::pane(&terminal(12, 4, &ran("echo aaaaaaaaaaaaa", "", 1)));
        assert_eq!(marked(&pane, 4), []);
    }

    #[test]
    fn full_screen_programs_show_no_marks() {
        let pane = pane(&terminal(20, 4, &(ran("false", "", 1) + "\x1b[?1049h")));
        assert_eq!(marked(&pane, 4), []);
    }

    #[test]
    fn a_mark_is_written_past_the_end_of_a_row_narrower_than_the_pane() {
        let mut builder = RowBuilder::new(4);
        builder.push("ab", &Style::DEFAULT, false);
        let row = builder.finish(false);
        let marked = with_text_at(&row, 4, "✗ 1", &colors::block_failed());
        assert_eq!(marked.width(), 7);
        assert_eq!(marked.text(), "ab  ✗ 1");
        assert_eq!(*marked.style(marked.cells()[4]), colors::block_failed());
        assert_eq!(*marked.style(marked.cells()[0]), Style::DEFAULT);
    }
}
