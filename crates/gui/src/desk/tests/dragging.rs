//! Dragging files out of an explorer with the mouse, driven through the
//! desk as a window drives it: onto a folder to move or copy, onto an
//! editor to open, onto a terminal to type the path; and a press that
//! barely moves stays a click.

use clackatoo_protocol::{FileOp, OpenFile};

use super::*;

const LEFT: MouseButton = MouseButton::Left;

/// An explorer of `/p` (`src/`, `a.txt`), an editor of `/p/notes.txt`
/// and a terminal, side by side.
fn three_panes() -> Desk {
    let layout = side_by_side(vec![files_pane(2, 1), file_pane(4, 7), pane(3)]);
    let mut desk = desk(one_tab(layout));
    load(&mut desk, 7, "/p/notes.txt", "one\n");
    screen_showing(&mut desk, 3, 20, "$ ");
    explore_p(&mut desk);
    answer_all(&mut desk);
    desk.show_pane(PaneId(2));
    desk
}

/// The middle of the cell on `row` of `pane`, near its left.
fn at(desk: &Desk, pane: u32, row: u16) -> [f32; 2] {
    let placement = desk.placement().unwrap();
    let (_, tile) = (placement.panes.iter())
        .find(|(id, _)| *id == PaneId(pane))
        .unwrap();
    [
        tile.origin[0] + 25.0,
        tile.origin[1] + f32::from(row) * 20.0 + 10.0,
    ]
}

/// Presses on row `row` of the explorer, goes to `to`, and lets go there,
/// with `modifiers` held as it's let go. Returns what the bar said on the
/// way.
fn drag(desk: &mut Desk, row: u16, to: [f32; 2], modifiers: Modifiers) -> String {
    let from = at(desk, 2, row);
    desk.mouse(from, MouseAction::Press(LEFT), 1, Modifiers::default());
    desk.mouse(to, MouseAction::Motion(Some(LEFT)), 1, modifiers);
    let said = note(desk);
    desk.mouse(to, MouseAction::Release(LEFT), 1, modifiers);
    said
}

/// What the explorer's bar says, as a question it asks.
fn asked(desk: &Desk) -> String {
    desk.explorer(PaneId(2)).unwrap().label()
}

#[test]
fn a_file_dragged_onto_a_folder_moves_there_or_with_ctrl_is_copied() {
    let mut desk = three_panes();
    // `..` is row 1, `src` 2, `a.txt` 3.
    let src = at(&desk, 2, 2);
    let said = drag(&mut desk, 3, src, Modifiers::default());
    assert_eq!(said, "move a.txt into ~/p/src/  (Ctrl copies, Esc stops)");
    let moved = FileOp::Move {
        from: "/p/a.txt".into(),
        to: "/p/src/a.txt".into(),
        overwrite: None,
        expect: None,
    };
    assert_eq!(file_ops(&mut desk), [(0, moved)]);
    assert!(!note(&desk).contains("Esc stops"), "{}", note(&desk));

    let said = drag(&mut desk, 3, src, CTRL);
    assert_eq!(said, "copy a.txt into ~/p/src/  (Esc stops)");
    assert!(matches!(
        &file_ops(&mut desk)[..],
        [(1, FileOp::Copy { .. })]
    ));

    // Onto the folder it's in, on `..`, or into itself: nothing.
    let root = at(&desk, 2, 0);
    let said = drag(&mut desk, 3, root, Modifiers::default());
    assert_eq!(said, "a.txt is in that folder already");
    // On `..`, out of the root, only once the bar has asked: `y` moves it,
    // any other key doesn't.
    let up = at(&desk, 2, 1);
    let said = drag(&mut desk, 3, up, Modifiers::default());
    assert_eq!(said, "move a.txt into /  (Ctrl copies, Esc stops)");
    assert_eq!(file_ops(&mut desk), []);
    assert_eq!(asked(&desk), "move a.txt out of ~/p, into /? (y/n)");
    type_keys(&mut desk, "n");
    assert_eq!(file_ops(&mut desk), []);
    drag(&mut desk, 3, up, CTRL);
    assert_eq!(asked(&desk), "copy a.txt out of ~/p, into /? (y/n)");
    // Only a key pressed now answers it, as a link's question: one a
    // macro types, or held while something was waited for, gives up.
    assert!(!desk.drop_question_key(true), "left for the explorer");
    assert!(desk.drop_question_key(false));
    assert_eq!(file_ops(&mut desk), []);
    assert!(!asked(&desk).contains("(y/n)"), "{}", asked(&desk));
    assert!(!desk.drop_question_key(false), "nothing asked");
    drag(&mut desk, 3, up, CTRL);
    type_keys(&mut desk, "y");
    let copied = FileOp::Copy {
        from: "/p/a.txt".into(),
        to: "/a.txt".into(),
        overwrite: None,
        expect: None,
    };
    assert_eq!(file_ops(&mut desk), [(2, copied)]);
    // Dragged off and back onto itself.
    desk.mouse(src, MouseAction::Press(LEFT), 1, Modifiers::default());
    let away = at(&desk, 2, 5);
    desk.mouse(
        away,
        MouseAction::Motion(Some(LEFT)),
        1,
        Modifiers::default(),
    );
    desk.mouse(
        src,
        MouseAction::Motion(Some(LEFT)),
        1,
        Modifiers::default(),
    );
    assert_eq!(note(&desk), "src can't go into itself");
    desk.mouse(src, MouseAction::Release(LEFT), 1, Modifiers::default());
    assert_eq!(
        note(&desk),
        "src can't go into itself",
        "said as it's let go"
    );
    assert_eq!(file_ops(&mut desk), []);
}

