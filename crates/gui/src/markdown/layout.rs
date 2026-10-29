//! Markdown laid out and drawn as the window shows prose: in the
//! interface's font, with headings, lists, quotes, code in the terminal's
//! font on a background of its own, coloured as its language's grammar
//! reads it, rules, and tables as grids. A chat lays out its agent's words
//! so, and an editor its rendered view of a Markdown file.
//!
//! Every size is a multiple of the terminal font's size, so the text grows
//! and shrinks with the text around it. Each block is laid out on its own,
//! and kept by what it was laid out from, so text laid out again as it
//! changes lays out only the blocks that did.

use std::borrow::Cow;

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    Layout, LineHeight, OverflowWrap, PositionedLayoutItem, StyleProperty,
};

use super::colours::{Colouring, Colours};
use super::tables::{Cells, TableLaid, lay_out_table};
use super::{self as markdown, BlockKind};
use crate::fonts::Fonts;
use crate::render::{Fill, Ink, Paint, PlacedGlyph};

/// How a piece of text is drawn, carried through layout.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Brush {
    pub(crate) tone: Tone,
    /// It's code, on a background of its own.
    pub(crate) code: bool,
    /// It's code its language's grammar says is this, coloured as an
    /// editor colours it, rather than in its tone.
    pub(crate) syntax: Option<clackatoo_protocol::Kind>,
}

/// The colour of a piece of text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum Tone {
    #[default]
    Plain,
    Muted,
    /// In clackatoo's own colour: focus and what to act on.
    Accent,
    /// A link, in the links' colour.
    Link,
    Warning,
    /// Amber: getting close to what [`Tone::Warning`] says.
    Caution,
    /// On something in the accent colour.
    OnAccent,
    /// Added to a file.
    Added,
    /// Removed from a file.
    Removed,
}

/// Space between lines of text, as a multiple of the text's size.
pub(crate) const LINE: f32 = 1.45;

/// What text laid out here draws: a chat pane, or an editor's rendered
/// Markdown.
#[derive(Debug, Default)]
pub(crate) struct TextPaint {
    pub(crate) images: Vec<crate::render::PaintedImage>,
    pub(crate) fills: Vec<Fill>,
    pub(crate) glyphs: Vec<PlacedGlyph>,
    /// Fills over the text, as the cursor and a menu are.
    pub(crate) over: Vec<Fill>,
    /// Glyphs over those, as a menu's text is.
    pub(crate) over_glyphs: Vec<PlacedGlyph>,
    /// Where the cursor in the message being typed is, for an input
    /// method's window: left, top, width and height.
    pub(crate) ime: Option<[f32; 4]>,
}

/// The size of prose's text, in pixels: the terminal's.
pub(crate) fn text_size(fonts: &Fonts) -> f32 {
    fonts.ppem
}

/// The interface's font family, with others behind it for what it hasn't
/// got: the terminal's, then emoji. Noto Sans, a common interface font,
/// has no `▾`, which the terminal's fonts nearly all have.
pub(crate) fn ui_family(fonts: &Fonts) -> FontFamily<'static> {
    FontFamily::List(Cow::Owned(vec![
        named_or(fonts.ui_family.as_ref(), GenericFamily::SansSerif),
        named_or(fonts.mono_family.as_ref(), GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Emoji),
    ]))
}

/// The terminal's font family, for code, with the interface's and emoji
/// behind it.
pub(crate) fn mono_family(fonts: &Fonts) -> FontFamily<'static> {
    FontFamily::List(Cow::Owned(vec![
        named_or(fonts.mono_family.as_ref(), GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Monospace),
        named_or(fonts.ui_family.as_ref(), GenericFamily::SansSerif),
        FontFamilyName::Generic(GenericFamily::Emoji),
    ]))
}

/// A character no font has: laid out, parley looks through every family
/// of a stack for it, and the fonts behind them, as it would for any
/// character the interface's font hasn't got, such as the send button's
/// `↑` or a symbol in an agent's words.
const IN_NO_FONT: char = '\u{10FFFD}';

/// Has the families the chat lays its text out in found on a thread of
/// their own, for its words and its code, all the way down each stack,
/// so the first chat drawn doesn't wait for fontconfig, which the first
/// look into each family behind the interface's takes milliseconds over
/// (see [`Fonts::warm`]). Returns the thread, if it started.
pub(crate) fn warm_fonts(fonts: &Fonts) -> Option<std::thread::JoinHandle<()>> {
    let text = format!("Aa1 {IN_NO_FONT}");
    fonts.warm(vec![ui_family(fonts), mono_family(fonts)], text)
}

