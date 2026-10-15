//! Links in a pane: the one under the mouse while Ctrl is held, underlined,
//! and opened by a Ctrl+click or, at the normal-mode cursor, by `gx`.
//!
//! A link a program printed with OSC 8 is the program's: its cells say
//! which link they're part of. Elsewhere, the logical line under the point
//! (rows that wrap onto each other, as far as the view shows them) is
//! searched for a URL or a path, on the fly (see [`crate::links`]).
//!
//! The mouse is clackatoo's to open links with whenever it's clackatoo's
//! to select with: when the program hasn't asked for mouse reports, or
//! Shift is held. So a program that takes the mouse gets Ctrl+click, as
//! kitty and `WezTerm` give it, and Ctrl+Shift+click opens the link.
//! Opening is the desk's: the pane only says what's to open.

use std::ops::Range;

use clackatoo_protocol::LinkId;
use clackatoo_term::Row;

use super::{Document, Pane};
use crate::input::Modifiers;
use crate::links::{self, Target};
use crate::motion::{Lines, Point};

/// The fewest links the window keeps what it learnt about for a pane
/// before it forgets those its rows no longer show.
pub(super) const KEPT_LINKS: usize = 4096;

/// The most rows either side of the point that are joined to find a link
/// in the text, as a logical line can be as long as history.
const MAX_JOINED: u64 = 64;

/// A link in the pane: where it is, and what it points to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneLink {
    /// The columns it covers on each line of the document it's on, in
    /// order.
    pub(crate) cells: Vec<(u64, Range<usize>)>,
    pub(crate) target: Target,
}

/// Where the mouse is over the pane, and the keys held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Hover {
    /// The cell's line and column in the view.
    pub(super) line: i64,
    pub(super) col: i64,
    pub(super) modifiers: Modifiers,
}

impl Pane {
    /// Whether a click with `modifiers` held would open the link under it:
    /// Ctrl is held, and the mouse is clackatoo's, not the program's.
    pub(crate) fn link_opens(&self, modifiers: Modifiers) -> bool {
        modifiers.ctrl
            && self.chat.is_none()
            && self.screen.is_some()
            && !self.reports_mouse(modifiers.shift)
    }

    /// Notes where the mouse is and the keys held, for the link under it.
    /// Returns whether that changes what's underlined.
    pub(crate) fn hover(&mut self, hover: Option<(i64, i64, Modifiers)>) -> bool {
        // Gone from a chat, the mouse takes away what it showed there.
        if hover.is_none()
            && let Some(chat) = &mut self.chat
        {
            return chat.unhover();
        }
        let before = self.hovered_link();
        self.hovered = hover.map(|(line, col, modifiers)| Hover {
            line,
            col,
            modifiers,
        });
        self.hovered_link() != before
    }

    /// The link the mouse is over, if a click would open it.
    pub(crate) fn hovered_link(&self) -> Option<PaneLink> {
        let hover = self.hovered?;
        if !self.link_opens(hover.modifiers) {
            return None;
        }
        self.link_in_view(hover.line, hover.col)
    }

    /// The link at the cell `line` and `col` of the view, if there is one.
    pub(crate) fn link_in_view(&self, line: i64, col: i64) -> Option<PaneLink> {
        let height = i64::try_from(self.height()).ok()?;
        let line = u64::try_from(line).ok().filter(|_| line < height)?;
        let col = usize::try_from(col).ok()?;
        self.link_at(Point {
            line: self.view_top() + line,
            col,
        })
    }

    /// The link at `at`, a point of the document in view.
    pub(super) fn link_at(&self, at: Point) -> Option<PaneLink> {
        let shown = self.lines_in_view();
        if !shown.contains(&at.line) {
            return None;
        }
        let row = Document(self).line(at.line)?;
        let col = cell_col(row, at.col)?;
        if let Some(id) = row.style(row.cells()[col]).link {
            return self.printed_link(Point { line: at.line, col }, id, &shown);
        }
        self.found_link(Point { line: at.line, col })
    }

