//! A strip of names side by side, such as the tabs in the bottom bar or the
//! workspaces in the top bar, when there may be more than fit: those that
//! do show whole, with a quiet count at either end of how many are hidden
//! that way, which scrolls the strip when clicked.
//!
//! The strip keeps the selected name in view, scrolling no further than it
//! must, so it stays put as names are clicked. Once a count is clicked, it
//! stays where that put it until the selection or the room changes. Room
//! for the counts is kept whenever the names overflow, so nothing moves as
//! one comes or goes.
//!
//! Laying it out measures only the names it shows, and those it passes on
//! the way to the selection.

use std::ops::Range;

use super::Rect;

/// Where a strip is scrolled to, kept by the window from one frame to the
/// next.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Scroll {
    /// The first name shown.
    pub(crate) first: usize,
    /// Whether a count was clicked to scroll it there, which holds it there
    /// even with the selection out of view.
    by_hand: bool,
    /// The selection, by its place and its ID, and how long the strip
    /// was, when it was last laid out: a change in any brings the
    /// selection back into view.
    followed: Option<(usize, u32, u32)>,
}

/// One end's count of the names hidden that way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Hidden {
    /// How many.
    pub(crate) count: usize,
    pub(crate) rect: Rect,
    /// Where clicking it scrolls the strip to: a strip's length on.
    pub(crate) to: Scroll,
}

/// Which end of a strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Before,
    After,
}

/// A strip, laid out.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Strip {
    /// The names shown, by their place in all of them, and where each is.
    pub(crate) shown: Vec<(usize, Rect)>,
    /// The counts of names hidden before and after those shown, where there
    /// are any.
    pub(crate) before: Option<Hidden>,
    pub(crate) after: Option<Hidden>,
    /// Where the strip ends: after its last name, or with room for the
    /// counts, at the end of its span.
    pub(crate) end: f32,
    /// Where it's scrolled to now, for the window to keep.
    pub(crate) scroll: Scroll,
}

impl Strip {
    /// The count at `end`, if there's one.
    pub(crate) fn hidden(&self, end: End) -> Option<&Hidden> {
        match end {
            End::Before => self.before.as_ref(),
            End::After => self.after.as_ref(),
        }
    }
}

/// How a strip is laid out, besides what's in it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Room {
    /// Where it may go, across.
    pub(crate) span: (f32, f32),
    /// Its top, and how high it is.
    pub(crate) y: f32,
    pub(crate) height: f32,
    /// The space between names.
    pub(crate) spacing: f32,
    /// How wide each count is.
    pub(crate) count_width: f32,
    /// The least room between the counts worth showing names in: with
    /// less, the selection shows alone, with no counts, cut short to the
    /// strip if it must be.
    pub(crate) least: f32,
    /// Whether the room for the count before is kept even while none are
    /// hidden that way, so the names don't move as one comes. Without it,
    /// the strip starts at its start while it's scrolled to its first name.
    pub(crate) keep_before: bool,
}

/// Whether `count` names, each as wide as `width` says for its place, fit
/// side by side in `span`, `spacing` apart. Measures no more than fit, and
/// one more.
pub(crate) fn fits_all(
    count: usize,
    width: &mut dyn FnMut(usize) -> f32,
    (start, end): (f32, f32),
    spacing: f32,
) -> bool {
    fits_in(0..count, end - start, spacing, width)
}