/// The family called `name`, or `generic` without one.
fn named_or(name: Option<&String>, generic: GenericFamily) -> FontFamilyName<'static> {
    match name {
        Some(name) => FontFamilyName::Named(Cow::Owned(name.clone())),
        None => FontFamilyName::Generic(generic),
    }
}

/// How prose's ordinary text is styled, at `size` pixels.
pub(crate) fn body_style(fonts: &Fonts, size: f32) -> Vec<StyleProperty<'static, Brush>> {
    vec![
        StyleProperty::FontFamily(ui_family(fonts)),
        StyleProperty::FontSize(size),
        StyleProperty::LineHeight(LineHeight::FontSizeRelative(LINE)),
        StyleProperty::Brush(Brush::default()),
    ]
}

/// Something laid out: the text it was laid out from, and its blocks.
pub(crate) struct Laid {
    pub(crate) em: f32,
    pub(crate) source: String,
    /// The tone plain text was laid out in, which [`ChatView::box_label`]
    /// keeps it by: plain, for Markdown.
    pub(crate) tone: Tone,
    pub(crate) blocks: Vec<LaidBlock>,
    /// Its tables, whose cells are among its blocks.
    pub(crate) tables: Vec<TableLaid>,
    /// Some of its code was laid out plain, as its colours were still to
    /// come: when they do, it's laid out again.
    pub(crate) waiting: bool,
    /// How many of its blocks were laid out afresh, rather than kept.
    pub(crate) fresh: usize,
    pub(crate) height: f32,
    /// The widest line.
    pub(crate) width: f32,
}

/// One block, placed within what it belongs to.
pub(crate) struct LaidBlock {
    pub(crate) layout: Layout<Brush>,
    /// The text it shows, without its Markdown.
    pub(crate) text: String,
    pub(crate) kind: BlockKind,
    /// From the left of the column.
    pub(crate) left: f32,
    /// From the top of what it belongs to.
    pub(crate) top: f32,
    /// A list item's marker, laid out, and where it goes from the left.
    pub(crate) marker: Option<(Mark, f32)>,
    /// What it was laid out from, hashed: text laid out again as more of
    /// it comes keeps the blocks, and tables' cells, that haven't changed,
    /// so an answer streaming in lays out only its last paragraph again,
    /// not every one before it.
    pub(crate) key: u64,
    /// Where it starts in the Markdown, in bytes (see [`markdown::Block`]).
    pub(crate) at: usize,
    /// Its links.
    pub(crate) links: Vec<markdown::Link>,
}

/// Sizes, from the text's size.
#[derive(Clone, Copy)]
pub(crate) struct Measures {
    pub(crate) em: f32,
}

impl Measures {
    pub(crate) fn of(em: f32) -> Self {
        Self { em }
    }

    pub(crate) fn em(self, ems: f32) -> f32 {
        (self.em * ems).round()
    }
}

/// What a piece of text is drawn as.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Look {
    /// The agent's words: Markdown.
    Words,
    /// Its thinking: Markdown, quieter.
    Thought,
    /// Plain text, as a message sent is, in `Tone`.
    Plain(Tone),
    /// Plain text in the terminal's font, as a command's output is.
    Mono(Tone),
    /// A Markdown file, as an editor's rendered view shows it: as the
    /// agent's words are, with larger headings further apart, task lists'
    /// boxes for their items' markers, and images said as such.
    Page,
    /// Markdown of clackatoo's own, in `Tone`, as how a turn failed is
    /// said under it: what the agent said in it is quoted as code.
    Note(Tone),
}

/// The fills and glyphs being drawn, and what they're cut off outside.
pub(crate) struct Canvas<'a> {
    pub(crate) paint: &'a mut TextPaint,
    pub(crate) fonts: &'a mut Fonts,
    pub(crate) clip: [f32; 4],
    /// The text's size, which spacing is a share of.
    pub(crate) em: f32,
    /// What it draws goes over everything, as a menu does.
    pub(crate) over: bool,
}

