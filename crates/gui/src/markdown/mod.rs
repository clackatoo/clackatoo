//! Markdown, read: blocks of text, each with the styles of its pieces,
//! ready to lay out (see [`layout`]), as a chat shows its agent's words
//! and an editor its rendered view of a Markdown file.
//!
//! Only what prose needs is kept: paragraphs, headings, lists, quotes,
//! code, rules and tables, with bold, italic, struck-through, code and
//! links inside them. Anything else is shown as its text. A table is a
//! block for each cell, which says where in the table it goes.

pub(crate) mod colours;
pub(crate) mod layout;
pub(crate) mod tables;

use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// One block of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Block {
    pub(crate) kind: BlockKind,
    pub(crate) text: String,
    /// The styles of its pieces. Text in no span is plain.
    pub(crate) spans: Vec<Span>,
    /// How many lists or quotes it's inside, which indent it.
    pub(crate) indent: u8,
    /// Where it starts in the Markdown, in bytes: at its marks, such as a
    /// heading's `#`s, a list item's marker or code's opening fence.
    pub(crate) at: usize,
    /// Its links, and its images, which go to the image.
    pub(crate) links: Vec<Link>,
    /// It's the first item of a list.
    pub(crate) starts_list: bool,
}

/// A link in a block's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    /// Where it is in the block's text.
    pub(crate) range: Range<usize>,
    /// Where it goes, as written: a URL, a file, or `#` and a heading.
    pub(crate) to: String,
}

/// What a block is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    Paragraph,
    /// A heading, from 1, the largest.
    Heading(u8),
    /// Code, in the terminal's font, on a background of its own, and the
    /// language to colour it as: the one its fence names, once the fence
    /// is closed. Code still coming isn't coloured.
    Code(Option<String>),
    /// A list item's first block, after its marker: "•" or "1.".
    Item(String),
    /// Text in a quote, with a bar beside it.
    Quote,
    /// A line across.
    Rule,
    /// A cell of a table.
    Cell(Cell),
}

/// Where a cell goes in its table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Cell {
    /// Which table of the text it's in, from 0.
    pub(crate) table: usize,
    /// Its row, from 0, the table's head.
    pub(crate) row: usize,
    pub(crate) column: usize,
    /// How its column lines its cells up.
    pub(crate) align: Align,
}

/// How a table's column lines its cells up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Align {
    #[default]
    Left,
    Middle,
    Right,
}

impl BlockKind {
    /// Whether it's code.
    pub(crate) fn code(&self) -> bool {
        matches!(self, Self::Code(_))
    }

    /// Where it goes in a table, if it's a cell of one.
    pub(crate) fn cell(&self) -> Option<Cell> {
        match self {
            Self::Cell(cell) => Some(*cell),
            _ => None,
        }
    }
}

/// A piece of a block's text and its style.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Span {
    pub(crate) range: Range<usize>,
    pub(crate) style: Style,
}

/// How a piece of text is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent styles, as in Markdown"
)]
pub(crate) struct Style {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) strike: bool,
    pub(crate) code: bool,
    pub(crate) link: bool,
    /// An image's text, said in its place.
    pub(crate) image: bool,
}

