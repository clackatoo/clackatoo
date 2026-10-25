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
        self.note_usage_wake();
        self.hits.clear();
        self.shown.clear();
        paint.over_glyphs.clear();
        paint.ime = None;
        let [x, y] = place.origin;
        let [pane_width, pane_height] = place.size;
        let width = m.em(80.0).min((pane_width - m.em(2.0)).max(1.0));
        let height = m.em(62.0).min((pane_height - m.em(2.0)).max(1.0));
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
            Measures::of(m.em(1.2)),
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

    fn note_usage_wake(&mut self) {
        let now = self.now();
        if let Some(delay) = self
            .node
            .provider_usage
            .iter()
            .flat_map(|provider| &provider.limits)
            .filter_map(|limit| limit.resets)
            .filter(|reset| *reset > now)
            .map(|reset| (reset - now - 1) % 60_000 + 1)
            .min()
        {
            let next = std::time::Instant::now() + std::time::Duration::from_millis(delay);
            self.live = Some(self.live.map_or(next, |live| live.min(next)));
        }
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
        let inner = width - m.em(3.0);
        let col = left + m.em(1.5);
        let card = card_height(m, inner);
        let content_height = m.em(3.0)
            + if self.node.provider_usage.is_empty() {
                m.em(3.0)
            } else {
                self.node
                    .provider_usage
                    .iter()
                    .map(|provider| {
                        m.em(4.2)
                            + provider
                                .limits
                                .iter()
                                .map(|_| card + m.em(0.8))
                                .sum::<f32>()
                            + if provider.notice.is_some() {
                                m.em(3.2)
                            } else {
                                0.0
                            }
                    })
                    .sum::<f32>()
            };
        self.usage_max_scroll = (content_height - canvas.clip[3]).max(0.0);
        self.usage_scroll = self.usage_scroll.min(self.usage_max_scroll);
        let start = canvas.clip[1];
        let mut at = start - self.usage_scroll;
        let context = self.node.usage.map_or_else(
            || "Conversation context · not reported yet".into(),
            |u| {
                format!(
                    "Conversation context · {}",
                    u.detail(self.now(), |_| String::new())[0].said
                )
            },
        );
        self.usage_text(canvas, small, &context, [col, at, inner], Tone::Muted);
        at += m.em(3.0);
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
        let small = Measures::of(m.em(0.75));
        let icon = Icon::provider(&provider.agent);
        canvas.fill(Fill::icon(
            [col, at, m.em(1.25), m.em(1.25)],
            icon,
            super::models::icon_paint(icon),
        ));
        let status = if provider.refreshing {
            "Refreshing…".to_owned()
        } else if provider.checked > 0 {
            format!("Updated {}", resets(provider.checked, now, zone))
        } else {
            "No reading yet".into()
        };
        let status_width = m.em(13.0).min(inner * 0.45);
        self.usage_text(
            canvas,
            m,
            &provider.title,
            [col + m.em(1.9), at, inner - status_width - m.em(2.4)],
            Tone::Plain,
        );
        self.usage_text(
            canvas,
            small,
            &status,
            [col + inner - status_width, at + m.em(0.2), status_width],
            Tone::Muted,
        );
        at += m.em(2.4);
        for limit in &provider.limits {
            let height = card_height(m, inner);
            // Offscreen cards contribute to scrolling without shaping their text.
            if at + height >= canvas.clip[1] && at < canvas.clip[1] + canvas.clip[3] {
                self.usage_card(canvas, m, limit, [col, at, inner, height], icon, now);
            }
            at += height + m.em(0.8);
        }
        if let Some(notice) = &provider.notice {
            canvas.fill(
                Fill::rounded([col, at, inner, m.em(2.6)], Paint::Clear, m.em(0.65))
                    .bordered(1.0, Paint::RaisedEdge),
            );
            self.usage_text(
                canvas,
                small,
                notice,
                [col + m.em(1.0), at + m.em(0.8), inner - m.em(2.0)],
                Tone::Muted,
            );
            at += m.em(3.2);
        }
        at + m.em(1.8)
    }

    fn usage_card(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        limit: &clackatoo_protocol::UsageLimit,
        rect: [f32; 4],
        icon: Icon,
        now: u64,
    ) {
        let [col, at, width, height] = rect;
        let narrow = width < m.em(40.0);
        let inset = m.em(1.2);
        let label_width = if narrow {
            width - inset * 2.0
        } else {
            m.em(13.0)
        };
        let expired = limit.resets.is_some_and(|reset| reset <= now);
        let left = remaining(limit, now);
        canvas.fill(Fill::rounded(rect, Paint::Clear, m.em(0.8)).bordered(1.0, Paint::RaisedEdge));
        let label = match limit.label.as_str() {
            "5-hour" => "Session",
            "7-day" => "Weekly",
            label => label,
        };
        let split = (!narrow).then(|| label.split_once(" · ")).flatten();
        let amount_top = if split.is_some() { 3.6 } else { 2.6 };
        self.usage_text(
            canvas,
            m,
            split.map_or(label, |(family, _)| family),
            [col + inset, at + m.em(1.15), label_width],
            Tone::Plain,
        );
        if let Some((_, window)) = split {
            self.usage_text(
                canvas,
                Measures::of(m.em(0.85)),
                window,
                [col + inset, at + m.em(2.4), label_width],
                Tone::Muted,
            );
        }
        let amount = left.map_or_else(|| "—".into(), |left| format!("{left}%"));
        let amount_width = self.usage_text(
            canvas,
            Measures::of(m.em(2.15)),
            &amount,
            [col + inset, at + m.em(amount_top), label_width],
            Tone::Plain,
        );
        self.usage_text(
            canvas,
            Measures::of(m.em(0.9)),
            if expired { "refresh needed" } else { "left" },
            [
                col + inset + amount_width + m.em(0.5),
                at + m.em(amount_top + 1.05),
                label_width.max(m.em(8.0)) - amount_width,
            ],
            Tone::Muted,
        );
        let bar = if narrow {
            [
                col + inset,
                at + height - m.em(3.4),
                width - 2.0 * inset,
                m.em(2.5),
            ]
        } else {
            [
                col + m.em(15.5),
                at + (height - m.em(2.5)) / 2.0,
                width - m.em(16.7),
                m.em(2.5),
            ]
        };
        self.usage_bar(canvas, m, bar, icon, left, &reset_label(limit.resets, now));
    }

    fn usage_bar(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        bar: [f32; 4],
        icon: Icon,
        left: Option<u8>,
        reset: &str,
    ) {
        let [x, y, width, height] = bar;
        let colour = Paint::Allowance(icon);
        canvas.fill(Fill::rounded(bar, Paint::ChatHover, m.em(0.6)));
        let filled = left.map_or(0.0, |left| width * f32::from(left) / 100.0);
        if filled > 0.0 {
            canvas.fill(Fill::rounded([x, y, filled, height], colour, m.em(0.6)));
        }
        // The spent part has texture, so it remains distinct without relying on colour.
        let saved = canvas.clip;
        canvas.clip = super::intersection(
            saved,
            [
                x + filled,
                y + 1.0,
                (width - filled - m.em(0.25)).max(0.0),
                height - 2.0,
            ],
        );
        let mut tile = x + filled;
        while tile < x + width {
            canvas.fill(Fill::icon([tile, y, height, height], Icon::Hatch, colour));
            tile += height;
        }
        canvas.clip = saved;
        let small = Measures::of(m.em(0.8));
        let badge_label = self.short_label(canvas.fonts, small, reset, width * 0.6, Tone::Plain);
        let badge_width = badge_label.width + m.em(1.0);
        let badge = [
            x + width - badge_width - m.em(0.5),
            y + m.em(0.45),
            badge_width,
            height - m.em(0.9),
        ];
        canvas.fill(Fill::rounded(badge, Paint::Palette, m.em(0.4)));
        draw_laid(
            canvas,
            small,
            &badge_label,
            [badge[0] + m.em(0.5), badge[1] + m.em(0.25)],
            badge_width - m.em(0.9),
        );
        self.keep_label(badge_label);
        let amount = left.map_or_else(|| "—".into(), |left| format!("{left}%"));
        self.usage_text(
            canvas,
            small,
            &amount,
            [
                x + m.em(0.65),
                y + m.em(0.8),
                (badge[0] - x - m.em(1.0)).max(1.0),
            ],
            Tone::Plain,
        );
    }

    fn usage_text(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        text: &str,
        [x, y, width]: [f32; 3],
        tone: Tone,
    ) -> f32 {
        let laid = self.short_label(canvas.fonts, m, text, width.max(1.0), tone);
        draw_laid(canvas, m, &laid, [x, y], width.max(1.0));
        let width = laid.width;
        self.keep_label(laid);
        width
    }
}

