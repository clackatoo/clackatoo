//! A right-hand sheet; narrow panes devote their full width to it.
use super::{Canvas, ChatView, Fill, Fonts, Hit, Measures, Paint, Place, TextPaint, Tone};
use crate::chat::side::Control;
impl ChatView {
    pub(super) fn draw_side(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
    ) {
        if !self.side.open {
            return;
        }
        let [x, y] = place.origin;
        let [pane_width, pane_height] = place.size;
        let width = if pane_width >= m.em(85.0) {
            (pane_width * 0.52).min(m.em(64.0))
        } else {
            pane_width
        };
        let left = x + pane_width - width;
        let header = m.em(7.8).min(pane_height * 0.35);
        self.side.body = [left - x, header, width, (pane_height - header).max(1.0)];
        self.side.controls.clear();
        self.hits.clear();
        self.shown.clear();
        paint.over_glyphs.clear();
        paint.ime = None;
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [x, y, pane_width, pane_height],
            em: m.em,
            over: true,
        };
        canvas.fill(
            Fill::solid([left, y, width, pane_height], Paint::Palette)
                .bordered(1.0, Paint::RaisedEdge),
        );
        self.usage_text(
            &mut canvas,
            m,
            "Side question",
            [left + m.em(1.0), y + m.em(0.6), width - m.em(5.0)],
            Tone::Plain,
        );
        self.usage_text(
            &mut canvas,
            Measures::of(m.em(0.72)),
            &format!(
                "{} · {} · No tools",
                self.title,
                if self.working() {
                    "Main agent working"
                } else {
                    "Main agent idle"
                }
            ),
            [left + m.em(1.0), y + m.em(2.1), width - m.em(2.0)],
            Tone::Muted,
        );
        let small = Measures::of(m.em(0.72));
        for (label, control, rect) in [
            (
                "×",
                Control::Hide,
                [
                    left + width - m.em(3.0),
                    y + m.em(0.5),
                    m.em(2.5),
                    m.em(2.0),
                ],
            ),
            (
                "Keep as chat",
                Control::Keep,
                [
                    left + m.em(1.0),
                    y + m.em(3.8),
                    m.em(12.0).min(width * 0.48),
                    m.em(2.0),
                ],
            ),
            (
                "Discard",
                Control::Discard,
                [left + width * 0.53, y + m.em(3.8), width * 0.44, m.em(2.0)],
            ),
        ] {
            self.usage_text(
                &mut canvas,
                small,
                label,
                [rect[0], rect[1], rect[2]],
                Tone::Muted,
            );
            self.side.controls.push((control, place.in_pane(rect)));
        }
        let status = self.side.status.clone();
        self.usage_text(
            &mut canvas,
            Measures::of(m.em(0.65)),
            &status,
            [left + m.em(1.0), y + m.em(6.0), width - m.em(2.0)],
            Tone::Muted,
        );
        self.paint_side_child(
            &mut canvas,
            [left, y + header, width, (pane_height - header).max(1.0)],
        );
    }
    fn paint_side_child(&mut self, canvas: &mut Canvas<'_>, [left, top, width, height]: [f32; 4]) {
        if let Some(child) = &mut self.side.child {
            let mut child_paint = child.paint(
                canvas.fonts,
                [left, top],
                [width, height],
                self.side.focused,
            );
            canvas.paint.over.append(&mut child_paint.fills);
            canvas.paint.over.append(&mut child_paint.over);
            canvas.paint.over_glyphs.append(&mut child_paint.glyphs);
            canvas
                .paint
                .over_glyphs
                .append(&mut child_paint.over_glyphs);
            canvas.paint.ime = child_paint.ime;
        }
    }

    pub(super) fn draw_side_answers(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
    ) {
        if !self.side_child {
            return;
        }
        let copies = self
            .hits
            .iter()
            .filter_map(|(hit, rect)| {
                if let Hit::CopyAnswer(turn) = hit {
                    Some((*turn, *rect))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
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
        for (turn, rect) in copies {
            let x = place.origin[0] + rect[0] + m.em(5.0);
            let y = place.origin[1] + rect[1];
            let width = (place.origin[0] + place.size[0] - x - m.em(1.0)).max(1.0);
            self.usage_text(
                &mut canvas,
                Measures::of(m.em(0.8)),
                "Send to main",
                [x, y, width],
                Tone::Accent,
            );
            self.hits
                .push((Hit::SideAnswer(turn), place.in_pane([x, y, width, rect[3]])));
        }
    }
}
