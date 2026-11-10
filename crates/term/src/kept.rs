//! A terminal's history kept across a restart of the daemon: its newest
//! lines copied out cheaply where the terminal lives, made plain data
//! elsewhere ([`KeptCopy::lines`]), and brought back, there too
//! ([`KeptLines::history`]), as the history above a new shell
//! ([`Terminal::take_kept_history`]).
//!
//! Lines keep their text and styles (colours, attributes, underlines),
//! but not links or a shell's marks, and the hints of how private they
//! are (see [`crate::hints`]): a line never to be written to disk is left
//! out, a placeholder in its place. Pictures aren't kept: each is its size
//! in words where it started (see [`crate::graphics::words`]), as copied
//! text says it.
//!
//! [`KeptLines`] is a file format: frozen, as [`crate::Snapshot`] is.

use crate::hints::{Hints, NOT_KEPT};
use crate::row::{Row, RowBuilder};
use crate::scrollback::Scrollback;
use crate::snapshot::{Line, RestoreError};
use crate::style::{Attributes, Style};
use crate::terminal::{Config, Terminal};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The newest lines of a terminal's primary screen and its history, as
/// they are, copied to be made into [`KeptLines`] off the thread the
/// terminal lives on: see [`Terminal::copy_to_keep`].
#[derive(Clone, Debug)]
pub struct KeptCopy {
    history: Scrollback,
    screen: Vec<Row>,
    /// The size of each picture its lines may show, in pixels, by number.
    pictures: std::collections::HashMap<u32, [u32; 2]>,
}

/// Lines kept across a restart, oldest first, as plain data: what's
/// written to disk, and read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KeptLines {
    lines: Vec<KeptLine>,
}

/// One kept line, and the hints it had, as bits: `NOSAVE` only where
/// config said to keep such lines all the same, and that one's dropped as
/// the line is read back.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct KeptLine {
    line: Line,
    hints: u8,
}

/// Which of a terminal's lines are kept (see [`KeptCopy::lines`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keeping {
    /// None numbered before this: they were forgotten.
    pub from: u64,
    /// Only those the user kept all the same, [`Hints::KEPT`]: the
    /// terminal's history is marked not to be kept.
    pub marked_only: bool,
    /// Lines a program said never to write to disk, [`Hints::NOSAVE`],
    /// are kept all the same: the user's config says to ignore it.
    pub ignore_nosave: bool,
}

/// History made from [`KeptLines`], ready to be taken by a new terminal
/// in a moment: see [`Terminal::take_kept_history`].
#[derive(Debug)]
pub struct KeptHistory {
    history: Scrollback,
}

impl Terminal {
    /// A copy of the newest `most` lines of the primary screen's history,
    /// and its screen, cheap enough to take where the terminal lives: the
    /// pages of history that hold them, copied whole, and the rows not
    /// compacted yet, which there are few of once the terminal's output
    /// has paused (see [`Terminal::scrollback_needs_compaction`]).
    #[must_use]
    pub fn copy_to_keep(&self, most: usize) -> KeptCopy {
        let (history, screen) = self.primary().copy_newest(most);
        let pictures = self.picture_sizes();
        KeptCopy {
            history,
            screen,
            pictures,
        }
    }

    /// Takes `kept` as this terminal's history, if it has none yet, nor
    /// ever had any: a new terminal, before its program has printed a
    /// line. The line `kept` ends with says where the restart was, `at`,
    /// in seconds since 1970 (see [`Grid::restarted`]). Returns its
    /// number, or `None` if `kept` wasn't taken.
    ///
    /// [`Grid::restarted`]: crate::Grid::restarted
    pub fn take_kept_history(&mut self, kept: KeptHistory, at: i64) -> Option<u64> {
        let last = kept.history.end().checked_sub(1)?;
        let primary = self.primary_mut();
        if !primary.take_history(kept.history) {
            return None;
        }
        primary.set_restarted(Some((last, at)));
        Some(last)
    }

    /// Says history line `line` is where history kept from before a
    /// restart ends, as [`Grid::restarted`] gave it: for a terminal handed
    /// to a newer build.
    ///
    /// [`Grid::restarted`]: crate::Grid::restarted
    pub fn set_restarted(&mut self, restarted: Option<(u64, i64)>) {
        self.primary_mut().set_restarted(restarted);
    }
}

impl KeptCopy {
    /// `row`, its pictures that start on it among `pieces` said in words
    /// where they start, if any do.
    fn pictures_said(&self, row: &Row, pieces: &[crate::graphics::Piece]) -> Option<Row> {
        let mut said: Option<Row> = None;
        for piece in pieces.iter().filter(|piece| piece.row == 0) {
            let Some(&[width, height]) = self.pictures.get(&piece.picture) else {
                continue;
            };
            let row = said.get_or_insert_with(|| row.clone());
            let words = crate::graphics::words(width, height);
            for (col, ch) in (usize::from(piece.col)..row.width()).zip(words.chars()) {
                row.write(col, ch, false, &Style::DEFAULT);
            }
        }
        said
    }