fn card_height(m: Measures, width: f32) -> f32 {
    m.em(if width < m.em(40.0) { 9.0 } else { 7.0 })
}

fn remaining(limit: &clackatoo_protocol::UsageLimit, now: u64) -> Option<u8> {
    limit
        .resets
        .is_none_or(|reset| reset > now)
        .then(|| 100_u8.saturating_sub(limit.used))
}

fn reset_label(reset: Option<u64>, now: u64) -> String {
    let Some(reset) = reset else {
        return "Reset not reported".into();
    };
    if reset <= now {
        return "Refresh needed".into();
    }
    let minutes = (reset - now).div_ceil(60_000);
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        format!("↻ {days}d {}h", hours % 24)
    } else if hours > 0 {
        format!("↻ {hours}h {}m", minutes % 60)
    } else {
        format!("↻ {minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remaining_and_countdown_do_not_invent_an_allowance_after_reset() {
        let limit = clackatoo_protocol::UsageLimit {
            id: "weekly".into(),
            label: "Weekly".into(),
            used: 94,
            resets: Some(60_001),
        };
        assert_eq!(remaining(&limit, 1), Some(6));
        assert_eq!(remaining(&limit, 60_001), None);
        assert_eq!(reset_label(Some(60_001), 1), "↻ 1m");
        assert_eq!(reset_label(Some(60_002), 1), "↻ 2m");
        assert_eq!(reset_label(Some(3_600_001), 1), "↻ 1h 0m");
        assert_eq!(reset_label(Some(90_000_001), 1), "↻ 1d 1h");
        assert_eq!(reset_label(Some(1), 1), "Refresh needed");
        assert_eq!(reset_label(None, 1), "Reset not reported");
    }
}
