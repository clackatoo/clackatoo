//! The completion menus over the box a message is typed in, as wide as
//! the box: the `/` menu, a line for each command, `/name`, what to type
//! after it, if anything, and what it does, cut to fit, those the window
//! does itself saying so at the line's end (see [`crate::chat::slash`]);
//! and the `@` menu, a line for each file (see
//! [`crate::chat::mentions`]). Both are drawn the same way: the one picked
//! is lit, and a click picks one and puts it in.

use super::{Canvas, ChatView, Fill, Hit, Measures, Paint, Place, Tone};
use crate::chat::slash::{HEIGHT, Item};
use crate::fonts::Fonts;
use crate::markdown::layout::{TextPaint, draw_laid};

/// A line's height, and the pad above and below them, in ems.
const ROW: f32 = 1.75;
const PAD: f32 = 0.35;

/// A line of a completion menu.
struct Row {
    name: String,
    /// What to type after it, if anything.
    hint: Option<String>,
    description: String,
    /// Whether the window does it, rather than the agent.
    ours: bool,
    picked: bool,
}

impl From<(Item, Vec<u32>, bool)> for Row {
    fn from((item, _, picked): (Item, Vec<u32>, bool)) -> Self {
        Self {
            name: format!("/{}", item.name),
            hint: item.hint,
            description: item.description,
            ours: item.ours,
            picked,
        }
    }
}

impl ChatView {
    /// Draws the `/` menu, if it shows, upward from just above the box,
    /// whose top is `box_top`.
    pub(super) fn draw_slash(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        box_top: f32,
    ) {
        if !self.slash.showing() {
            return;
        }
        let (rows, count) = self.slash.in_view(HEIGHT);
        let rows = rows.into_iter().map(Row::from).collect();
        let said = format!("{count} commands: ↑↓ for more");
        self.draw_completions(
            paint,
            fonts,
            (m, place, box_top),
            (rows, count, said),
            Hit::Slash,
        );
    }

    /// Draws the `@` menu, if it shows, as the `/` menu is drawn.
    pub(super) fn draw_mentions(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        box_top: f32,
    ) {
        if !self.mentions.showing() {
            return;
        }
        let (rows, count) = self.mentions.in_view(crate::chat::mentions::HEIGHT);
        let rows = (rows.into_iter())
            .map(|(name, picked)| Row {
                name,
                hint: None,
                description: String::new(),
                ours: false,
                picked,
            })
            .collect();
        let said = format!("{count} files: ↑↓ for more");
        self.draw_completions(
            paint,
            fonts,
            (m, place, box_top),
            (rows, count, said),
            Hit::Mention,
        );
    }

    /// Draws a completion menu of `rows`, of `count` in all, saying
    /// `more` if there are more than show, upward from just above the
    /// box, whose top is `box_top`; each line clicked is `hit` of its
    /// place in view.
    fn draw_completions(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        (m, place, box_top): (Measures, Place, f32),
        (rows, count, more_said): (Vec<Row>, usize, String),
        hit: fn(usize) -> Hit,
    ) {
        if rows.is_empty() {
            return;
        }
        let row = m.em(ROW);
        let pad = m.em(PAD);
        let lines = f32::from(u8::try_from(rows.len()).unwrap_or(u8::MAX));
        let more = count > rows.len();
        let height = pad * 2.0 + row * (lines + f32::from(u8::from(more)));
        let left = place.origin[0] + place.left + m.em(0.65);
        let width = (place.column - m.em(1.3)).max(m.em(4.0));
        let bottom = box_top - m.em(0.45);
        let top = (bottom - height).max(place.origin[1] + m.em(0.25));
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
        self.hits.push((Hit::MenuSurface, place.in_pane(bounds)));
        let inner = left + m.em(0.7);
        let room = width - m.em(1.4);
        let mut y = top + pad;
        for (index, item) in rows.into_iter().enumerate() {
            let rect = [left + pad, y, width - 2.0 * pad, row];
            if item.picked {
                canvas.fill(Fill::rounded(rect, Paint::Chosen, m.em(0.5)));
            }
            self.hits.push((hit(index), place.in_pane(rect)));
            self.draw_slash_row(&mut canvas, m, &item, [inner, y, room, row]);
            y += row;
        }
        if more {
            let mut x = inner;
            self.slash_label(
                &mut canvas,
                m,
                (&more_said, Tone::Muted),
                [inner, y, room, row],
                &mut x,
            );
        }
    }

    /// Draws `item`'s line in `line`, its left, top, width and height:
    /// its name, the hint of what to type after it, and what it does,
    /// cut to fit; and if the window does it, says so at the end, which
    /// the rest leaves room for.
    fn draw_slash_row(&mut self, canvas: &mut Canvas<'_>, m: Measures, item: &Row, line: [f32; 4]) {
        let [inner, top, room, row] = line;
        let tag = if item.ours { m.em(5.5) } else { 0.0 };
        let end = inner + room - tag;
        let mut x = inner;
        // A line with nothing said after its name has all the room.
        let share = if item.description.is_empty() && item.hint.is_none() {
            1.0
        } else {
            0.6
        };
        self.slash_label(
            canvas,
            m,
            (&item.name, Tone::Plain),
            [x, top, (end - x) * share, row],
            &mut x,
        );
        if let Some(hint) = &item.hint {
            x += m.em(0.4);
            let hint = format!("‹{hint}›");
            self.slash_label(
                canvas,
                m,
                (&hint, Tone::Muted),
                [x, top, (end - x) * 0.5, row],
                &mut x,
            );
        }
        x += m.em(0.9);
        let description = (&*item.description, Tone::Muted);
        self.slash_label(canvas, m, description, [x, top, end - x, row], &mut x);
        if item.ours {
            let mut at = end + m.em(0.5);
            let rect = [at, top, tag - m.em(0.5), row];
            self.slash_label(canvas, m, ("clackatoo", Tone::Muted), rect, &mut at);
        }
    }

    /// Draws `text` in `tone`, a little smaller than the chat's text, in
    /// the middle of `rect`, cut to its width, and moves `x` past it.
    /// Nothing, if it's empty or there's next to no room.
    fn slash_label(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        (text, tone): (&str, Tone),
        rect: [f32; 4],
        x: &mut f32,
    ) {
        let [left, top, wide, height] = rect;
        if wide < m.em(1.0) || text.is_empty() {
            return;
        }
        let small = Measures::of(m.em(0.9));
        let laid = self.short_label(canvas.fonts, small, text, wide, tone);
        draw_laid(
            canvas,
            small,
            &laid,
            [left, top + (height - laid.height) / 2.0],
            wide,
        );
        *x = left + laid.width;
        self.keep_label(laid);
    }
}
