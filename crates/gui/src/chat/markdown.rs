//! The agent's words, read as Markdown: blocks of text, each with the
//! styles of its pieces, ready to lay out.
//!
//! Only what a chat needs is kept: paragraphs, headings, lists, quotes,
//! code, rules and tables, with bold, italic, struck-through, code and
//! links inside them. Anything else is shown as its text.

use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One block of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Block {
    pub(super) kind: BlockKind,
    pub(super) text: String,
    /// The styles of its pieces. Text in no span is plain.
    pub(super) spans: Vec<Span>,
    /// How many lists or quotes it's inside, which indent it.
    pub(super) indent: u8,
}

/// What a block is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BlockKind {
    Paragraph,
    /// A heading, from 1, the largest.
    Heading(u8),
    /// Code, in the terminal's font, on a background of its own.
    Code,
    /// A list item's first block, after its marker: "•" or "1.".
    Item(String),
    /// Text in a quote, with a bar beside it.
    Quote,
    /// A line across.
    Rule,
}

/// A piece of a block's text and its style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) range: Range<usize>,
    pub(super) style: Style,
}

/// How a piece of text is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent styles, as in Markdown"
)]
pub(super) struct Style {
    pub(super) bold: bool,
    pub(super) italic: bool,
    pub(super) strike: bool,
    pub(super) code: bool,
    pub(super) link: bool,
}

/// The blocks `markdown` is made of.
pub(super) fn blocks(markdown: &str) -> Vec<Block> {
    let mut reader = Reader::default();
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(markdown, options) {
        reader.event(event);
    }
    reader.finish_block();
    reader.blocks
}

/// Reads events into blocks.
#[derive(Default)]
struct Reader {
    blocks: Vec<Block>,
    /// The block being read.
    open: Option<Block>,
    /// The lists it's in: the next item's number, for an ordered one.
    lists: Vec<Option<u64>>,
    /// The marker of an item whose first block hasn't started.
    marker: Option<String>,
    quotes: u8,
    /// How deep in each style the text is.
    bold: u8,
    italic: u8,
    strike: u8,
    link: u8,
}

impl Reader {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(&tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
                self.push(&text, false);
            }
            Event::Code(text) => self.push(&text, true),
            Event::SoftBreak => self.push(" ", false),
            Event::HardBreak => self.push("\n", false),
            Event::Rule => {
                self.finish_block();
                self.blocks.push(Block {
                    kind: BlockKind::Rule,
                    text: String::new(),
                    spans: Vec::new(),
                    indent: self.indent(),
                });
            }
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }, false),
            _ => {}
        }
    }

    fn start(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::TableHead | Tag::TableRow => self.begin(BlockKind::Paragraph),
            Tag::Heading { level, .. } => self.begin(BlockKind::Heading(heading(*level))),
            Tag::CodeBlock(_) => self.begin(BlockKind::Code),
            Tag::List(start) => {
                self.finish_block();
                self.lists.push(*start);
            }
            Tag::Item => {
                self.finish_block();
                let marker = match self.lists.last_mut() {
                    Some(Some(number)) => {
                        let marker = format!("{number}.");
                        *number += 1;
                        marker
                    }
                    _ => "•".to_owned(),
                };
                self.marker = Some(marker);
            }
            Tag::BlockQuote(_) => {
                self.finish_block();
                self.quotes += 1;
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link += 1,
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::Item
            | TagEnd::TableHead
            | TagEnd::TableRow => self.finish_block(),
            TagEnd::CodeBlock => {
                if let Some(block) = &mut self.open {
                    let end = block.text.trim_end_matches('\n').len();
                    block.text.truncate(end);
                    for span in &mut block.spans {
                        span.range.end = span.range.end.min(end);
                    }
                }
                self.finish_block();
            }
            TagEnd::TableCell => self.push("   ", false),
            TagEnd::List(_) => {
                self.finish_block();
                self.lists.pop();
            }
            TagEnd::BlockQuote(_) => {
                self.finish_block();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            _ => {}
        }
    }

    /// How far blocks are indented here.
    fn indent(&self) -> u8 {
        u8::try_from(self.lists.len().saturating_sub(1)).unwrap_or(u8::MAX) + self.quotes
    }

    /// Starts a block of `kind`: an item's first block, if one is waiting.
    fn begin(&mut self, kind: BlockKind) {
        self.finish_block();
        let in_item = !self.lists.is_empty();
        let kind = match (self.marker.take(), kind) {
            (Some(marker), BlockKind::Paragraph) => BlockKind::Item(marker),
            (_, BlockKind::Paragraph) if self.quotes > 0 => BlockKind::Quote,
            (_, kind) => kind,
        };
        // A list's later blocks line up with its items' text.
        let indent = self.indent() + u8::from(in_item && !matches!(kind, BlockKind::Item(_)));
        self.open = Some(Block {
            kind,
            text: String::new(),
            spans: Vec::new(),
            indent,
        });
    }

    /// Adds text to the open block, opening one if there's none, as for a
    /// list item's text that isn't in a paragraph.
    fn push(&mut self, text: &str, code: bool) {
        if self.open.is_none() {
            self.begin(BlockKind::Paragraph);
        }
        let style = Style {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            code,
            link: self.link > 0,
        };
        let block = self.open.as_mut().expect("opened above");
        let start = block.text.len();
        block.text.push_str(text);
        let end = block.text.len();
        if style == Style::default() {
            return;
        }
        // Next to a piece in the same style: one piece.
        match block.spans.last_mut() {
            Some(span) if span.range.end == start && span.style == style => span.range.end = end,
            _ => block.spans.push(Span {
                range: start..end,
                style,
            }),
        }
    }

    fn finish_block(&mut self) {
        if let Some(block) = self.open.take()
            && (!block.text.trim().is_empty() || block.kind == BlockKind::Code)
        {
            self.blocks.push(block);
        }
    }
}

