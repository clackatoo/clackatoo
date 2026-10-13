//! The time, between turns sent far apart: a thin line across the column
//! with when the later turn was sent in its middle, muted, as "14:32",
//! "Yesterday 09:10" or "Monday 18:05".
//!
//! The line is the first piece of the turn after the gap, and always as
//! tall, so a turn's height doesn't depend on the time of day. What it
//! says does: "14:32" becomes "Yesterday 14:32" at midnight. So, as with
//! the line a turn's work folds under, what it says is laid out apart from
//! the turn, for the turns near the view, and again only when it changes.

use std::cell::OnceCell;

use clackatoo_protocol::Turn;
use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::{Canvas, LINE, Look, Measures, draw_laid, lay_out};
use crate::chat::{ChatView, Tone};
use crate::fonts::Fonts;
use crate::render::{Fill, Paint};

/// How long after the turn before it a turn is sent for the time to show
/// between them: half an hour, in milliseconds.
const GAP: u64 = 30 * 60 * 1000;

/// How tall the line is.
pub(super) fn height(m: Measures) -> f32 {
    (m.em * LINE).ceil()
}

/// The space between the line and the turn's message below it.
pub(super) fn below(m: Measures) -> f32 {
    m.em(1.2)
}

/// When turn `index` of `turns` was sent, in milliseconds since the Unix
/// epoch, if that was long enough after the turn before it ended for the
/// time to show above it. Turns from before turns said when they were
/// sent never show it.
pub(in crate::chat) fn divided(turns: &[Turn], index: usize) -> Option<u64> {
    let turn = turns.get(index)?;
    let before = turns.get(index.checked_sub(1)?)?;
    let ended = (before.end.as_ref()).map_or(before.sent, |end| end.at.max(before.sent));
    (turn.sent > 0 && before.sent > 0 && turn.sent.saturating_sub(ended) > GAP).then_some(turn.sent)
}

/// What the line says of `sent`, at `now`, both in milliseconds since the
/// Unix epoch, in time zone `zone`: the time alone for today, then
/// "Yesterday" and the time, the day of the week and the time for the six
/// days before, and the date and the time for longer ago, with the year if
/// it was another year. The clock is 24 hours.
pub(in crate::chat) fn when(sent: u64, now: u64, zone: &TimeZone) -> String {
    let local = |ms: u64| {
        let ms = i64::try_from(ms).ok()?;
        Some(Timestamp::from_millisecond(ms).ok()?.to_zoned(zone.clone()))
    };
    let (Some(sent), Some(now)) = (local(sent), local(now)) else {
        return String::new();
    };
    let time = sent.strftime("%H:%M");
    let days = (now.date().since(sent.date())).map_or(-1, |span| span.get_days());
    match days {
        0 => time.to_string(),
        1 => format!("Yesterday {time}"),
        2..=6 => sent.strftime("%A %H:%M").to_string(),
        _ if sent.year() == now.year() => sent.strftime("%-d %b %H:%M").to_string(),
        _ => sent.strftime("%-d %b %Y %H:%M").to_string(),
    }
}

impl ChatView {
    /// Lays out what the line above each turn in `turns` sent long after
    /// the one before it says, if that's changed.
    pub(super) fn label_dividers(
        &mut self,
        fonts: &mut Fonts,
        m: Measures,
        turns: std::ops::Range<usize>,
    ) {
        let now = self.now();
        // Only looked up if a line's in view.
        let zone = OnceCell::new();
        for index in turns {
            let sent = divided(&self.node.turns, index);
            let Some(slot) = self.cache.turns.get_mut(index) else {
                continue;
            };
            let Some(sent) = sent else {
                slot.divider = None;
                continue;
            };
            let label = when(sent, now, zone.get_or_init(TimeZone::system));
            if (slot.divider.as_ref()).is_none_or(|laid| laid.source != label) {
                let look = Look::Plain(Tone::Muted);
                let laid = lay_out(&mut self.layout_cx, fonts, m, &label, look, f32::INFINITY);
                self.cache.turns[index].divider = Some(laid);
            }
        }
    }