impl Canvas<'_> {
    pub(crate) fn fill(&mut self, fill: Fill) {
        let fill = fill.clipped(self.clip);
        if self.over {
            self.paint.over.push(fill);
        } else {
            self.paint.fills.push(fill);
        }
    }

    /// Draws `layout` with its top left at `origin`.
    pub(crate) fn text(&mut self, layout: &Layout<Brush>, origin: [f32; 2]) {
        for line in layout.lines() {
            let metrics = *line.metrics();
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let brush = run.style().brush;
                if brush.code {
                    let pad =
                        (metrics.line_height - metrics.ascent - metrics.descent).max(0.0) / 2.0;
                    let side = (self.em * 0.2).round();
                    self.fill(Fill::rounded(
                        [
                            origin[0] + run.offset() - side,
                            origin[1] + metrics.block_min_coord + pad * 0.5,
                            run.advance() + 2.0 * side,
                            metrics.block_max_coord - metrics.block_min_coord - pad,
                        ],
                        Paint::Code,
                        (self.em * 0.25).round(),
                    ));
                }
                let font = run.run().font();
                let Some(face) = self.fonts.face_for(font, run.run().synthesis()) else {
                    continue;
                };
                let ppem = run.run().font_size();
                let ink = match brush.tone {
                    _ if let Some(kind) = brush.syntax => {
                        Ink::Color(crate::colors::syntax(kind).fg)
                    }
                    Tone::Plain => Ink::Plain,
                    Tone::Muted => Ink::Muted,
                    Tone::Accent => Ink::Accent,
                    Tone::Link => Ink::Link,
                    Tone::Warning => Ink::Warning,
                    Tone::Caution => Ink::Caution,
                    Tone::Added => Ink::Added,
                    Tone::Removed => Ink::Removed,
                    Tone::OnAccent => Ink::OnColor,
                };
                for glyph in run.positioned_glyphs() {
                    let Ok(id) = u16::try_from(glyph.id) else {
                        continue;
                    };
                    let glyphs = if self.over {
                        &mut self.paint.over_glyphs
                    } else {
                        &mut self.paint.glyphs
                    };
                    glyphs.push(PlacedGlyph {
                        face,
                        glyph: id,
                        ppem,
                        pen: [origin[0] + glyph.x, origin[1] + glyph.y],
                        ink,
                        clip: self.clip,
                    });
                }
                if run.style().strikethrough.is_some() {
                    self.fill(Fill::solid(
                        [
                            origin[0] + run.offset(),
                            origin[1] + metrics.baseline - metrics.ascent * 0.3,
                            run.advance(),
                            1.0,
                        ],
                        Paint::Icon,
                    ));
                }
            }
        }
    }
}

impl Canvas<'_> {
    /// Draws the glyphs of `layout`, with its top left at `origin`, that
    /// start inside `rect`, over everything, in `ink`: the character
    /// under a solid cursor, in the colour that shows on it.
    pub(crate) fn text_over(
        &mut self,
        layout: &Layout<Brush>,
        origin: [f32; 2],
        rect: [f32; 4],
        ink: Ink,
    ) {
        let [left, top, width, height] = rect;
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let Some(face) = self.fonts.face_for(run.run().font(), run.run().synthesis())
                else {
                    continue;
                };
                let ppem = run.run().font_size();
                for glyph in run.positioned_glyphs() {
                    let pen = [origin[0] + glyph.x, origin[1] + glyph.y];
                    let inside = pen[0] >= left - 0.5
                        && pen[0] < left + width - 0.5
                        && pen[1] > top
                        && pen[1] <= top + height;
                    let Ok(id) = u16::try_from(glyph.id) else {
                        continue;
                    };
                    if inside {
                        self.paint.over_glyphs.push(PlacedGlyph {
                            face,
                            glyph: id,
                            ppem,
                            pen,
                            ink,
                            clip: self.clip,
                        });
                    }
                }
            }
        }
    }
}

/// Where rectangles `a` and `b` overlap: nothing wide or high, if they
/// don't.
pub(crate) fn intersection(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (left, top) = (a[0].max(b[0]), a[1].max(b[1]));
    let right = (a[0] + a[2]).min(b[0] + b[2]);
    let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    [left, top, (right - left).max(0.0), (bottom - top).max(0.0)]
}

