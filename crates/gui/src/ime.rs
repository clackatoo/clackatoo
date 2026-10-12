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
//! key and a commit of the same text, one straight after the other, count
//! once. See [`InputMethod`].

use std::time::{Duration, Instant};

use clackatoo_term::{Row, RowBuilder, Style, Underline};
use unicode_width::UnicodeWidthChar;
use winit::event::Ime;

/// How far apart a key and a commit of the same text can come and still be
/// one keypress sent twice. The second comes after the input method's
/// round trip, a few milliseconds on a busy machine. A person can't type
/// one character twice this fast, and when a key is held, its repeats all
/// come the same way, so they're never taken for each other.
const ECHO: Duration = Duration::from_millis(50);

/// What the input method is doing, and what's been typed through it lately,
/// to decide what keys and commits type. The window drives it with the
/// events it gets, and does what it says.
#[derive(Debug, Default)]
pub(crate) struct InputMethod {
    /// Whether the window has let the input method compose.
    allowed: bool,
    /// What it's composing.
    preedit: Option<Preedit>,
    /// The last text typed, if nothing has happened since, to drop the
    /// same text coming the other way.
    last: Option<Typed>,
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
            self.preedit = None;
            self.last = None;
        }
        Some(wanted)
    }

    /// What it's composing, if anything.
    pub(crate) fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// Whether a key that types `text`, if any, goes on to the window.
    /// While the input method composes, keys are its. A key that types
    /// what was just committed is that commit sent twice.
    pub(crate) fn key(&mut self, text: Option<&str>, now: Instant) -> bool {
        if self.preedit.is_some() {
            return false;
        }
        if !self.allowed {
            return true;
        }
        let Some(text) = text.filter(|text| !text.is_empty()) else {
            // A key that types nothing, such as an arrow, comes between.
            self.last = None;
            return true;
        };
        if self.echoes(Way::Commit, text, now) {
            self.last = None;
            return false;
        }
        self.typed(Way::Key, text, now);
        true
    }

    /// Takes an event from the input method. Returns text it committed,
    /// to be typed, unless a key typed it already.
    pub(crate) fn event(&mut self, event: Ime, now: Instant) -> Option<String> {
        match event {
            Ime::Enabled => None,
            Ime::Preedit(text, cursor) => {
                // An empty preedit ends composing, and comes before every
                // commit: it isn't something happening in between.
                if text.is_empty() {
                    self.preedit = None;
                } else {
                    self.last = None;
                    self.preedit = Some(Preedit {
                        text,
                        cursor: cursor.map(|(start, _)| start),
                    });
                }
                None
            }
            Ime::Commit(text) => {
                self.preedit = None;
                if text.is_empty() {
                    return None;
                }
                if self.echoes(Way::Key, &text, now) {
                    self.last = None;
                    return None;
                }
                self.typed(Way::Commit, &text, now);
                Some(text)
            }
            Ime::Disabled => {
                self.preedit = None;
                self.last = None;
                None
            }
        }
    }

    /// Drops what was being composed, as when the window loses the
    /// keyboard: the input method may never say it stopped, and keys
    /// would be held back for it until it did.
    pub(crate) fn reset(&mut self) {
        self.preedit = None;
        self.last = None;
    }

    /// Whether `text` is what came `by` that way last, just now.
    fn echoes(&self, by: Way, text: &str, now: Instant) -> bool {
        self.last.as_ref().is_some_and(|last| {
            last.by == by && last.text == text && now.duration_since(last.at) <= ECHO
        })
    }

    fn typed(&mut self, by: Way, text: &str, at: Instant) {
        self.last = Some(Typed {
            text: text.to_owned(),
            by,
            at,
        });
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
        assert_eq!(ime.event(commit("c"), t + ms(200)).as_deref(), Some("c"));
        // Something in between: a key that types nothing...
        assert!(ime.key(Some("d"), t + ms(300)));
        assert!(ime.key(None, t + ms(301)));
        assert_eq!(ime.event(commit("d"), t + ms(302)).as_deref(), Some("d"));
        // ...or composing.
        assert!(ime.key(Some("e"), t + ms(400)));
        assert_eq!(ime.event(compose("x"), t + ms(401)), None);
        assert_eq!(ime.event(commit("e"), t + ms(402)).as_deref(), Some("e"));
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
        // While it's off, keys aren't remembered, so a commit that comes
        // late is typed.
        assert!(ime.key(Some("a"), t));
        assert_eq!(ime.allow(true), Some(true));
        assert_eq!(ime.event(commit("a"), t + ms(1)).as_deref(), Some("a"));
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
