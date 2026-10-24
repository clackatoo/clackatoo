//! A compact pending-message shelf immediately above the composer.
use super::{BoxFrame, Canvas, ChatView, Fill, Hit, Icon, Measures, Paint, Tone, draw_laid};

impl ChatView {
    pub(super) fn draw_queue(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
    ) -> f32 {
        if !self.queue_supported {
            return 0.0;
        }
        let total = self.node.delivery.queued.len();
        self.queue_page = self.queue_page.min(total.saturating_sub(1) / 3);
        // Only shape a bounded excerpt from the visible rows, never copy or
        // shape eight potentially 64-KiB drafts on every animation frame.
        let messages: Vec<_> = self
            .node
            .delivery
            .queued
            .iter()
            .skip(self.queue_page * 3)
            .take(3)
            .map(|message| (message.id, excerpt(message)))
            .collect();
        let writing = !self.editor.raw_text().trim().is_empty() || !self.images.is_empty();
        let steering = self.busy() && writing && self.node.question.is_none();
        if messages.is_empty() && !steering {
            return 0.0;
        }
        let row = m.em(2.15);
        let header = m.em(1.65);
        let pad = m.em(0.45);
        let gap = m.em(0.45);
        let panel = if messages.is_empty() {
            0.0
        } else {
            header + row * f32::from(u8::try_from(messages.len()).unwrap_or(3)) + pad * 2.0
        };
        let height = panel + if steering { row + gap } else { 0.0 } + gap;
        let mut top = frame.top - height;
        let left = frame.left + m.em(0.65);
        let width = frame.place.column - m.em(1.3);
        let small = Measures::of(m.em(0.85));
        if !messages.is_empty() {
            canvas.fill(
                Fill::rounded([left, top, width, panel], Paint::Raised, m.em(0.9))
                    .bordered(1.0, Paint::RaisedEdge),
            );
            let inner_left = left + m.em(0.8);
            let inner_width = width - m.em(1.6);
            top += pad;
            let heading = format!(
                "{}  ·  {}",
                if self.node.delivery.paused {
                    "Queue paused"
                } else {
                    "Queued"
                },
                total
            );
            self.queue_text(
                canvas,
                small,
                &heading,
                [inner_left, top, inner_width - m.em(4.0), header],
                Tone::Muted,
            );
            if total > 3 {
                for (text, page, offset) in [("‹", -1, 3.6), ("›", 1, 1.8)] {
                    self.queue_button(
                        canvas,
                        m,
                        frame,
                        text,
                        Hit::QueuePage(page),
                        [
                            inner_left + inner_width - m.em(offset),
                            top,
                            m.em(1.8),
                            header,
                        ],
                    );
                }
            }
            top += header;
            for (index, (id, text)) in messages.iter().enumerate() {
                if index > 0 {
                    canvas.fill(Fill::solid(
                        [inner_left, top, inner_width, 1.0],
                        Paint::RaisedEdge,
                    ));
                }
                self.draw_queue_message(
                    canvas,
                    m,
                    frame,
                    (*id, text),
                    [inner_left, top, inner_width, row],
                );
                top += row;
            }
            top += pad + gap;
        }
        if steering {
            self.draw_steering(canvas, m, frame, [left, top, width, row]);
        }
        height
    }

    fn draw_queue_message(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        (id, text): (u64, &str),
        [left, top, width, row]: [f32; 4],
    ) {
        self.queue_text(
            canvas,
            m,
            text,
            [left, top, (width - m.em(4.5)).max(0.0), row],
            Tone::Plain,
        );
        if self.node.delivery.sending == Some(id) {
            self.queue_text(
                canvas,
                Measures::of(m.em(0.85)),
                "Sending…",
                [left + width - m.em(4.5), top, m.em(4.5), row],
                Tone::Muted,
            );
        } else {
            for (icon, hit, offset) in [
                (Icon::Send, Hit::SendQueued(id), 3.8),
                (Icon::Close, Hit::RemoveQueued(id), 1.8),
            ] {
                self.queue_icon(
                    canvas,
                    m,
                    frame,
                    icon,
                    hit,
                    [
                        left + width - m.em(offset),
                        top + (row - m.em(1.8)) / 2.0,
                        m.em(1.8),
                        m.em(1.8),
                    ],
                );
            }
        }
    }

    fn draw_steering(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
    ) {
        let [left, top, width, row] = rect;

        if width >= m.em(30.0) {
            self.queue_text(
                canvas,
                m,
                "Enter queues",
                [left, top, (width - m.em(20.0)).max(0.0), row],
                Tone::Muted,
            );
        }
        let native =
            self.node.delivery.native_steering && self.images.is_empty() && self.handoff.is_none();
        let narrow = width < m.em(24.0);
        self.queue_button(
            canvas,
            m,
            frame,
            if narrow {
                if native { "Steer" } else { "Stop & continue" }
            } else if native {
                "Steer · Ctrl+Enter"
            } else {
                "Stop & continue · Ctrl+Enter"
            },
            Hit::Steer,
            [
                left + (width - m.em(19.5)).max(0.0),
                top,
                m.em(16.0).min((width - m.em(3.5)).max(0.0)),
                row,
            ],
        );
        self.queue_button(
            canvas,
            m,
            frame,
            "Stop",
            Hit::Stop,
            [left + width - m.em(3.0), top, m.em(3.0), row],
        );
    }

    fn queue_text(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        text: &str,
        rect: [f32; 4],
        tone: Tone,
    ) {
        let laid = self.short_label(canvas.fonts, m, text, rect[2], tone);
        draw_laid(
            canvas,
            m,
            &laid,
            [rect[0], rect[1] + (rect[3] - laid.height) / 2.0],
            rect[2],
        );
        self.keep_label(laid);
    }

    fn queue_icon(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        icon: Icon,
        hit: Hit,
        rect: [f32; 4],
    ) {
        let hovered = self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p));
        if hovered {
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, m.em(0.5)));
        }
        let side = m.em(0.95);
        canvas.fill(Fill::icon(
            [
                rect[0] + (rect[2] - side) / 2.0,
                rect[1] + (rect[3] - side) / 2.0,
                side,
                side,
            ],
            icon,
            if hovered { Paint::Ink } else { Paint::Icon },
        ));
        self.hits.push((hit, frame.place.in_pane(rect)));
    }

    fn queue_button(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        text: &str,
        hit: Hit,
        rect: [f32; 4],
    ) {
        if self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p))
        {
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, m.em(0.35)));
        }
        let small = Measures::of(m.em(0.85));
        let laid = self.short_label(canvas.fonts, small, text, rect[2], Tone::Muted);
        draw_laid(
            canvas,
            small,
            &laid,
            [
                rect[0] + (rect[2] - laid.width) / 2.0,
                rect[1] + (rect[3] - laid.height) / 2.0,
            ],
            rect[2],
        );
        self.keep_label(laid);
        self.hits.push((hit, frame.place.in_pane(rect)));
    }
}

fn excerpt(message: &clackatoo_protocol::QueuedMessage) -> String {
    let mut text: String = message
        .text
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(256)
        .collect();
    if message.images > 0 {
        text = format!(
            "{text} · {} image{}",
            message.images,
            if message.images == 1 { "" } else { "s" }
        );
    }
    if let Some(target) = &message.target {
        text = format!(
            "{} → {text}",
            target.model.as_deref().unwrap_or(&target.agent)
        );
    }
    text
}