/// Draws `laid` with the column's left at `origin[0]`, its top at
/// `origin[1]`: its blocks, and what goes with them.
pub(crate) fn draw_laid(
    canvas: &mut Canvas<'_>,
    m: Measures,
    laid: &Laid,
    origin: [f32; 2],
    column: f32,
) {
    draw_scrolled(canvas, m, laid, origin, column, &|_| 0.0);
}

/// Draws `laid` as [`draw_laid`] does, with each of its tables scrolled
/// sideways as far as `scrolled` says, by the table's number.
pub(crate) fn draw_scrolled(
    canvas: &mut Canvas<'_>,
    m: Measures,
    laid: &Laid,
    origin: [f32; 2],
    column: f32,
    scrolled: &dyn Fn(usize) -> f32,
) {
    for (number, table) in laid.tables.iter().enumerate() {
        // The line under its head, as far across as it shows.
        let shown = table.width.min(column - table.left);
        let y = origin[1] + table.top + table.rule;
        let rule = [origin[0] + table.left, y, shown, 1.0];
        canvas.fill(Fill::solid(rule, Paint::Divider));
        let scroll = scrolled(number).clamp(0.0, table.max_scroll(column));
        let view = canvas.clip;
        let room = [
            origin[0] + table.left,
            view[1],
            column - table.left,
            view[3],
        ];
        canvas.clip = intersection(view, room);
        for block in &laid.blocks[table.blocks.clone()] {
            let at = [origin[0] + block.left - scroll, origin[1] + block.top];
            let size = [block.layout.width(), block.layout.height()];
            if overlaps(canvas.clip, [at[0], at[1], size[0], size[1]]) {
                canvas.text(&block.layout, at);
            }
        }
        canvas.clip = view;
        draw_cut_off(
            canvas,
            m,
            table,
            [origin[0] + table.left, origin[1]],
            (column, scroll),
        );
    }
    for (index, block) in laid.blocks.iter().enumerate() {
        let x = origin[0] + block.left;
        let y = origin[1] + block.top;
        let height = block.layout.height();
        // Out of view, as most of a long answer is, or a table's cell:
        // nothing to draw here. What's drawn round a block reaches an em
        // past it at most.
        let reach = [x - m.em, y - m.em, column + 2.0 * m.em, height + 2.0 * m.em];
        if laid.table_of(index).is_some() || !overlaps(canvas.clip, reach) {
            continue;
        }
        match &block.kind {
            // Its background from the left of the text around it, which
            // is where the cards start too, to the column's right edge.
            BlockKind::Code(_) => {
                let inset = code_inset(m);
                canvas.fill(Fill::rounded(
                    [
                        x - inset,
                        y - m.em(0.4),
                        column - block.left + inset,
                        height + m.em(0.8),
                    ],
                    Paint::Code,
                    m.em(0.5),
                ));
            }
            // Its bar where the text around it starts.
            BlockKind::Quote => canvas.fill(Fill::solid(
                [x - quote_inset(m), y, quote_bar(m), height],
                Paint::RaisedEdge,
            )),
            BlockKind::Rule => canvas.fill(Fill::solid(
                [x, y + m.em(0.5), column - block.left, 1.0],
                Paint::Divider,
            )),
            _ => {}
        }
        if let Some((marker, left)) = &block.marker {
            canvas.text(&marker.layout, [origin[0] + left, y]);
        }
        canvas.text(&block.layout, [x, y]);
    }
}

/// Lays out `text` as `look` says, `width` wide.
pub(crate) fn lay_out(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    m: Measures,
    text: &str,
    look: Look,
    width: f32,
) -> Laid {
    lay_out_coloured(
        layout_cx,
        fonts,
        m,
        (text, look),
        (width, &mut Cells::new()),
        None,
    )
}

/// Lays out `text` as `look` says, `width` wide, its code coloured as
/// `colours` knows it, if it's given, and wanted from them if it isn't
/// known, and its blocks, tables' cells among them, taken from `cells` where
/// they're there, laid out just so.
pub(crate) fn lay_out_coloured(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    m: Measures,
    (text, look): (&str, Look),
    (width, cells): (f32, &mut Cells),
    colours: Option<&mut Colours>,
) -> Laid {
    let blocks = match look {
        Look::Words | Look::Thought | Look::Page | Look::Note(_) => markdown::blocks(text),
        Look::Plain(_) | Look::Mono(_) => vec![markdown::Block {
            kind: BlockKind::Paragraph,
            text: text.to_owned(),
            spans: Vec::new(),
            indent: 0,
            at: 0,
            links: Vec::new(),
            starts_list: false,
        }],
    };
    lay_out_blocks(
        layout_cx,
        fonts,
        m,
        (blocks, text.to_owned(), look),
        (width, cells),
        colours,
    )
}

