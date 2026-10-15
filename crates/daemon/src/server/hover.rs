//! Colouring the code in what a language server says is at a place, as
//! `K` shows it: each fenced block of its markdown, in the language it's
//! tagged with, or the buffer's own if it has no tag.
//!
//! The answer goes to the window at once, plain, and the blocks to the
//! thread parsing buffers, which has the grammars; its colours follow, as
//! a [`BufferUpdate::Coloured`], once it's read them. Code in a language
//! that isn't installed, or tagged with one clackatoo doesn't know, stays
//! plain, and nothing is offered to install.
//!
//! The code in agents' words is coloured the same way, for the windows
//! that lay chats out: a window sends the blocks it shows, already read
//! out of the Markdown, in a [`ClientMessage::Colour`], and their colours
//! go back in an [`Event::Coloured`].
//!
//! [`ClientMessage::Colour`]: clackatoo_protocol::ClientMessage::Colour

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use clackatoo_protocol::{Answer, BufferId, BufferUpdate, Code, wire};
use clackatoo_syntax::{Coloured, Pack, Snippet};
use clackatoo_text::Span;
use mio::Token;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

use super::{Daemon, Source};

/// The most blocks of one hover coloured: more than any server writes,
/// and few enough that a hover of thousands can't hold up the thread
/// parsing every buffer. Those after are left plain.
const MAX_SNIPPETS: usize = 64;

/// The most bytes of code one window's ask has coloured, all its blocks
/// together: more than a chat shows at once, and few enough that a window
/// can't hold up the thread parsing every buffer. Blocks past it, or past
/// [`MAX_SNIPPETS`], are left plain.
const MAX_CODE: usize = 256 * 1024;

/// The most of one window's asks about its chats' code the thread parsing
/// buffers has at once: the rest wait their turn, so a window can't fill
/// the thread's queue ahead of the buffers it parses.
const MOST_ASKS: usize = 3;

/// The most of one window's asks that wait their turn: more than a
/// window that's working right ever has, as it asks for each block once.
/// Past it, an ask is dropped.
const MOST_WAITING: usize = 256;

/// The hovers whose code the thread parsing buffers is colouring, and the
/// chats' code, by the number it answers with; and the asks about chats'
/// code waiting their turn, by whose they are.
#[derive(Debug, Default)]
pub(super) struct Colourings {
    asked: HashMap<u64, Colouring>,
    code: HashMap<u64, CodeColouring>,
    waiting: HashMap<Token, VecDeque<Ask>>,
    next_ticket: u64,
}

/// A window's ask about its chats' code: its number, and the blocks.
type Ask = (u64, Vec<Code>);

/// A window's chats' code being coloured: whose it is, and which of the
/// blocks it asked about went to be coloured, by where each is among
/// those that went.
#[derive(Debug)]
struct CodeColouring {
    token: Token,
    /// The window's number for its ask.
    id: u64,
    sent: Vec<Option<usize>>,
}

/// A hover being coloured: whose it is, and where each snippet's text is
/// in its own.
#[derive(Debug)]
struct Colouring {
    token: Token,
    buffer: BufferId,
    /// The client's number for the query it answered.
    id: u64,
    /// For each snippet, in order, its pieces.
    pieces: Vec<Vec<Piece>>,
}

/// A stretch of a snippet's text, and where it is in the hover's: the
/// lines of a block indented in a list are apart there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Piece {
    /// Where it starts in the snippet.
    at: usize,
    /// Where it starts in the hover's text.
    from: usize,
    len: usize,
}

/// A fenced or indented block of code in markdown: its language's tag,
/// if it has one, its code, and where each piece of the code is in the
/// markdown.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    tag: Option<String>,
    code: String,
    pieces: Vec<Piece>,
}

