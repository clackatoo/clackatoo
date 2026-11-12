//! The settings menu scrolls as panes do (see `desk::scrolling`): Easy's
//! page by pixels, laid out once and drawn moved, Advanced's list by rows;
//! smoothly with `smoothscroll`, at once without, and clicks go where it's
//! drawn, part way there.
#![allow(clippy::float_cmp, reason = "a view at rest is exactly where it's put")]

use std::time::{Duration, Instant};

use super::*;
use crate::chrome::{Geometry, Metrics};
use crate::render::Paint;
use crate::settings::view::{Laid, Scrolled, SettingsHit, Sizes, lay_out};

const METRICS: Metrics = Metrics {
    cell: [10.0, 20.0],
    ui_line: 16.0,
    padding: 4.0,
};

/// A desk with the menu open in Easy, on Appearance, which is longer than
/// a short window, scrolling smoothly if `smooth`.
fn easy_desk(smooth: bool) -> Desk {
    let mut desk = three_tabs();
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    if !smooth {
        command(&mut desk, "set nosmoothscroll");
    }
    command(&mut desk, "Settings");
    if desk.settings_mode_shown() != Some(crate::settings::Mode::Easy) {
        type_keys(&mut desk, "m");
    }
    desk
}

/// Easy laid out in a short window, as the window lays it out, and told
/// to the desk.
fn laid(desk: &mut Desk) -> Laid {
    let view = desk.easy_view().expect("Easy");
    let geometry = Geometry::new([1280, 500], METRICS);
    #[allow(clippy::cast_precision_loss)]
    let mut measure = |text: &str, _bold: bool| 8.0 * text.chars().count() as f32;
    let laid = lay_out(&view, &geometry, Sizes { mono_em: 8.0 }, &mut measure);
    desk.settings_drawn(&laid);
    laid
}

/// Where row `label`'s words are drawn, scrolled to `at`.
fn drawn(laid: &Laid, at: Scrolled, label: &str) -> Option<[f32; 2]> {
    let mut painted = crate::chrome::Painted::default();
    laid.paint(None, at, &mut painted);
    (painted.over_texts.iter())
        .find(|text| text.content == label)
        .map(|text| [text.origin[0] + 2.0, text.origin[1] + 4.0])
}

/// The scroll bar's thumb, as drawn scrolled to `at`.
fn thumb(laid: &Laid, at: Scrolled) -> [f32; 4] {
    let mut painted = crate::chrome::Painted::default();
    laid.paint(None, at, &mut painted);
    (painted.over.iter())
        .rfind(|fill| fill.paint == Paint::Track && fill.rect[2] < 10.0)
        .expect("a scroll bar")
        .rect
}

#[test]
fn easys_page_scrolls_smoothly_without_laying_it_out_again_and_clicks_follow() {
    let mut desk = easy_desk(true);
    let laid = laid(&mut desk);
    assert!(
        laid.scrolling().unwrap().most() > 200.0,
        "longer than the room"
    );
    let start = desk.settings_scrolled();
    assert_eq!(start.page, 0.0);
    let bar = thumb(&laid, start);
    // Ten lines down, 20 pixels a line: on its way there, not there.
    assert_eq!(desk.settings_smooth_wheel(-10.0, false, 20.0), Some(true));
    assert!(desk.scroll_wakes().is_some(), "frames while it animates");
    let now = Instant::now();
    assert!(desk.tick_scrolling(now + Duration::from_millis(30)));
    let between = desk.settings_scrolled();
    assert!(between.page > 0.0 && between.page < 200.0, "{between:?}");
    // What's drawn there mid-way is what a click there hits, the bar too.
    let label = "Size";
    let point = drawn(&laid, between, label).expect("the row is drawn");
    let row = match laid.hit(point, between) {
        Some(SettingsHit::Row(row)) => row,
        other => panic!("{other:?} at {point:?}"),
    };
    // Unscrolled, it isn't there.
    assert_ne!(
        laid.hit(point, Scrolled::default()),
        Some(SettingsHit::Row(row))
    );
    assert!(thumb(&laid, between)[1] > bar[1], "the bar moves with it");
    // And there, exactly, after which nothing asks for frames.
    assert!(desk.tick_scrolling(now + Duration::from_millis(200)));
    assert!((desk.settings_scrolled().page - 200.0).abs() < 0.001);
    assert!(
        desk.scroll_wakes().is_none(),
        "the idle menu doesn't redraw"
    );
    assert!(!desk.tick_scrolling(now + Duration::from_millis(300)));
    // Never past its ends.
    desk.settings_smooth_wheel(1000.0, false, 20.0);
    desk.tick_scrolling(now + Duration::from_secs(1));
    assert_eq!(desk.settings_scrolled().page, 0.0);
}