/// A message as it was typed, laid out as plain text in `tone`, `width`
/// wide, but with each file it mentions, `@path` (see
/// [`clackatoo_protocol::mentions`]), as a link to the file, drawn as a
/// chip: code's background, the links' colour.
pub(crate) fn lay_out_message(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    m: Measures,
    (text, tone): (&str, Tone),
    (width, cells): (f32, &mut Cells),
) -> Laid {
    let mentioned = clackatoo_protocol::mentions(text);
    let block = markdown::Block {
        kind: BlockKind::Paragraph,
        text: text.to_owned(),
        spans: (mentioned.iter())
            .map(|mention| markdown::Span {
                range: mention.range.clone(),
                style: markdown::Style {
                    code: true,
                    link: true,
                    ..markdown::Style::default()
                },
            })
            .collect(),
        indent: 0,
        at: 0,
        links: (mentioned.iter())
            .map(|mention| markdown::Link {
                range: mention.range.clone(),
                to: mention.path.to_owned(),
            })
            .collect(),
        starts_list: false,
    };
    let look = (vec![block], text.to_owned(), Look::Plain(tone));
    lay_out_blocks(layout_cx, fonts, m, look, (width, cells), None)
}

/// Lays out `blocks`, read from `source`, as [`lay_out_coloured`] lays
/// out the blocks it reads.
pub(crate) fn lay_out_blocks(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    m: Measures,
    (blocks, source, look): (Vec<markdown::Block>, String, Look),
    (width, cells): (f32, &mut Cells),
    mut colours: Option<&mut Colours>,
) -> Laid {
    let tone = match look {
        Look::Plain(tone) | Look::Mono(tone) | Look::Note(tone) => tone,
        Look::Words | Look::Thought | Look::Page => Tone::Plain,
    };
    let mut laid = Laid {
        em: m.em,
        source,
        tone,
        blocks: Vec::new(),
        tables: Vec::new(),
        waiting: false,
        fresh: 0,
        height: 0.0,
        width: 0.0,
    };
    let step = m.em(1.5);
    let mut blocks = blocks.into_iter().peekable();
    let mut first = true;
    let mut after_code = false;
    while let Some(block) = blocks.next() {
        let block = if look == Look::Page {
            as_page(block)
        } else {
            block
        };
        if !first {
            laid.height += gap(m, look, &block, after_code);
        }
        first = false;
        after_code = block.kind.code();
        if let Some(cell) = block.kind.cell() {
            // The table's cells, all of them.
            let mut table = vec![block];
            while let Some(next) = blocks.next_if(|next| {
                next.kind
                    .cell()
                    .is_some_and(|next| next.table == cell.table)
            }) {
                table.push(next);
            }
            lay_out_table((layout_cx, fonts), m, (look, cells), table, &mut laid);
            continue;
        }
        let indent = f32::from(block.indent) * step
            + match block.kind {
                BlockKind::Item(_) => step,
                _ => 0.0,
            };
        // A quote's text goes just after its bar, which is where the text
        // around it starts; code goes inside its background, which starts
        // there too.
        let indent = match block.kind {
            BlockKind::Quote => indent - step + quote_inset(m),
            BlockKind::Code(_) => indent + code_inset(m),
            _ => indent,
        };
        let size = text_size_of(&block.kind, look, m);
        let syntax = code_colours(&block, colours.as_deref_mut(), &mut laid.waiting);
        let wrap = (width - indent).max(m.em(4.0));
        // As it was, if it was laid out before just so.
        let key = block_key(&block, &syntax, look, size, wrap);
        let layout = cells.remove(key).unwrap_or_else(|| {
            laid.fresh += 1;
            lay_out_block(layout_cx, fonts, (&block, &syntax), look, size, wrap)
        });
        let marker = match &block.kind {
            BlockKind::Item(marker) => {
                let mark = Mark::of((layout_cx, fonts), cells, marker, size);
                let left = (indent - mark.layout.width() - m.em(0.5)).max(0.0);
                Some((mark, left))
            }
            _ => None,
        };
        let height = if block.kind == BlockKind::Rule {
            m.em(1.0)
        } else {
            layout.height()
        };
        laid.width = laid.width.max(layout.width() + indent);
        laid.blocks.push(LaidBlock {
            layout,
            text: block.text,
            kind: block.kind,
            left: indent,
            top: laid.height,
            marker,
            key,
            at: block.at,
            links: block.links,
        });
        laid.height += height;
    }
    laid
}