#[test]
fn a_press_that_barely_moves_is_a_click_and_esc_drops_nothing() {
    let mut desk = three_panes();
    let from = at(&desk, 2, 3);
    let nudged = [from[0] + 4.0, from[1] + 4.0];
    desk.mouse(from, MouseAction::Press(LEFT), 1, Modifiers::default());
    desk.mouse(
        nudged,
        MouseAction::Motion(Some(LEFT)),
        1,
        Modifiers::default(),
    );
    desk.mouse(nudged, MouseAction::Release(LEFT), 1, Modifiers::default());
    assert_eq!(file_ops(&mut desk), []);
    assert!(note(&desk).contains("a.txt"), "the cursor's on it");

    let src = at(&desk, 2, 2);
    desk.mouse(from, MouseAction::Press(LEFT), 1, Modifiers::default());
    desk.mouse(
        src,
        MouseAction::Motion(Some(LEFT)),
        1,
        Modifiers::default(),
    );
    // The folder it would go into is lit.
    let lit = |desk: &mut Desk| -> Vec<usize> {
        desk.settle_explorers();
        let explorer = desk.explorer(PaneId(2)).unwrap();
        (explorer.highlights().into_iter())
            .filter(|lit| lit.kind == crate::render::HighlightKind::Selection)
            .map(|lit| lit.line)
            .collect()
    };
    assert_eq!(lit(&mut desk), [2]);
    press(
        &mut desk,
        Key::Named(NamedKey::Escape),
        None,
        Modifiers::default(),
    );
    assert!(lit(&mut desk).is_empty(), "put out");
    desk.mouse(src, MouseAction::Release(LEFT), 1, Modifiers::default());
    assert_eq!(file_ops(&mut desk), []);
    assert_eq!(shown_pane(&desk), PaneId(2), "Esc went nowhere else");
}

#[test]
fn a_file_dragged_onto_an_editor_opens_there_and_onto_a_terminal_types_its_path() {
    let mut desk = three_panes();
    let editor = at(&desk, 4, 2);
    let said = drag(&mut desk, 3, editor, Modifiers::default());
    assert_eq!(said, "open a.txt here");
    assert_eq!(shown_pane(&desk), PaneId(4));
    assert!(answer_all(&mut desk).contains(&Request::OpenHere {
        pane: PaneId(4),
        file: OpenFile {
            path: "/p/a.txt".into(),
            cwd: None,
        },
    }));

    desk.show_pane(PaneId(2));
    let terminal = at(&desk, 3, 2);
    let said = drag(&mut desk, 3, terminal, Modifiers::default());
    assert_eq!(said, "type a.txt's path here");
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert!(answer_all(&mut desk).contains(&Request::SendInput {
        terminal: TerminalId(3),
        bytes: b"/p/a.txt ".to_vec(),
    }));
}

/// An explorer of `/p` and one of `/q`, which holds the folder `d`, side
/// by side, the first focused.
fn two_explorers() -> Desk {
    use clackatoo_protocol::{FilesId, FilesUpdate};
    let layout = side_by_side(vec![files_pane(2, 1), files_pane(5, 2)]);
    let mut desk = desk(one_tab(layout));
    explore_p(&mut desk);
    let root = FilesUpdate::Root {
        path: "/q".into(),
        name: "/q".into(),
    };
    desk.files_changed(FilesId(2), root);
    let listed = FilesUpdate::Listed {
        dir: "/q".into(),
        entries: vec![entry("d", true)],
        cut_short: false,
    };
    desk.files_changed(FilesId(2), listed);
    answer_all(&mut desk);
    desk.show_pane(PaneId(2));
    desk
}

