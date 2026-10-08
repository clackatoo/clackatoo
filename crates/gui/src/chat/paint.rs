//! Laying out a chat and drawing it: its turns in a column down the
//! middle of the pane, scrolled, and the box a message is typed in below.
//!
//! Every size is a multiple of the terminal font's size, so a chat grows
//! and shrinks with the text around it. Turns are laid out once for a
//! width, and again only when their text changes, as the turn being
//! answered does.

use std::borrow::Cow;
use std::collections::HashMap;

use clackatoo_protocol::{
    ChatState, ChoiceKind, Item, Outcome, Setting, SettingKind, ToolStatus, Turn,
};
use parley::{
    Alignment, AlignmentOptions, FontFamily, FontFamilyName, FontStyle, FontWeight, GenericFamily,
    Layout, LineHeight, PositionedLayoutItem, StyleProperty,
};

use super::markdown::{self, BlockKind};
use super::{Brush, ChatView, Hit, Menu, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Ink, Paint, PlacedGlyph};

/// The widest the column of turns gets, in ems.
const COLUMN: f32 = 46.0;
/// Space between lines of text, as a multiple of the text's size.
const LINE: f32 = 1.45;
/// The most lines the box a message is typed in grows to.
const EDITOR_LINES: f32 = 10.0;

/// What a chat pane draws.
#[derive(Debug, Default)]
pub(crate) struct ChatPaint {
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

/// The size of the chat's text, in pixels: the terminal's.
pub(super) fn text_size(fonts: &Fonts) -> f32 {
    fonts.ppem
}

/// The interface's font family, with others behind it for what it hasn't
/// got: the terminal's, then emoji. Noto Sans, a common interface font,
/// has no `▾`, which the terminal's fonts nearly all have.
fn ui_family(fonts: &Fonts) -> FontFamily<'static> {
    FontFamily::List(Cow::Owned(vec![
        named_or(fonts.ui_family.as_ref(), GenericFamily::SansSerif),
        named_or(fonts.mono_family.as_ref(), GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Emoji),
    ]))
}

/// The terminal's font family, for code, with the interface's and emoji
/// behind it.
fn mono_family(fonts: &Fonts) -> FontFamily<'static> {
    FontFamily::List(Cow::Owned(vec![
        named_or(fonts.mono_family.as_ref(), GenericFamily::Monospace),
        FontFamilyName::Generic(GenericFamily::Monospace),
        named_or(fonts.ui_family.as_ref(), GenericFamily::SansSerif),
        FontFamilyName::Generic(GenericFamily::Emoji),
    ]))
}

/// The family called `name`, or `generic` without one.
fn named_or(name: Option<&String>, generic: GenericFamily) -> FontFamilyName<'static> {
    match name {
        Some(name) => FontFamilyName::Named(Cow::Owned(name.clone())),
        None => FontFamilyName::Generic(generic),
    }
}

/// How the chat's ordinary text is styled, at `size` pixels.
pub(super) fn body_style(fonts: &Fonts, size: f32) -> Vec<StyleProperty<'static, Brush>> {
    vec![
        StyleProperty::FontFamily(ui_family(fonts)),
        StyleProperty::FontSize(size),
        StyleProperty::LineHeight(LineHeight::FontSizeRelative(LINE)),
        StyleProperty::Brush(Brush::default()),
    ]
}

/// A turn's pieces, laid out for one width, kept while their text stays
/// the same.
#[derive(Default)]
pub(super) struct Cache {
    /// The column's width and the text's size they were laid out for.
    width: f32,
    size: f32,
    /// By turn, then by piece: 0 is the message, and the agent's items
    /// follow.
    pieces: HashMap<(usize, usize), Laid>,
    /// Text of the chat's own, such as "Working…", by what it says.
    notes: HashMap<(String, Tone), Laid>,
}

/// Something laid out: the text it was laid out from, and its blocks.
struct Laid {
    source: String,
    blocks: Vec<LaidBlock>,
    height: f32,
    /// The widest line.
    width: f32,
}

/// One block, placed within what it belongs to.
struct LaidBlock {
    layout: Layout<Brush>,
    kind: BlockKind,
    /// From the left of the column.
    left: f32,
    /// From the top of what it belongs to.
    top: f32,
    /// A list item's marker, laid out, and where it goes from the left.
    marker: Option<(Layout<Brush>, f32)>,
}

/// Sizes, from the text's size.
#[derive(Clone, Copy)]
struct Measures {
    em: f32,
}

impl Measures {
    fn of(em: f32) -> Self {
        Self { em }
    }

    fn em(self, ems: f32) -> f32 {
        (self.em * ems).round()
    }
}

/// What a piece of the chat is drawn as.
#[derive(Clone, Copy)]
enum Look {
    /// The agent's words: Markdown.
    Words,
    /// Its thinking: Markdown, quieter.
    Thought,
    /// Plain text, as a message sent is, in `Tone`.
    Plain(Tone),
}

/// The fills and glyphs being drawn, and what they're cut off outside.
struct Canvas<'a> {
    paint: &'a mut ChatPaint,
    fonts: &'a mut Fonts,
    clip: [f32; 4],
    /// The text's size, which spacing is a share of.
    em: f32,
    /// What it draws goes over everything, as a menu does.
    over: bool,
}