impl Daemon {
    /// The code in `answer`, if it's what buffer `buffer`'s language
    /// server said is at a place, in markdown, to colour for the client at
    /// `token`: each block as a snippet in its language, with where its
    /// pieces are in the answer's text. None for a client too old to be
    /// told its colours.
    pub(super) fn hover_code(
        &mut self,
        token: Token,
        buffer: BufferId,
        answer: &Answer,
    ) -> Vec<(Snippet, Vec<Piece>)> {
        let Answer::Hover {
            text,
            markdown: true,
            ..
        } = answer
        else {
            return Vec::new();
        };
        let wire = match self.sources.get(token.0) {
            Some(Source::Client(client)) => client.wire,
            _ => return Vec::new(),
        };
        let (Some(languages), Some(slot)) = (&mut self.languages, self.buffers.get(&buffer)) else {
            return Vec::new();
        };
        if !wire::daemon_has(wire, wire::HOVER) {
            return Vec::new();
        }
        snippets(text, |tag| match tag {
            Some(tag) => languages.named(tag),
            None => languages.find(slot.buffer.path()),
        })
    }

    /// Has `code`, from [`hover_code`](Self::hover_code), coloured, for
    /// the client at `token`'s query numbered `id` about `buffer`, starting
    /// the thread parsing buffers if it's the first need of it.
    pub(super) fn colour_hover(
        &mut self,
        (token, buffer, id): (Token, BufferId, u64),
        code: Vec<(Snippet, Vec<Piece>)>,
    ) {
        self.start_syntax();
        let Some(syntax) = self.syntax.as_ref().filter(|syntax| !syntax.stopped()) else {
            return;
        };
        let (snippets, pieces) = code.into_iter().unzip();
        let colourings = &mut self.colourings;
        let ticket = colourings.next_ticket;
        colourings.next_ticket += 1;
        syntax.colour(ticket, snippets);
        let colouring = Colouring {
            token,
            buffer,
            id,
            pieces,
        };
        colourings.asked.insert(ticket, colouring);
    }

    /// Sends the client whose hover it is the colours the thread parsing
    /// buffers found for its code, as places in the hover's text, if it
    /// found any and the client still follows the buffer.
    pub(super) fn coloured(&mut self, coloured: &Coloured) {
        if let Some(colouring) = self.colourings.code.remove(&coloured.ticket) {
            self.code_coloured(&colouring, &coloured.spans);
            return self.next_code_colourings(colouring.token);
        }
        let Some(colouring) = self.colourings.asked.remove(&coloured.ticket) else {
            return;
        };
        let spans: Vec<Span> = (coloured.spans.iter().zip(&colouring.pieces))
            .flat_map(|(spans, pieces)| placed(spans, pieces))
            .collect();
        let following = (self.buffers.get(&colouring.buffer))
            .is_some_and(|slot| slot.viewers.contains(&colouring.token));
        if spans.is_empty() || !following {
            return;
        }
        let update = BufferUpdate::Coloured {
            id: colouring.id,
            spans,
        };
        self.tell(colouring.token, colouring.buffer, update);
    }

    /// Forgets the hovers being coloured for the client at `token` about
    /// `buffer`, or any buffer, as it's asked about another, stopped
    /// following the buffer, or gone; and with no buffer, its chats' code.
    pub(super) fn forget_colourings(&mut self, token: Token, buffer: Option<BufferId>) {
        (self.colourings.asked).retain(|_, colouring| {
            colouring.token != token || buffer.is_some_and(|buffer| buffer != colouring.buffer)
        });
        if buffer.is_none() {
            (self.colourings.code).retain(|_, colouring| colouring.token != token);
            self.colourings.waiting.remove(&token);
        }
    }

