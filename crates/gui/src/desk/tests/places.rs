//! Cross-pane navigation goes through the window, as actual keys do.
use super::*;
use crate::vim::place::Place;

fn reading_terminal() -> Desk {
    let mut desk = desk(one_tab(pane(1)));
    let mut terminal = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
    terminal.advance(b"alpha\r\nbeta\r\ngamma\r\ndelta\r\n");
    desk.view_changed(
        TerminalId(1),
        &ViewUpdate::Snapshot(View::capture(&terminal)),
    );
    normal_mode(&mut desk);
    type_keys(&mut desk, "gg");
    desk
}
fn line(desk: &Desk) -> u64 {
    let Place::Line { at, .. } = desk.place_of(PaneId(1), None).unwrap().place else {
        panic!("terminal place")
    };
    at.line
}
#[test]
fn terminal_macros_marks_and_jump_history_work_together() {
    let mut desk = reading_terminal();
    type_keys(&mut desk, "qajjq");
    assert_eq!(desk.recording(), None);
    assert_eq!(line(&desk), 2);
    type_keys(&mut desk, "@a");
    assert_eq!(line(&desk), 4);
    type_keys(&mut desk, "ma10G");
    assert_eq!(line(&desk), 9);
    ctrl(&mut desk, "o");
    assert_eq!(line(&desk), 4);
    ctrl(&mut desk, "i");
    assert_eq!(line(&desk), 9);
    type_keys(&mut desk, "`a");
    assert_eq!(line(&desk), 4);
    type_keys(&mut desk, "''");
    assert_eq!(line(&desk), 9);
}
#[test]
fn global_marks_move_between_files_and_terminals_and_share_one_namespace() {
    let mut desk = desk(one_tab(side_by_side(vec![file_pane(1, 7), pane(2)])));
    load(&mut desk, 7, "/t.txt", "one\ntwo\nthree\n");
    snapshot(&mut desk, 2);
    type_keys(&mut desk, "jmamA");
    ctrl(&mut desk, "l");
    type_keys(&mut desk, "5GmB");
    type_keys(&mut desk, "`A");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert!(
        matches!(desk.place_of(PaneId(1), None).unwrap().place, Place::File(at) if at.line == 1)
    );
    type_keys(&mut desk, "`B");
    assert_eq!(shown_pane(&desk), PaneId(2));
    let at = desk.place_of(PaneId(2), None).unwrap();
    assert!(matches!(at.place, Place::Line { at, .. } if at.line == 4));
    // Reassigning the same capital letter in another pane replaces it.
    type_keys(&mut desk, "mA");
    ctrl(&mut desk, "h");
    type_keys(&mut desk, "`A");
    assert_eq!(shown_pane(&desk), PaneId(2));
}
#[test]
fn a_missing_mark_stops_the_rest_of_a_macro() {
    let mut desk = reading_terminal();
    type_keys(&mut desk, "qa`zjq");
    type_keys(&mut desk, "gg@a");
    assert_eq!(line(&desk), 0);
}

#[test]
fn a_terminal_snapshot_invalidates_saved_marks_instead_of_reusing_line_numbers() {
    let mut desk = reading_terminal();
    type_keys(&mut desk, "jjma");
    snapshot(&mut desk, 1);
    type_keys(&mut desk, "gg`a");
    assert_eq!(line(&desk), 0);
    assert!(note(&desk).contains("no longer available"));
}

#[test]
fn terminal_search_previews_are_not_jumps_but_acceptance_and_repeat_are() {
    let mut desk = reading_terminal();
    type_keys(&mut desk, "/delta");
    escape(&mut desk);
    assert_eq!(line(&desk), 0);
    type_keys(&mut desk, "/delta");
    enter(&mut desk);
    desk.take_outbox();
    desk.answered(Response::Found(Some(clackatoo_term::Match {
        start: clackatoo_protocol::Position { line: 3, col: 0 },
        cells: 5,
    })));
    assert_eq!(line(&desk), 3);
    ctrl(&mut desk, "o");
    assert_eq!(line(&desk), 0);
    ctrl(&mut desk, "i");
    assert_eq!(line(&desk), 3);
}

#[test]
fn chat_marks_macros_and_jumps_work_through_layout_settling() {
    let (mut desk, mut fonts) = chat_with_code();
    normal_mode(&mut desk);
    type_keys(&mut desk, "ggma");
    let first = desk.place_of(PaneId(1), None).unwrap();
    type_keys(&mut desk, "qaGq");
    desk.settle_chats(&mut fonts);
    let last = desk.place_of(PaneId(1), None).unwrap();
    assert_ne!(first, last);
    assert_eq!(desk.recording(), None);
    ctrl(&mut desk, "o");
    desk.settle_chats(&mut fonts);
    assert_eq!(desk.place_of(PaneId(1), None), Some(first.clone()));
    ctrl(&mut desk, "i");
    desk.settle_chats(&mut fonts);
    assert_eq!(desk.place_of(PaneId(1), None), Some(last.clone()));
    type_keys(&mut desk, "`a");
    desk.settle_chats(&mut fonts);
    assert_eq!(desk.place_of(PaneId(1), None), Some(first));
    type_keys(&mut desk, "@a");
    desk.settle_chats(&mut fonts);
    assert_eq!(desk.place_of(PaneId(1), None), Some(last));
}