    /// The copy's lines, oldest first, the screen's last blank rows left
    /// out, as plain data, taking no more than `most_bytes` with what
    /// frames each line, the oldest let go first, and only those `keeping`
    /// says. A run of lines never to be written to disk becomes one
    /// placeholder line.
    #[must_use]
    pub fn lines(&self, keeping: Keeping, most_bytes: usize) -> KeptLines {
        let end = self.history.end();
        let shown = self
            .screen
            .iter()
            .rposition(|row| row.is_wrapped() || row.text_end() > 0)
            .map_or(0, |last| last + 1);
        let screen = (end..).zip(self.screen[..shown].iter().map(|row| (row.hints(), row)));
        let mut hinted = self.history.hints().peekable();
        let history = (self.history.oldest()..)
            .zip(self.history.rows())
            .map(|(number, row)| {
                while hinted.next_if(|&(line, _)| line < number).is_some() {}
                let hints = hinted
                    .next_if(|&(line, _)| line == number)
                    .map_or(Hints::empty(), |(_, hints)| hints);
                (number, (hints, row))
            });
        let widths = self.history.widths();
        let mut lines = Vec::new();
        let mut placeholder = false;
        let mut pieces = self.history.pieces_from(0).peekable();
        let mut push = |number: u64, hints: Hints, row: &Row| {
            // History's pictures are beside its rows; the screen's in them.
            let mut starting = Vec::new();
            while let Some((line, piece)) = pieces.next_if(|&(line, _)| line <= number) {
                if line == number {
                    starting.push(piece);
                }
            }
            starting.extend(row.pieces().iter().copied());
            let said = self.pictures_said(row, &starting);
            let row = said.as_ref().unwrap_or(row);
            if number < keeping.from || keeping.marked_only && !hints.contains(Hints::KEPT) {
                return;
            }
            if hints.contains(Hints::NOSAVE) && !keeping.ignore_nosave {
                if !placeholder {
                    placeholder = true;
                    lines.push(KeptLine {
                        line: not_kept(row.width()),
                        hints: 0,
                    });
                }
                return;
            }
            placeholder = false;
            lines.push(KeptLine {
                line: Line::of_row(row, widths).0,
                hints: hints.bits(),
            });
        };
        for (number, (hints, row)) in history {
            push(number, hints, &row);
        }
        for (number, (hints, row)) in screen {
            push(number, hints, row);
        }
        // The newest lines are kept: the oldest go past the bytes allowed.
        let mut bytes = 0;
        let keep = lines
            .iter()
            .rev()
            .take_while(|kept| {
                bytes += LINE_FRAMING
                    + kept.line.text.len()
                    + kept.line.runs.len() * size_of::<crate::snapshot::Run>();
                bytes <= most_bytes
            })
            .count();
        lines.drain(..lines.len() - keep);
        KeptLines { lines }
    }
}

impl KeptLines {
    /// How many lines are kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Whether no line is.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The text of each line, for tests.
    #[must_use]
    pub fn texts(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|kept| kept.line.text.as_str())
            .collect()
    }

    /// The lines as history for a terminal made with `config`, laid out by
    /// its rules (see [`Widths`](crate::Widths)), followed by
    /// one more, `divider`, dimmed, which says where the restart was: made
    /// where it won't hold up the terminal's own thread, as each line is
    /// made a row again and compacted.
    ///
    /// # Errors
    ///
    /// If a line can't be made a row, as one with no columns.
    pub fn history(&self, config: &Config, divider: &str) -> Result<KeptHistory, RestoreError> {
        let rows = self.lines.iter().map(|kept| {
            let mut row = kept.line.to_linked_row(&[], config.widths)?;
            row.add_hints(Hints::from_bits_truncate(kept.hints) - Hints::NOSAVE);
            Ok(row)
        });
        let divider = {
            let width = divider
                .chars()
                .filter_map(crate::char_width)
                .sum::<usize>()
                .max(1);
            let mut builder = RowBuilder::new(width);
            let style = Style {
                attributes: Attributes::DIM,
                ..Style::DEFAULT
            };
            builder.push(divider, &style, true);
            builder.finish(false)
        };
        let history = Scrollback::restored(
            config.scrollback_lines.max(1),
            config.scrollback_compaction,
            rows.chain(std::iter::once(Ok(divider))),
            0,
            config.widths,
        )?;
        Ok(KeptHistory { history })
    }
}

/// What a line takes besides its text and runs, at most, as it's written:
/// its width, whether it continues, its hints and the lengths of each.
const LINE_FRAMING: usize = 16;