    /// Has the client at `token`'s blocks of code, `code`, from the chats
    /// it shows, coloured, for its ask numbered `id`: at once, or once
    /// fewer than [`MOST_ASKS`] of its asks are being coloured. A device on
    /// the web app has no grammars to ask for, and is refused.
    pub(super) fn colour_code(&mut self, token: Token, id: u64, code: Vec<Code>) {
        match self.sources.get(token.0) {
            Some(Source::Client(client)) if !matches!(client.link, super::Link::Web(_)) => {}
            _ => {
                log::warn!("chats' code is coloured only for windows");
                return;
            }
        }
        let colourings = &mut self.colourings;
        let busy = (colourings.code.values())
            .filter(|colouring| colouring.token == token)
            .count();
        if busy < MOST_ASKS {
            return self.start_code_colouring(token, id, code);
        }
        let waiting = colourings.waiting.entry(token).or_default();
        if waiting.len() < MOST_WAITING {
            waiting.push_back((id, code));
        } else {
            log::warn!("a window asked to colour more code than any needs: an ask is dropped");
        }
    }

    /// Starts the asks of the client at `token` that waited, while fewer
    /// than [`MOST_ASKS`] of its are being coloured.
    fn next_code_colourings(&mut self, token: Token) {
        loop {
            let colourings = &mut self.colourings;
            let busy = (colourings.code.values())
                .filter(|colouring| colouring.token == token)
                .count();
            if busy >= MOST_ASKS {
                return;
            }
            let Some(waiting) = colourings.waiting.get_mut(&token) else {
                return;
            };
            let Some((id, code)) = waiting.pop_front() else {
                colourings.waiting.remove(&token);
                return;
            };
            self.start_code_colouring(token, id, code);
        }
    }

    /// Has `code` coloured for the client at `token`'s ask numbered `id`:
    /// each block in the language its fence names, if it's installed, as
    /// many as [`MAX_SNIPPETS`] and [`MAX_CODE`] allow, every block
    /// counting, and those past them left plain without being looked up.
    /// The answer is always sent, at once with no colours if none can be
    /// found, so the window never waits for ever.
    fn start_code_colouring(&mut self, token: Token, id: u64, code: Vec<Code>) {
        let mut snippets = Vec::new();
        let mut sent = Vec::with_capacity(code.len());
        let mut bytes = 0;
        if let Some(languages) = &mut self.languages {
            // The directory is read again once for the ask, not for each
            // block.
            languages.refresh();
            let mut packs: HashMap<String, Option<Arc<Pack>>> = HashMap::new();
            for (index, block) in code.into_iter().enumerate() {
                bytes += block.text.len();
                let room = index < MAX_SNIPPETS && bytes <= MAX_CODE;
                let pack = if room && !block.text.trim().is_empty() {
                    let tag = block.language.to_ascii_lowercase();
                    let pack =
                        (packs.entry(tag)).or_insert_with_key(|tag| languages.named_as_read(tag));
                    pack.clone()
                } else {
                    None
                };
                sent.push(pack.map(|pack| {
                    snippets.push(Snippet {
                        pack,
                        text: block.text,
                    });
                    snippets.len() - 1
                }));
            }
        } else {
            sent.resize(code.len(), None);
        }
        let colouring = CodeColouring { token, id, sent };
        if snippets.is_empty() {
            return self.code_coloured(&colouring, &[]);
        }
        self.start_syntax();
        let Some(syntax) = self.syntax.as_ref().filter(|syntax| !syntax.stopped()) else {
            return self.code_coloured(&colouring, &[]);
        };
        let colourings = &mut self.colourings;
        let ticket = colourings.next_ticket;
        colourings.next_ticket += 1;
        syntax.colour(ticket, snippets);
        colourings.code.insert(ticket, colouring);
    }

    /// Sends the window whose chats' code `colouring` is the colours of
    /// the blocks that went to be coloured, `spans`, and none for the
    /// rest.
    fn code_coloured(&mut self, colouring: &CodeColouring, spans: &[Vec<Span>]) {
        let spans = (colouring.sent.iter())
            .map(|sent| {
                sent.and_then(|at| spans.get(at).cloned())
                    .unwrap_or_default()
            })
            .collect();
        let event = clackatoo_protocol::Event::Coloured {
            id: colouring.id,
            spans,
        };
        self.send_event(colouring.token, event);
    }

