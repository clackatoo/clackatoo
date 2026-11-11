//! Context controls reuse the chat's pane-modal presentation.
use super::{Canvas, ChatView, Fill, Fonts, Hit, Measures, Paint, Place, Tone, draw_laid};

impl ChatView {
    #[allow(
        clippy::too_many_lines,
        reason = "one pane modal lays out search, attachments and its preview together"
    )]
    pub(super) fn draw_context_modal(
        &mut self,
        paint: &mut super::TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
    ) {
        if !self.context.open {
            return;
        }
        self.hits.clear();
        self.shown.clear();
        paint.over_glyphs.clear();
        paint.ime = None;
        let [x, y] = place.origin;
        let [pane_width, pane_height] = place.size;
        let width = (pane_width - m.em(2.0)).max(1.0).min(m.em(100.0));
        let height = (pane_height - m.em(2.0)).max(1.0).min(m.em(68.0));
        let left = x + (pane_width - width) / 2.0;
        let top = y + (pane_height - height) / 2.0;
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [x, y, pane_width, pane_height],
            em: m.em,
            over: true,
        };
        canvas.fill(Fill::solid(canvas.clip, Paint::ImageBackdrop));
        canvas.fill(
            Fill::rounded([left, top, width, height], Paint::Palette, m.em(0.8))
                .bordered(1.0, Paint::RaisedEdge),
        );
        self.hits.push((
            Hit::ContextSurface,
            place.in_pane([left, top, width, height]),
        ));
        let label = format!(
            "Context · {} attached · {:?}",
            self.context.attached.len(),
            self.context.kind
        );
        self.usage_text(
            &mut canvas,
            m,
            &label,
            [left + m.em(1.0), top + m.em(1.0), width - m.em(5.0)],
            Tone::Plain,
        );
        self.usage_text(
            &mut canvas,
            m,
            "×",
            [left + width - m.em(2.2), top + m.em(0.8), m.em(1.5)],
            Tone::Muted,
        );
        self.hits.push((
            Hit::CloseContext,
            place.in_pane([left + width - m.em(3.0), top, m.em(3.0), m.em(3.0)]),
        ));
        let small = Measures::of(m.em(0.8));
        let query = if self.context.narrowing {
            format!("Lines first-last: {}▏", self.context.range)
        } else {
            format!("Search: {}▏   · Tab changes category", self.context.query)
        };
        self.hits.push((
            Hit::ContextCategory,
            place.in_pane([
                left + m.em(1.0),
                top + m.em(0.7),
                width - m.em(5.0),
                m.em(1.8),
            ]),
        ));
        self.usage_text(
            &mut canvas,
            small,
            &query,
            [left + m.em(1.0), top + m.em(3.0), width - m.em(2.0)],
            Tone::Plain,
        );
        let mut row_y = top + m.em(5.0);
        let attached = self
            .context
            .attached
            .iter()
            .map(|a| a.preview.label.clone())
            .collect::<Vec<_>>();
        let attached_start = self
            .context
            .attached_offset
            .min(attached.len().saturating_sub(1));
        for (index, label) in attached.iter().enumerate().skip(attached_start).take(4) {
            self.usage_text(
                &mut canvas,
                small,
                &format!("Attached · {label}"),
                [left + m.em(1.0), row_y, width - m.em(5.0)],
                Tone::Accent,
            );
            self.usage_text(
                &mut canvas,
                small,
                "×",
                [left + width - m.em(2.0), row_y, m.em(1.0)],
                Tone::Muted,
            );
            self.hits.push((
                Hit::InspectAttachedContext(index),
                place.in_pane([left + m.em(1.0), row_y, width - m.em(5.0), m.em(1.7)]),
            ));
            self.hits.push((
                Hit::RemoveContext(index),
                place.in_pane([left + width - m.em(3.0), row_y, m.em(2.0), m.em(1.7)]),
            ));
            row_y += m.em(1.8);
        }
        if attached.len() > 4 {
            for (at, label, next) in [
                (1.0, "Earlier attachments", false),
                (15.0, "Later attachments", true),
            ] {
                self.usage_text(
                    &mut canvas,
                    small,
                    label,
                    [left + m.em(at), row_y, m.em(13.0)],
                    Tone::Muted,
                );
                self.hits.push((
                    Hit::ContextAttachments(next),
                    place.in_pane([left + m.em(at), row_y, m.em(13.0), m.em(1.8)]),
                ));
            }
            row_y += m.em(2.0);
        }
        let body_top = row_y;
        let footer = top + height - m.em(7.0);
        let remaining = (footer - row_y).max(0.0);
        let list_width = if width > m.em(55.0) {
            width * 0.43
        } else {
            width - m.em(2.0)
        };
        let mut rows: usize = 0;
        let mut used = 0.0;
        while used + m.em(2.6) <= remaining {
            rows += 1;
            used += m.em(2.6);
        }
        let start = self.context.selected.saturating_sub(rows.saturating_sub(1));
        let items = self
            .context
            .items
            .iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, c)| (i, c.label.clone(), c.detail.clone()))
            .collect::<Vec<_>>();
        for (i, label, detail) in items {
            let rect = [left + m.em(0.6), row_y, list_width, m.em(2.5)];
            if i == self.context.selected {
                canvas.fill(Fill::rounded(rect, Paint::ChatHover, m.em(0.3)));
            }
            self.usage_text(
                &mut canvas,
                small,
                &label,
                [rect[0] + m.em(0.3), row_y, list_width - m.em(0.6)],
                Tone::Plain,
            );
            self.usage_text(
                &mut canvas,
                Measures::of(m.em(0.65)),
                &detail,
                [
                    rect[0] + m.em(0.3),
                    row_y + m.em(1.1),
                    list_width - m.em(0.6),
                ],
                Tone::Muted,
            );
            self.hits
                .push((Hit::ContextCandidate(i), place.in_pane(rect)));
            row_y += m.em(2.6);
        }
        if let Some(preview) = self.context.preview.clone() {
            let preview_x = if width > m.em(55.0) {
                left + list_width + m.em(1.5)
            } else {
                left + m.em(1.0)
            };
            let preview_y = body_top;
            let preview_width = left + width - m.em(1.0) - preview_x;
            if width <= m.em(55.0) {
                canvas.fill(Fill::solid(
                    [preview_x, preview_y, preview_width, footer - preview_y],
                    Paint::Palette,
                ));
            }
            let meta = format!(
                "{}\n{}\n{:?} · {} bytes · ~{} tokens\n{}\n\n{}",
                preview.label,
                preview.location,
                preview.mode,
                preview.bytes,
                preview.bytes / 4,
                preview.note,
                preview.text
            );
            let small = Measures::of(m.em(0.7));
            let laid = self.box_label(canvas.fonts, small, &meta, Tone::Plain, preview_width);
            self.context.max_scroll = (laid.height - (footer - preview_y).max(0.0)).max(0.0);
            self.context.scroll = self.context.scroll.min(self.context.max_scroll);
            let old = canvas.clip;
            canvas.clip = [
                preview_x,
                preview_y,
                preview_width,
                (footer - preview_y).max(0.0),
            ];
            draw_laid(
                &mut canvas,
                small,
                &laid,
                [preview_x, preview_y - self.context.scroll],
                preview_width,
            );
            canvas.clip = old;
            self.keep_label(laid);
        }
        let status = self.context.status.clone();
        self.usage_text(
            &mut canvas,
            small,
            &status,
            [left + m.em(1.0), footer, width - m.em(2.0)],
            Tone::Muted,
        );
        let gap = m.em(0.5);
        let button_width = ((width - m.em(2.0) - gap * 2.0) / 3.0).max(1.0);
        for (index, (text, hit)) in [
            ("Attach", Hit::AttachContext),
            ("Refresh", Hit::RefreshContext),
            ("Contents / ref", Hit::ContextMode),
            ("Narrow lines", Hit::NarrowContext),
            ("Copy snapshot", Hit::CopyContext),
        ]
        .into_iter()
        .enumerate()
        {
            let rect = [
                left + m.em(1.0)
                    + f32::from(u8::try_from(index % 3).expect("three columns"))
                        * (button_width + gap),
                footer
                    + m.em(1.5)
                    + f32::from(u8::try_from(index / 3).expect("two rows")) * m.em(2.3),
                button_width,
                m.em(2.0),
            ];
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, m.em(0.3)));
            self.usage_text(
                &mut canvas,
                small,
                text,
                [
                    rect[0] + m.em(0.4),
                    rect[1] + m.em(0.3),
                    rect[2] - m.em(0.8),
                ],
                Tone::Plain,
            );
            self.hits.push((hit, place.in_pane(rect)));
        }
    }
}