    /// The document's lines in view, as far as they're to hand.
    fn lines_in_view(&self) -> Range<u64> {
        let (first, last) = Document(self).bounds();
        let top = self.view_top().max(first);
        top..(self.view_top() + self.height()).min(last + 1)
    }

    /// The link `id` a program printed, at `at`: with an `id` of its own,
    /// every cell in view that's part of it, or else the cells next to
    /// `at`'s, across rows that wrap onto the next.
    fn printed_link(&self, at: Point, id: LinkId, shown: &Range<u64>) -> Option<PaneLink> {
        let printed = self.screen.as_ref()?.links.get(&id)?;
        let target = links::target_of_uri(&printed.uri)?;
        let document = Document(self);
        let part = |line: u64| -> Vec<Range<usize>> {
            let Some(row) = document.line(line) else {
                return Vec::new();
            };
            let mut ranges: Vec<Range<usize>> = Vec::new();
            for (col, &cell) in row.cells().iter().enumerate() {
                if row.style(cell).link != Some(id) {
                    continue;
                }
                match ranges.last_mut() {
                    Some(range) if range.end == col => range.end = col + 1,
                    _ => ranges.push(col..col + 1),
                }
            }
            ranges
        };
        let mut cells = Vec::new();
        if printed.named {
            for line in shown.clone() {
                cells.extend(part(line).into_iter().map(|range| (line, range)));
            }
        } else {
            let here = part(at.line)
                .into_iter()
                .find(|range| range.contains(&at.col))?;
            let width = |line: u64| document.line(line).map_or(0, Row::width);
            let wraps = |line: u64| document.line(line).is_some_and(Row::is_wrapped);
            // Back over rows that wrap onto this one, while the link runs on.
            let mut line = at.line;
            let mut start = here.start;
            while start == 0 && line > shown.start && wraps(line - 1) {
                let Some(before) = part(line - 1).pop().filter(|r| r.end == width(line - 1)) else {
                    break;
                };
                cells.push((line - 1, before.clone()));
                start = before.start;
                line -= 1;
            }
            cells.reverse();
            cells.push((at.line, here.clone()));
            let (mut line, mut end) = (at.line, here.end);
            while end == width(line) && line + 1 < shown.end && wraps(line) {
                let Some(after) = part(line + 1).into_iter().next().filter(|r| r.start == 0) else {
                    break;
                };
                cells.push((line + 1, after.clone()));
                end = after.end;
                line += 1;
            }
        }
        Some(PaneLink { cells, target })
    }

    /// The URL or path at `at` in its logical line's text, as far as the
    /// view shows it.
    fn found_link(&self, at: Point) -> Option<PaneLink> {
        let document = Document(self);
        let (top, bottom) = document.bounds();
        // The rows wrapping onto `at`'s, beyond the view too, as far as
        // they're to hand. If the line may go on past them, into a row not
        // fetched or past the most looked at, it's cut short there.
        let mut first = at.line;
        let mut cut_before = false;
        while first > top {
            match document.line(first - 1) {
                Some(row) if row.is_wrapped() && at.line - first < MAX_JOINED => first -= 1,
                Some(row) => {
                    cut_before = row.is_wrapped();
                    break;
                }
                None => {
                    cut_before = true;
                    break;
                }
            }
        }
        let mut last = at.line;
        let mut cut_after = false;
        while last < bottom && document.line(last).is_some_and(Row::is_wrapped) {
            if last - at.line >= MAX_JOINED || document.line(last + 1).is_none() {
                cut_after = true;
                break;
            }
            last += 1;
        }
        // The text, and where each cell's starts in it.
        let mut text = String::new();
        let mut starts: Vec<(usize, Point, usize)> = Vec::new();
        let mut byte = None;
        for line in first..=last {
            let row = document.line(line)?;
            for (col, cell) in row.cells().iter().enumerate() {
                if cell.is_wide_spacer() {
                    continue;
                }
                let point = Point { line, col };
                if point == at {
                    byte = Some(text.len());
                }
                let cells = if cell.is_wide() { 2 } else { 1 };
                starts.push((text.len(), point, cells));
                row.push_text(col, &mut text);
            }
        }
        let found = links::find_at(&text, byte?)?;
        // A link running into a cut end may go on past it, so what's found
        // may be the wrong link, such as a URL's tail taken for a path:
        // better none.
        let to_cut = |text: &str| !text.contains(char::is_whitespace);
        if (cut_before && to_cut(&text[..found.bytes.start]))
            || (cut_after && to_cut(&text[found.bytes.end..]))
        {
            return None;
        }
        let mut cells: Vec<(u64, Range<usize>)> = Vec::new();
        let covered = starts
            .iter()
            .filter(|(start, _, _)| found.bytes.contains(start));
        for &(_, point, width) in covered {
            match cells.last_mut() {
                Some((line, range)) if *line == point.line => range.end = point.col + width,
                _ => cells.push((point.line, point.col..point.col + width)),
            }
        }
        Some(PaneLink {
            cells,
            target: found.target,
        })
    }