    /// Forgets every hover being coloured, as the thread parsing buffers
    /// has stopped: they stay plain.
    pub(super) fn forget_all_colourings(&mut self) {
        self.colourings.asked.clear();
        // The windows asking about chats' code are told there's none to be
        // had, rather than left to wait.
        let code: Vec<CodeColouring> = self.colourings.code.drain().map(|(_, code)| code).collect();
        for colouring in code {
            self.code_coloured(&colouring, &[]);
        }
        let waiting: Vec<(Token, VecDeque<Ask>)> = self.colourings.waiting.drain().collect();
        for (token, asks) in waiting {
            for (id, code) in asks {
                let sent = vec![None; code.len()];
                self.code_coloured(&CodeColouring { token, id, sent }, &[]);
            }
        }
    }
}

/// The blocks of code in `markdown` to colour, each as a snippet in the
/// language `pack_for` says its tag, or no tag, means, with where its
/// pieces are in `markdown`: no more than [`MAX_SNIPPETS`], and none of
/// blocks with nothing in them or in a language with no pack. Each tag is
/// asked about once, as `pack_for` reads the packs' directory.
fn snippets(
    markdown: &str,
    mut pack_for: impl FnMut(Option<&str>) -> Option<Arc<Pack>>,
) -> Vec<(Snippet, Vec<Piece>)> {
    let mut packs: HashMap<Option<String>, Option<Arc<Pack>>> = HashMap::new();
    let mut snippets = Vec::new();
    for block in blocks(markdown) {
        if snippets.len() == MAX_SNIPPETS {
            break;
        }
        if block.code.trim().is_empty() {
            continue;
        }
        let tag = block.tag.map(|tag| tag.to_ascii_lowercase());
        let pack = (packs.entry(tag)).or_insert_with_key(|tag| pack_for(tag.as_deref()));
        if let Some(pack) = pack.clone() {
            let snippet = Snippet {
                pack,
                text: block.code,
            };
            snippets.push((snippet, block.pieces));
        }
    }
    snippets
}

/// The blocks of code in `markdown`, as the window's panel reads it: with
/// the same parser and options, so the places of their pieces are where
/// the window looks for them.
fn blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut reading: Option<Block> = None;
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES;
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let tag = match kind {
                    CodeBlockKind::Fenced(info) => tag(&info),
                    CodeBlockKind::Indented => None,
                };
                reading = Some(Block {
                    tag,
                    code: String::new(),
                    pieces: Vec::new(),
                });
            }
            Event::Text(text) => {
                let Some(block) = &mut reading else { continue };
                // A piece whose text isn't the markdown's own, as a line
                // break made plain may not be, is read but not coloured.
                if markdown.get(range.clone()) == Some(&*text) {
                    block.pieces.push(Piece {
                        at: block.code.len(),
                        from: range.start,
                        len: text.len(),
                    });
                }
                block.code.push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) => blocks.extend(reading.take()),
            _ => {}
        }
    }
    blocks
}

/// The language a fenced block's info string names: its first word, such
/// as `rust` in ```` ```rust ```` or `rust,ignore`, as rustdoc writes, or
/// `None` if it names none.
fn tag(info: &str) -> Option<String> {
    let word = info
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '{')
        .next()
        .unwrap_or_default();
    (!word.is_empty()).then(|| word.to_owned())
}

