//! A compact context ring beside Send, with the percentage inside.
//! The ring takes the fuller warning colour of context and account limits.
//! Hovering over it, clicking it, or `K` in the chat's normal mode shows
//! the detail above the box: the tokens, and each limit the agent has
//! said, how much of it is used and when it resets.
//!
//! Nothing here changes by itself: a limit says when it resets as a time
//! of day, never as "in 2h", so nothing wakes the window to count. Once
//! that time has passed, the limit isn't shown, as how much of it is used
//! isn't known until the agent says again, next time anything is drawn.

use crate::render::icons::Icon;
use clackatoo_protocol::Fullness;
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::divider::local_zone;
use super::{BoxFrame, Canvas, Laid, Look, Measures, Place, TextPaint, draw_laid, lay_out};
use crate::chat::{ChatView, Hit, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Paint};

/// How the detail came to be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::chat) enum Detail {
    /// The mouse is over the meter: it goes once the mouse leaves.
    Hovered,
    /// It was clicked, or `K` was pressed: it goes with the next key or
    /// click.
    Asked,
}

/// The colour of what's said of something `fullness` full: `quiet` while
/// there's room.
fn tone(fullness: Fullness, quiet: Tone) -> Tone {
    match fullness {
        Fullness::Room => quiet,
        Fullness::Filling => Tone::Caution,
        Fullness::Full => Tone::Warning,
    }
}

/// The colour of a bar's filling, `fullness` full.
fn paint(fullness: Fullness) -> Paint {
    match fullness {
        Fullness::Room => Paint::Gauge,
        Fullness::Filling => Paint::Caution,
        Fullness::Full => Paint::Full,
    }
}

/// When a limit resets, `at`, as said at `now`, both in milliseconds
/// since the Unix epoch, in `zone`: "at" and the time today, "tomorrow"
/// and the time, the day of the week and the time within the week, and
/// the date and the time further off, with the year if it's another. One
/// already past says the time it was, as the agent hasn't said since.
pub(in crate::chat) fn resets(at: u64, now: u64, zone: &TimeZone) -> String {
    let local = |ms: u64| {
        let ms = i64::try_from(ms).ok()?;
        Some(Timestamp::from_millisecond(ms).ok()?.to_zoned(zone.clone()))
    };
    let (Some(at), Some(now)) = (local(at), local(now)) else {
        return String::new();
    };
    let time = at.strftime("%H:%M");
    let days = (at.date().since(now.date())).map_or(-1, |span| span.get_days());
    match days {
        0 => format!("at {time}"),
        1 => format!("tomorrow {time}"),
        2..=6 => at.strftime("%A %H:%M").to_string(),
        _ if at.year() == now.year() => at.strftime("%-d %b %H:%M").to_string(),
        _ => at.strftime("%-d %b %Y %H:%M").to_string(),
    }
}

impl ChatView {
    pub(super) fn draw_meter(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
    ) {
        let usage = match self.node.usage {
            Some(usage) => usage,
            None if self.usage_supported => clackatoo_protocol::Usage {
                used: 0,
                size: 0,
                five_hour: None,
                weekly: None,
            },
            None => return,
        };
        let percent = usage.percent();
        let fullness = percent.map_or(Fullness::Room, Fullness::of);
        let worst = usage
            .pressing(self.now())
            .map_or(fullness, |(_, limit)| limit.max(fullness));
        canvas.fill(Fill::icon(rect, Icon::Ring, Paint::RaisedEdge));
        if let Some(percent) = percent {
            canvas.fill(Fill::icon(rect, Icon::Progress(percent), paint(worst)));
        }
        let text = percent.map_or_else(|| "—".to_owned(), |percent| format!("{percent}%"));
        let small = Measures::of(m.em * 0.72);
        let laid = self.box_label(
            canvas.fonts,
            small,
            &text,
            tone(worst, Tone::Muted),
            f32::INFINITY,
        );
        let at = [
            rect[0] + (rect[2] - laid.width) / 2.0,
            rect[1] + (rect[3] - laid.height) / 2.0,
        ];
        draw_laid(canvas, small, &laid, at, rect[2]);
        self.keep_label(laid);
        self.hits.push((Hit::Usage, frame.place.in_pane(rect)));
        if self.detail.is_some() {
            self.detail_anchor = Some([rect[0] + rect[2], rect[1]]);
        }
    }