/// The blocks `markdown` is made of.
pub(crate) fn blocks(markdown: &str) -> Vec<Block> {
    let mut reader = Reader::default();
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(markdown, options).into_offset_iter() {
        if event == Event::End(TagEnd::CodeBlock)
            && !closed(&markdown[range.clone()])
            && let Some(Block {
                kind: BlockKind::Code(language),
                ..
            }) = &mut reader.open
        {
            *language = None;
        }
        reader.at = range.start;
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
    /// The table being read: how many came before it, how its columns
    /// line up, and the row and column of the cell next.
    table: Option<TableAt>,
    tables: usize,
    /// How deep in each style the text is.
    bold: u8,
    italic: u8,
    strike: u8,
    link: u8,
    image: u8,
    /// Where the event being read starts in the Markdown.
    at: usize,
    /// Where the item whose first block hasn't started starts.
    item_at: Option<usize>,
    /// A list has started whose first item hasn't.
    list_starts: bool,
    /// The links being read: where each starts in the open block's text,
    /// and where it goes.
    links: Vec<(usize, String)>,
}

/// Where in a table the reader is.
struct TableAt {
    index: usize,
    aligns: Vec<Align>,
    row: usize,
    column: usize,
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
                    at: self.at,
                    links: Vec::new(),
                    starts_list: false,
                });
            }
            Event::TaskListMarker(done) => self.push(if done { "☑ " } else { "☐ " }, false),
            _ => {}
        }
    }

    fn start(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Paragraph => self.begin(BlockKind::Paragraph),
            Tag::Heading { level, .. } => self.begin(BlockKind::Heading(heading(*level))),
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next(),
                    CodeBlockKind::Indented => None,
                };
                self.begin(BlockKind::Code(language.map(str::to_owned)));
            }
            Tag::Table(aligns) => {
                self.finish_block();
                let aligns = (aligns.iter())
                    .map(|align| match align {
                        Alignment::Center => Align::Middle,
                        Alignment::Right => Align::Right,
                        Alignment::Left | Alignment::None => Align::Left,
                    })
                    .collect();
                self.table = Some(TableAt {
                    index: self.tables,
                    aligns,
                    row: 0,
                    column: 0,
                });
                self.tables += 1;
            }
            Tag::TableHead | Tag::TableRow => {
                self.finish_block();
                if let Some(table) = &mut self.table {
                    table.column = 0;
                }
            }
            Tag::TableCell => {
                if let Some(table) = &self.table {
                    let cell = Cell {
                        table: table.index,
                        row: table.row,
                        column: table.column,
                        align: table.aligns.get(table.column).copied().unwrap_or_default(),
                    };
                    self.begin(BlockKind::Cell(cell));
                }
            }
            Tag::List(start) => {
                self.finish_block();
                self.lists.push(*start);
                self.list_starts = true;
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
                self.item_at = Some(self.at);
            }
            Tag::BlockQuote(_) => {
                self.finish_block();
                self.quotes += 1;
            }
            Tag::Strong => self.bold += 1,
            Tag::Emphasis => self.italic += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } => {
                self.link += 1;
                self.open_link(dest_url);
            }
            Tag::Image { dest_url, .. } => {
                self.image += 1;
                self.open_link(dest_url);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item => self.finish_block(),
            TagEnd::TableHead | TagEnd::TableRow => {
                self.finish_block();
                if let Some(table) = &mut self.table {
                    table.row += 1;
                }
            }
            TagEnd::TableCell => {
                self.finish_block();
                if let Some(table) = &mut self.table {
                    table.column += 1;
                }
            }
            TagEnd::Table => {
                self.finish_block();
                self.table = None;
            }
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
            TagEnd::Link => {
                self.link = self.link.saturating_sub(1);
                self.close_link();
            }
            TagEnd::Image => {
                self.image = self.image.saturating_sub(1);
                self.close_link();
            }
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
        let item = matches!(kind, BlockKind::Item(_));
        let indent = self.indent() + u8::from(in_item && !item);
        let starts_list = item && std::mem::take(&mut self.list_starts);
        self.open = Some(Block {
            kind,
            text: String::new(),
            spans: Vec::new(),
            indent,
            at: self.item_at.take().unwrap_or(self.at),
            links: Vec::new(),
            starts_list,
        });
    }

    /// A link to `to` starts here, or an image of it.
    fn open_link(&mut self, to: &str) {
        let start = self.open.as_ref().map_or(0, |block| block.text.len());
        self.links.push((start, to.to_owned()));
    }

    /// The link last opened ends here.
    fn close_link(&mut self) {
        if let Some((start, to)) = self.links.pop()
            && let Some(block) = &mut self.open
            && block.text.len() > start
        {
            let range = start..block.text.len();
            block.links.push(Link { range, to });
        }
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
            image: self.image > 0,
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
            && (!block.text.trim().is_empty() || block.kind.code())
        {
            self.blocks.push(block);
        }
    }
}

