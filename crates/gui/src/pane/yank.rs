//! Copying text out of a terminal, and where it goes.
//!
//! A yank in normal or visual mode, `y` and its kin, goes into the
//! window's registers exactly as an editor's does (see
//! [`crate::editor::Registers`]): the register named with `"x` first, if
//! one was, or else the unnamed register and `"0`, and the clipboard as
//! `clipboard` says. It keeps its kind, so `p` in an editor puts lines
//! copied by lines as lines, and a block as a block. Ctrl+Shift+C is `"+y`
//! but by characters: the clipboard, with the text as shown, as other
//! terminals copy it, and the unnamed register too, as Vim's `"+y` is.
//!
//! Text selected with the mouse goes onto the primary selection alone, as
//! Vim puts a mouse selection in `"*` and leaves the unnamed register be;
//! `y` or Ctrl+Shift+C then yanks it as above.
//!
//! The daemon holds the text, so a copy asks it, and the text goes on once
//! it answers.

use clackatoo_term::{MOST_LINES, MOST_TEXT};
use unicode_width::UnicodeWidthStr;

use super::{Pane, Pending, Selection};
use crate::editor::Register;
use crate::motion::{Block, Point};

/// Where a terminal's copy goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyTo {
    /// The registers, as an editor's yank: the one named, if any.
    Register(Option<char>),
    /// `"+`, and so the clipboard, as Ctrl+Shift+C copies, by characters:
    /// the text as shown, whatever was selected, as other terminals copy
    /// it, with no line break after a last line.
    Clipboard,
    /// The primary selection alone, as a mouse selection.
    Primary,
}

impl CopyTo {
    /// Whether a copy here is lit up and said: not one for the primary
    /// selection, which selecting with the mouse makes all the time, nor
    /// one thrown away into `"_`.
    fn shown(self) -> bool {
        !matches!(self, Self::Primary | Self::Register(Some('_')))
    }
}

/// Text a terminal copied, for the window to keep where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Yanked {
    /// The text, and whether it's by characters, lines or a block.
    pub(crate) register: Register,
    pub(crate) to: CopyTo,
}

/// A copy asked of the daemon: where it goes, and what it's of.
#[derive(Clone, Copy, Debug)]
pub(super) struct Copying {
    to: CopyTo,
    kind: Selection,
    /// A block's width in columns, `None` if it goes to each line's end.
    width: Option<usize>,
    /// How many lines were asked for.
    lines: u64,
}

impl Pane {
    /// Asks the daemon for the text from `start` to `end`, for `to`. Text
    /// yanked is highlighted for a moment, to show what went. A block goes
    /// from its top left corner, `start`, to its bottom right, `end`.
    pub(super) fn copy(&mut self, start: Point, end: Point, kind: Selection, to: CopyTo) {
        if to.shown() {
            self.flash = Some(super::Flash {
                start,
                end,
                kind,
                until: std::time::Instant::now() + crate::render::FLASH,
            });
        }
        let (from, to_place) = (super::normal::position(start), super::normal::position(end));
        let request = match kind {
            Selection::Block => clackatoo_protocol::Request::BlockText {
                terminal: self.terminal(),
                from,
                to: to_place,
            },
            Selection::Characters | Selection::Lines => clackatoo_protocol::Request::Text {
                terminal: self.terminal(),
                from,
                to: to_place,
                whole_lines: kind == Selection::Lines,
            },
        };
        let width = (kind == Selection::Block && end.col != Block::TO_END)
            .then(|| end.col.saturating_sub(start.col) + 1);
        let copying = Copying {
            to,
            kind,
            width,
            lines: end.line.abs_diff(start.line) + 1,
        };
        self.request(request, Pending::Text(copying));
    }