/// What marks an image's text, said in its place on a page.
const IMAGE: &str = "▣ ";

/// `block` as a page shows it (see [`Look::Page`]): a task's box for its
/// item's marker, and an image's text after a mark saying it's one.
fn as_page(mut block: markdown::Block) -> markdown::Block {
    let task = ["☐ ", "☑ "]
        .into_iter()
        .find(|task| block.text.starts_with(task));
    if let (BlockKind::Item(marker), Some(task)) = (&mut block.kind, task) {
        task.trim_end().clone_into(marker);
        splice(&mut block, 0..task.len(), "", None);
    }
    let mut images: Vec<usize> = (block.spans.iter())
        .filter(|span| span.style.image)
        .map(|span| span.range.start)
        .collect();
    images.dedup();
    let style = markdown::Style {
        image: true,
        ..markdown::Style::default()
    };
    for at in images.into_iter().rev() {
        splice(&mut block, at..at, IMAGE, Some(style));
    }
    block
}

/// Puts `text` in place of `range` of `block`'s text, in `style` if it's
/// given, moving its styles and links with what's after it.
fn splice(
    block: &mut markdown::Block,
    range: std::ops::Range<usize>,
    text: &str,
    style: Option<markdown::Style>,
) {
    block.text.replace_range(range.clone(), text);
    let moved = |at: usize, end: bool| {
        // What ends where text goes in ends before it.
        let after = at > range.end || (at == range.end && (!end || !range.is_empty()));
        if after {
            at - range.len() + text.len()
        } else {
            at.min(range.start)
        }
    };
    for span in &mut block.spans {
        span.range = moved(span.range.start, false)..moved(span.range.end, true);
    }
    for link in &mut block.links {
        link.range = moved(link.range.start, false)..moved(link.range.end, true);
    }
    block.spans.retain(|span| !span.range.is_empty());
    if let Some(style) = style.filter(|_| !text.is_empty()) {
        let range = range.start..range.start + text.len();
        let at = (block.spans.iter()).position(|span| span.range.start >= range.start);
        let span = markdown::Span { range, style };
        block.spans.insert(at.unwrap_or(block.spans.len()), span);
    }
}

/// The space above `block`, after one of code if `after_code`, laid out
/// as `look` says. Code's background reaches past its text: what's around
/// it is as far from that as from other text. On a page, headings stand
/// further from what's above them than in a chat, and a list from what's
/// above it, as far as a paragraph would.
pub(crate) fn gap(m: Measures, look: Look, block: &markdown::Block, after_code: bool) -> f32 {
    let page = look == Look::Page;
    let before = match block.kind {
        BlockKind::Item(_) if page && block.starts_list && block.indent == 0 => m.em(0.7),
        BlockKind::Item(_) => m.em(0.3),
        BlockKind::Code(_) => m.em(1.1),
        BlockKind::Heading(_) if page => m.em(1.4),
        _ => m.em(0.7),
    };
    before + if after_code { m.em(0.4) } else { 0.0 }
}

/// What `block`, a block other than a table's cell, is laid out from,
/// hashed: its text, styles and kind, its code's colours, `syntax`, how it
/// looks, and its text's size and the width it wraps to.
fn block_key(
    block: &markdown::Block,
    syntax: &[clackatoo_protocol::Span],
    look: Look,
    size: f32,
    width: f32,
) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    let sizes = (size.to_bits(), width.to_bits());
    (&block.text, &block.spans, &block.kind, syntax, look, sizes).hash(&mut hasher);
    hasher.finish()
}