/// `spans`, as bytes of a snippet whose pieces are `pieces`, as bytes of
/// the markdown it's from: a span over pieces apart is cut into one for
/// each, and one of text that isn't the markdown's own is left out.
fn placed<'a>(spans: &'a [Span], pieces: &'a [Piece]) -> impl Iterator<Item = Span> + 'a {
    spans.iter().flat_map(move |span| {
        let within = |piece: &&Piece| piece.at < span.end && span.start < piece.at + piece.len;
        pieces.iter().filter(within).map(move |piece| {
            let Range { start, end } = span.start.max(piece.at)..span.end.min(piece.at + piece.len);
            Span {
                start: piece.from + start - piece.at,
                end: piece.from + end - piece.at,
                kind: span.kind,
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use clackatoo_text::Kind;

    use super::*;

    #[test]
    fn blocks_of_code_are_found_with_their_tags_and_places() {
        let markdown = "# f\n\n```rust,ignore\nfn f()\n```\n\n    indented\n\n- in a list:\n\n  ```\n  a\n  b\n  ```\n";
        let found = blocks(markdown);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[0].tag.as_deref(), Some("rust"));
        assert_eq!(found[0].code, "fn f()\n");
        // The pieces are where the code is in the markdown.
        for block in &found {
            for piece in &block.pieces {
                assert_eq!(
                    &markdown[piece.from..piece.from + piece.len],
                    &block.code[piece.at..piece.at + piece.len]
                );
            }
        }
        // An indented block has no tag; one in a list is in pieces apart,
        // its indent left out.
        assert_eq!(
            (found[1].tag.as_deref(), found[1].code.as_str()),
            (None, "indented\n")
        );
        assert_eq!(
            (found[2].tag.as_deref(), found[2].code.as_str()),
            (None, "a\nb\n")
        );
        assert!(found[2].pieces.len() > 1, "{:?}", found[2].pieces);
    }

    #[test]
    fn a_hovers_code_is_looked_up_once_a_tag_and_coloured_only_so_far() {
        let pack = |name: &str| {
            Arc::new(Pack {
                name: name.to_owned(),
                dir: std::path::PathBuf::from(name),
                grammar: name.to_owned(),
                extensions: Vec::new(),
                filenames: Vec::new(),
                server: None,
                word: None,
            })
        };
        // Empty blocks, and one in a language with no pack, are passed
        // over; then a hostile number of blocks, of which only the first
        // are coloured.
        let mut markdown =
            "```json\n```\n\n```\n  \n```\n\n```nothing\nx\n```\n\n```\nown\n```\n\n".to_owned();
        for n in 0..1000 {
            write!(markdown, "```JSON\n[{n}]\n```\n\n").unwrap();
        }
        let mut asked = Vec::new();
        let found = snippets(&markdown, |tag| {
            asked.push(tag.map(str::to_owned));
            match tag {
                Some("json") => Some(pack("json")),
                None => Some(pack("own")),
                Some(_) => None,
            }
        });
        assert_eq!(found.len(), MAX_SNIPPETS);
        assert_eq!(
            (found[0].0.pack.name.as_str(), found[0].0.text.as_str()),
            ("own", "own\n")
        );
        assert_eq!(
            (found[1].0.pack.name.as_str(), found[1].0.text.as_str()),
            ("json", "[0]\n")
        );
        // Each snippet keeps its own pieces.
        let last = &found[MAX_SNIPPETS - 1];
        let at = markdown.find(&format!("[{}]", MAX_SNIPPETS - 2)).unwrap();
        assert_eq!(last.1[0].from, at);
        // Each tag, case aside, was asked about once.
        asked.sort();
        assert_eq!(
            asked,
            [None, Some("json".to_owned()), Some("nothing".to_owned())]
        );
    }

    #[test]
    fn a_fenced_blocks_tag_is_its_first_word() {
        assert_eq!(tag("rust").as_deref(), Some("rust"));
        assert_eq!(tag("rust,no_run").as_deref(), Some("rust"));
        assert_eq!(tag("python title=\"a.py\"").as_deref(), Some("python"));
        assert_eq!(tag("{.lua}"), None);
        assert_eq!(tag(""), None);
    }

    #[test]
    fn spans_are_placed_in_the_markdown_cut_where_pieces_are_apart() {
        let pieces = [
            Piece {
                at: 0,
                from: 10,
                len: 4,
            },
            Piece {
                at: 4,
                from: 20,
                len: 4,
            },
        ];
        let span = |start, end| Span {
            start,
            end,
            kind: Kind::String,
        };
        let placed: Vec<Span> = placed(&[span(1, 3), span(2, 6)], &pieces).collect();
        assert_eq!(placed, [span(11, 13), span(12, 14), span(20, 22)]);
    }
}