/// Lays out `count` names, each as wide as `width` says for its place, in
/// `room`, scrolled from `scroll` as it was last time, keeping `selected`,
/// by its place and its ID, in view unless a count was clicked since it
/// last changed. A name too wide for the strip alone is given the strip's
/// width, for its text to be cut short, as is the selection shown alone
/// in a strip too short for the counts.
pub(crate) fn lay_out(
    count: usize,
    selected: Option<(usize, u32)>,
    width: &mut dyn FnMut(usize) -> f32,
    room: Room,
    scroll: Scroll,
) -> Strip {
    let Room {
        span: (start, end),
        y,
        height,
        spacing,
        count_width,
        least,
        keep_before,
    } = room;
    let rect = |x: f32, width: f32| Rect {
        x,
        y,
        width,
        height,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let length = (end - start).max(0.0).round() as u32;
    let selected = selected.filter(|&(selected, _)| selected < count);
    let followed = selected.map(|(selected, id)| (selected, id, length));
    let selected = selected.map(|(selected, _)| selected);
    // All of them, if they fit.
    let shown = side_by_side(0..count, (start, end), spacing, &rect, width);
    if shown.len() == count {
        let scroll = Scroll {
            first: 0,
            by_hand: false,
            followed,
        };
        return uncounted(shown, start, scroll);
    }
    // They overflow: the counts take their room at either end, if there's
    // room enough left between them.
    let inner = (start + count_width + spacing)..(end - count_width - spacing);
    if inner.end - inner.start < least {
        let shown = (selected.into_iter())
            .filter(|_| end > start)
            .map(|selected| (selected, rect(start, width(selected).min(end - start))))
            .collect();
        return uncounted(shown, start, scroll);
    }
    let fits = |range: Range<usize>, width: &mut dyn FnMut(usize) -> f32| {
        fits_in(range, inner.end - inner.start, spacing, width)
    };
    let mut first = scroll.first.min(count - 1);
    let follow = !scroll.by_hand || scroll.followed != followed;
    if let Some(selected) = selected.filter(|_| follow) {
        if selected < first {
            first = selected;
        } else if !fits(first..selected + 1, width) {
            first = earliest(selected + 1, inner.end - inner.start, spacing, width);
        }
    }
    // No room left empty after the last name, while there are names before
    // the first to fill it.
    first = first.min(earliest(count, inner.end - inner.start, spacing, width));
    // Scrolled to the first, the room before needn't be kept.
    let x = if first == 0 && !keep_before {
        start
    } else {
        inner.start
    };
    let mut shown = side_by_side(first..count, (x, inner.end), spacing, &rect, width);
    // A name too wide for the strip alone still shows, cut short.
    if shown.is_empty() {
        shown.push((first, rect(x, (inner.end - x).max(0.0))));
    }
    let past = first + shown.len();
    let latest = earliest(count, inner.end - inner.start, spacing, width);
    let scroll = Scroll {
        first,
        by_hand: scroll.by_hand && !follow,
        followed,
    };
    let to = |first: usize| Scroll {
        first,
        by_hand: true,
        followed,
    };
    let before = (first > 0).then(|| Hidden {
        count: first,
        rect: rect(start, count_width),
        to: to(earliest(first, inner.end - inner.start, spacing, width)),
    });
    let after = (past < count).then(|| Hidden {
        count: count - past,
        rect: rect(end - count_width, count_width),
        to: to(past.min(latest)),
    });
    Strip {
        shown,
        before,
        after,
        end,
        scroll,
    }
}

/// The names in `range` that fit side by side from `x` to `end`, `spacing`
/// apart, each where `rect` puts it: none after the first that doesn't.
fn side_by_side(
    range: Range<usize>,
    (mut x, end): (f32, f32),
    spacing: f32,
    rect: &dyn Fn(f32, f32) -> Rect,
    width: &mut dyn FnMut(usize) -> f32,
) -> Vec<(usize, Rect)> {
    let mut shown = Vec::new();
    for index in range {
        let item = width(index);
        if x + item > end {
            break;
        }
        shown.push((index, rect(x, item)));
        x += item + spacing;
    }
    shown
}

/// A strip of the names `shown`, from `start`, with none hidden, or none
/// counted, scrolled to `scroll`.
fn uncounted(shown: Vec<(usize, Rect)>, start: f32, scroll: Scroll) -> Strip {
    let end = shown.last().map_or(start, |(_, rect)| rect.x + rect.width);
    Strip {
        shown,
        before: None,
        after: None,
        end,
        scroll,
    }
}

/// Whether the names in `range` fit side by side in `length`, `spacing`
/// apart. Measures no more than fit, and one more.
fn fits_in(
    range: Range<usize>,
    length: f32,
    spacing: f32,
    width: &mut dyn FnMut(usize) -> f32,
) -> bool {
    let mut used = -spacing;
    for index in range {
        used += width(index) + spacing;
        if used > length {
            return false;
        }
    }
    true
}

/// The earliest name from which those up to `past` fit side by side in
/// `length`, `spacing` apart, counting back from it: at most `past - 1`,
/// so there's always one.
fn earliest(past: usize, length: f32, spacing: f32, width: &mut dyn FnMut(usize) -> f32) -> usize {
    let mut first = past.saturating_sub(1);
    let mut used = width(first);
    while first > 0 {
        let next = used + spacing + width(first - 1);
        if next > length {
            break;
        }
        used = next;
        first -= 1;
    }
    first
}

#[cfg(test)]
// Every position here is a whole number of pixels, so exact.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// A strip from 100 to 500, names 10 apart, counts 30 wide: 320 for
    /// names when they overflow, between 140 and 460.
    const ROOM: Room = Room {
        span: (100.0, 500.0),
        y: 0.0,
        height: 20.0,
        spacing: 10.0,
        count_width: 30.0,
        least: 50.0,
        keep_before: true,
    };

    /// Lays out `count` names 100 wide, `selected` picked, from `scroll`.
    fn strip(count: usize, selected: usize, scroll: Scroll) -> Strip {
        lay_out(count, Some((selected, 0)), &mut |_| 100.0, ROOM, scroll)
    }

    fn shown(strip: &Strip) -> Vec<usize> {
        strip.shown.iter().map(|&(index, _)| index).collect()
    }

    fn counts(strip: &Strip) -> (Option<usize>, Option<usize>) {
        (
            strip.before.map(|hidden| hidden.count),
            strip.after.map(|hidden| hidden.count),
        )
    }

    #[test]
    fn names_that_fit_show_with_no_counts() {
        let strip = strip(3, 0, Scroll::default());
        assert_eq!(shown(&strip), [0, 1, 2]);
        assert_eq!(counts(&strip), (None, None));
        assert_eq!(strip.end, 100.0 + 3.0 * 100.0 + 2.0 * 10.0);
        assert_eq!(strip.shown[0].1.x, 100.0);
    }

    #[test]
    fn overflowing_names_count_those_hidden_either_way() {
        // Three of 100 fit in 320.
        let strip = strip(15, 0, Scroll::default());
        assert_eq!(shown(&strip), [0, 1, 2]);
        assert_eq!(counts(&strip), (None, Some(12)));
        // The first starts after the room kept for the count before, so it
        // doesn't move once there's one.
        assert_eq!(strip.shown[0].1.x, 140.0);
        let after = strip.after.unwrap();
        assert_eq!((after.rect.x, after.rect.width), (470.0, 30.0));
        assert_eq!(strip.end, 500.0);
    }

    #[test]
    fn the_selection_is_kept_in_view_scrolling_no_further_than_it_must() {
        let strip = self::strip(15, 7, Scroll::default());
        // Brought in at the end, scrolling as little as it can.
        assert_eq!(shown(&strip), [5, 6, 7]);
        assert_eq!(counts(&strip), (Some(5), Some(7)));
        // Picking one shown moves nothing.
        let again = self::strip(15, 6, strip.scroll);
        assert_eq!(again.shown, strip.shown);
        // One before comes in at the start.
        let back = self::strip(15, 2, again.scroll);
        assert_eq!(shown(&back), [2, 3, 4]);
        // The last: no room left empty after it.
        let last = self::strip(15, 14, back.scroll);
        assert_eq!(shown(&last), [12, 13, 14]);
        assert_eq!(counts(&last), (Some(12), None));
    }

    #[test]
    fn laying_out_again_from_where_it_was_changes_nothing() {
        for selected in [0, 4, 7, 14] {
            let once = strip(15, selected, Scroll::default());
            let twice = strip(15, selected, once.scroll);
            assert_eq!(once, twice, "{selected}");
        }
    }

    #[test]
    fn a_count_clicked_scrolls_a_strip_on_and_it_stays_there() {
        let strip = self::strip(15, 0, Scroll::default());
        let on = strip.after.unwrap().to;
        let scrolled = self::strip(15, 0, on);
        // The next three, though the selection is out of view now.
        assert_eq!(shown(&scrolled), [3, 4, 5]);
        assert_eq!(counts(&scrolled), (Some(3), Some(9)));
        // Laid out again, as every frame is, it stays.
        assert_eq!(self::strip(15, 0, scrolled.scroll), scrolled);
        // And back, to where it started.
        let back = self::strip(15, 0, scrolled.before.unwrap().to);
        assert_eq!(shown(&back), [0, 1, 2]);
        // Scrolling on to the end stops with the last name at the end.
        let mut scroll = Scroll::default();
        for _ in 0..10 {
            let laid = self::strip(15, 0, scroll);
            match laid.after {
                Some(after) => scroll = after.to,
                None => break,
            }
        }
        assert_eq!(shown(&self::strip(15, 0, scroll)), [12, 13, 14]);
    }

    #[test]
    fn picking_another_or_resizing_brings_the_selection_back() {
        let strip = self::strip(15, 0, Scroll::default());
        let scrolled = self::strip(15, 0, strip.after.unwrap().to).scroll;
        // Another picked.
        assert_eq!(shown(&self::strip(15, 1, scrolled)), [1, 2, 3]);
        // The strip resized.
        let wider = Room {
            span: (100.0, 610.0),
            ..ROOM
        };
        let laid = lay_out(15, Some((0, 0)), &mut |_| 100.0, wider, scrolled);
        assert_eq!(shown(&laid)[0], 0);
    }

    #[test]
    fn names_shown_keep_their_places_as_a_count_comes_and_goes() {
        // The selection scrolled one on: the count before appears, in room
        // kept for it, so the strip still starts where it did.
        let start = strip(15, 2, Scroll::default());
        assert_eq!(start.before, None);
        let next = strip(15, 3, start.scroll);
        assert_eq!(shown(&next), [1, 2, 3]);
        assert!(next.before.is_some());
        assert_eq!(start.shown[0].1.x, 140.0);
        assert_eq!(next.shown[0].1.x, 140.0);
    }

    #[test]
    fn closing_names_at_the_end_scrolls_back_to_fill_the_strip() {
        // Scrolled to the end by hand, away from the selection.
        let scroll = Scroll {
            first: 12,
            by_hand: true,
            followed: strip(15, 0, Scroll::default()).scroll.followed,
        };
        assert_eq!(shown(&strip(15, 0, scroll)), [12, 13, 14]);
        let fewer = strip(13, 0, scroll);
        assert_eq!(shown(&fewer), [10, 11, 12]);
        assert_eq!(counts(&fewer), (Some(10), None));
    }

    #[test]
    fn a_name_too_wide_for_the_strip_shows_cut_to_it() {
        let laid = lay_out(
            4,
            Some((2, 0)),
            &mut |index| if index == 2 { 900.0 } else { 100.0 },
            ROOM,
            Scroll::default(),
        );
        assert_eq!(shown(&laid), [2]);
        assert_eq!(laid.shown[0].1.width, 320.0);
    }

    #[test]
    fn a_strip_too_short_for_the_counts_shows_the_selection_alone() {
        // 120 long: 20 between the counts, less than the least of 50.
        let short = Room {
            span: (100.0, 220.0),
            ..ROOM
        };
        let laid = lay_out(15, Some((7, 0)), &mut |_| 100.0, short, Scroll::default());
        assert_eq!(shown(&laid), [7]);
        assert_eq!(counts(&laid), (None, None));
        assert_eq!((laid.shown[0].1.x, laid.shown[0].1.width), (100.0, 100.0));
        // Shorter than the name: cut to the strip.
        let shorter = Room {
            span: (100.0, 160.0),
            ..ROOM
        };
        let laid = lay_out(15, Some((7, 0)), &mut |_| 100.0, shorter, Scroll::default());
        assert_eq!(laid.shown[0].1.width, 60.0);
        assert_eq!(laid.end, 160.0);
    }

    #[test]
    fn without_room_kept_before_the_strip_starts_at_its_start_until_scrolled() {
        let room = Room {
            keep_before: false,
            ..ROOM
        };
        let laid = lay_out(15, Some((0, 0)), &mut |_| 100.0, room, Scroll::default());
        assert_eq!(laid.shown[0].1.x, 100.0);
        assert_eq!(laid.before, None);
        // Scrolled on, the count before takes its room.
        let on = lay_out(
            15,
            Some((0, 0)),
            &mut |_| 100.0,
            room,
            laid.after.unwrap().to,
        );
        assert_eq!(on.shown[0].1.x, 140.0);
        assert!(on.before.is_some());
    }

    #[test]
    fn only_the_names_shown_and_those_passed_are_measured() {
        let mut measured = 0;
        let laid = lay_out(
            1000,
            Some((500, 0)),
            &mut |_| {
                measured += 1;
                100.0
            },
            ROOM,
            Scroll {
                first: 499,
                ..Scroll::default()
            },
        );
        assert_eq!(shown(&laid), [499, 500, 501]);
        assert!(measured < 40, "{measured}");
    }
}
