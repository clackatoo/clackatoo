//! Hints about how private a line's text is: whether it's masked on
//! screen, never written to disk, or kept from agents.
//!
//! Each hint only ever adds protection. A line has the hints of every
//! region its text was printed in, and a line rewrapped from several keeps
//! them all. Lines on screen carry their hints with them, as they carry a
//! shell's marks; history keeps the few lines that have any on the side,
//! by number, so the others pay nothing for them.
//!
//! Programs set them with clackatoo's own escape sequence, OSC 46373 (see
//! [`crate::regions`]); the terminal's owner can add them too, with
//! [`Terminal::add_hints`], as for a line a password was typed on.
//!
//! One mark that isn't a hint rides with them, as it must follow the text
//! the same way: [`Hints::KEPT`], the user's own, which only the user
//! sets, never a program.
//!
//! [`Terminal::add_hints`]: crate::Terminal::add_hints

use bitflags::bitflags;

bitflags! {
    /// How private a line's text is.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct Hints: u8 {
        /// Masked on screen until the user reveals it.
        const HIDE = 1 << 0;
        /// Never written to disk: a terminal's history kept across a
        /// restart leaves it out.
        const NOSAVE = 1 << 1;
        /// Kept from agents: what they read of the terminal leaves it out.
        const NOAGENT = 1 << 2;
        /// Not a hint, but the user's mark (`gs`): kept across a restart
        /// though the terminal's history is marked not to be, unless it's
        /// [`NOSAVE`](Self::NOSAVE). Set only by the user, never by a
        /// program's escape sequence.
        const KEPT = 1 << 3;
    }
}

/// What a line masked on screen (see [`Hints::HIDE`]) shows, and what a
/// copy of masked lines gives, once for each run of them: as long
/// whatever was there, so it says nothing of what's hidden.
pub const MASK: &str = "\u{25cf}\u{25cf}\u{25cf}\u{25cf}";

/// What text kept from agents reads as, in its place: once for each run of
/// such lines.
pub const KEPT_FROM_AGENTS: &str = "[secret]";

/// What text never written to disk reads as, in its place, once a restart
/// has brought back the history around it: once for each run of such
/// lines.
pub const NOT_KEPT: &str = "[secret output not kept]";

impl crate::Terminal {
    /// Adds `hints` to the lines numbered within `lines`, numbered as in
    /// [`crate::document`]: on the screen shown, and in the primary
    /// screen's history.
    pub fn add_hints(&mut self, lines: std::ops::Range<u64>, hints: Hints) {
        if self.is_alternate_screen() {
            // The alternate screen keeps no history: its rows are numbered
            // on from the primary screen's.
            let end = self.primary().scrollback_end();
            let lines = lines.start.saturating_sub(end)..lines.end.saturating_sub(end);
            self.shown_grid_mut().add_hints(lines, hints);
        } else {
            self.primary_mut().add_hints(lines, hints);
        }
    }

    /// The primary screen's lines with hints, in its history and on it,
    /// numbered as in [`crate::document`], oldest first: what a newer
    /// build is handed, for [`set_hints`](Self::set_hints).
    #[must_use]
    pub fn hints(&self) -> Vec<(u64, Hints)> {
        self.primary().hints().collect()
    }

    /// Gives the primary screen's lines in `hints` those hints, as
    /// [`hints`](Self::hints) gave them.
    pub fn set_hints(&mut self, hints: &[(u64, Hints)]) {
        self.primary_mut().set_hints(hints);
    }

    /// The run of lines marked to be hidden ([`Hints::HIDE`]) that the
    /// document's line `line` is in, if it's one, by number: but no more
    /// than [`MOST_LINES`](crate::MOST_LINES) either way.
    #[must_use]
    pub fn hidden_run(&self, line: u64) -> Option<std::ops::Range<u64>> {
        let hidden = |line| self.line_hints(line).contains(Hints::HIDE);
        let (first, last) = self.document_bounds();
        if !(first..=last).contains(&line) || !hidden(line) {
            return None;
        }
        let most = crate::MOST_LINES;
        let mut start = line;
        while start > first && line - start < most && hidden(start - 1) {
            start -= 1;
        }
        let mut end = line + 1;
        while end <= last && end - line < most && hidden(end) {
            end += 1;
        }
        Some(start..end)
    }

    /// The document's lines from `first` on with hints, in order, as
    /// [`hints`](Self::hints) gives them.
    pub(crate) fn document_hints(&self, first: u64) -> Vec<(u64, Hints)> {
        let end = self.primary().scrollback_end();
        if self.is_alternate_screen() {
            (self.grid().hints())
                .map(|(line, hints)| (line + end, hints))
                .filter(|&(line, _)| line >= first)
                .collect()
        } else {
            self.primary().hints_from(first).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, Position, Size, Terminal};

    fn terminal(cols: u16, output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(cols, 3).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    fn all(terminal: &Terminal, agents: bool) -> String {
        let (first, last) = terminal.document_bounds();
        let (from, to) = (
            Position {
                line: first,
                col: 0,
            },
            Position { line: last, col: 0 },
        );
        if agents {
            terminal.text_for_agents(from, to, true)
        } else {
            terminal.text(from, to, true)
        }
    }

    #[test]
    fn agents_read_a_placeholder_for_each_run_of_lines_kept_from_them() {
        let mut t = terminal(10, "ls\r\npw1\r\npw2\r\nok\r\nkey\r\nend");
        t.add_hints(1..3, Hints::NOAGENT | Hints::NOSAVE);
        t.add_hints(4..5, Hints::NOAGENT);
        t.add_hints(3..4, Hints::NOSAVE);
        assert_eq!(all(&t, true), "ls\n[secret]\nok\n[secret]\nend");
        // The user's own copy has it all.
        assert_eq!(all(&t, false), "ls\npw1\npw2\nok\nkey\nend");
        // Hints follow the lines into history, and stay with them.
        assert_eq!(
            t.hints(),
            [
                (1, Hints::NOAGENT | Hints::NOSAVE),
                (2, Hints::NOAGENT | Hints::NOSAVE),
                (3, Hints::NOSAVE),
                (4, Hints::NOAGENT),
            ]
        );
    }

    #[test]
    fn hints_stay_with_their_text_as_the_screen_is_rewrapped() {
        // "secretword" wraps over two rows at 6 columns.
        let mut t = terminal(6, "a\r\nsecretword\r\nb");
        let end = t.primary().scrollback_end();
        let hidden = (0..4)
            .find(|&row| t.grid().row(row).text().starts_with("secret"))
            .unwrap();
        let line = end + hidden as u64;
        t.add_hints(line..line + 2, Hints::HIDE);
        t.resize(Size::new(20, 3).unwrap());
        let shown: Vec<(String, Hints)> = (0..3)
            .map(|row| (t.grid().row(row).text(), t.grid().row(row).hints()))
            .collect();
        assert!(
            shown.contains(&("secretword".to_owned(), Hints::HIDE)),
            "{shown:?}"
        );
        assert!(
            shown.iter().filter(|(_, hints)| !hints.is_empty()).count() == 1,
            "{shown:?}"
        );
        t.resize(Size::new(4, 3).unwrap());
        let hidden = t.hints().len()
            + (0..3)
                .filter(|&row| !t.grid().row(row).hints().is_empty())
                .count();
        assert!(
            hidden >= 3,
            "every row the text lands on keeps it: {:?}",
            t.hints()
        );
    }
}