/// How big the text of a block of `kind` is, laid out as `look` says.
fn text_size_of(kind: &BlockKind, look: Look, m: Measures) -> f32 {
    match kind {
        _ if matches!(look, Look::Mono(_)) => m.em * 0.93,
        BlockKind::Heading(1) if look == Look::Page => m.em * 1.6,
        BlockKind::Heading(2) if look == Look::Page => m.em * 1.35,
        BlockKind::Heading(3) if look == Look::Page => m.em * 1.15,
        BlockKind::Heading(1) => m.em * 1.35,
        BlockKind::Heading(2) => m.em * 1.2,
        BlockKind::Heading(_) => m.em * 1.08,
        BlockKind::Code(_) => m.em * 0.93,
        _ => m.em,
    }
}

/// How `block` is coloured, if it's code `colours` know: none if it
/// isn't, or they don't, and then, if they're to come, `waiting`.
fn code_colours(
    block: &markdown::Block,
    colours: Option<&mut Colours>,
    waiting: &mut bool,
) -> Vec<clackatoo_protocol::Span> {
    let (BlockKind::Code(Some(language)), Some(colours)) = (&block.kind, colours) else {
        return Vec::new();
    };
    match colours.of(language, &block.text) {
        Colouring::Known(spans) => spans.to_vec(),
        Colouring::Waiting => {
            *waiting = true;
            Vec::new()
        }
        Colouring::Plain => Vec::new(),
    }
}

/// Whether `rect`, its left, top, width and height, overlaps `clip`.
pub(crate) fn overlaps(clip: [f32; 4], rect: [f32; 4]) -> bool {
    let [width, height] = [intersection(clip, rect)[2], intersection(clip, rect)[3]];
    width > 0.0 && height > 0.0
}

/// Marks the sides of `table`, drawn from `origin`, its left, and the top
/// of what it belongs to, where it's cut off, scrolled `scroll` in a
/// column `column` wide: a `‹` or `›` on the background, over the cells
/// it hides.
fn draw_cut_off(
    canvas: &mut Canvas<'_>,
    m: Measures,
    table: &TableLaid,
    origin: [f32; 2],
    (column, scroll): (f32, f32),
) {
    let most = table.max_scroll(column);
    if most <= 0.0 {
        return;
    }
    let (wide, top) = (m.em(1.2), origin[1] + table.top);
    let sides = [
        (scroll > 0.5, origin[0], &table.marks[0].layout),
        (
            scroll < most - 0.5,
            origin[0] + column - table.left - wide,
            &table.marks[1].layout,
        ),
    ];
    for (cut, left, mark) in sides {
        if !cut {
            continue;
        }
        canvas.fill(Fill::solid([left, top, wide, table.height], Paint::Tile));
        let at = [
            left + (wide - mark.width()) / 2.0,
            top + (table.height - mark.height()) / 2.0,
        ];
        canvas.text(mark, at);
    }
}

/// How far a quote's text is from its bar's left, which is the left of
/// the text around it: the bar, and a gap.
pub(crate) fn quote_inset(m: Measures) -> f32 {
    quote_bar(m) + m.em(0.75)
}

/// How wide a quote's bar is.
fn quote_bar(m: Measures) -> f32 {
    m.em(0.2).max(2.0)
}

/// How far a block of code's text is inside its background, whose left is
/// the left of the text around it.
pub(crate) fn code_inset(m: Measures) -> f32 {
    m.em(0.75)
}

