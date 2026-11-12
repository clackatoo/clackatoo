//! A shelf above the box a message is typed in: a raised card with a
//! heading line, then a line for each thing it lists, each with a button
//! at its end if it has one. What the agent runs in the background, its
//! plan and the MCP servers to sign in to are drawn as shelves, stacked
//! up from the box.

use super::{BoxFrame, Canvas, ChatView, Fill, Hit, Measures, Paint, Tone};

/// What a shelf shows.
pub(super) struct Shelf {
    /// Its heading, in the muted colour.
    pub(super) heading: String,
    /// What a click on the heading does, if anything: opening or folding
    /// the shelf.
    pub(super) heading_hit: Option<Hit>,
    pub(super) rows: Vec<ShelfRow>,
}

/// A line of a shelf.
pub(super) struct ShelfRow {
    pub(super) text: String,
    pub(super) tone: Tone,
    /// The square button at the line's end, which stops what it's for,
    /// and what a click on it does.
    pub(super) stop: Option<Hit>,
    /// What a click on the rest of the line does, if anything: it's lit
    /// while the mouse is over it.
    pub(super) open: Option<Hit>,
    /// A button at the line's end with words on it, and what a click on
    /// it does: gold if it does something, quiet if not.
    pub(super) button: Option<(String, Option<Hit>)>,
}

/// The height of a shelf's line, its heading's, and the pad above and
/// below them, in ems.
const ROW: f32 = 1.9;
const HEADER: f32 = 1.65;
const PAD: f32 = 0.45;

/// How many lines a shelf has room for within `height`, heading and all.
pub(super) fn rows_within(m: Measures, height: f32) -> usize {
    let room = height - m.em(HEADER) - m.em(PAD) * 2.0;
    // Whole lines only, and never more than a shelf could list.
    (0..u8::MAX)
        .take_while(|&rows| m.em(ROW) * f32::from(rows + 1) <= room)
        .count()
}

impl ChatView {
    /// Draws `card` above the box and whatever's above it already,
    /// `above` high. Returns how high it is, with the gap above it.
    pub(super) fn draw_shelf(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        above: f32,
        card: Shelf,
    ) -> f32 {
        let row = m.em(ROW);
        let header = m.em(HEADER);
        let pad = m.em(PAD);
        let gap = m.em(0.45);
        let rows = f32::from(u8::try_from(card.rows.len()).unwrap_or(u8::MAX));
        let panel = header + row * rows + pad * 2.0;
        let mut top = frame.top - above - panel - gap;
        let left = frame.left + m.em(0.65);
        let width = frame.place.column - m.em(1.3);
        let bounds = [left, top, width, panel];
        canvas
            .fill(Fill::rounded(bounds, Paint::Raised, m.em(0.9)).bordered(1.0, Paint::RaisedEdge));
        let inner_left = left + m.em(0.8);
        let inner_width = width - m.em(1.6);
        top += pad;
        let small = Measures::of(m.em(0.85));
        if let Some(hit) = card.heading_hit {
            // The whole heading line takes the click, pad and all.
            let line = [left, top - pad, width, header + pad];
            self.hits.push((hit, frame.place.in_pane(line)));
        }
        self.queue_text(
            canvas,
            small,
            &card.heading,
            [inner_left, top, inner_width, header],
            Tone::Muted,
        );
        top += header;
        for (index, line) in card.rows.into_iter().enumerate() {
            if index > 0 {
                canvas.fill(Fill::solid(
                    [inner_left, top, inner_width, 1.0],
                    Paint::RaisedEdge,
                ));
            }
            let button = m.em(1.8);
            let label = (line.button.as_ref()).map(|(label, hit)| {
                let tone = if hit.is_some() {
                    Tone::Accent
                } else {
                    Tone::Muted
                };
                self.short_label(canvas.fonts, small, label, inner_width / 2.0, tone)
            });
            let pill = label.as_ref().map_or(0.0, |label| label.width + m.em(1.4));
            let text_width = if line.stop.is_some() {
                (inner_width - button - m.em(0.5)).max(0.0)
            } else if label.is_some() {
                (inner_width - pill - m.em(0.5)).max(0.0)
            } else {
                inner_width
            };
            if let Some(hit) = line.open {
                let rect = [inner_left - m.em(0.3), top, text_width + m.em(0.3), row];
                if (self.mouse)
                    .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p))
                {
                    canvas.fill(Fill::rounded(rect, Paint::ChatHover, m.em(0.4)));
                }
                self.hits.push((hit, frame.place.in_pane(rect)));
            }
            self.queue_text(
                canvas,
                small,
                &line.text,
                [inner_left, top, text_width, row],
                line.tone,
            );
            if let Some(hit) = line.stop {
                let rect = [
                    inner_left + inner_width - button,
                    top + (row - button) / 2.0,
                    button,
                    button,
                ];
                self.draw_stop_button(canvas, m, frame, rect, hit);
            }
            if let (Some(label), Some((_, hit))) = (label, line.button) {
                let height = m.em(1.45);
                let rect = [
                    inner_left + inner_width - pill,
                    top + (row - height) / 2.0,
                    pill,
                    height,
                ];
                self.draw_text_button(canvas, small, frame, rect, label, hit);
            }
            top += row;
        }
        panel + gap
    }

    /// Draws a button with `label` on it in `rect`, as Settings draws
    /// one: gold if a click on it does something, `hit`, quiet if not.
    fn draw_text_button(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
        label: super::Laid,
        hit: Option<Hit>,
    ) {
        let radius = rect[3] / 2.0;
        canvas.fill(match hit {
            Some(_) => {
                Fill::rounded(rect, Paint::AccentFaint, radius).bordered(1.0, Paint::AccentEdge)
            }
            None => Fill::rounded(rect, Paint::Clear, radius).bordered(1.0, Paint::RaisedEdge),
        });
        if let Some(hit) = hit {
            if (self.mouse).is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p)) {
                canvas.fill(Fill::rounded(rect, Paint::ChatHover, radius));
            }
            self.hits.push((hit, frame.place.in_pane(rect)));
        }
        let x = rect[0] + (rect[2] - label.width) / 2.0;
        super::draw_laid(
            canvas,
            m,
            &label,
            [x, rect[1] + (rect[3] - label.height) / 2.0],
            label.width,
        );
        self.keep_label(label);
    }

    /// Draws a button that stops something, in `rect`: the square the
    /// chat's own Stop is.
    fn draw_stop_button(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
        hit: Hit,
    ) {
        let side = rect[2];
        if self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p))
        {
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, side / 2.0));
        }
        let extent = m.em(0.5);
        canvas.fill(Fill::rounded(
            [
                rect[0] + (side - extent) / 2.0,
                rect[1] + (side - extent) / 2.0,
                extent,
                extent,
            ],
            Paint::Icon,
            m.em(0.1),
        ));
        self.hits.push((hit, frame.place.in_pane(rect)));
    }
}
