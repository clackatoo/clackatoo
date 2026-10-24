//! A compact pending-message shelf immediately above the composer.
use super::{BoxFrame, Canvas, ChatView, Fill, Hit, Measures, Paint, Tone, draw_laid};

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
        let row = m.em(2.1);
        let count = total.min(3);
        let height = row
            * (f32::from(u8::try_from(count).unwrap_or(3))
                + f32::from(u8::from(!messages.is_empty()))
                + f32::from(u8::from(steering)))
            + m.em(0.5);
        let mut top = frame.top - height;
        let left = frame.left + m.em(0.5);
        let width = frame.place.column - m.em(1.0);
        if !messages.is_empty() {
            let heading = format!(
                "{} · {}",
                if self.node.delivery.paused {
                    "Queue paused"
                } else {
                    "Up next"
                },
                total
            );
            self.queue_text(
                canvas,
                m,
                &heading,
                [left, top, width - m.em(5.0), row],
                Tone::Muted,
            );
            if total > 3 {
                for (text, page, offset) in [("‹", -1, 4.0), ("›", 1, 2.0)] {
                    self.queue_button(
                        canvas,
                        m,
                        frame,
                        text,
                        Hit::QueuePage(page),
                        [left + width - m.em(offset), top, m.em(2.0), row],
                    );
                }
            }
            top += row;
            for (id, text) in &messages {
                canvas.fill(Fill::rounded(
                    [left, top, width, row - m.em(0.15)],
                    Paint::Raised,
                    m.em(0.45),
                ));
                self.queue_text(
                    canvas,
                    m,
                    text,
                    [left + m.em(0.65), top, (width - m.em(5.0)).max(0.0), row],
                    Tone::Muted,
                );
                if self.node.delivery.sending == Some(*id) {
                    top += row;
                    continue;
                }
                self.queue_button(
                    canvas,
                    m,
                    frame,
                    "↑",
                    Hit::SendQueued(*id),
                    [left + width - m.em(4.0), top, m.em(2.0), row],
                );
                self.queue_button(
                    canvas,
                    m,
                    frame,
                    "×",
                    Hit::RemoveQueued(*id),
                    [left + width - m.em(2.0), top, m.em(2.0), row],
                );
                top += row;
            }
        }
        if steering {
            self.draw_steering(canvas, m, frame, [left, top, width, row]);
        }
        height
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
        self.queue_text(canvas, m, text, rect, Tone::Plain);
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