impl Canvas<'_> {
    fn fill(&mut self, fill: Fill) {
        let fill = fill.clipped(self.clip);
        if self.over {
            self.paint.over.push(fill);
        } else {
            self.paint.fills.push(fill);
        }
    }

    /// Draws `layout` with its top left at `origin`.
    fn text(&mut self, layout: &Layout<Brush>, origin: [f32; 2]) {
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
                    Tone::Plain => Ink::Plain,
                    Tone::Muted => Ink::Muted,
                    Tone::Accent => Ink::Accent,
                    Tone::Warning => Ink::Warning,
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

impl ChatView {
    /// Lays the chat out in a pane `size` pixels, whose top left is at
    /// `origin`, and says what to draw. `caret` draws the cursor in the
    /// message being typed.
    pub(crate) fn paint(
        &mut self,
        fonts: &mut Fonts,
        origin: [f32; 2],
        size: [f32; 2],
        caret: bool,
    ) -> ChatPaint {
        self.style_editor(fonts);
        let em = text_size(fonts);
        let m = Measures::of(em);
        self.line_height = (em * LINE).round();
        let margin = m.em(1.5);
        let column = (size[0] - 2.0 * margin).clamp(m.em(8.0), m.em(COLUMN));
        let left = ((size[0] - column) / 2.0).max(0.0).round();
        // Laid out for another width or size: again.
        if self.cache.width.to_bits() != column.to_bits()
            || self.cache.size.to_bits() != em.to_bits()
        {
            self.cache = Cache {
                width: column,
                size: em,
                ..Cache::default()
            };
        }

        let mut paint = ChatPaint::default();
        self.hits.clear();

        // The box below, first, as the conversation gets what's left.
        let place = Place {
            origin,
            size,
            left,
            column,
        };
        let box_height = self.paint_box(fonts, &mut paint, m, place, caret);
        let status = self.status();
        let status_height = if status.is_some() { m.em(1.8) } else { 0.0 };
        let view_height = (size[1] - box_height - status_height - m.em(1.0)).max(0.0);
        if let Some((text, tone)) = status {
            let laid = self.note(fonts, m, &text, tone, column);
            let top = origin[1] + view_height + (status_height - laid.height) / 2.0;
            let x = origin[0] + left + (column - laid.width) / 2.0;
            let mut canvas = Canvas {
                paint: &mut paint,
                fonts,
                clip: [origin[0], origin[1], size[0], size[1]],
                em,
                over: false,
            };
            for block in &laid.blocks {
                canvas.text(&block.layout, [x, top + block.top]);
            }
            self.cache.notes.insert((text, tone), laid);
        }

        // The conversation, laid out from the top.
        let spans = self.lay_out_turns(fonts, m, column);
        let content_height = spans.last().map_or(0.0, |span| span.bottom) + m.em(1.5);
        self.view_height = view_height;
        self.max_scroll = (content_height - view_height).max(0.0);
        if self.follow {
            self.scroll = self.max_scroll;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll);
        let clip = [origin[0], origin[1], size[0], view_height];
        let top = origin[1] - self.scroll;
        let mut canvas = Canvas {
            paint: &mut paint,
            fonts,
            clip,
            em,
            over: false,
        };
        for span in &spans {
            // Out of view: nothing to draw.
            if top + span.bottom < origin[1] || top + span.top > origin[1] + view_height {
                continue;
            }
            self.draw_span(&mut canvas, m, span, [origin[0] + left, top], column);
        }
        if self.node.turns.is_empty() && matches!(self.node.state, ChatState::Ready) {
            let laid = self.note(canvas.fonts, m, EMPTY, Tone::Muted, column);
            let x = origin[0] + left + (column - laid.width) / 2.0;
            let y = origin[1] + (view_height - laid.height) / 2.0;
            for block in &laid.blocks {
                canvas.text(&block.layout, [x, y + block.top]);
            }
            self.cache
                .notes
                .insert((EMPTY.to_owned(), Tone::Muted), laid);
        }
        // The menu goes over everything, the box included.
        if let Some(anchor) = self.menu_anchor.take() {
            self.draw_menu(&mut paint, fonts, m, place, anchor);
        }
        paint
    }

    /// What's said just above the box, if anything: the agent starting,
    /// or failing to, or a notice.
    fn status(&self) -> Option<(String, Tone)> {
        match &self.node.state {
            ChatState::Starting => Some(("Starting the agent…".to_owned(), Tone::Muted)),
            ChatState::Failed(why) => {
                Some((format!("The agent couldn't start: {why}"), Tone::Warning))
            }
            _ => self.node.notice.clone().map(|notice| (notice, Tone::Muted)),
        }
    }

    /// Lays out every turn, as needed, and says where each piece goes.
    fn lay_out_turns(&mut self, fonts: &mut Fonts, m: Measures, column: f32) -> Vec<Span> {
        // Out while they're laid out, which changes the cache, and back
        // after: not copied, as a long chat is a lot to copy every frame.
        let turns = std::mem::take(&mut self.node.turns);
        let spans = self.place_turns(fonts, m, column, &turns);
        self.node.turns = turns;
        spans
    }

    fn place_turns(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        column: f32,
        turns: &[Turn],
    ) -> Vec<Span> {
        let mut spans = Vec::new();
        let mut y = m.em(1.5);
        let last = turns.len().checked_sub(1);
        let state = self.node.state.clone();
        for (index, turn) in turns.iter().enumerate() {
            if index > 0 {
                y += m.em(2.0);
            }
            if !turn.prompt.is_empty() {
                let laid = self.piece(
                    fonts,
                    m,
                    (index, 0),
                    &turn.prompt,
                    Look::Plain(Tone::Plain),
                    (column * 0.78 - m.em(2.0)).max(m.em(4.0)),
                );
                let height = laid.height + m.em(1.2);
                spans.push(Span {
                    piece: Piece::Prompt(index),
                    top: y,
                    bottom: y + height,
                });
                y += height + m.em(1.0);
            }
            for (item_index, item) in turn.items.iter().enumerate() {
                let key = (index, item_index + 1);
                let (text, look) = match item {
                    Item::Words(words) => (words.clone(), Look::Words),
                    Item::Thought(words) => (words.clone(), Look::Thought),
                    Item::Tool(tool) => (
                        tool_text(tool.status, &tool.title),
                        Look::Plain(tool_tone(tool.status)),
                    ),
                };
                if text.trim().is_empty() {
                    continue;
                }
                let laid = self.piece(fonts, m, key, &text, look, column);
                let gap = match item {
                    Item::Tool(_) => m.em(0.35),
                    _ => m.em(0.75),
                };
                if spans.last().is_some_and(|span: &Span| {
                    matches!(span.piece, Piece::Item(..)) && span.turn() == Some(index)
                }) {
                    y += gap;
                }
                spans.push(Span {
                    piece: Piece::Item(index, item_index + 1),
                    top: y,
                    bottom: y + laid.height,
                });
                y += laid.height;
            }
            // How it ended, or that it's still going.
            let note = match (&turn.end, Some(index) == last, &state) {
                (Some(end), _, _) => match &end.outcome {
                    Outcome::Done => None,
                    Outcome::Stopped => Some(("Stopped.".to_owned(), Tone::Muted)),
                    Outcome::Failed(why) => {
                        Some((format!("⚠ {}", capitalised(why)), Tone::Warning))
                    }
                },
                (None, true, ChatState::Starting) => {
                    Some(("Starting the agent…".to_owned(), Tone::Muted))
                }
                (None, true, _) if self.node.question.is_none() => {
                    Some(("Working…".to_owned(), Tone::Muted))
                }
                _ => None,
            };
            if let Some((text, tone)) = note {
                y += m.em(0.75);
                let laid = self.note(fonts, m, &text, tone, column);
                spans.push(Span {
                    piece: Piece::Note(text.clone(), tone),
                    top: y,
                    bottom: y + laid.height,
                });
                y += laid.height;
                self.cache.notes.insert((text, tone), laid);
            }
        }
        spans
    }

    /// A turn's piece, laid out, from the cache if its text is the same.
    fn piece(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        key: (usize, usize),
        text: &str,
        look: Look,
        width: f32,
    ) -> &Laid {
        let fresh = self
            .cache
            .pieces
            .get(&key)
            .is_none_or(|laid| laid.source != text);
        if fresh {
            let laid = lay_out(&mut self.layout_cx, fonts, m, text, look, width);
            self.cache.pieces.insert(key, laid);
        }
        &self.cache.pieces[&key]
    }

    /// Text of the chat's own, laid out.
    fn note(&mut self, fonts: &mut Fonts, m: Measures, text: &str, tone: Tone, width: f32) -> Laid {
        match self.cache.notes.remove(&(text.to_owned(), tone)) {
            Some(laid) => laid,
            None => lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                text,
                Look::Plain(tone),
                width,
            ),
        }
    }

    /// Draws one piece of the conversation, the column's left at
    /// `origin[0]`, the conversation's top at `origin[1]`.
    fn draw_span(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        span: &Span,
        origin: [f32; 2],
        column: f32,
    ) {
        let top = origin[1] + span.top;
        match &span.piece {
            Piece::Prompt(turn) => {
                let Some(laid) = self.cache.pieces.get(&(*turn, 0)) else {
                    return;
                };
                let (pad_x, pad_y) = (m.em(1.0), m.em(0.6));
                let width = laid.width + 2.0 * pad_x;
                let left = origin[0] + column - width;
                canvas.fill(Fill::rounded(
                    [left, top, width, span.bottom - span.top],
                    Paint::Raised,
                    m.em(1.1),
                ));
                for block in &laid.blocks {
                    canvas.text(
                        &block.layout,
                        [left + pad_x + block.left, top + pad_y + block.top],
                    );
                }
            }
            Piece::Item(turn, piece) => {
                if let Some(laid) = self.cache.pieces.get(&(*turn, *piece)) {
                    draw_laid(canvas, m, laid, [origin[0], top], column);
                }
            }
            Piece::Note(text, tone) => {
                if let Some(laid) = self.cache.notes.get(&(text.clone(), *tone)) {
                    draw_laid(canvas, m, laid, [origin[0], top], column);
                }
            }
        }
    }

    /// Draws the box a message is typed in, or the agent's question in
    /// its place, at the bottom of the pane. Returns its height, with the
    /// space below it.
    fn paint_box(
        &mut self,
        fonts: &mut Fonts,
        paint: &mut ChatPaint,
        m: Measures,
        place: Place,
        caret: bool,
    ) -> f32 {
        let pad = m.em(0.9);
        let inner = (place.column - 2.0 * pad).max(m.em(4.0));
        let question = self.lay_out_question(fonts, m, inner);
        let editor_height = if question.is_none() {
            self.editor.set_width(Some(inner));
            let layout = self.editor.layout(&mut fonts.font_cx, &mut self.layout_cx);
            layout.height().max(m.em(LINE))
        } else {
            0.0
        };
        let content_height = question
            .as_ref()
            .map_or(editor_height.min(m.em(LINE * EDITOR_LINES)), |question| {
                question.height
            });
        let height = pad + content_height + m.em(0.4) + m.em(2.0) + m.em(0.5);
        let frame = BoxFrame {
            place,
            left: place.origin[0] + place.left,
            top: place.origin[1] + place.size[1] - m.em(1.0) - height,
            height,
            inner,
            pad,
        };
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [
                place.origin[0],
                place.origin[1],
                place.size[0],
                place.size[1],
            ],
            em: m.em,
            over: false,
        };
        let bounds = [frame.left, frame.top, place.column, height];
        let shadow = [bounds[0], bounds[1] + m.em(0.15), bounds[2], bounds[3]];
        canvas.fill(Fill::rounded(shadow, Paint::Shadow, m.em(1.2)));
        canvas
            .fill(Fill::rounded(bounds, Paint::Raised, m.em(1.2)).bordered(1.0, Paint::RaisedEdge));
        match question {
            Some(question) => self.draw_question(&mut canvas, m, &frame, question),
            None => self.draw_editor(&mut canvas, m, &frame, editor_height, caret),
        }
        self.draw_send(&mut canvas, m, &frame);
        self.draw_controls(&mut canvas, m, &frame);
        height + m.em(1.0)
    }

    /// The agent's question, and its answers as buttons in rows, laid out
    /// `inner` wide, if it's asking one.
    fn lay_out_question(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        inner: f32,
    ) -> Option<QuestionLaid> {
        let question = self.node.question.clone()?;
        let title = format!("Allow the agent to {}?", question.title);
        let title = lay_out(
            &mut self.layout_cx,
            fonts,
            m,
            &format!("**{}**", escape(&title)),
            Look::Words,
            inner,
        );
        let (gap, height) = (m.em(0.5), m.em(2.0));
        let mut rows = height;
        let mut x = 0.0;
        let mut buttons = Vec::new();
        for (index, choice) in question.choices.iter().enumerate() {
            // The first answer that says yes is the one to press.
            let primary = index == 0 && choice.kind == ChoiceKind::AllowOnce;
            let tone = match choice.kind {
                _ if primary => Tone::OnAccent,
                ChoiceKind::AllowOnce | ChoiceKind::AllowAlways => Tone::Plain,
                ChoiceKind::RejectOnce | ChoiceKind::RejectAlways => Tone::Muted,
            };
            let text = format!("{}  {}", index + 1, choice.name);
            let laid = lay_out(
                &mut self.layout_cx,
                fonts,
                m,
                &text,
                Look::Plain(tone),
                inner,
            );
            let width = laid.width + m.em(1.6);
            if x > 0.0 && x + width > inner {
                x = 0.0;
                rows += height + gap;
            }
            buttons.push(Button {
                laid,
                id: choice.id.clone(),
                at: [x, rows - height],
                width,
                primary,
            });
            x += width + gap;
        }
        let height = title.height + m.em(0.7) + rows;
        Some(QuestionLaid {
            title,
            buttons,
            height,
        })
    }

    fn draw_question(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        question: QuestionLaid,
    ) {
        let origin = frame.text_origin();
        draw_laid(canvas, m, &question.title, origin, frame.inner);
        let top = origin[1] + question.title.height + m.em(0.7);
        for button in question.buttons {
            let rect = [
                origin[0] + button.at[0],
                top + button.at[1],
                button.width,
                m.em(2.0),
            ];
            canvas.fill(if button.primary {
                Fill::rounded(rect, Paint::Accent, m.em(1.0))
            } else {
                Fill::rounded(rect, Paint::Hover, m.em(1.0)).bordered(1.0, Paint::RaisedEdge)
            });
            let text_top = rect[1] + (rect[3] - button.laid.height) / 2.0;
            draw_laid(
                canvas,
                m,
                &button.laid,
                [rect[0] + m.em(0.8), text_top],
                button.width,
            );
            self.hits
                .push((Hit::Choice(button.id), frame.place.in_pane(rect)));
        }
    }

    /// Draws the message being typed, `height` tall laid out, with its
    /// selection and, with `caret`, its cursor.
    fn draw_editor(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        height: f32,
        caret: bool,
    ) {
        let origin = frame.text_origin();
        let visible = height.min(m.em(LINE * EDITOR_LINES));
        // A long message scrolls inside the box, to keep the cursor in
        // view.
        let cursor = (self.editor.cursor_geometry((m.em / 8.0).round().max(1.5)))
            .map(|cursor| rect_at(cursor, [0.0; 2]));
        let offset = cursor.map_or(0.0, |[_, top, _, cursor_height]| {
            (top + cursor_height - visible).clamp(0.0, (height - visible).max(0.0))
        });
        let clip = [origin[0] - 2.0, origin[1], frame.inner + 4.0, visible];
        let mut canvas = Canvas {
            paint: canvas.paint,
            fonts: canvas.fonts,
            clip,
            em: m.em,
            over: false,
        };
        let shown = [origin[0], origin[1] - offset];
        let [left, top, _, _] = frame.place.in_pane([shown[0], shown[1], 0.0, 0.0]);
        self.editor_origin = [left, top];
        self.hits.push((
            Hit::Editor,
            frame
                .place
                .in_pane([origin[0], origin[1], frame.inner, visible]),
        ));
        for (bounds, _) in self.editor.selection_geometry() {
            canvas.fill(Fill::solid(rect_at(bounds, shown), Paint::Picked));
        }
        if self.editor.raw_text().is_empty() {
            let laid = self.note(canvas.fonts, m, PLACEHOLDER, Tone::Muted, frame.inner);
            for block in &laid.blocks {
                canvas.text(&block.layout, [shown[0], shown[1] + block.top]);
            }
            self.cache
                .notes
                .insert((PLACEHOLDER.to_owned(), Tone::Muted), laid);
        } else if let Some(layout) = self.editor.try_layout() {
            canvas.text(layout, shown);
        }
        // The cursor goes over the text.
        if let Some([x, y, width, cursor_height]) = cursor {
            let rect = [shown[0] + x, shown[1] + y, width.max(1.5), cursor_height];
            canvas.paint.ime = Some(rect);
            if caret {
                canvas
                    .paint
                    .over
                    .push(Fill::solid(rect, Paint::Accent).clipped(clip));
            }
        }
    }

    /// Draws the button that sends the message, or stops the agent while
    /// it answers.
    fn draw_send(&mut self, canvas: &mut Canvas<'_>, m: Measures, frame: &BoxFrame) {
        let busy = self.busy();
        let ready = !self.editor.raw_text().trim().is_empty() && self.node.question.is_none();
        let (hit, label) = if busy {
            (Hit::Stop, "■")
        } else {
            (Hit::Send, "↑")
        };
        let lit = busy || ready;
        let size = m.em(2.0);
        let rect = [
            frame.left + frame.place.column - frame.pad * 0.7 - size,
            frame.top + frame.height - m.em(0.5) - size,
            size,
            size,
        ];
        canvas.fill(Fill::rounded(
            rect,
            if lit { Paint::Accent } else { Paint::Hover },
            size / 2.0,
        ));
        let tone = if lit { Tone::OnAccent } else { Tone::Muted };
        let laid = lay_out(
            &mut self.layout_cx,
            canvas.fonts,
            m,
            label,
            Look::Plain(tone),
            size,
        );
        let text = [
            rect[0] + (size - laid.width) / 2.0,
            rect[1] + (size - laid.height) / 2.0,
        ];
        draw_laid(canvas, m, &laid, text, size);
        self.hits.push((hit, frame.place.in_pane(rect)));
    }
}

