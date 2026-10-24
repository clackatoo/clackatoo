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
        if messages.is_empty() {
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
        let height = panel + gap;
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

    pub(super) fn steering_controls(&self) -> bool {
        self.queue_supported
            && self.busy()
            && self.node.question.is_none()
            && (!self.editor.raw_text().trim().is_empty() || !self.images.is_empty())
    }

    pub(super) fn draw_steering(&mut self, canvas: &mut Canvas<'_>, m: Measures, frame: &BoxFrame) {
        if !self.steering_controls() {
            return;
        }
        let small = Measures::of(m.em(0.8));
        let row = m.em(1.8);
        let top = frame.top + frame.height - m.em(5.1);
        let left = frame.left + frame.pad;
        let right = frame.left + frame.place.column - frame.pad;
        let native =
            self.node.delivery.native_steering && self.images.is_empty() && self.handoff.is_none();
        let label = if native { "Steer" } else { "Stop & continue" };
        let wide = frame.inner >= m.em(26.0);
        let text = if wide {
            format!("{label}  ·  Ctrl+Enter")
        } else {
            label.to_owned()
        };
        let laid = self.short_label(
            canvas.fonts,
            small,
            &text,
            frame.inner - row - m.em(0.5),
            Tone::Plain,
        );
        let width = (laid.width + m.em(1.2)).min(frame.inner - row - m.em(0.5));
        let rect = [right - row - m.em(0.45) - width, top, width, row];
        canvas.fill(
            Fill::rounded(rect, Paint::ChatHover, row / 2.0).bordered(1.0, Paint::RaisedEdge),
        );
        if self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p))
        {
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, row / 2.0));
        }
        draw_laid(
            canvas,
            small,
            &laid,
            [
                rect[0] + (width - laid.width) / 2.0,
                top + (row - laid.height) / 2.0,
            ],
            width,
        );
        self.keep_label(laid);
        self.hits.push((Hit::Steer, frame.place.in_pane(rect)));
        let hint_width = rect[0] - left - m.em(0.8);
        if hint_width >= m.em(7.0) {
            self.queue_text(
                canvas,
                small,
                "Enter to queue",
                [left, top, hint_width, row],
                Tone::Muted,
            );
        }
        let stop = [right - row, top, row, row];
        if self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(stop), p))
        {
            canvas.fill(Fill::rounded(stop, Paint::ChatHover, row / 2.0));
        }
        let extent = m.em(0.55);
        canvas.fill(Fill::rounded(
            [
                stop[0] + (row - extent) / 2.0,
                top + (row - extent) / 2.0,
                extent,
                extent,
            ],
            Paint::Icon,
            m.em(0.1),
        ));
        self.hits.push((Hit::Stop, frame.place.in_pane(stop)));
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
