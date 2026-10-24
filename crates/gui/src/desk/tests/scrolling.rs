//! The window's real wheel route: fractional rows, animation lifetime,
//! cancellation and application ownership, independent of wall-clock sleeps.
use super::*;
use std::time::{Duration, Instant};

fn text() -> String {
    use std::fmt::Write;
    (0..100).fold(String::new(), |mut text, n| {
        writeln!(text, "line {n}").unwrap();
        text
    })
}

#[test]
fn trackpad_pixels_keep_partial_rows_and_clicks_match_the_visible_text() {
    let mut desk = editing("/t.txt", &text());
    normal_mode(&mut desk);
    let point = cell_point(0, 6);
    assert_eq!(
        desk.smooth_wheel(point, -0.5, true, Modifiers::default()),
        Some(true)
    );
    desk.settle_editors();
    let offset = desk.row_scroll(PaneId(1)).expect("half a row");
    assert!((offset.fraction - 0.5).abs() < 0.0001);
    assert!(
        desk.scroll_wakes().is_none(),
        "trackpads have their own momentum"
    );
    assert_eq!(desk.cell_of(point, PaneId(1)).unwrap()[0], 1);
    let editor = desk.editor(PaneId(1)).unwrap();
    assert_eq!(editor.scroll_extra, 1);
    // Reverse the remaining half row at the top, without being stuck at a bound.
    desk.smooth_wheel(point, 0.5, true, Modifiers::default());
    assert!(desk.row_scroll(PaneId(1)).is_none());
    assert_eq!(desk.editor(PaneId(1)).unwrap().scroll_extra, 0);
    assert_eq!(desk.cell_of(point, PaneId(1)).unwrap()[0], 0);
}

#[test]
fn wheel_motion_finishes_exactly_and_keyboard_input_cancels_it() {
    let mut desk = editing("/t.txt", &text());
    normal_mode(&mut desk);
    let point = cell_point(0, 6);
    desk.smooth_wheel(point, -3.0, false, Modifiers::default());
    assert!(desk.scroll_wakes().is_some());
    assert!(desk.tick_scrolling(Instant::now() + Duration::from_millis(150)));
    desk.settle_editors();
    let (rows, _) = desk.editor(PaneId(1)).unwrap().scene();
    assert!(rows[0].text().contains("line 3"));
    assert!(desk.row_scroll(PaneId(1)).is_none());
    assert!(desk.scroll_wakes().is_none());
    desk.smooth_wheel(point, -3.0, false, Modifiers::default());
    type_keys(&mut desk, "j");
    assert!(desk.scroll_wakes().is_none());
    assert!(!desk.tick_scrolling(Instant::now() + Duration::from_secs(1)));
    command(&mut desk, "set nosmoothscroll");
    desk.smooth_wheel(point, -3.0, false, Modifiers::default());
    assert!(desk.scroll_wakes().is_none());
}

#[test]
fn a_programs_mouse_reports_and_alternate_screen_are_never_animated() {
    let mut desk = desk(one_tab(pane(1)));
    let mut term = Terminal::new(Size::new(80, 23).unwrap(), &Config::default());
    term.advance(b"\x1b[?1049h\x1b[?1000h");
    desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&term)));
    assert_eq!(
        desk.smooth_wheel(cell_point(0, 0), 3.0, false, Modifiers::default()),
        None
    );
    assert!(desk.scroll_wakes().is_none());
}

#[test]
fn terminal_scrollback_keeps_pixel_precision_without_sending_program_input() {
    let mut desk = desk(one_tab(pane(1)));
    let mut term = Terminal::new(Size::new(80, 23).unwrap(), &Config::default());
    for line in 0..100 {
        term.advance(format!("line {line}\r\n").as_bytes());
    }
    desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&term)));
    desk.take_outbox();
    assert_eq!(
        desk.smooth_wheel(cell_point(0, 0), 1.5, true, Modifiers::default()),
        Some(true)
    );
    let offset = desk.row_scroll(PaneId(1)).unwrap();
    assert!((offset.fraction - 0.5).abs() < 0.0001);
    assert_eq!(mode(&desk, 1), Mode::Insert);
    assert!(desk.scroll_wakes().is_none());
    assert!(
        !desk
            .take_outbox()
            .iter()
            .any(|message| matches!(message, ClientMessage::Request(Request::SendInput { .. })))
    );
}

#[test]
fn explorer_headers_stay_fixed_while_body_clicks_follow_fractional_rows() {
    let mut desk = desk(one_tab(files_pane(2, 1)));
    explore_with(
        &mut desk,
        (0..100)
            .map(|n| entry(&format!("file-{n:03}"), false))
            .collect(),
    );
    desk.settle_explorers();
    let header = desk.explorer(PaneId(2)).unwrap().scene().0[0].text();
    desk.smooth_wheel(cell_point(2, 3), -0.5, true, Modifiers::default());
    desk.settle_explorers();
    let scroll = desk.row_scroll(PaneId(2)).unwrap();
    assert_eq!(scroll.first, 1);
    assert_eq!(
        desk.explorer(PaneId(2)).unwrap().scene().0[0].text(),
        header
    );
    assert_eq!(desk.cell_of(cell_point(0, 3), PaneId(2)).unwrap()[0], 0);
    assert_eq!(desk.cell_of(cell_point(1, 3), PaneId(2)).unwrap()[0], 2);
    assert!(
        desk.explorers
            .panes
            .get_mut(&PaneId(2))
            .unwrap()
            .click(scroll.end, false)
    );
    desk.smooth_wheel(cell_point(2, 3), 1000.0, true, Modifiers::default());
    assert!(
        desk.row_scroll(PaneId(2)).is_none(),
        "no overscroll above the first entry"
    );
}