impl ChatView {
    /// Draws the box's controls along its bottom: the mode on the left,
    /// and on the right, by the send button, how much context the chat
    /// fills and the model and effort.
    fn draw_controls(&mut self, canvas: &mut Canvas<'_>, m: Measures, frame: &BoxFrame) {
        let size = m.em(2.0);
        let top = frame.top + frame.height - m.em(0.5) - size;
        let mut right = frame.left + frame.place.column - frame.pad * 0.7 - size - m.em(0.4);
        let of = |kind| {
            (self.node.settings.iter())
                .find(|setting| setting.kind == kind)
                .cloned()
        };
        let (mode, model, effort) = (
            of(SettingKind::Mode),
            of(SettingKind::Model),
            of(SettingKind::Effort),
        );

        let picked: Vec<Setting> = model.into_iter().chain(effort).collect();
        if !picked.is_empty() {
            let label = picked
                .iter()
                .map(chosen_name)
                .collect::<Vec<_>>()
                .join(" · ");
            let laid = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                &format!("{label}  ▾"),
                Look::Plain(Tone::Muted),
                frame.inner,
            );
            let width = laid.width + m.em(1.2);
            let rect = [right - width, top, width, size];
            let ids = picked.into_iter().map(|setting| setting.id).collect();
            self.draw_chip(canvas, m, frame, rect, &laid, ids);
            right = rect[0] - m.em(0.6);
        }
        if let Some(usage) = self.node.usage {
            let text = format!("{} / {}", tokens(usage.used), tokens(usage.size));
            let laid = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                &text,
                Look::Plain(Tone::Muted),
                frame.inner,
            );
            let at = [right - laid.width, top + (size - laid.height) / 2.0];
            draw_laid(canvas, m, &laid, at, laid.width);
        }
        if let Some(mode) = mode {
            let label = format!("{}  ▾", chosen_name(&mode));
            let laid = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                &label,
                Look::Plain(Tone::Muted),
                frame.inner,
            );
            let width = laid.width + m.em(1.2);
            let rect = [frame.left + frame.pad * 0.4, top, width, size];
            self.draw_chip(canvas, m, frame, rect, &laid, vec![mode.id]);
        }
    }

    /// Draws a chip that opens the menu for `settings`, lit while it's
    /// open, which then opens from it.
    fn draw_chip(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
        laid: &Laid,
        settings: Vec<String>,
    ) {
        let open = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.settings == settings);
        // The menu opens above the box, lined up with the chip.
        if open {
            canvas.fill(Fill::rounded(rect, Paint::Hover, m.em(1.0)));
            self.menu_anchor = Some([rect[0], frame.top, rect[2], rect[3]]);
        }
        let at = [rect[0] + m.em(0.6), rect[1] + (rect[3] - laid.height) / 2.0];
        draw_laid(canvas, m, laid, at, rect[2]);
        self.hits
            .push((Hit::Chip(settings), frame.place.in_pane(rect)));
    }

    /// Draws one of a menu's choices in `rect`: lit if the keyboard or the
    /// mouse is on it, ticked if it's the setting's value.
    fn draw_choice(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        rect: [f32; 4],
        choice: &MenuChoice<'_>,
    ) {
        if choice.lit {
            canvas.fill(Fill::rounded(rect, Paint::Hover, m.em(0.5)));
        }
        let mark = m.em(1.6);
        let left = rect[0] + m.em(0.4);
        let top = rect[1] + (rect[3] - choice.name.height) / 2.0;
        if choice.current {
            let tick = lay_out(
                &mut self.layout_cx,
                canvas.fonts,
                m,
                "✓",
                Look::Plain(Tone::Accent),
                mark,
            );
            draw_laid(canvas, m, &tick, [left, top], mark);
        }
        draw_laid(canvas, m, choice.name, [left + mark, top], rect[2]);
        if let Some(description) = choice.description {
            let at = [left + mark + choice.name.width + m.em(1.2), top];
            draw_laid(canvas, m, description, at, rect[2]);
        }
    }

    /// The rows of `menu`, laid out at most `wide`: each setting's
    /// choices, under its name when there are several.
    fn menu_rows(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        menu: &Menu,
        wide: f32,
    ) -> Vec<MenuRow> {
        let settings: Vec<Setting> = (menu.settings.iter())
            .filter_map(|id| self.setting(id).cloned())
            .collect();
        let mut rows = Vec::new();
        let mut text = |text: &str, tone| {
            lay_out(&mut self.layout_cx, fonts, m, text, Look::Plain(tone), wide)
        };
        for setting in &settings {
            if settings.len() > 1 {
                rows.push(MenuRow::Heading(text(&setting.name, Tone::Muted)));
            }
            for choice in &setting.choices {
                let description = (choice.description.as_deref())
                    .filter(|description| !description.is_empty())
                    .map(|description| text(description, Tone::Muted));
                rows.push(MenuRow::Choice {
                    setting: setting.id.clone(),
                    value: choice.value.clone(),
                    name: text(&choice.name, Tone::Plain),
                    description,
                    current: choice.value == setting.value,
                });
            }
        }
        rows
    }

    /// Draws the settings menu, opening upward from `anchor`: the left of
    /// the chip that opened it, and the top of the box.
    fn draw_menu(
        &mut self,
        paint: &mut ChatPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        anchor: [f32; 4],
    ) {
        let Some(menu) = self.menu.clone() else {
            return;
        };
        let rows = self.menu_rows(fonts, m, &menu, place.size[0]);
        let (row, heading, pad, mark) = (m.em(1.9), m.em(1.6), m.em(0.4), m.em(1.6));
        let width = (rows.iter())
            .map(|row| match row {
                MenuRow::Heading(laid) => laid.width,
                MenuRow::Choice {
                    name, description, ..
                } => {
                    name.width
                        + description
                            .as_ref()
                            .map_or(0.0, |laid| m.em(1.2) + laid.width)
                }
            })
            .fold(0.0, f32::max)
            + mark
            + m.em(1.4);
        let height: f32 = 2.0 * pad
            + (rows.iter())
                .map(|menu_row| match menu_row {
                    MenuRow::Heading(_) => heading,
                    MenuRow::Choice { .. } => row,
                })
                .sum::<f32>();
        let width = width.min(place.size[0] - m.em(1.0));
        let left = anchor[0].clamp(
            place.origin[0] + m.em(0.5),
            (place.origin[0] + place.size[0] - width - m.em(0.5)).max(place.origin[0]),
        );
        let bottom = anchor[1] - m.em(0.4);
        let top = (bottom - height).max(place.origin[1] + m.em(0.5));
        let bounds = [left, top, width, bottom - top];
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [
                place.origin[0],
                place.origin[1],
                place.size[0],
                place.size[1],
            ],
            em: m.em,
            over: true,
        };
        canvas.fill(Fill::rounded(
            [bounds[0], bounds[1] + m.em(0.2), bounds[2], bounds[3]],
            Paint::Shadow,
            m.em(0.8),
        ));
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(0.8)).bordered(1.0, Paint::PaletteEdge),
        );
        canvas.clip = bounds;
        let mut y = top + pad;
        for menu_row in rows {
            match menu_row {
                MenuRow::Heading(laid) => {
                    draw_laid(
                        &mut canvas,
                        m,
                        &laid,
                        [left + m.em(0.8), y + (heading - laid.height) / 2.0],
                        width,
                    );
                    y += heading;
                }
                MenuRow::Choice {
                    setting,
                    value,
                    name,
                    description,
                    current,
                } => {
                    let rect = [left + pad, y, width - 2.0 * pad, row];
                    let lit = menu.highlighted.as_ref() == Some(&(setting.clone(), value.clone()));
                    let choice = MenuChoice {
                        name: &name,
                        description: description.as_ref(),
                        current,
                        lit,
                    };
                    self.draw_choice(&mut canvas, m, rect, &choice);
                    self.hits
                        .push((Hit::Pick { setting, value }, place.in_pane(rect)));
                    y += row;
                }
            }
        }
    }
}

