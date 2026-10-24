//! Account quotas in a pane-modal; hover remains the current chat's detail.
use super::{
    Canvas, ChatView, Fill, Fonts, Hit, Icon, Measures, Paint, Place, TextPaint, Tone, draw_laid,
};
use super::{divider::local_zone, usage::resets};

impl ChatView {
    pub(super) fn draw_usage_modal(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
    ) {
        if !self.usage_modal {
            return;
        }
        self.hits.clear();
        self.shown.clear();
        paint.over_glyphs.clear();
        paint.ime = None;
        let [x, y] = place.origin;
        let [pane_width, pane_height] = place.size;
        let width = m.em(39.0).min((pane_width - m.em(2.0)).max(1.0));
        let height = m.em(38.0).min((pane_height - m.em(2.0)).max(1.0));
        let left = x + (pane_width - width) / 2.0;
        let top = y + (pane_height - height) / 2.0;
        let bounds = [left, top, width, height];
        let mut canvas = Canvas {
            paint,
            fonts,
            clip: [x, y, pane_width, pane_height],
            em: m.em,
            over: true,
        };
        canvas.fill(Fill::solid(canvas.clip, Paint::ImageBackdrop));
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(1.0)).bordered(1.0, Paint::RaisedEdge),
        );
        self.hits.push((Hit::UsageSurface, place.in_pane(bounds)));
        self.usage_text(
            &mut canvas,
            m,
            "Usage & limits",
            [left + m.em(1.2), top + m.em(1.0), width - m.em(4.0)],
            Tone::Plain,
        );
        let close = [
            left + width - m.em(3.0),
            top + m.em(0.6),
            m.em(2.0),
            m.em(2.0),
        ];
        canvas.fill(Fill::icon(
            [
                close[0] + m.em(0.5),
                close[1] + m.em(0.5),
                m.em(1.0),
                m.em(1.0),
            ],
            Icon::Close,
            Paint::Icon,
        ));
        self.hits.push((Hit::CloseUsage, place.in_pane(close)));
        let small = Measures::of(m.em(0.8));
        self.usage_text(
            &mut canvas,
            small,
            "Subscription allowances across your providers",
            [left + m.em(1.2), top + m.em(2.6), width - m.em(2.4)],
            Tone::Muted,
        );
        let footer = top + height - m.em(3.5);
        canvas.fill(Fill::solid(
            [left + m.em(1.2), footer, width - m.em(2.4), 1.0],
            Paint::RaisedEdge,
        ));
        let refresh = [left + m.em(1.0), footer + m.em(0.7), m.em(6.0), m.em(1.8)];
        canvas.fill(Fill::rounded(refresh, Paint::ChatHover, m.em(0.5)));
        let refreshing = self.node.provider_usage.iter().any(|p| p.refreshing);
        self.usage_text(
            &mut canvas,
            small,
            if refreshing {
                "Refreshing…"
            } else {
                "Refresh"
            },
            [
                refresh[0] + m.em(0.6),
                refresh[1] + m.em(0.3),
                refresh[2] - m.em(0.8),
            ],
            Tone::Plain,
        );
        if !refreshing {
            self.hits.push((Hit::RefreshUsage, place.in_pane(refresh)));
        }
        if width > m.em(26.0) {
            self.usage_text(
                &mut canvas,
                small,
                "Esc or Ctrl+; to close",
                [left + width - m.em(12.0), footer + m.em(1.0), m.em(11.0)],
                Tone::Muted,
            );
        }
        self.usage_body(&mut canvas, m, bounds);
    }

    fn usage_body(&mut self, canvas: &mut Canvas<'_>, m: Measures, bounds: [f32; 4]) {
        let [left, top, width, height] = bounds;
        let small = Measures::of(m.em(0.8));
        canvas.clip = [
            left + m.em(0.6),
            top + m.em(4.4),
            width - m.em(1.2),
            (height - m.em(8.4)).max(1.0),
        ];
        let content_height = m.em(4.3)
            + if self.node.provider_usage.is_empty() {
                m.em(3.0)
            } else {
                self.node
                    .provider_usage
                    .iter()
                    .map(|provider| {
                        m.em(5.3
                            + provider.limits.iter().map(|_| 4.35).sum::<f32>()
                            + if provider.notice.is_some() { 2.2 } else { 0.0 })
                    })
                    .sum::<f32>()
            };
        self.usage_max_scroll = (content_height - canvas.clip[3]).max(0.0);
        self.usage_scroll = self.usage_scroll.min(self.usage_max_scroll);
        let start = canvas.clip[1];
        let mut at = start - self.usage_scroll;
        let inner = width - m.em(2.4);
        let col = left + m.em(1.2);
        self.usage_text(
            canvas,
            small,
            "THIS CONVERSATION",
            [col, at, inner],
            Tone::Muted,
        );
        at += m.em(1.6);
        let context = self.node.usage.map_or_else(
            || "Context not reported yet".into(),
            |u| u.detail(self.now(), |_| String::new())[0].said.clone(),
        );
        self.usage_text(canvas, m, &context, [col, at, inner], Tone::Plain);
        at += m.em(2.7);
        let now = self.now();
        let zone = local_zone();
        let providers = self.node.provider_usage.clone();
        if providers.is_empty() {
            self.usage_text(
                canvas,
                m,
                if self.offline {
                    "Reconnect to read provider usage."
                } else {
                    "No provider readings yet."
                },
                [col, at, inner],
                Tone::Muted,
            );
            at += m.em(3.0);
        }
        for provider in providers {
            at = self.usage_provider(canvas, m, &provider, [col, at, inner], now, &zone);
        }
        if self.usage_max_scroll > 0.0 {
            let track = canvas.clip[3];
            let thumb = (track * track / (track + self.usage_max_scroll))
                .max(m.em(2.0))
                .min(track);
            let pos = self.usage_scroll / self.usage_max_scroll * (track - thumb);
            canvas.clip = bounds;
            canvas.fill(Fill::rounded(
                [left + width - m.em(0.55), start + pos, m.em(0.2), thumb],
                Paint::Icon,
                m.em(0.1),
            ));
        }
    }

    fn usage_provider(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        provider: &clackatoo_protocol::ProviderUsage,
        [col, mut at, inner]: [f32; 3],
        now: u64,
        zone: &jiff::tz::TimeZone,
    ) -> f32 {
        let small = Measures::of(m.em(0.8));

        canvas.fill(Fill::solid([col, at, inner, 1.0], Paint::RaisedEdge));
        at += m.em(1.0);
        let icon = Icon::provider(&provider.agent);
        canvas.fill(Fill::icon(
            [col, at, m.em(1.25), m.em(1.25)],
            icon,
            super::models::icon_paint(icon),
        ));
        self.usage_text(
            canvas,
            m,
            &provider.title,
            [col + m.em(1.8), at, inner - m.em(1.8)],
            Tone::Plain,
        );
        at += m.em(1.8);
        let status = if provider.refreshing {
            "Refreshing…".to_owned()
        } else if provider.checked > 0 {
            format!("Updated {}", resets(provider.checked, now, zone))
        } else {
            "No reading yet".into()
        };
        self.usage_text(canvas, small, &status, [col, at, inner], Tone::Muted);
        at += m.em(1.8);
        for limit in &provider.limits {
            let expired = limit.resets.is_some_and(|reset| reset <= now);
            self.usage_text(
                canvas,
                small,
                &limit.label,
                [col, at, (inner - m.em(6.0)).max(1.0)],
                Tone::Plain,
            );
            let amount = if expired {
                "Needs refresh".into()
            } else {
                format!("{}% used", limit.used)
            };
            self.usage_text(
                canvas,
                small,
                &amount,
                [col + inner - m.em(6.0), at, m.em(6.0)],
                Tone::Muted,
            );
            at += m.em(1.35);
            let bar = [col, at, inner, m.em(0.3)];
            canvas.fill(Fill::rounded(bar, Paint::RaisedEdge, m.em(0.15)));
            if !expired && limit.used > 0 {
                let ink = if limit.used >= 95 {
                    Paint::Full
                } else if limit.used >= 85 {
                    Paint::Caution
                } else {
                    Paint::Gauge
                };
                canvas.fill(Fill::rounded(
                    [
                        col,
                        at,
                        inner * f32::from(limit.used.min(100)) / 100.0,
                        bar[3],
                    ],
                    ink,
                    m.em(0.15),
                ));
            }
            at += m.em(0.8);
            let reset = limit.resets.map_or_else(
                || "Reset time not reported".into(),
                |reset| {
                    if expired {
                        "Reset passed · refresh for current usage".into()
                    } else {
                        format!("Resets {}", resets(reset, now, zone))
                    }
                },
            );
            self.usage_text(canvas, small, &reset, [col, at, inner], Tone::Muted);
            at += m.em(2.2);
        }
        if let Some(notice) = &provider.notice {
            self.usage_text(canvas, small, notice, [col, at, inner], Tone::Muted);
            at += m.em(2.2);
        }
        at += m.em(0.7);
        at
    }

    fn usage_text(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        text: &str,
        [x, y, width]: [f32; 3],
        tone: Tone,
    ) {
        let laid = self.short_label(canvas.fonts, m, text, width.max(1.0), tone);
        draw_laid(canvas, m, &laid, [x, y], width.max(1.0));
        self.keep_label(laid);
    }
}
