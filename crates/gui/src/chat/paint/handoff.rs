//! One quiet transition row for a draft's pending handoff and history alike.
use super::{Canvas, ChatView, Fonts, Laid, Measures, Tone, draw_laid};
use crate::render::{Fill, Paint, icons::Icon};

pub(super) struct Transition {
    pub from: String,
    pub source: String,
    pub to: String,
    pub destination: String,
    pub pending: bool,
}

pub(super) struct LaidHandoff {
    status: Laid,
    source: Laid,
    destination: Laid,
    arrow: Laid,
    cancel: Option<Laid>,
    icons: [Icon; 2],
    stacked: bool,
    pub height: f32,
}

impl ChatView {
    pub(super) fn pending_handoff(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        width: f32,
    ) -> Option<LaidHandoff> {
        let pending = self
            .handoff
            .as_ref()
            .filter(|_| self.handoff_sent.is_none())?;
        let source = self
            .node
            .settings
            .iter()
            .find(|s| s.kind == clackatoo_protocol::SettingKind::Model)
            .map_or_else(|| self.agent().to_owned(), super::chosen_name);
        let transition = Transition {
            from: self.agent().to_owned(),
            source,
            to: pending.title.clone(),
            destination: pending.label.clone(),
            pending: true,
        };
        Some(self.lay_handoff(fonts, m, width, &transition))
    }

    pub(super) fn draw_pending_handoff(
        &mut self,
        (paint, fonts): (&mut super::TextPaint, &mut Fonts),
        m: Measures,
        place: super::Place,
        top: f32,
        laid: Option<LaidHandoff>,
    ) {
        let Some(laid) = laid else { return };
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
        draw(
            &mut canvas,
            m,
            &laid,
            [place.origin[0] + place.left, place.origin[1] + top],
            place.column,
        );
        self.hits.push((
            crate::chat::Hit::CancelHandoff,
            [
                place.left + place.column - m.em(1.8),
                top,
                m.em(1.8),
                laid.height,
            ],
        ));
        self.keep_handoff(laid);
    }

    pub(super) fn keep_handoff(&mut self, laid: LaidHandoff) {
        for label in [laid.status, laid.source, laid.destination, laid.arrow]
            .into_iter()
            .chain(laid.cancel)
        {
            self.keep_label(label);
        }
    }

    pub(super) fn lay_handoff(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        width: f32,
        transition: &Transition,
    ) -> LaidHandoff {
        let small = Measures::of(m.em * 0.78);
        let status = self.box_label(
            fonts,
            small,
            if transition.pending {
                "⇄  Context handoff pending"
            } else {
                "⇄  Context handed over"
            },
            Tone::Muted,
            f32::INFINITY,
        );
        let stacked = width < m.em(36.0);
        let room = width
            - m.em(7.0)
            - if stacked {
                0.0
            } else {
                status.width + m.em(0.8)
            };
        let source = self.short_label(fonts, small, &transition.source, room * 0.5, Tone::Muted);
        let destination = self.short_label(
            fonts,
            small,
            &transition.destination,
            room * 0.5,
            Tone::Plain,
        );
        let arrow = self.box_label(fonts, small, "→", Tone::Muted, f32::INFINITY);
        let cancel = transition
            .pending
            .then(|| self.box_label(fonts, small, "×", Tone::Muted, f32::INFINITY));
        LaidHandoff {
            status,
            source,
            destination,
            arrow,
            cancel,
            icons: [
                Icon::provider(&transition.from),
                Icon::provider(&transition.to),
            ],
            stacked,
            height: m.em(if stacked { 3.5 } else { 2.5 }),
        }
    }
}

pub(super) fn draw(
    canvas: &mut Canvas<'_>,
    m: Measures,
    laid: &LaidHandoff,
    origin: [f32; 2],
    width: f32,
) {
    let small = Measures::of(m.em * 0.78);
    let gap = m.em(0.5);
    let side = m.em(0.95);
    let models_width =
        laid.source.width + laid.destination.width + laid.arrow.width + 2.0 * side + 4.0 * gap;
    let content = if laid.stacked {
        models_width.max(laid.status.width)
    } else {
        laid.status.width + gap + models_width
    };
    let available = width
        - if laid.cancel.is_some() {
            m.em(1.8)
        } else {
            0.0
        };
    let left = origin[0] + (available - content).max(0.0) / 2.0;
    let top = origin[1] + m.em(0.55);
    let line_y = top + laid.status.height / 2.0;
    for (x, end) in [
        (origin[0], left - gap),
        (left + content + gap, origin[0] + available),
    ] {
        if end > x {
            canvas.fill(Fill::solid([x, line_y, end - x, 1.0], Paint::RaisedEdge));
        }
    }
    let status_x = if laid.stacked {
        origin[0] + (available - laid.status.width) / 2.0
    } else {
        left
    };
    draw_laid(
        canvas,
        small,
        &laid.status,
        [status_x, top],
        laid.status.width,
    );
    let mut x = if laid.stacked {
        origin[0] + (available - models_width) / 2.0
    } else {
        left + laid.status.width + gap
    };
    let y = top + if laid.stacked { m.em(1.3) } else { 0.0 };
    for (index, label) in [&laid.source, &laid.destination].into_iter().enumerate() {
        let icon = laid.icons[index];
        canvas.fill(Fill::icon(
            [x, y + (label.height - side) / 2.0, side, side],
            icon,
            super::models::icon_paint(icon),
        ));
        x += side + gap;
        draw_laid(canvas, small, label, [x, y], label.width);
        x += label.width + gap;
        if index == 0 {
            draw_laid(canvas, small, &laid.arrow, [x, y], laid.arrow.width);
            x += laid.arrow.width + gap;
        }
    }
    if let Some(cancel) = &laid.cancel {
        draw_laid(
            canvas,
            small,
            cancel,
            [origin[0] + width - m.em(1.2), top],
            cancel.width,
        );
    }
}