    /// Draws the line above turn `index`, with the column's left and the
    /// line's top at `origin`: the time in the middle, and a rule either
    /// side of it.
    pub(super) fn draw_divider(
        &self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        index: usize,
        origin: [f32; 2],
    ) {
        let Some(laid) = (self.cache.turns.get(index)).and_then(|slot| slot.divider.as_ref())
        else {
            return;
        };
        let column = self.cache.width;
        let middle = (origin[1] + height(m) / 2.0).round();
        let left = origin[0] + ((column - laid.width) / 2.0).round();
        draw_laid(
            canvas,
            m,
            laid,
            [left, origin[1] + (height(m) - laid.height) / 2.0],
            laid.width,
        );
        let gap = m.em(0.8);
        let rules = [
            [origin[0], left - gap],
            [left + laid.width + gap, origin[0] + column],
        ];
        for [from, to] in rules {
            if to > from {
                canvas.fill(Fill::solid([from, middle, to - from, 1.0], Paint::Divider));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{Item, Outcome, Turn, TurnEnd};
    use jiff::tz::{Offset, TimeZone};

    use super::super::PROMPT;
    use super::{divided, when};
    use crate::chat::ChatView;
    use crate::chat::fixtures::{fonts, paint, talk};

    /// 2026-10-13 14:32 UTC, a Tuesday, in milliseconds since the epoch.
    const TUESDAY: u64 = 1_791_901_920_000;
    const MINUTE: u64 = 60_000;
    const DAY: u64 = 24 * 60 * MINUTE;

    #[test]
    fn the_time_says_the_day_as_far_back_as_it_needs() {
        let utc = TimeZone::UTC;
        assert_eq!(when(TUESDAY, TUESDAY + 5 * MINUTE, &utc), "14:32");
        assert_eq!(when(TUESDAY - DAY, TUESDAY, &utc), "Yesterday 14:32");
        assert_eq!(when(TUESDAY - 2 * DAY, TUESDAY, &utc), "Sunday 14:32");
        assert_eq!(when(TUESDAY - 6 * DAY, TUESDAY, &utc), "Wednesday 14:32");
        assert_eq!(when(TUESDAY - 7 * DAY, TUESDAY, &utc), "6 Oct 14:32");
        assert_eq!(
            when(TUESDAY - 365 * DAY, TUESDAY, &utc),
            "13 Oct 2025 14:32"
        );
        // A clock behind the turn's: its date.
        assert_eq!(when(TUESDAY + DAY, TUESDAY, &utc), "14 Oct 14:32");
        // The day is the zone's: 14:32 UTC is just past midnight in UTC+10,
        // where 13:55 UTC is five to midnight the day before.
        let sydney = TimeZone::fixed(Offset::constant(10));
        assert_eq!(when(TUESDAY, TUESDAY, &sydney), "00:32");
        assert_eq!(
            when(TUESDAY - 37 * MINUTE, TUESDAY, &sydney),
            "Yesterday 23:55"
        );
    }

    /// A chat whose turns were sent at `sent`, each ending a minute later.
    fn sent_at(sent: &[u64]) -> clackatoo_protocol::ChatNode {
        let mut node = talk(&vec![("question", "answer"); sent.len()]);
        for (turn, &at) in node.turns.iter_mut().zip(sent) {
            turn.sent = at;
            turn.end = Some(TurnEnd {
                at: at + MINUTE,
                outcome: Outcome::Done,
            });
        }
        node
    }

    #[test]
    fn the_time_shows_only_after_half_an_hour_from_the_end_of_the_turn_before() {
        let node = sent_at(&[
            TUESDAY,
            TUESDAY + 20 * MINUTE,
            TUESDAY + 2 * 60 * MINUTE,
            0,
            TUESDAY + 9 * 60 * MINUTE,
        ]);
        let shown: Vec<Option<u64>> = (0..5).map(|turn| divided(&node.turns, turn)).collect();
        // Never above the first, nor 20 minutes on; above one sent two
        // hours on; never beside a turn that doesn't say when it was sent.
        assert_eq!(
            shown,
            [None, None, Some(TUESDAY + 2 * 60 * MINUTE), None, None]
        );
        // From the turn before's end: one that worked for an hour, and a
        // message sent ten minutes after it ended, shows none.
        let mut node = sent_at(&[TUESDAY, TUESDAY + 70 * MINUTE]);
        node.turns[0].end.as_mut().unwrap().at = TUESDAY + 60 * MINUTE;
        assert_eq!(divided(&node.turns, 1), None);
    }

    #[test]
    fn the_line_is_part_of_its_turns_height_and_guessed_with_it() {
        let mut fonts = fonts();
        let close = sent_at(&[TUESDAY, TUESDAY + 5 * MINUTE]);
        let apart = sent_at(&[TUESDAY, TUESDAY + 3 * 60 * MINUTE]);
        let mut height = |node| {
            let mut chat = ChatView::new(String::new(), node);
            chat.set_clock(TUESDAY + DAY);
            paint(&mut chat, &mut fonts);
            let laid = chat.cache.turns[1].height.unwrap();
            let guessed = super::super::guess(
                super::super::Measures::of(chat.cache.size),
                chat.cache.width,
                &chat.node.turns[1],
                false,
                divided(&chat.node.turns, 1).is_some(),
            );
            (laid, guessed, chat)
        };
        let (near, near_guess, _) = height(close);
        let (far, far_guess, chat) = height(apart);
        let m = super::super::Measures::of(chat.cache.size);
        let line = super::height(m) + super::below(m);
        assert!((far - near - line).abs() < 0.5, "{near} {far}");
        assert!((far_guess - near_guess - line).abs() < 0.5);
        // It says when, and isn't text to select: the turn's first text is
        // its message, as for a turn without it.
        let label = chat.cache.turns[1].divider.as_ref().expect("a label");
        assert!(label.source.contains(':'), "{}", label.source);
        assert_eq!(
            chat.turn_texts(1).first().map(|key| key.piece),
            Some(PROMPT)
        );
        assert!(chat.cache.turns[0].divider.is_none());
    }

    #[test]
    fn a_turn_ending_late_moves_the_line_from_the_one_after() {
        use clackatoo_protocol::{ChatPatch, Patch, ViewUpdate};

        let mut fonts = fonts();
        let mut node = sent_at(&[TUESDAY, TUESDAY + 40 * MINUTE]);
        node.turns[0].end = None;
        node.turns[0].items.push(Item::Words("more".into()));
        let mut chat = ChatView::new(String::new(), node);
        chat.set_clock(TUESDAY + 41 * MINUTE);
        paint(&mut chat, &mut fonts);
        assert!(chat.cache.turns[1].divider.is_some());
        // The turn before is replaced, as ending late: no gap now.
        let mut ended: Turn = chat.node.turns[0].clone();
        ended.end = Some(TurnEnd {
            at: TUESDAY + 30 * MINUTE,
            outcome: Outcome::Done,
        });
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Turn {
            index: 0,
            turn: ended,
        })]));
        paint(&mut chat, &mut fonts);
        assert!(divided(&chat.node.turns, 1).is_none());
        let laid = chat.cache.turns[1].laid.as_ref().unwrap();
        assert!(
            laid.spans
                .iter()
                .all(|span| span.piece != super::super::Piece::Divider)
        );
    }
}