/// Lays out one block, its text `size` pixels, `width` wide, the code
/// in it coloured as `syntax` says.
pub(crate) fn lay_out_block(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    (block, syntax): (&markdown::Block, &[clackatoo_protocol::Span]),
    look: Look,
    size: f32,
    width: f32,
) -> Layout<Brush> {
    let tone = match look {
        Look::Words | Look::Page => Tone::Plain,
        Look::Thought => Tone::Muted,
        Look::Plain(tone) | Look::Mono(tone) | Look::Note(tone) => tone,
    };
    let (body, mono) = (body_style(fonts, size), mono_family(fonts));
    let mut builder = layout_cx.ranged_builder(&mut fonts.font_cx, &block.text, 1.0, true);
    for property in body {
        builder.push_default(property);
    }
    // Plain messages can contain long URLs, identifiers or adapter text.
    // Break an otherwise unbreakable word rather than widening its bubble.
    if matches!(look, Look::Plain(_) | Look::Note(_)) {
        builder.push_default(StyleProperty::OverflowWrap(OverflowWrap::Anywhere));
    }
    builder.push_default(StyleProperty::Brush(Brush {
        tone,
        code: false,
        syntax: None,
    }));
    if matches!(look, Look::Thought) {
        builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    if matches!(look, Look::Mono(_)) {
        builder.push_default(StyleProperty::FontFamily(mono.clone()));
        builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
            1.35,
        )));
    }
    match block.kind {
        BlockKind::Heading(_) => builder.push_default(StyleProperty::FontWeight(FontWeight::BOLD)),
        BlockKind::Cell(cell) if cell.row == 0 => {
            builder.push_default(StyleProperty::FontWeight(FontWeight::BOLD));
        }
        BlockKind::Code(_) => {
            builder.push_default(StyleProperty::FontFamily(mono.clone()));
            builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
                1.35,
            )));
        }
        BlockKind::Quote => builder.push_default(StyleProperty::Brush(Brush {
            tone: Tone::Muted,
            code: false,
            syntax: None,
        })),
        _ => {}
    }
    for span in &block.spans {
        let (range, style) = (span.range.clone(), span.style);
        if style.bold {
            builder.push(StyleProperty::FontWeight(FontWeight::BOLD), range.clone());
        }
        if style.italic {
            builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
        }
        if style.strike {
            builder.push(StyleProperty::Strikethrough(true), range.clone());
        }
        if style.code {
            builder.push(StyleProperty::FontFamily(mono.clone()), range.clone());
            builder.push(StyleProperty::FontSize(size * 0.93), range.clone());
            // Code that's a link, as a file a message mentions, is in the
            // links' colour on code's background.
            builder.push(
                StyleProperty::Brush(Brush {
                    tone: if style.link { Tone::Link } else { tone },
                    code: true,
                    syntax: None,
                }),
                range,
            );
        } else if style.link {
            let brush = Brush {
                tone: Tone::Link,
                code: false,
                syntax: None,
            };
            builder.push(StyleProperty::Brush(brush), range);
        } else if style.image && look == Look::Page {
            // In a link, it's the link's colour, to show it goes there.
            let brush = Brush {
                tone: Tone::Muted,
                code: false,
                syntax: None,
            };
            builder.push(StyleProperty::FontStyle(FontStyle::Italic), range.clone());
            builder.push(StyleProperty::Brush(brush), range);
        }
    }
    for span in syntax {
        let brush = Brush {
            tone,
            code: false,
            syntax: Some(span.kind),
        };
        builder.push(StyleProperty::Brush(brush), span.start..span.end);
    }
    let mut layout = builder.build(&block.text);
    // Code keeps its lines, however long, and a table's cells theirs.
    let unbroken = block.kind.code() || block.kind.cell().is_some();
    layout.break_all_lines((!unbroken).then_some(width));
    layout.align(Alignment::Start, AlignmentOptions::default());
    layout
}

/// A short text laid out on its own, as a list item's marker, or the mark
/// of a side of a table where it's cut off, and what it was laid out from,
/// hashed, to keep it by: the same for many.
pub(crate) struct Mark {
    pub(crate) layout: Layout<Brush>,
    pub(crate) key: u64,
}

impl Mark {
    /// `text` laid out as a list item's marker is, `size` pixels: as it was
    /// in `cells`, if it was laid out before just so.
    pub(crate) fn of(
        (layout_cx, fonts): (&mut parley::LayoutContext<Brush>, &mut Fonts),
        cells: &mut Cells,
        text: &str,
        size: f32,
    ) -> Self {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        ("mark", text, size.to_bits()).hash(&mut hasher);
        let key = hasher.finish();
        let layout =
            (cells.remove(key)).unwrap_or_else(|| lay_out_marker(layout_cx, fonts, text, size));
        Self { layout, key }
    }
}

/// Lays out a list item's marker, its text `size` pixels.
fn lay_out_marker(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    marker: &str,
    size: f32,
) -> Layout<Brush> {
    let body = body_style(fonts, size);
    let mut builder = layout_cx.ranged_builder(&mut fonts.font_cx, marker, 1.0, true);
    for property in body {
        builder.push_default(property);
    }
    builder.push_default(StyleProperty::Brush(Brush {
        tone: Tone::Muted,
        code: false,
        syntax: None,
    }));
    let mut layout = builder.build(marker);
    layout.break_all_lines(None);
    layout.align(Alignment::Start, AlignmentOptions::default());
    layout
}