#[test]
fn a_drop_on_another_explorer_focuses_it_so_its_question_is_answered_there() {
    let mut desk = two_explorers();
    // `..` is row 1 of `/q`, `d` row 2.
    let d = at(&desk, 5, 2);
    let said = drag(&mut desk, 3, d, Modifiers::default());
    assert_eq!(said, "move a.txt into /q/d/  (Ctrl copies, Esc stops)");
    assert_eq!(shown_pane(&desk), PaneId(5));
    let ops = file_ops(&mut desk);
    let [(id, FileOp::Move { to, .. })] = &ops[..] else {
        panic!("one move: {ops:?}");
    };
    assert_eq!(to, "/q/d/a.txt");
    let stamp = clackatoo_protocol::FileStamp {
        device: 1,
        inode: 2,
        len: 3,
        seconds: 4,
        nanoseconds: 5,
    };
    let exists = clackatoo_protocol::FileOpError::Exists {
        path: "/q/d/a.txt".into(),
        what: "a file of 3 bytes".into(),
        stamp,
    };
    desk.file_op_done(*id, Err(exists));
    let Status::Typing { text, .. } = desk.status() else {
        panic!("no question asked");
    };
    assert!(text.contains("write over it"), "{text}");
    type_keys(&mut desk, "y");
    assert!(
        matches!(
            &file_ops(&mut desk)[..],
            [(_, FileOp::Move { overwrite: Some(over), .. })] if *over == stamp
        ),
        "the answer went to the explorer that asked"
    );
}

#[test]
fn a_lesson_explorer_takes_in_nothing_from_outside_its_folder() {
    let mut desk = two_explorers();
    desk.fence_explorer(PaneId(5), "/q", "lesson");
    let d = at(&desk, 5, 1);
    let said = drag(&mut desk, 3, d, Modifiers::default());
    assert_eq!(said, crate::explorer::FENCED);
    assert_eq!(file_ops(&mut desk), []);
}

#[test]
fn a_path_with_a_control_character_is_never_typed() {
    use clackatoo_protocol::{FilesId, FilesUpdate};
    let mut desk = three_panes();
    let listed = FilesUpdate::Listed {
        dir: "/p".into(),
        entries: vec![entry("src", true), entry("a\nrm -rf ~\n", false)],
        cut_short: false,
    };
    desk.files_changed(FilesId(1), listed);
    answer_all(&mut desk);
    let terminal = at(&desk, 3, 2);
    let said = drag(&mut desk, 3, terminal, Modifiers::default());
    assert_eq!(
        said,
        "a\\nrm -rf ~\\n's name has a control character in it, so it isn't typed"
    );
    let typed = (answer_all(&mut desk).into_iter())
        .any(|request| matches!(request, Request::SendInput { .. }));
    assert!(!typed);
}

#[test]
fn a_path_dropped_on_a_terminal_goes_to_its_program_whatever_is_being_typed() {
    let mut desk = three_panes();
    type_keys(&mut desk, ":");
    let (from, terminal) = (at(&desk, 2, 3), at(&desk, 3, 2));
    let none = Modifiers::default();
    desk.mouse(from, MouseAction::Press(LEFT), 1, none);
    desk.mouse(terminal, MouseAction::Motion(Some(LEFT)), 1, none);
    desk.mouse(terminal, MouseAction::Release(LEFT), 1, none);
    assert!(answer_all(&mut desk).contains(&Request::SendInput {
        terminal: TerminalId(3),
        bytes: b"/p/a.txt ".to_vec(),
    }));
    if let Status::Typing { text, .. } = desk.status() {
        assert!(!text.contains("a.txt"), "{text}");
    }
}

#[test]
fn a_drag_whose_release_was_lost_ends_as_the_mouse_moves_with_no_button() {
    let mut desk = three_panes();
    let (from, src) = (at(&desk, 2, 3), at(&desk, 2, 2));
    desk.mouse(from, MouseAction::Press(LEFT), 1, Modifiers::default());
    desk.mouse(
        src,
        MouseAction::Motion(Some(LEFT)),
        1,
        Modifiers::default(),
    );
    assert!(note(&desk).starts_with("move a.txt"));
    desk.mouse(src, MouseAction::Motion(None), 1, Modifiers::default());
    assert!(!note(&desk).starts_with("move a.txt"), "{}", note(&desk));
    desk.mouse(src, MouseAction::Release(LEFT), 1, Modifiers::default());
    assert_eq!(file_ops(&mut desk), []);
}
