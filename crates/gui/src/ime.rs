//! Typing through an input method (IME), as Chinese, Japanese and Korean
//! are typed: keys build up text the input method shows choices for, and
//! the text chosen is committed, then typed into the pane as if keyed.
//!
//! Until then the text being composed, the preedit, is drawn underlined
//! over the cells at the cursor, without touching the terminal.
//!
//! The input method is on only while keys type text, as in insert mode or
//! on the command line. In normal mode keys are commands, and an input
//! method would take them for text to compose.
//!
//! Text can reach the window two ways: as a key, when the input method lets
//! one through untouched, and as a commit. Each keypress should come only
//! one way, but some setups send it both, which would type it twice. So a
//! key and a commit of the same text, close together, count once. See
//! [`InputMethod`].

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use clackatoo_term::{Row, RowBuilder, Style, Underline};
use unicode_width::UnicodeWidthChar;
use winit::event::Ime;

/// How far apart a key and a commit of the same text can come and still be
/// one keypress sent twice. The second comes after the input method's
/// round trip, and both are timed when the window gets to them, which can
/// be a frame or two late. Two real keypresses never pair up in this time:
/// they'd have to come one each way, and a setup sends each key one way,
/// or both.
const ECHO: Duration = Duration::from_millis(200);

/// The most typed texts kept for pairing: far more than anyone types in
/// [`ECHO`], so the list can't grow without end.
const KEPT: usize = 32;

/// What the input method is doing, and what's been typed through it lately,
/// to decide what keys and commits type. The window drives it with the
/// events it gets, and does what it says.
#[derive(Debug, Default)]
pub(crate) struct InputMethod {
    /// Whether the window has let the input method compose.
    allowed: bool,
    /// What it's composing.
    preedit: Option<Preedit>,
    /// Text typed in the last [`ECHO`], oldest first, each waiting for the
    /// same text to come the other way, which is then dropped.
    typed: VecDeque<Typed>,
}

/// Text typed by a key or a commit, and when.
#[derive(Debug)]
struct Typed {
    text: String,
    by: Way,
    at: Instant,
}

/// Which way text came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    Key,
    Commit,
}

impl InputMethod {
    /// Whether the input method may compose.
    pub(crate) fn allowed(&self) -> bool {
        self.allowed
    }

    /// Lets the input method compose, or not, as `wanted` says: whether
    /// keys type text, and the option allows it. Returns the new setting
    /// if it changed, for the window to pass on. Turning it off drops what
    /// it was composing, which never reaches the pane.
    pub(crate) fn allow(&mut self, wanted: bool) -> Option<bool> {
        if wanted == self.allowed {
            return None;
        }
        self.allowed = wanted;
        if !wanted {
            self.reset();
        }
        Some(wanted)
    }

    /// What it's composing, if anything.
    pub(crate) fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// Whether a key that types `text`, if any, goes on to the window.
    /// While the input method composes, keys are its. A key that types
    /// what was just committed is that commit sent twice. Keys that only
    /// hold a modifier, such as Shift, needn't be passed.
    pub(crate) fn key(&mut self, text: Option<&str>, now: Instant) -> bool {
        if !self.allowed {
            return true;
        }
        if self.preedit.is_some() {
            return false;
        }
        match text.filter(|text| !text.is_empty()) {
            Some(text) => self.pair(Way::Key, text, now),
            None => true,
        }
    }

    /// Takes an event from the input method. Returns text it committed,
    /// to be typed, unless a key typed it already. While it isn't allowed,
    /// what it composes or commits is late, and dropped: in normal mode it
    /// would be taken for commands, or hold keys back for good.
    pub(crate) fn event(&mut self, event: Ime, now: Instant) -> Option<String> {
        match event {
            Ime::Enabled => None,
            Ime::Preedit(..) | Ime::Commit(_) if !self.allowed => None,
            Ime::Preedit(text, cursor) => {
                // An empty preedit ends composing, and comes before every
                // commit.
                if text.is_empty() {
                    self.preedit = None;
                } else {
                    self.typed.clear();
                    self.preedit = Some(Preedit {
                        text,
                        cursor: cursor.map(|(start, _)| start),
                    });
                }
                None
            }
            Ime::Commit(text) => {
                self.preedit = None;
                (!text.is_empty() && self.pair(Way::Commit, &text, now)).then_some(text)
            }
            Ime::Disabled => {
                self.reset();
                None
            }
        }
    }

