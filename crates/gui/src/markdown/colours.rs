//! The colours of the code in the agent's words, as its language's
//! grammar reads it. The grammars are the daemon's, so a chat asks it, as
//! an editor's hover does: for each block of code it lays out whose fence
//! names a language and is closed, once, and lays the block out again when
//! the colours come. Until then, and for good if the language isn't
//! installed, it's plain.
//!
//! Nothing is asked while the code is still coming, as each word of it
//! would ask again, and nothing twice: what's known is kept by the code's
//! text, so the same block in another turn, or laid out again, costs
//! nothing.

use std::collections::{HashMap, HashSet};

use clackatoo_protocol::{Code, Span};

/// The most blocks one ask may hold, as the daemon colours no more.
const MOST_BLOCKS: usize = 64;

/// The most bytes of code one ask may hold, as the daemon colours no more.
const MOST_BYTES: usize = 256 * 1024;

/// The longest block asked about: longer, it stays plain.
const LONGEST: usize = 64 * 1024;

/// How many blocks' colours are kept before they're all let go, to be
/// asked for again as they're needed.
const MOST_KEPT: usize = 2048;

/// What's known of a block's colours.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Colouring<'a> {
    /// Its spans, as bytes of its text: none if its language has none.
    Known(&'a [Span]),
    /// Asked for, or about to be.
    Waiting,
    /// Never asked for, as it's too long.
    Plain,
}

/// The colours of a chat's code.
#[derive(Debug, Default)]
pub(crate) struct Colours {
    known: HashMap<Code, Vec<Span>>,
    /// Blocks asked for but not yet answered, or about to be.
    waiting: HashSet<Code>,
    /// Blocks to ask for.
    wanted: Vec<Code>,
    /// Asks not yet answered, by their number.
    asked: HashMap<u64, Vec<Code>>,
}