#[test]
fn without_smoothscroll_the_page_jumps_and_a_touchpad_never_animates() {
    let mut desk = easy_desk(true);
    laid(&mut desk);
    // A touchpad's pixels are its own motion: at once, part rows and all.
    desk.settings_smooth_wheel(-1.5, true, 20.0);
    assert!((desk.settings_scrolled().page - 30.0).abs() < 0.001);
    assert!(desk.scroll_wakes().is_none());
    let mut desk = easy_desk(false);
    laid(&mut desk);
    desk.settings_smooth_wheel(-1.5, true, 20.0);
    desk.settings_smooth_wheel(-3.0, false, 20.0);
    assert!((desk.settings_scrolled().page - 90.0).abs() < 0.001);
    assert!(desk.scroll_wakes().is_none());
}

#[test]
fn keys_bring_the_row_focused_into_view_through_the_same_motion() {
    let mut desk = easy_desk(true);
    let first = laid(&mut desk);
    let most = first.scrolling().unwrap().most();
    // The last row, which is below the room.
    type_keys(&mut desk, "G");
    laid(&mut desk);
    assert!(desk.scroll_wakes().is_some(), "it scrolls there smoothly");
    desk.tick_scrolling(Instant::now() + Duration::from_millis(200));
    assert!((desk.settings_scrolled().page - most).abs() < 0.5);
    // A new page starts at its top, at once.
    type_keys(&mut desk, "}");
    laid(&mut desk);
    assert_eq!(desk.settings_scrolled().page, 0.0);
    assert!(desk.scroll_wakes().is_none());
}

#[test]
fn advanceds_list_scrolls_by_rows_and_a_click_picks_the_row_drawn() {
    let mut desk = easy_desk(true);
    type_keys(&mut desk, "m");
    let size = (10, 100);
    let view = desk.finder_view(size, None).expect("Advanced");
    assert_eq!(view.shift, 0.0);
    // Far enough down that the list follows, which it does in time.
    type_keys(&mut desk, "jjjjjjjjjjjjjjj");
    desk.finder_view(size, None);
    assert!(desk.scroll_wakes().is_some());
    desk.tick_scrolling(Instant::now() + Duration::from_millis(40));
    let view = desk.finder_view(size, None).expect("Advanced");
    assert!(view.shift > 0.0, "part way between rows");
    assert_eq!(view.rows.len(), 11, "and one more drawn at the foot");
    // A click on the first row drawn picks that one.
    let first = view.rows[0].text.clone();
    desk.finder_click(0, false);
    let view = desk.finder_view(size, None).expect("Advanced");
    let picked = view.selected.map(|at| view.rows[at].text.clone());
    assert_eq!(picked, Some(first));
    desk.tick_scrolling(Instant::now() + Duration::from_secs(1));
    assert_eq!(desk.finder_view(size, None).unwrap().shift, 0.0);
    // Without smoothscroll, never between rows.
    let mut desk = easy_desk(false);
    type_keys(&mut desk, "m");
    desk.finder_view(size, None);
    type_keys(&mut desk, "jjjjjjjjjjjjjjjjjjjj");
    assert_eq!(desk.finder_view(size, None).unwrap().shift, 0.0);
    assert!(desk.scroll_wakes().is_none());
}