/// A setting's choice as a chip says it: the name of the choice it has.
fn chosen_name(setting: &Setting) -> String {
    setting
        .current()
        .map_or_else(|| setting.value.clone(), |choice| choice.name.clone())
}

/// A count of tokens, short: "830", "38k", "1M", "1.2M".
fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", (count + 500) / 1_000),
        _ => {
            let tenths = (count + 50_000) / 100_000;
            if tenths.is_multiple_of(10) {
                format!("{}M", tenths / 10)
            } else {
                format!("{}.{}M", tenths / 10, tenths % 10)
            }
        }
    }
}

/// One of a menu's choices, to draw.
struct MenuChoice<'a> {
    name: &'a Laid,
    description: Option<&'a Laid>,
    /// It's the setting's value now.
    current: bool,
    /// The keyboard or the mouse is on it.
    lit: bool,
}

/// One row of the settings menu.
enum MenuRow {
    /// A section's heading: its setting's name.
    Heading(Laid),
    /// A choice: its setting and value, laid out, with its description,
    /// and whether it's the setting's value now.
    Choice {
        setting: String,
        value: String,
        name: Laid,
        description: Option<Laid>,
        current: bool,
    },
}

/// What the box says while nothing's typed.
const PLACEHOLDER: &str = "Message the agent";