fn heading(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(markdown: &str) -> Vec<(BlockKind, String, u8)> {
        blocks(markdown)
            .into_iter()
            .map(|block| (block.kind, block.text, block.indent))
            .collect()
    }

    #[test]
    fn paragraphs_headings_and_code_are_blocks() {
        assert_eq!(
            kinds("# Title\n\nSome text\nwrapped.\n\n```rust\nfn main() {}\n```\n\n---"),
            [
                (BlockKind::Heading(1), "Title".into(), 0),
                (BlockKind::Paragraph, "Some text wrapped.".into(), 0),
                (BlockKind::Code, "fn main() {}".into(), 0),
                (BlockKind::Rule, String::new(), 0),
            ]
        );
    }

    #[test]
    fn list_items_get_markers_and_nest() {
        assert_eq!(
            kinds("- one\n- two\n  - deeper\n\n3. three\n4. four"),
            [
                (BlockKind::Item("•".into()), "one".into(), 0),
                (BlockKind::Item("•".into()), "two".into(), 0),
                (BlockKind::Item("•".into()), "deeper".into(), 1),
                (BlockKind::Item("3.".into()), "three".into(), 0),
                (BlockKind::Item("4.".into()), "four".into(), 0),
            ]
        );
    }

    #[test]
    fn inline_styles_are_spans() {
        let blocks = blocks("Run **`cargo test`** and *see*, or [docs](https://x).");
        let block = &blocks[0];
        assert_eq!(block.text, "Run cargo test and see, or docs.");
        let styled: Vec<(&str, Style)> = (block.spans.iter())
            .map(|span| (&block.text[span.range.clone()], span.style))
            .collect();
        assert_eq!(
            styled,
            [
                (
                    "cargo test",
                    Style {
                        bold: true,
                        code: true,
                        ..Style::default()
                    }
                ),
                (
                    "see",
                    Style {
                        italic: true,
                        ..Style::default()
                    }
                ),
                (
                    "docs",
                    Style {
                        link: true,
                        ..Style::default()
                    }
                ),
            ]
        );
    }

    #[test]
    fn half_written_markdown_still_reads() {
        // As it streams in: an unclosed fence, and bold not yet ended.
        assert_eq!(
            kinds("Look:\n\n```\nlet x"),
            [
                (BlockKind::Paragraph, "Look:".into(), 0),
                (BlockKind::Code, "let x".into(), 0),
            ]
        );
        assert_eq!(kinds("**bold so f")[0].1, "**bold so f");
    }
}