impl Colours {
    /// What's known of the colours of `text`, code in `language`: a block
    /// not asked about yet is wanted.
    pub(crate) fn of(&mut self, language: &str, text: &str) -> Colouring<'_> {
        if text.len() > LONGEST || text.trim().is_empty() {
            return Colouring::Plain;
        }
        let code = Code {
            language: language.to_owned(),
            text: text.to_owned(),
        };
        if self.known.contains_key(&code) {
            return Colouring::Known(&self.known[&code]);
        }
        if self.waiting.insert(code.clone()) {
            self.wanted.push(code);
        }
        Colouring::Waiting
    }

    /// The blocks to ask for, in asks no bigger than the daemon colours.
    pub(crate) fn take_wanted(&mut self) -> Vec<Vec<Code>> {
        let mut asks: Vec<Vec<Code>> = Vec::new();
        let mut bytes = 0;
        for code in std::mem::take(&mut self.wanted) {
            let full = asks
                .last()
                .is_none_or(|ask| ask.len() == MOST_BLOCKS || bytes + code.text.len() > MOST_BYTES);
            if full {
                asks.push(Vec::new());
                bytes = 0;
            }
            bytes += code.text.len();
            asks.last_mut().expect("pushed above").push(code);
        }
        asks
    }

    /// `code` was asked for, numbered `id`.
    pub(crate) fn asked(&mut self, id: u64, code: Vec<Code>) {
        self.asked.insert(id, code);
    }

    /// `code` can't be asked for, as the daemon doesn't colour chats'
    /// code: it's plain.
    pub(crate) fn plain(&mut self, code: Vec<Code>) {
        for code in code {
            self.know(code, Vec::new());
        }
    }

    /// The daemon's answer to the ask numbered `id`: each block's spans.
    /// Spans that don't fit their block's text are left out. Returns
    /// whether it was this chat's ask.
    pub(crate) fn answered(&mut self, id: u64, spans: Vec<Vec<Span>>) -> bool {
        let Some(code) = self.asked.remove(&id) else {
            return false;
        };
        let mut spans = spans.into_iter();
        for code in code {
            let text = &code.text;
            let fits = |span: &Span| {
                span.start < span.end
                    && span.end <= text.len()
                    && text.is_char_boundary(span.start)
                    && text.is_char_boundary(span.end)
            };
            let spans = spans.next().unwrap_or_default();
            let spans = spans.into_iter().filter(fits).collect();
            self.know(code, spans);
        }
        true
    }

    /// Forgets what was asked and not answered, as the daemon's gone out
    /// of reach and won't answer it: it's asked for again as it's laid
    /// out.
    pub(crate) fn forget_asked(&mut self) {
        self.asked.clear();
        self.wanted.clear();
        self.waiting.clear();
    }

    fn know(&mut self, code: Code, spans: Vec<Span>) {
        if self.known.len() >= MOST_KEPT {
            self.known.clear();
        }
        self.waiting.remove(&code);
        self.known.insert(code, spans);
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::Kind;

    use super::*;

    fn span(start: usize, end: usize) -> Span {
        Span {
            start,
            end,
            kind: Kind::Keyword,
        }
    }

    #[test]
    fn a_block_is_asked_for_once_and_known_once_answered() {
        let mut colours = Colours::default();
        assert_eq!(colours.of("rust", "fn x() {}"), Colouring::Waiting);
        assert_eq!(colours.of("rust", "fn x() {}"), Colouring::Waiting);
        // The same text in another language is another block.
        assert_eq!(colours.of("c", "fn x() {}"), Colouring::Waiting);
        let asks = colours.take_wanted();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].len(), 2);
        assert!(colours.take_wanted().is_empty(), "asked once");
        colours.asked(7, asks[0].clone());
        assert_eq!(colours.of("rust", "fn x() {}"), Colouring::Waiting);
        assert!(colours.take_wanted().is_empty(), "not again while asked");
        // Answered: what fits its text is kept, and nothing else.
        assert!(!colours.answered(8, Vec::new()), "not this chat's");
        let spans = vec![vec![span(0, 2), span(5, 99), span(3, 3)], Vec::new()];
        assert!(colours.answered(7, spans));
        assert_eq!(
            colours.of("rust", "fn x() {}"),
            Colouring::Known(&[span(0, 2)])
        );
        assert_eq!(colours.of("c", "fn x() {}"), Colouring::Known(&[]));
    }

    #[test]
    fn asks_are_no_bigger_than_the_daemon_colours_and_long_blocks_stay_plain() {
        let mut colours = Colours::default();
        for n in 0..(MOST_BLOCKS + 3) {
            colours.of("rust", &format!("let x = {n};"));
        }
        let asks = colours.take_wanted();
        assert_eq!(
            asks.iter().map(Vec::len).collect::<Vec<_>>(),
            [MOST_BLOCKS, 3]
        );
        for n in 0..5 {
            colours.of("rust", &format!("{n}{}", "x".repeat(LONGEST - 1)));
        }
        let asks = colours.take_wanted();
        assert!(
            asks.iter()
                .all(|ask| { ask.iter().map(|code| code.text.len()).sum::<usize>() <= MOST_BYTES })
        );
        assert_eq!(asks.iter().map(Vec::len).sum::<usize>(), 5);
        assert_eq!(
            colours.of("rust", &"x".repeat(LONGEST + 1)),
            Colouring::Plain
        );
    }

    #[test]
    fn what_wasnt_answered_is_asked_for_again_once_the_daemon_is_back() {
        let mut colours = Colours::default();
        colours.of("rust", "fn x() {}");
        let asks = colours.take_wanted();
        colours.asked(1, asks[0].clone());
        colours.forget_asked();
        assert!(!colours.answered(1, vec![vec![span(0, 2)]]), "too late");
        assert_eq!(colours.of("rust", "fn x() {}"), Colouring::Waiting);
        assert_eq!(colours.take_wanted().len(), 1);
        // A daemon that doesn't colour chats' code: plain, and not asked.
        colours.plain(vec![Code {
            language: "rust".into(),
            text: "fn x() {}".into(),
        }]);
        assert_eq!(colours.of("rust", "fn x() {}"), Colouring::Known(&[]));
    }
}