/// What a chat with no turns says.
const EMPTY: &str = "Send a message to start.";

/// Where a chat is drawn: its pane, and the column of turns in it.
#[derive(Clone, Copy)]
struct Place {
    /// The pane's top left, and its size.
    origin: [f32; 2],
    size: [f32; 2],
    /// The column's left, from the pane's, and its width.
    left: f32,
    column: f32,
}

impl Place {
    /// `rect`, from the pane's top left rather than the window's.
    fn in_pane(self, [x, y, width, height]: [f32; 4]) -> [f32; 4] {
        [x - self.origin[0], y - self.origin[1], width, height]
    }
}

/// Where the box goes.
struct BoxFrame {
    place: Place,
    left: f32,
    top: f32,
    height: f32,
    /// How wide its text is.
    inner: f32,
    pad: f32,
}

impl BoxFrame {
    /// Where its text starts.
    fn text_origin(&self) -> [f32; 2] {
        [self.left + self.pad, self.top + self.pad]
    }
}

/// The agent's question, laid out.
struct QuestionLaid {
    title: Laid,
    buttons: Vec<Button>,
    /// The title's height and the buttons' together.
    height: f32,
}

/// One of the answers to the agent's question, as a button.
struct Button {
    laid: Laid,
    /// The answer's ID.
    id: String,
    /// Where it goes, from below the title.
    at: [f32; 2],
    width: f32,
    /// It's the one to press, in the accent colour.
    primary: bool,
}