    /// Opens the link at the cell `line` and `col` of the view, if there's
    /// one there. Returns whether there was.
    pub(super) fn open_link_in_view(&mut self, line: i64, col: i64) -> bool {
        let Some(found) = self.link_in_view(line, col) else {
            return false;
        };
        self.opening = Some(found.target);
        true
    }

    /// `gx`: opens the link at the normal-mode cursor, as Vim does.
    pub(super) fn open_link_at_cursor(&mut self) -> bool {
        let link = self.cursor.and_then(|cursor| self.link_at(cursor.at));
        match link {
            Some(link) => self.opening = Some(link.target),
            None => self.status = Some("no link under the cursor".into()),
        }
        true
    }

    /// Takes the link to open, for the desk to open.
    pub(crate) fn take_opening(&mut self) -> Option<Target> {
        self.opening.take()
    }

    /// The working directory the terminal's shell last reported.
    pub(crate) fn cwd(&self) -> Option<&str> {
        self.screen.as_ref()?.cwd.as_deref()
    }

    /// Learns what the links in fetched history lines point to.
    pub(super) fn learn_links(&mut self, links: &clackatoo_protocol::Links) {
        if let Some(screen) = &mut self.screen {
            screen.links.extend(links.iter().cloned());
        }
        self.forget_unseen_links();
    }

    /// Forgets, once there are many, the links no row of the screen or of
    /// the history kept shows.
    pub(super) fn forget_unseen_links(&mut self) {
        let Some(screen) = &mut self.screen else {
            return;
        };
        if screen.links.len() > self.links_kept {
            let shown: std::collections::HashSet<LinkId> = (screen.rows.iter())
                .chain(self.history.rows())
                .flat_map(Row::links)
                .collect();
            screen.links.retain(|id, _| shown.contains(id));
            // The next look waits until there are twice as many, so it
            // costs little per update however many links the rows show.
            self.links_kept = (2 * screen.links.len()).max(KEPT_LINKS);
        }
    }
}