    /// Drops what was being composed, as when the window loses the
    /// keyboard: the input method may never say it stopped, and keys
    /// would be held back for it until it did.
    pub(crate) fn reset(&mut self) {
        self.preedit = None;
        self.typed.clear();
    }

    /// Whether `text`, which just came `by` one way, is to be typed: not if
    /// it came the other way lately, which counts once, and is forgotten.
    /// Otherwise it's kept, for its twin to be dropped.
    fn pair(&mut self, by: Way, text: &str, now: Instant) -> bool {
        while (self.typed.front()).is_some_and(|old| now.duration_since(old.at) > ECHO) {
            self.typed.pop_front();
        }
        let twin = (self.typed.iter()).position(|old| old.by != by && old.text == text);
        if let Some(twin) = twin {
            self.typed.remove(twin);
            return false;
        }
        if self.typed.len() == KEPT {
            self.typed.pop_front();
        }
        self.typed.push_back(Typed {
            text: text.to_owned(),
            by,
            at: now,
        });
        true
    }
}

/// Text the input method is composing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Preedit {
    pub(crate) text: String,
    /// Where in `text` the input method's cursor is, in bytes, if it shows
    /// one.
    pub(crate) cursor: Option<usize>,
}

impl Preedit {
    /// `row` with this text over its cells from `col`, underlined, and the
    /// column the cursor goes in. Whatever the text covers is hidden, and a
    /// wide character it covers half of is blanked.
    pub(crate) fn over(&self, row: &Row, col: usize) -> (Row, usize) {
        let width = row.width();
        let mut builder = RowBuilder::new(width);
        let mut text = String::new();
        let mut filled = 0;
        let mut push_cells = |builder: &mut RowBuilder, from: usize, to: usize| {
            let mut col = from;
            while col < to.min(width) {
                let cell = row.cells()[col];
                let cells = if cell.is_wide() { 2 } else { 1 };
                text.clear();
                if cell.is_wide_spacer() || col + cells > to {
                    text.push(' ');
                    builder.push(&text, &Style::DEFAULT, true);
                    col += 1;
                    continue;
                }
                row.push_text(col, &mut text);
                builder.push(&text, row.style(cell), true);
                col += cells;
            }
        };
        let start = col.min(width);
        push_cells(&mut builder, 0, start);
        let underlined = Style {
            underline: Underline::Single,
            ..Style::DEFAULT
        };
        let mut cursor = None;
        for (index, ch) in self.text.char_indices() {
            if self.cursor == Some(index) {
                cursor = Some(start + filled);
            }
            let cells = ch.width().unwrap_or(0);
            if start + filled + cells > width {
                break;
            }
            let mut buffer = [0; 4];
            builder.push(ch.encode_utf8(&mut buffer), &underlined, false);
            filled += cells;
        }
        let end = start + filled;
        // The cell the text ends in may have been half of a wide one.
        let resume = if end < width && row.cells()[end].is_wide_spacer() {
            builder.push(" ", &Style::DEFAULT, true);
            end + 1
        } else {
            end
        };
        push_cells(&mut builder, resume, width);
        let cursor = cursor.unwrap_or(end).min(width.saturating_sub(1));
        (builder.finish(row.is_wrapped()), cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        let mut builder = RowBuilder::new(10);
        builder.push(text, &Style::DEFAULT, false);
        builder.finish(false)
    }

    fn preedit(text: &str, cursor: Option<usize>) -> Preedit {
        Preedit {
            text: text.to_owned(),
            cursor,
        }
    }

    #[test]
    fn the_text_covers_the_cells_at_the_cursor() {
        let (over, cursor) = preedit("ni", None).over(&row("$ abcdef"), 2);
        assert_eq!(over.text().trim_end(), "$ nicdef");
        assert_eq!(cursor, 4, "after the text, without a cursor of its own");
        let underlined = |col| over.style(over.cells()[col]).underline;
        assert_eq!(underlined(1), Underline::None);
        assert_eq!(underlined(2), Underline::Single);
        assert_eq!(underlined(4), Underline::None);
    }

    #[test]
    fn wide_text_takes_two_cells_and_half_covered_ones_are_blanked() {
        // 你 is cols 2-3 and 好 is 4-5; two cells from col 3 cover half of
        // each.
        let (over, cursor) = preedit("ab", Some(1)).over(&row("$ 你好x"), 3);
        assert_eq!(over.text().trim_end(), "$  ab x");
        assert_eq!(cursor, 4);
        let (over, _) = preedit("日本", None).over(&row("$ abcdef"), 2);
        assert_eq!(over.text().trim_end(), "$ 日本ef");
    }

    #[test]
    fn text_past_the_edge_is_cut_off() {
        let (over, cursor) = preedit("abcdef", None).over(&row("$"), 7);
        assert_eq!(over.text().trim_end(), "$      abc");
        assert_eq!(cursor, 9);
    }

    /// An input method that's allowed to compose, and the time it starts.
    fn allowed() -> (InputMethod, Instant) {
        let mut ime = InputMethod::default();
        assert_eq!(ime.allow(true), Some(true));
        (ime, Instant::now())
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn compose(text: &str) -> Ime {
        Ime::Preedit(text.to_owned(), Some((text.len(), text.len())))
    }

    fn commit(text: &str) -> Ime {
        Ime::Commit(text.to_owned())
    }

    /// The empty preedit every input method sends before its commit.
    fn done() -> Ime {
        Ime::Preedit(String::new(), None)
    }

    #[test]
    fn composed_text_is_held_then_committed() {
        let (mut ime, t) = allowed();
        assert_eq!(ime.event(compose("n"), t), None);
        assert_eq!(ime.event(compose("ni"), t), None);
        let shown = ime.preedit().expect("composing");
        assert_eq!((shown.text.as_str(), shown.cursor), ("ni", Some(2)));
        // Keys are the input method's while it composes.
        assert!(!ime.key(Some("h"), t));
        assert!(!ime.key(None, t), "nor an arrow");
        assert_eq!(ime.event(done(), t), None);
        assert_eq!(ime.event(commit("你"), t).as_deref(), Some("你"));
        assert!(ime.preedit().is_none());
        assert!(ime.key(Some("a"), t + ms(200)), "keys type again after");
    }

    #[test]
    fn a_commit_without_composing_is_typed() {
        // As from an emoji picker, or an input method that commits each
        // letter rather than letting its key through.
        let (mut ime, t) = allowed();
        assert_eq!(ime.event(done(), t), None);
        assert_eq!(ime.event(commit("👍"), t).as_deref(), Some("👍"));
        assert_eq!(ime.event(commit("a"), t + ms(100)).as_deref(), Some("a"));
        assert_eq!(ime.event(commit("a"), t + ms(110)).as_deref(), Some("a"));
        assert_eq!(ime.event(commit(""), t + ms(120)), None);
    }

    #[test]
    fn a_key_and_a_commit_of_the_same_text_type_it_once() {
        let (mut ime, t) = allowed();
        // The key first, then the commit...
        assert!(ime.key(Some("a"), t));
        assert_eq!(ime.event(done(), t + ms(5)), None);
        assert_eq!(ime.event(commit("a"), t + ms(5)), None);
        // ...or the commit first, then the key.
        let t = t + ms(500);
        assert_eq!(ime.event(commit("b"), t).as_deref(), Some("b"));
        assert!(!ime.key(Some("b"), t + ms(5)));
        // Each pair counts once, so a third is typed.
        assert!(ime.key(Some("b"), t + ms(10)));
        assert_eq!(ime.event(commit("b"), t + ms(12)), None);
        assert_eq!(ime.event(commit("b"), t + ms(14)).as_deref(), Some("b"));
    }

    #[test]
    fn different_or_far_apart_text_is_typed_both_ways() {
        let (mut ime, t) = allowed();
        assert!(ime.key(Some("a"), t));
        assert_eq!(ime.event(commit("b"), t + ms(1)).as_deref(), Some("b"));
        // The same text, but too long after to be one keypress.
        assert!(ime.key(Some("c"), t + ms(100)));
        assert_eq!(ime.event(commit("c"), t + ms(400)).as_deref(), Some("c"));
        // Composing in between starts something new.
        assert!(ime.key(Some("e"), t + ms(1000)));
        assert_eq!(ime.event(compose("x"), t + ms(1001)), None);
        assert_eq!(ime.event(commit("e"), t + ms(1002)).as_deref(), Some("e"));
    }

    #[test]
    fn keypresses_sent_twice_pair_up_however_they_interleave() {
        let (mut ime, t) = allowed();
        // Typed fast: both keys come before either commit.
        assert!(ime.key(Some("a"), t));
        assert!(ime.key(Some("b"), t + ms(20)));
        assert_eq!(ime.event(commit("a"), t + ms(40)), None);
        assert_eq!(ime.event(commit("b"), t + ms(60)), None);
        // A held key: each repeat sent twice still types once.
        let t = t + ms(1000);
        assert!(ime.key(Some("j"), t));
        assert!(ime.key(Some("j"), t + ms(30)));
        assert_eq!(ime.event(commit("j"), t + ms(35)), None);
        assert_eq!(ime.event(commit("j"), t + ms(65)), None);
        // A key that types nothing, such as an arrow, coming between a key
        // and its twin doesn't part them.
        let t = t + ms(1000);
        assert!(ime.key(Some("d"), t));
        assert!(ime.key(None, t + ms(1)));
        assert_eq!(ime.event(commit("d"), t + ms(2)), None);
        // Nor does the window being slow to get to the twin.
        let t = t + ms(1000);
        assert!(ime.key(Some("f"), t));
        assert_eq!(ime.event(commit("f"), t + ms(150)), None);
    }

    #[test]
    fn held_keys_repeat_whichever_way_they_come() {
        let (mut ime, t) = allowed();
        for n in 0..5 {
            assert!(ime.key(Some("j"), t + ms(n * 30)));
        }
        let t = t + ms(1000);
        for n in 0..5 {
            assert_eq!(ime.event(commit("j"), t + ms(n * 30)).as_deref(), Some("j"));
        }
        // However many come, only so many are kept.
        let t = t + ms(1000);
        for n in 0..100 {
            assert!(ime.key(Some("k"), t + ms(n)));
        }
        assert!(ime.typed.len() <= KEPT);
    }

    #[test]
    fn turning_it_off_drops_what_it_was_composing() {
        let (mut ime, t) = allowed();
        assert_eq!(ime.allow(true), None, "no change");
        assert_eq!(ime.event(compose("ni"), t), None);
        // Esc to normal mode: the window turns it off.
        assert_eq!(ime.allow(false), Some(false));
        assert!(!ime.allowed());
        assert!(ime.preedit().is_none());
        assert!(ime.key(Some("j"), t), "normal mode's keys go through");
        // What it composes or commits late, after Esc, is dropped: it
        // mustn't hold normal mode's keys back, or be taken for commands.
        assert_eq!(ime.event(compose("j"), t + ms(1)), None);
        assert!(ime.preedit().is_none());
        assert!(ime.key(Some("j"), t + ms(2)));
        assert_eq!(ime.event(commit("x"), t + ms(3)), None);
        // While it's off, keys aren't remembered, so a commit after it's
        // back on is typed.
        assert!(ime.key(Some("a"), t));
        assert_eq!(ime.allow(true), Some(true));
        assert_eq!(ime.event(commit("a"), t + ms(4)).as_deref(), Some("a"));
    }

    #[test]
    fn losing_the_keyboard_or_being_disabled_stops_composing() {
        let (mut ime, t) = allowed();
        assert_eq!(ime.event(compose("ni"), t), None);
        ime.reset();
        assert!(ime.preedit().is_none());
        assert!(ime.key(Some("a"), t));
        assert_eq!(ime.event(compose("ni"), t), None);
        assert_eq!(ime.event(Ime::Disabled, t), None);
        assert!(ime.preedit().is_none());
        assert!(ime.allowed(), "the window still lets it compose");
        assert_eq!(ime.event(Ime::Enabled, t), None);
    }
}
