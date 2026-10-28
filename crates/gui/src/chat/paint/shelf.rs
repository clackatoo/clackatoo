//! A shelf above the box a message is typed in: a raised card with a
//! heading line, then a line for each thing it lists, each with a button
//! at its end if it has one. What the agent runs in the background and
//! its plan are drawn as shelves, stacked up from the box.

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
}

/// The height of a shelf's line, its heading's, and the pad above and
/// below them, in ems.
const ROW: f32 = 1.9;
const HEADER: f32 = 1.65;
const PAD: f32 = 0.45;

/// How many lines a shelf has room for within `height`, heading and all.
pub(super) fn rows_within(m: Measures, height: f32) -> usize {
    let room = (height - m.em(HEADER) - m.em(PAD) * 2.0) / m.em(ROW);
    // Whole lines only; a negative or NaN room is none.
    if room >= 1.0 {
        room as usize
    } else {
        0
    }
}

impl ChatView {
    /// Draws `shelf` above the box and whatever's above it already,
    /// `above` high. Returns how high it is, with the gap above it.
    pub(super) fn draw_shelf(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        above: f32,
        shelf: Shelf,
    ) -> f32 {
        let row = m.em(ROW);
        let header = m.em(HEADER);
        let pad = m.em(PAD);
        let gap = m.em(0.45);
        let rows = f32::from(u8::try_from(shelf.rows.len()).unwrap_or(u8::MAX));
        let panel = header + row * rows + pad * 2.0;
        let mut top = frame.top - above - panel - gap;
        let left = frame.left + m.em(0.65);
        let width = frame.place.column - m.em(1.3);
        let card = [left, top, width, panel];
        canvas.fill(
            Fill::rounded(card, Paint::Raised, m.em(0.9)).bordered(1.0, Paint::RaisedEdge),
        );
        let inner_left = left + m.em(0.8);
        let inner_width = width - m.em(1.6);
        top += pad;
        let small = Measures::of(m.em(0.85));
        if let Some(hit) = shelf.heading_hit {
            // The whole heading line takes the click, pad and all.
            let line = [left, top - pad, width, header + pad];
            self.hits.push((hit, frame.place.in_pane(line)));
        }
        self.queue_text(
            canvas,
            small,
            &shelf.heading,
            [inner_left, top, inner_width, header],
            Tone::Muted,
        );
        top += header;
        for (index, line) in shelf.rows.into_iter().enumerate() {
            if index > 0 {
                canvas.fill(Fill::solid(
                    [inner_left, top, inner_width, 1.0],
                    Paint::RaisedEdge,
                ));
            }
            let button = m.em(1.8);
            let text_width = if line.stop.is_some() {
                (inner_width - button - m.em(0.5)).max(0.0)
            } else {
                inner_width
            };
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
            top += row;
        }
        panel + gap
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