/// `bounds`, as parley gives them, as a rectangle from `origin`: left,
/// top, width and height.
#[allow(
    clippy::cast_possible_truncation,
    reason = "pixels in a pane, far inside f32's range"
)]
fn rect_at(bounds: parley::BoundingBox, origin: [f32; 2]) -> [f32; 4] {
    [
        origin[0] + bounds.x0 as f32,
        origin[1] + bounds.y0 as f32,
        (bounds.x1 - bounds.x0) as f32,
        (bounds.y1 - bounds.y0) as f32,
    ]
}

/// One piece of the conversation and where it goes, from the top.
struct Span {
    piece: Piece,
    top: f32,
    bottom: f32,
}

impl Span {
    fn turn(&self) -> Option<usize> {
        match self.piece {
            Piece::Prompt(turn) | Piece::Item(turn, _) => Some(turn),
            Piece::Note(..) => None,
        }
    }
}

enum Piece {
    /// A turn's message, as a bubble.
    Prompt(usize),
    /// What the agent did, by turn and piece.
    Item(usize, usize),
    /// Text of the chat's own.
    Note(String, Tone),
}

/// Draws `laid` with the column's left at `origin[0]`, its top at
/// `origin[1]`: its blocks, and what goes with them.
fn draw_laid(canvas: &mut Canvas<'_>, m: Measures, laid: &Laid, origin: [f32; 2], column: f32) {
    for block in &laid.blocks {
        let x = origin[0] + block.left;
        let y = origin[1] + block.top;
        let height = block.layout.height();
        match &block.kind {
            // From a little left of the text to the column's right edge.
            BlockKind::Code => canvas.fill(Fill::rounded(
                [
                    x - m.em(0.6),
                    y - m.em(0.4),
                    column - block.left + m.em(0.6),
                    height + m.em(0.8),
                ],
                Paint::Code,
                m.em(0.5),
            )),
            BlockKind::Quote => canvas.fill(Fill::solid(
                [x - m.em(0.9), y, m.em(0.2).max(2.0), height],
                Paint::RaisedEdge,
            )),
            BlockKind::Rule => canvas.fill(Fill::solid(
                [x, y + m.em(0.5), column - block.left, 1.0],
                Paint::Divider,
            )),
            _ => {}
        }
        if let Some((marker, left)) = &block.marker {
            canvas.text(marker, [origin[0] + left, y]);
        }
        canvas.text(&block.layout, [x, y]);
    }
}