/// The line that stands for a run of lines never written to disk.
fn not_kept(width: usize) -> Line {
    let width = width.max(NOT_KEPT.len());
    let mut builder = RowBuilder::new(width);
    builder.push(NOT_KEPT, &Style::DEFAULT, true);
    Line::of_row(&builder.finish(false), crate::Widths::LEGACY).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Size;

    fn terminal(output: &str) -> Terminal {
        let mut terminal = Terminal::new(Size::new(20, 4).unwrap(), &Config::default());
        terminal.advance(output.as_bytes());
        terminal
    }

    #[test]
    fn kept_lines_keep_their_styles_and_come_back_as_history() {
        let mut old = terminal("one\r\n\x1b[1;31mtwo\x1b[m\r\nthree\r\nfour\r\nfive\r\nsix");
        old.compact_scrollback();
        let kept = old.copy_to_keep(100).lines(Keeping::default(), usize::MAX);
        assert_eq!(kept.texts(), ["one", "two", "three", "four", "five", "six"]);
        let history = kept.history(&Config::default(), "── restarted ──").unwrap();
        let mut new = terminal("");
        assert_eq!(new.take_kept_history(history, 7), Some(6));
        assert_eq!(new.primary().restarted(), Some((6, 7)));
        let rows: Vec<Row> = new
            .primary()
            .scrollback_lines(0..7)
            .map(|(_, row)| row)
            .collect();
        assert_eq!(rows[1].text(), "two");
        let style = rows[1].style(rows[1].cells()[0]);
        assert!(style.attributes.contains(Attributes::BOLD));
        assert_eq!(rows[6].text(), "── restarted ──");
        assert!(
            rows[6]
                .style(rows[6].cells()[0])
                .attributes
                .contains(Attributes::DIM)
        );
        // A terminal that has printed a line takes none.
        let mut printed = terminal("a\r\nb\r\nc\r\nd\r\ne");
        let history = kept.history(&Config::default(), "x").unwrap();
        assert_eq!(printed.take_kept_history(history, 7), None);
    }

    #[test]
    fn kept_lines_come_back_in_the_columns_they_had_by_the_terminals_rules() {
        // An emoji VS16 widens, one at the edge it couldn't, and a ZWJ
        // sequence, then text after each.
        let output = "\u{2764}\u{FE0F}x\r\n\x1b[20G\u{2764}\u{FE0F}\r\n\u{1F469}\u{200D}\u{1F4BB}y\r\n\r\n\r\n\r\n";
        let old = terminal(output);
        assert!(Config::default().widths.vs16);
        let cells = |row: &Row| -> Vec<(String, bool)> {
            (0..row.width())
                .map(|col| {
                    let mut text = String::new();
                    row.push_text(col, &mut text);
                    (text, row.cells()[col].is_wide())
                })
                .collect()
        };
        let before: Vec<_> = (old.primary().scrollback_lines(0..3))
            .map(|(_, row)| cells(&row))
            .collect();
        let kept = old.copy_to_keep(100).lines(Keeping::default(), usize::MAX);
        let history = kept.history(&Config::default(), "--").unwrap();
        let mut new = terminal("");
        new.take_kept_history(history, 0);
        let after: Vec<_> = (new.primary().scrollback_lines(0..3))
            .map(|(_, row)| cells(&row))
            .collect();
        assert_eq!(after, before);
        assert!(before[0][0].1, "widened");
    }

    #[test]
    fn lines_never_written_become_one_placeholder_and_other_hints_stay() {
        let mut old = terminal("a\r\npw1\r\npw2\r\nb\r\nc\r\nd\r\ne");
        old.add_hints(1..3, Hints::NOSAVE | Hints::NOAGENT);
        old.add_hints(3..4, Hints::NOAGENT);
        let kept = old.copy_to_keep(100).lines(Keeping::default(), usize::MAX);
        assert_eq!(kept.texts(), ["a", NOT_KEPT, "b", "c", "d", "e"]);
        let history = kept.history(&Config::default(), "--").unwrap();
        let mut new = terminal("");
        new.take_kept_history(history, 0);
        assert_eq!(new.hints(), [(2, Hints::NOAGENT)]);
    }

    #[test]
    fn only_the_lines_asked_for_and_the_newest_bytes_are_kept() {
        let mut old = terminal("a\r\nb\r\nc\r\nd\r\ne\r\nf");
        old.add_hints(1..3, Hints::KEPT);
        let copy = old.copy_to_keep(100);
        let keeping = |from, marked_only| Keeping {
            from,
            marked_only,
            ignore_nosave: false,
        };
        assert_eq!(
            copy.lines(keeping(4, false), usize::MAX).texts(),
            ["e", "f"]
        );
        assert_eq!(copy.lines(keeping(0, true), usize::MAX).texts(), ["b", "c"]);
        assert_eq!(copy.lines(keeping(2, true), usize::MAX).texts(), ["c"]);
        // Kept by the user, a line never to be written still isn't.
        let mut other = terminal("a\r\nb\r\nc\r\nd\r\ne\r\nf");
        other.add_hints(1..3, Hints::KEPT);
        other.add_hints(2..3, Hints::NOSAVE);
        let copy = other.copy_to_keep(100);
        assert_eq!(
            copy.lines(keeping(0, true), usize::MAX).texts(),
            ["b", NOT_KEPT]
        );
        assert_eq!(
            copy.lines(Keeping::default(), 2 * (LINE_FRAMING + 1))
                .texts(),
            ["e", "f"]
        );
        // History holds a and b; the screen, c to f.
        assert_eq!(
            old.copy_to_keep(1)
                .lines(Keeping::default(), usize::MAX)
                .texts(),
            ["b", "c", "d", "e", "f"]
        );
    }
}