    /// Takes in the text the daemon answered a copy with. What was asked
    /// for was cut short by the daemon if it was over [`MOST_LINES`] or
    /// [`MOST_TEXT`], as it cuts any copy, and that's said, wherever it
    /// went, but for `"_`. Nothing copied, as `yl` on an empty line copies,
    /// keeps nothing, so it's neither lit up nor said; an empty line
    /// copied whole is a line.
    pub(super) fn copied(&mut self, text: String, copying: Copying) {
        let lines = text.lines().count().max(1);
        let cut = copying.lines > MOST_LINES || text.len() + 4 > MOST_TEXT;
        let register = match (copying.to, copying.kind) {
            // The primary selection takes the text as it is, as other
            // terminals put it there.
            (CopyTo::Primary | CopyTo::Clipboard, _) | (_, Selection::Characters) => {
                Register::chars(text)
            }
            // The daemon puts a line break between lines, and an editor's
            // lines each end with one, the last too, even if it's blank.
            (_, Selection::Lines) => {
                let mut text = text;
                text.push('\n');
                Register::new(text, true)
            }
            (_, Selection::Block) => {
                let rows: Vec<String> = text.split('\n').map(str::to_owned).collect();
                let longest = || rows.iter().map(|row| row.width()).max().unwrap_or(0);
                let width = copying.width.unwrap_or_else(longest);
                Register::block(&rows, width)
            }
        };
        if register.text.is_empty() {
            self.flash = None;
        } else if cut && copying.to != CopyTo::Register(Some('_')) {
            self.status = Some(format!(
                "copied {lines} lines, cut short: a copy stops at {MOST_LINES} lines or 4 MiB"
            ));
        } else if !cut && copying.to.shown() {
            self.status = Some(if lines == 1 {
                "copied".into()
            } else {
                format!("copied {lines} lines")
            });
        }
        self.yanked = Some(Yanked {
            register,
            to: copying.to,
        });
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::TerminalId;

    use super::*;

    fn copying(to: CopyTo, kind: Selection, width: Option<usize>, lines: u64) -> Copying {
        Copying {
            to,
            kind,
            width,
            lines,
        }
    }

    /// What `text`, answered for `copying`, was kept as.
    fn kept(text: &str, copying: Copying) -> Yanked {
        let mut pane = Pane::new(TerminalId(1));
        pane.copied(text.into(), copying);
        pane.take_yanked().expect("something yanked")
    }

    #[test]
    fn a_copy_the_daemon_cut_short_says_so() {
        let mut pane = Pane::new(TerminalId(1));
        let yank = CopyTo::Register(None);
        pane.copied("a\nb".into(), copying(yank, Selection::Characters, None, 2));
        assert_eq!(pane.label().as_deref(), Some("copied 2 lines"));
        let many = MOST_LINES + 1;
        let primary = copying(CopyTo::Primary, Selection::Characters, None, many);
        pane.copied("a\nb".into(), primary);
        assert!(
            pane.label()
                .is_some_and(|label| label.contains("cut short")),
            "{:?}",
            pane.label()
        );
    }

    #[test]
    fn a_yank_keeps_its_kind_as_an_editors_register_does() {
        let to = CopyTo::Register(Some('a'));
        let chars = kept("one\ntw", copying(to, Selection::Characters, None, 2));
        assert_eq!(chars.register, Register::chars("one\ntw".into()));
        assert_eq!(chars.to, to);
        // Each line ends with a line break, as an editor's do: the last
        // too, blank or not.
        let lines = kept("one\ntwo", copying(to, Selection::Lines, None, 2));
        assert_eq!(lines.register, Register::new("one\ntwo\n".into(), true));
        let blank = kept("one\n", copying(to, Selection::Lines, None, 2));
        assert_eq!(blank.register, Register::new("one\n\n".into(), true));
        // The primary selection takes the text as it is.
        let primary = kept("one", copying(CopyTo::Primary, Selection::Lines, None, 1));
        assert_eq!(primary.register.text, "one");
        // A block is as wide as its columns, or its longest row if it goes
        // to each line's end.
        let block = kept("ab\nc", copying(to, Selection::Block, Some(3), 2));
        let rows = ["ab".to_owned(), "c".to_owned()];
        assert_eq!(block.register, Register::block(&rows, 3));
        let to_end = kept("abcd\nc", copying(to, Selection::Block, None, 2));
        assert_eq!(to_end.register.block, Some(4));
    }

    #[test]
    fn a_mouse_selection_says_nothing_and_a_yank_says_copied() {
        let mut pane = Pane::new(TerminalId(1));
        let primary = copying(CopyTo::Primary, Selection::Characters, None, 1);
        pane.copied("x".into(), primary);
        assert_eq!(pane.label(), None);
        let yank = copying(CopyTo::Register(None), Selection::Characters, None, 1);
        pane.copied("x".into(), yank);
        assert_eq!(pane.label().as_deref(), Some("copied"));
    }

    #[test]
    fn nothing_copied_or_thrown_away_says_nothing_but_an_empty_line_is_a_line() {
        let says = |text: &str, to, kind| {
            let mut pane = Pane::new(TerminalId(1));
            pane.copied(text.into(), copying(to, kind, None, 1));
            pane.label()
        };
        let yank = CopyTo::Register(None);
        assert_eq!(says("", yank, Selection::Characters), None);
        assert_eq!(
            says("x", CopyTo::Register(Some('_')), Selection::Lines),
            None
        );
        assert_eq!(says("", yank, Selection::Lines).as_deref(), Some("copied"));
    }
}
