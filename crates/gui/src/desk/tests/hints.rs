//! Hints mode in the window (CLACKA-240): labelling what's worth picking
//! in a terminal or an editor, and opening, copying or typing it by its
//! label.

use super::*;

/// Presses `letter` with `modifiers`.
fn letter(desk: &mut Desk, letter: &str, modifiers: Modifiers) {
    press(desk, Key::Character(letter.into()), None, modifiers);
}

/// What register `name` holds.
fn held(desk: &mut Desk, name: char) -> Option<String> {
    let registers = &desk.shared_mut().registers;
    registers
        .get(Some(name))
        .map(|register| register.text.clone())
}

fn labels(desk: &Desk) -> Vec<(String, [f32; 4])> {
    (desk.hint_marks().iter())
        .map(|mark| (mark.label.to_owned(), mark.rects[0]))
        .collect()
}

#[test]
fn a_terminals_links_and_hashes_are_labelled_and_a_label_opens_one() {
    let mut desk = desk(one_tab(pane(1)));
    screen_showing(&mut desk, 1, 60, "see https://x.example/a and 7e84c2e9\r\n");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    assert!(desk.hinting());
    // The nearest the end get labels first; each where it's drawn, in a
    // pane whose cells start 5 pixels in and are 10 by 20.
    assert_eq!(
        labels(&desk),
        [
            ("a".to_owned(), [285.0, 5.0, 80.0, 20.0]),
            ("s".to_owned(), [45.0, 5.0, 190.0, 20.0]),
        ]
    );
    assert!(matches!(desk.status(), Status::Typing { .. }));
    letter(&mut desk, "s", Modifiers::default());
    assert!(!desk.hinting());
    assert_eq!(desk.take_opened(), ["https://x.example/a"]);
}

#[test]
fn shift_copies_what_a_label_picks_and_alt_types_it() {
    let mut desk = desk(one_tab(pane(1)));
    screen_showing(&mut desk, 1, 60, "commit 7e84c2e9 and 1234abcd\r\n");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    letter(&mut desk, "S", shift);
    assert_eq!(held(&mut desk, '+').as_deref(), Some("7e84c2e9"));
    // A hash has nothing to open: its label alone copies it too.
    ctrl_b(&mut desk, "f");
    letter(&mut desk, "a", Modifiers::default());
    assert_eq!(held(&mut desk, '+').as_deref(), Some("1234abcd"));
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    letter(&mut desk, "s", alt);
    let typed = answer_all(&mut desk);
    assert!(
        typed.iter().any(|request| matches!(
            request,
            Request::SendInput { terminal: TerminalId(1), bytes } if bytes == b"7e84c2e9"
        )),
        "{typed:?}"
    );
}

#[test]
fn esc_or_a_letter_no_label_starts_with_puts_hints_away() {
    let mut desk = desk(one_tab(pane(1)));
    screen_showing(&mut desk, 1, 60, "7e84c2e9\r\n");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    press(
        &mut desk,
        Key::Named(NamedKey::Escape),
        None,
        Modifiers::default(),
    );
    assert!(!desk.hinting());
    ctrl_b(&mut desk, "f");
    letter(&mut desk, "q", Modifiers::default());
    assert!(!desk.hinting());
    assert!(desk.take_opened().is_empty());
    // Nothing to pick says so.
    screen_showing(&mut desk, 1, 60, "\x1b[2J\x1b[Hnothing here\r\n");
    command(&mut desk, "hints");
    assert!(!desk.hinting());
}

/// An editor's path opens in it, as Ctrl+click on one in a terminal opens
/// in an editor in the tab.
#[test]
fn an_editors_paths_are_labelled_and_open_from_its_files_folder() {
    let mut desk = desk(one_tab(file_pane(1, 7)));
    let snapshot = clackatoo_protocol::BufferSnapshot {
        path: "/src/notes.txt".into(),
        text: "see lib/a.rs:3 or https://x.example/e\n".into(),
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    };
    desk.buffer_changed(
        BufferId(7),
        clackatoo_protocol::BufferUpdate::Snapshot(snapshot),
    );
    normal_mode(&mut desk);
    answer_all(&mut desk);
    // Laid out, as for a frame.
    desk.settle_editors();
    ctrl_b(&mut desk, "f");
    let texts: Vec<String> = labels(&desk).into_iter().map(|(label, _)| label).collect();
    assert_eq!(texts, ["a", "s"]);
    letter(&mut desk, "s", Modifiers::default());
    let asked = answer_all(&mut desk);
    assert!(
        asked.iter().any(|request| matches!(
            request,
            Request::OpenHere { pane: PaneId(1), file } if file.path == "/src/lib/a.rs"
        )),
        "{asked:?}"
    );
}

#[test]
fn a_link_wrapped_over_rows_is_one_hint_drawn_on_each() {
    let mut desk = desk(one_tab(pane(1)));
    screen_showing(&mut desk, 1, 20, "go https://x.example/a/long/path\r\n");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    let marks = desk.hint_marks();
    assert_eq!(marks.len(), 1);
    assert_eq!(
        marks[0].rects,
        [[35.0, 5.0, 170.0, 20.0], [5.0, 25.0, 120.0, 20.0]]
    );
}

#[test]
fn hintpattern_adds_matches_of_its_own_and_one_that_cant_be_read_is_said() {
    let mut desk = desk(one_tab(pane(1)));
    screen_showing(&mut desk, 1, 60, "fixes CLACKA-240\r\n");
    answer_all(&mut desk);
    desk.options.hintpattern = r"CLACKA-\d+".into();
    ctrl_b(&mut desk, "f");
    letter(&mut desk, "a", Modifiers::default());
    assert_eq!(held(&mut desk, '+').as_deref(), Some("CLACKA-240"));
    desk.options.hintpattern = "(".into();
    ctrl_b(&mut desk, "f");
    assert!(!desk.hinting(), "nothing else to pick");
}

#[test]
fn a_click_puts_hints_away_and_the_next_letter_is_the_panes() {
    let mut desk = desk(one_tab(side_by_side(vec![pane(1), pane(2)])));
    screen_showing(&mut desk, 1, 30, "7e84c2e9\r\n");
    screen_showing(&mut desk, 2, 30, "$ ");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "f");
    assert!(desk.hinting());
    assert!(desk.cancel_keys());
    assert!(!desk.hinting());
    // Gone with its pane too, as when a macro or another window moves
    // the focus: the next letter is the pane's.
    ctrl_b(&mut desk, "f");
    answer_all(&mut desk);
    desk.show_pane(PaneId(2));
    answer_all(&mut desk);
    let typed_into_two = |desk: &mut Desk| {
        (answer_all(desk).iter()).any(|request| {
            matches!(request, Request::SendInput { terminal: TerminalId(2), bytes } if bytes == b"a")
        })
    };
    normal_mode(&mut desk);
    type_keys(&mut desk, "i");
    answer_all(&mut desk);
    letter(&mut desk, "a", Modifiers::default());
    assert!(!desk.hinting());
    assert!(typed_into_two(&mut desk));
}