    /// Draws the detail, over everything, opening upward from `anchor`:
    /// the meter's right and top.
    pub(super) fn draw_detail(
        &mut self,
        paint: &mut TextPaint,
        fonts: &mut Fonts,
        m: Measures,
        place: Place,
        anchor: [f32; 2],
    ) {
        let Some(usage) = self.node.usage else {
            return;
        };
        let (now, zone) = (self.now(), local_zone());
        let lines = usage.detail(now, |at| resets(at, now, &zone));
        let laid: Vec<(Laid, Laid)> = (lines.into_iter())
            .map(|line| {
                let mut lay = |text: &str, tone| {
                    let look = Look::Plain(tone);
                    lay_out(&mut self.layout_cx, fonts, m, text, look, place.size[0])
                };
                let said = line
                    .fullness
                    .map_or(Tone::Plain, |full| tone(full, Tone::Plain));
                (lay(&line.name, Tone::Muted), lay(&line.said, said))
            })
            .collect();
        let pad = m.em(0.8);
        let row = m.em(1.8);
        let names = (laid.iter())
            .map(|(name, _)| name.width)
            .fold(0.0, f32::max);
        let saids = (laid.iter())
            .map(|(_, said)| said.width)
            .fold(0.0, f32::max);
        let gap = m.em(1.2);
        #[allow(clippy::cast_precision_loss, reason = "a few lines")]
        let height = 2.0 * m.em(0.4) + row * laid.len() as f32;
        let width = (2.0 * pad + names + gap + saids).min(place.size[0] - m.em(1.0));
        let left = (anchor[0] - width).clamp(
            place.origin[0] + m.em(0.5),
            (place.origin[0] + place.size[0] - width - m.em(0.5)).max(place.origin[0]),
        );
        let bottom = anchor[1] - m.em(0.4);
        let top = (bottom - height).max(place.origin[1] + m.em(0.5));
        let bounds = [left, top, width, bottom - top];
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
        canvas.fill(Fill::rounded(
            [bounds[0], bounds[1] + m.em(0.2), bounds[2], bounds[3]],
            Paint::Shadow,
            m.em(0.8),
        ));
        canvas.fill(
            Fill::rounded(bounds, Paint::Palette, m.em(0.8)).bordered(1.0, Paint::PaletteEdge),
        );
        canvas.clip = bounds;
        let mut y = top + m.em(0.4);
        for (name, said) in &laid {
            let middle = |laid: &Laid| y + (row - laid.height) / 2.0;
            draw_laid(&mut canvas, m, name, [left + pad, middle(name)], width);
            let x = left + pad + names + gap;
            draw_laid(&mut canvas, m, said, [x, middle(said)], width);
            y += row;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reset_is_said_as_a_time_never_counting_down() {
        let zone = TimeZone::UTC;
        // Wednesday 15 October 2025, 12:00.
        let now = 1_760_529_600_000;
        let (hour, day) = (3_600_000, 86_400_000);
        assert_eq!(resets(now + 3 * hour + 2_400_000, now, &zone), "at 15:40");
        assert_eq!(resets(now + day + 1_800_000, now, &zone), "tomorrow 12:30");
        assert_eq!(
            resets(now + 6 * day - 3 * hour, now, &zone),
            "Tuesday 09:00"
        );
        assert_eq!(resets(now + 20 * day, now, &zone), "4 Nov 12:00");
        assert_eq!(resets(now + 120 * day, now, &zone), "12 Feb 2026 12:00");
        assert_eq!(resets(u64::MAX, now, &zone), "");
    }
}