/// Lays out `text` as `look` says, `width` wide.
fn lay_out(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    m: Measures,
    text: &str,
    look: Look,
    width: f32,
) -> Laid {
    let blocks = match look {
        Look::Words | Look::Thought => markdown::blocks(text),
        Look::Plain(_) => vec![markdown::Block {
            kind: BlockKind::Paragraph,
            text: text.to_owned(),
            spans: Vec::new(),
            indent: 0,
        }],
    };
    let mut laid = Laid {
        source: text.to_owned(),
        blocks: Vec::new(),
        height: 0.0,
        width: 0.0,
    };
    let step = m.em(1.5);
    for (index, block) in blocks.into_iter().enumerate() {
        if index > 0 {
            laid.height += match block.kind {
                BlockKind::Item(_) => m.em(0.3),
                BlockKind::Code => m.em(1.1),
                _ => m.em(0.7),
            };
        }
        let indent = f32::from(block.indent) * step
            + match block.kind {
                BlockKind::Item(_) | BlockKind::Quote => step,
                _ => 0.0,
            };
        let size = match block.kind {
            BlockKind::Heading(1) => m.em * 1.35,
            BlockKind::Heading(2) => m.em * 1.2,
            BlockKind::Heading(_) => m.em * 1.08,
            BlockKind::Code => m.em * 0.93,
            _ => m.em,
        };
        let layout = lay_out_block(
            layout_cx,
            fonts,
            &block,
            look,
            size,
            (width - indent).max(m.em(4.0)),
        );
        let marker = match &block.kind {
            BlockKind::Item(marker) => {
                let layout = lay_out_marker(layout_cx, fonts, marker, size);
                let left = (indent - layout.width() - m.em(0.5)).max(0.0);
                Some((layout, left))
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
            kind: block.kind,
            left: indent,
            top: laid.height,
            marker,
        });
        laid.height += height;
    }
    laid
}