/// Whether `code`, a fenced block of code as written, has its closing
/// fence: a last line of as many of the opening fence's backticks or
/// tildes, or more, and nothing else, but the `>` of the quotes it's in.
fn closed(code: &str) -> bool {
    fn quoted(line: &str) -> &str {
        line.trim_start_matches(|ch: char| ch == '>' || ch.is_whitespace())
    }
    let code = code.trim_end_matches(['\n', '\r']);
    let opening = quoted(code);
    let Some(fence) = opening.chars().next().filter(|ch| matches!(ch, '`' | '~')) else {
        return false;
    };
    let length = opening.len() - opening.trim_start_matches(fence).len();
    let Some((_, last)) = code.rsplit_once('\n') else {
        return false;
    };
    let last = quoted(last).trim_end();
    last.len() >= length && last.chars().all(|ch| ch == fence)
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
                (
                    BlockKind::Code(Some("rust".into())),
                    "fn main() {}".into(),
                    0
                ),
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
    fn a_table_is_its_cells_each_saying_where_it_goes() {
        let cell = |table, row, column, align| {
            BlockKind::Cell(Cell {
                table,
                row,
                column,
                align,
            })
        };
        let read = kinds("| a | b |\n|---|--:|\n| 1 | `2` |\n|  | 4 |\n\n| c |\n|:-:|\n| 5 |");
        assert_eq!(
            read,
            [
                (cell(0, 0, 0, Align::Left), "a".into(), 0),
                (cell(0, 0, 1, Align::Right), "b".into(), 0),
                (cell(0, 1, 0, Align::Left), "1".into(), 0),
                (cell(0, 1, 1, Align::Right), "2".into(), 0),
                // An empty cell is no block: the next says where it goes.
                (cell(0, 2, 1, Align::Right), "4".into(), 0),
                (cell(1, 0, 0, Align::Middle), "c".into(), 0),
                (cell(1, 1, 0, Align::Middle), "5".into(), 0),
            ]
        );
    }

    #[test]
    fn code_is_coloured_only_once_its_fence_is_closed() {
        let language = |markdown: &str| match &blocks(markdown)[0].kind {
            BlockKind::Code(language) => language.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(language("```rust\nlet x = 1;\n```"), Some("rust".into()));
        assert_eq!(language("~~~~py\nx = 1\n~~~~\n\nmore"), Some("py".into()));
        assert_eq!(language("```rust {.x}\nlet x\n```"), Some("rust".into()));
        // Still coming: the fence isn't closed, or only half.
        assert_eq!(language("```rust\nlet x = 1;"), None);
        assert_eq!(language("```rust\nlet x = 1;\n``"), None);
        assert_eq!(language("```rust"), None);
        assert_eq!(language("````rust\nlet x\n```"), None);
        // No language, nothing to colour.
        assert_eq!(language("```\nlet x\n```"), None);
        // In a quote, and a quote in a quote.
        assert_eq!(language("> ```rust\n> let x\n> ```"), Some("rust".into()));
        assert_eq!(
            language("> > ```rust\n> > let x\n> > ```"),
            Some("rust".into())
        );
        assert_eq!(language("> ```rust\n> let x"), None);
    }

    #[test]
    fn half_written_markdown_still_reads() {
        // As it streams in: an unclosed fence, and bold not yet ended.
        assert_eq!(
            kinds("Look:\n\n```\nlet x"),
            [
                (BlockKind::Paragraph, "Look:".into(), 0),
                (BlockKind::Code(None), "let x".into(), 0),
            ]
        );
        assert_eq!(kinds("**bold so f")[0].1, "**bold so f");
    }

    #[test]
    fn each_block_says_where_its_markdown_starts() {
        let markdown = "# Title\n\n- one\n- two\n  - deeper\n\n> quoted\n\n```\ncode\n```\n\n---\n\n| a |\n|---|\n| 1 |\n";
        let starts: Vec<(String, &str)> = (blocks(markdown).into_iter())
            .map(|block| {
                let line = markdown[block.at..].lines().next().unwrap_or_default();
                (block.text, line)
            })
            .collect();
        assert_eq!(
            starts,
            [
                ("Title".into(), "# Title"),
                ("one".into(), "- one"),
                ("two".into(), "- two"),
                ("deeper".into(), "- deeper"),
                ("quoted".into(), "quoted"),
                ("code".into(), "```"),
                (String::new(), "---"),
                ("a".into(), " a |"),
                ("1".into(), " 1 |"),
            ]
        );
    }

    #[test]
    fn links_and_images_say_where_they_go() {
        let blocks = blocks(
            "See [the docs](https://x.org/docs) and [notes](notes.md#setup), ![a cat](cat.png).",
        );
        let block = &blocks[0];
        assert_eq!(block.text, "See the docs and notes, a cat.");
        let links: Vec<(&str, &str)> = (block.links.iter())
            .map(|link| (&block.text[link.range.clone()], link.to.as_str()))
            .collect();
        assert_eq!(
            links,
            [
                ("the docs", "https://x.org/docs"),
                ("notes", "notes.md#setup"),
                ("a cat", "cat.png"),
            ]
        );
        let image = (block.spans.iter()).find(|span| span.style.image);
        assert_eq!(
            image.map(|span| &block.text[span.range.clone()]),
            Some("a cat")
        );
    }

    #[test]
    fn a_linked_image_goes_where_the_link_does() {
        let blocks = blocks("[![build](badge.svg)](https://ci.example/run)");
        let block = &blocks[0];
        // The image's link closes first, inside the one around it.
        assert_eq!(
            (block.links.iter())
                .map(|link| link.to.as_str())
                .collect::<Vec<_>>(),
            ["badge.svg", "https://ci.example/run"]
        );
    }
}