/// The column of the cell holding `col`'s character: a wide character's
/// own for its spacer. `None` past the row's end.
fn cell_col(row: &Row, col: usize) -> Option<usize> {
    let cell = row.cells().get(col)?;
    Some(if cell.is_wide_spacer() {
        col.saturating_sub(1)
    } else {
        col
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use clackatoo_protocol::{
        ClientMessage, HistoryLine, HistoryLines, Line, LinkTarget, Request, Response, Size,
        TerminalId, View, ViewUpdate,
    };
    use clackatoo_term::mouse::{MouseAction, MouseButton};
    use clackatoo_term::{Config, Terminal};
    use winit::keyboard::{Key, KeyLocation};

    use super::*;
    use crate::input::KeyPress;
    use crate::motion::Cursor;
    use crate::pane::{Mode, PaneMouse};
    use crate::render::HighlightKind;

    const CTRL: Modifiers = Modifiers {
        shift: false,
        ctrl: true,
        alt: false,
    };

    const CTRL_SHIFT: Modifiers = Modifiers {
        shift: true,
        ctrl: true,
        alt: false,
    };

    fn terminal(cols: u16, rows: u16, output: &str) -> Terminal {
        let config = Config {
            scrollback_lines: 100,
            ..Config::default()
        };
        let mut terminal = Terminal::new(Size::new(cols, rows).unwrap(), &config);
        terminal.advance(output.as_bytes());
        terminal
    }

    fn pane_of(terminal: &Terminal) -> Pane {
        let mut pane = Pane::new(TerminalId(1));
        pane.view_changed(&ViewUpdate::Snapshot(View::capture(terminal)));
        pane.take_outbox();
        pane
    }

    fn pane(cols: u16, rows: u16, output: &str) -> Pane {
        pane_of(&terminal(cols, rows, output))
    }

    fn mouse(pane: &mut Pane, action: MouseAction, [line, col]: [i64; 2], modifiers: Modifiers) {
        pane.mouse(&PaneMouse {
            action,
            line,
            col,
            pixel: [0.0; 2],
            clicks: 1,
            modifiers,
        });
    }

    fn press(pane: &mut Pane, at: [i64; 2], modifiers: Modifiers) {
        mouse(pane, MouseAction::Press(MouseButton::Left), at, modifiers);
    }

    fn url(text: &str) -> Target {
        Target::Url(text.to_owned())
    }

    /// The cells the link under `at` covers, with Ctrl held.
    fn link_cells(pane: &mut Pane, at: [i64; 2]) -> Option<Vec<(u64, Range<usize>)>> {
        pane.hover(Some((at[0], at[1], CTRL)));
        pane.hovered_link().map(|link| link.cells)
    }

    #[test]
    fn ctrl_underlines_a_printed_link_and_a_click_opens_it() {
        let mut pane = pane(
            20,
            3,
            "see \x1b]8;;https://x.example/\x1b\\here\x1b]8;;\x1b\\ now",
        );
        mouse(
            &mut pane,
            MouseAction::Motion(None),
            [0, 5],
            Modifiers::default(),
        );
        assert_eq!(pane.hovered_link(), None, "Ctrl isn't held");
        assert!(pane.hover(Some((0, 5, CTRL))));
        let link = pane.hovered_link().unwrap();
        assert_eq!(link.cells, [(0, 4..8)]);
        assert_eq!(link.target, url("https://x.example/"));
        let underlined: Vec<_> = (pane.highlights(3).into_iter())
            .filter(|highlight| highlight.kind == HighlightKind::Link)
            .map(|highlight| (highlight.line, highlight.cols))
            .collect();
        assert_eq!(underlined, [(0, 4..8)]);

        // A click opens it, selecting nothing; one beside it selects.
        press(&mut pane, [0, 5], CTRL);
        assert_eq!(pane.take_opening(), Some(url("https://x.example/")));
        assert!(pane.mouse_selected().is_none());
        press(&mut pane, [0, 1], CTRL);
        mouse(
            &mut pane,
            MouseAction::Motion(Some(MouseButton::Left)),
            [0, 2],
            CTRL,
        );
        assert_eq!(pane.take_opening(), None);
        assert!(pane.mouse_selected().is_some());
    }

    #[test]
    fn a_link_with_an_id_is_all_its_cells_and_one_without_only_those_together() {
        let named =
            "\x1b]8;id=a;https://a/\x07ab\x1b]8;;\x07 cd \x1b]8;id=a;https://a/\x07ef\x1b]8;;\x07";
        assert_eq!(
            link_cells(&mut pane(20, 2, named), [0, 7]),
            Some(vec![(0, 0..2), (0, 6..8)])
        );
        let anonymous = named.replace("id=a", "");
        assert_eq!(
            link_cells(&mut pane(20, 2, &anonymous), [0, 7]),
            Some(vec![(0, 6..8)])
        );
    }

    #[test]
    fn a_printed_link_runs_on_across_rows_it_wraps_over() {
        let output = "ab\x1b]8;;https://wrap/\x07cdefghijklmn\x1b]8;;\x07 o";
        assert_eq!(
            link_cells(&mut pane(6, 4, output), [1, 2]),
            Some(vec![(0, 2..6), (1, 0..6), (2, 0..2)])
        );
    }

    #[test]
    fn links_with_schemes_that_dont_open_are_not_links() {
        let output = "\x1b]8;;javascript:alert(1)\x07https://plain.example/\x1b]8;;\x07";
        assert_eq!(link_cells(&mut pane(40, 2, output), [0, 3]), None);
    }

    #[test]
    fn urls_and_paths_in_the_text_are_found_across_wrapped_rows() {
        // "go https://e", "xample.com/a", "bc ok".
        let mut pane = pane(12, 4, "go https://example.com/abc ok");
        assert_eq!(
            link_cells(&mut pane, [1, 3]),
            Some(vec![(0, 3..12), (1, 0..12), (2, 0..2)])
        );
        assert_eq!(
            pane.hovered_link().unwrap().target,
            url("https://example.com/abc")
        );
        assert_eq!(link_cells(&mut pane, [2, 4]), None);

        let mut pane = pane_of(&terminal(30, 3, "  --> src/main.rs:12:5\r\n"));
        assert_eq!(link_cells(&mut pane, [0, 9]), Some(vec![(0, 6..22)]));
        assert_eq!(
            pane.hovered_link().unwrap().target,
            Target::File {
                path: "src/main.rs".into(),
                line: Some(12),
                col: Some(5),
            }
        );
    }

    /// Answers the pane's requests for history from `terminal`.
    fn serve_history(pane: &mut Pane, terminal: &Terminal) {
        for message in pane.take_outbox() {
            let ClientMessage::Request(Request::History { end, count, .. }) = message else {
                continue;
            };
            let start = end.saturating_sub(u64::from(count));
            let numbered: Vec<_> = terminal.primary().scrollback_lines(start..end).collect();
            let lines: Vec<HistoryLine> = (numbered.iter())
                .map(|(_, row)| HistoryLine {
                    width: u16::try_from(row.width()).unwrap(),
                    line: Line::from_row(row),
                })
                .collect();
            let shown = LinkTarget::of(lines.iter().map(|line| &line.line), terminal);
            pane.answered(Response::History(HistoryLines {
                first: numbered[0].0,
                lines,
                links: shown,
            }));
        }
    }

    #[test]
    fn a_url_wrapped_from_rows_above_the_view_is_whole_or_not_a_link() {
        // "go https://e" scrolls into history; the screen shows
        // "xample.com/a" and "bc", which alone would look like a path.
        let terminal = terminal(12, 2, "go https://example.com/abc");
        let mut pane = pane_of(&terminal);
        assert_eq!(link_cells(&mut pane, [0, 3]), None);
        // Once the row above is to hand, the URL is found whole, though
        // its start is out of view.
        pane.wheel(1, ([0, 0], [0.0; 2]), Modifiers::default());
        serve_history(&mut pane, &terminal);
        pane.wheel(-1, ([0, 0], [0.0; 2]), Modifiers::default());
        assert_eq!(
            link_cells(&mut pane, [0, 3]),
            Some(vec![(0, 3..12), (1, 0..12), (2, 0..2)])
        );
        assert_eq!(
            pane.hovered_link().unwrap().target,
            url("https://example.com/abc")
        );
    }

    #[test]
    fn what_links_point_to_is_forgotten_less_often_the_more_rows_show() {
        // A link in each of 5000 cells, all on screen.
        let mut output = String::new();
        for n in 0..5000 {
            write!(output, "\x1b]8;;https://{n}/\x07x").unwrap();
        }
        let terminal = terminal(100, 50, &output);
        let pane = pane_of(&terminal);
        assert_eq!(pane.screen.as_ref().unwrap().links.len(), 5000);
        // Every one is still shown, so the next look waits for twice as
        // many.
        assert_eq!(pane.links_kept, 10_000);
    }

    #[test]
    fn wide_characters_before_a_link_keep_its_cells_in_place() {
        let mut pane = pane(20, 2, "字字 https://x.y/z");
        assert_eq!(link_cells(&mut pane, [0, 8]), Some(vec![(0, 5..18)]));
    }

    #[test]
    fn a_program_that_takes_the_mouse_gets_ctrl_click_and_ctrl_shift_click_opens() {
        let mut pane = pane(20, 2, "\x1b[?1000h\x1b[?1006hhttps://x.example/");
        assert!(!pane.link_opens(CTRL));
        press(&mut pane, [0, 3], CTRL);
        assert_eq!(pane.take_opening(), None);
        let sent: Vec<_> = (pane.take_outbox().into_iter())
            .filter_map(|message| match message {
                ClientMessage::Request(Request::SendInput { bytes, .. }) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(sent, [b"\x1b[<16;4;1M".to_vec()]);
        assert!(pane.link_opens(CTRL_SHIFT));
        press(&mut pane, [0, 3], CTRL_SHIFT);
        assert_eq!(pane.take_opening(), Some(url("https://x.example/")));
        assert!(pane.take_outbox().is_empty());
    }

    fn key(pane: &mut Pane, text: &str) {
        let key = Key::Character(text.into());
        pane.key(&KeyPress {
            key: &key,
            text: Some(text),
            location: KeyLocation::Standard,
            code: None,
            modifiers: Modifiers::default(),
        });
    }

    #[test]
    fn gx_opens_the_link_at_the_normal_mode_cursor() {
        let mut pane = pane(30, 3, "error at src/a.rs:3:1\r\n");
        pane.enter_normal_mode();
        pane.cursor = Some(Cursor::new(Point { line: 0, col: 12 }));
        key(&mut pane, "g");
        key(&mut pane, "x");
        assert_eq!(
            pane.take_opening(),
            Some(Target::File {
                path: "src/a.rs".into(),
                line: Some(3),
                col: Some(1),
            })
        );
        assert_eq!(pane.mode, Mode::Normal);
        pane.cursor = Some(Cursor::new(Point { line: 0, col: 2 }));
        key(&mut pane, "g");
        key(&mut pane, "x");
        assert_eq!(pane.take_opening(), None);
        assert_eq!(pane.label().as_deref(), Some("no link under the cursor"));
    }

    #[test]
    fn links_scrolled_into_history_are_found_where_the_view_shows_them() {
        let output = "\x1b]8;;https://old/\x07old\x1b]8;;\x07\r\n1\r\n2\r\n3";
        let terminal = terminal(10, 2, output);
        let mut pane = pane_of(&terminal);
        pane.wheel(3, ([0, 0], [0.0; 2]), Modifiers::default());
        // Answer the fetch for history, as the daemon would.
        for message in pane.take_outbox() {
            let ClientMessage::Request(Request::History { end, count, .. }) = message else {
                continue;
            };
            let start = end.saturating_sub(u64::from(count));
            let numbered: Vec<_> = terminal.primary().scrollback_lines(start..end).collect();
            let lines: Vec<HistoryLine> = (numbered.iter())
                .map(|(_, row)| HistoryLine {
                    width: u16::try_from(row.width()).unwrap(),
                    line: Line::from_row(row),
                })
                .collect();
            let shown = LinkTarget::of(lines.iter().map(|line| &line.line), &terminal);
            pane.answered(Response::History(HistoryLines {
                first: numbered[0].0,
                lines,
                links: shown,
            }));
        }
        assert_eq!(pane.scene(2).0[0].text(), "old");
        assert_eq!(link_cells(&mut pane, [0, 1]), Some(vec![(0, 0..3)]));
        assert_eq!(pane.hovered_link().unwrap().target, url("https://old/"));
    }
}