/// Lays out one block, its text `size` pixels, `width` wide.
fn lay_out_block(
    layout_cx: &mut parley::LayoutContext<Brush>,
    fonts: &mut Fonts,
    block: &markdown::Block,
    look: Look,
    size: f32,
    width: f32,
) -> Layout<Brush> {
    let tone = match look {
        Look::Words => Tone::Plain,
        Look::Thought => Tone::Muted,
        Look::Plain(tone) => tone,
    };
    let (body, mono) = (body_style(fonts, size), mono_family(fonts));
    let mut builder = layout_cx.ranged_builder(&mut fonts.font_cx, &block.text, 1.0, true);
    for property in body {
        builder.push_default(property);
    }
    builder.push_default(StyleProperty::Brush(Brush { tone, code: false }));
    if matches!(look, Look::Thought) {
        builder.push_default(StyleProperty::FontStyle(FontStyle::Italic));
    }
    match block.kind {
        BlockKind::Heading(_) => builder.push_default(StyleProperty::FontWeight(FontWeight::BOLD)),
        BlockKind::Code => {
            builder.push_default(StyleProperty::FontFamily(mono.clone()));
            builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
                1.35,
            )));
        }
        BlockKind::Quote => builder.push_default(StyleProperty::Brush(Brush {
            tone: Tone::Muted,
            code: false,
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
            builder.push(StyleProperty::Brush(Brush { tone, code: true }), range);
        } else if style.link {
            let brush = Brush {
                tone: Tone::Accent,
                code: false,
            };
            builder.push(StyleProperty::Brush(brush), range);
        }
    }
    let mut layout = builder.build(&block.text);
    // Code keeps its lines, however long.
    layout.break_all_lines((block.kind != BlockKind::Code).then_some(width));
    layout.align(Alignment::Start, AlignmentOptions::default());
    layout
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
    }));
    let mut layout = builder.build(marker);
    layout.break_all_lines(None);
    layout.align(Alignment::Start, AlignmentOptions::default());
    layout
}

/// A tool call's line: a mark for how it's going, and its title.
fn tool_text(status: ToolStatus, title: &str) -> String {
    let mark = match status {
        ToolStatus::Running => "○",
        ToolStatus::Done => "✓",
        ToolStatus::Failed => "✗",
    };
    format!("{mark}  {title}")
}

fn tool_tone(status: ToolStatus) -> Tone {
    match status {
        ToolStatus::Failed => Tone::Warning,
        _ => Tone::Muted,
    }
}

/// `text` with its first letter capital.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// `text` with what Markdown would read as its own escaped.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if "\\`*_{}[]()#+-.!|<>~".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontOptions;

    #[test]
    fn characters_the_interface_font_lacks_come_from_the_fonts_behind_it() {
        let mut fonts = Fonts::load(&FontOptions::default(), 1.0).expect("a font");
        // Noto Sans has no ▾. Without it here, there's nothing to check.
        fonts.load_ui(Some("Noto Sans"), 11.0, 1.0);
        if fonts.ui_family.as_deref() != Some("Noto Sans") {
            return;
        }
        let mut layout_cx = parley::LayoutContext::new();
        let laid = lay_out(
            &mut layout_cx,
            &mut fonts,
            Measures::of(16.0),
            "Auto ▾ ✓ ✗ ○",
            Look::Plain(Tone::Plain),
            400.0,
        );
        let layout = &laid.blocks[0].layout;
        for line in layout.lines() {
            for item in line.items() {
                if let PositionedLayoutItem::GlyphRun(run) = item {
                    // Glyph 0 is a font's box for what it hasn't got.
                    assert!(run.glyphs().all(|glyph| glyph.id != 0));
                }
            }
        }
    }
}
