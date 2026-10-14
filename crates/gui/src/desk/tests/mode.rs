//! The window's one mode, which every pane it arrives at takes, driven
//! through the desk as a window would drive it.

use clackatoo_protocol::{FilesId, FilesUpdate};

use super::*;

const CTRL_SHIFT: Modifiers = Modifiers {
    shift: true,
    ctrl: true,
    alt: false,
};

/// Three terminals side by side, each with a screen, focused on the
/// first, in insert mode.
fn three_side_by_side() -> Desk {
    let mut desk = desk(one_tab(side_by_side(vec![pane(1), pane(2), pane(3)])));
    for terminal in 1..=3 {
        snapshot(&mut desk, terminal);
    }
    desk
}

fn key(desk: &mut Desk, text: &str) {
    type_keys(desk, text);
}

/// What the bottom bar says the mode is.
fn bar_mode(desk: &Desk) -> Option<ModeName> {
    match desk.status() {
        Status::Tabs { mode, .. } => mode,
        Status::Typing { .. } => None,
    }
}

/// The focused editor's mode.
fn editor_mode(desk: &Desk) -> Mode {
    desk.focused_editor().expect("an editor").0.mode
}

#[test]
fn ctrl_hjkl_and_ctrl_b_moves_arrive_in_the_windows_mode() {
    let mut desk = three_side_by_side();
    // Pane 1 is left in normal mode, and pane 2 put in insert mode.
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Normal);
    key(&mut desk, "i");
    assert_eq!(desk.mode(), WindowMode::Insert);
    // In insert mode, back to pane 1, left in normal mode: it's in insert.
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Insert);
    assert_eq!(bar_mode(&desk), Some(ModeName::Insert));
    // In normal mode, to pane 2, left in insert mode: it's in normal.
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Normal);
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 3), Mode::Normal, "Ctrl+B moves alike");
    ctrl_b(&mut desk, "p1");
    assert_eq!(mode(&desk, 1), Mode::Normal);
    assert_eq!(bar_mode(&desk), Some(ModeName::Normal));
    // Nothing went to a program.
    assert!(
        !answer_all(&mut desk)
            .iter()
            .any(|request| matches!(request, Request::SendInput { .. }))
    );
}

#[test]
fn a_click_on_another_pane_arrives_in_the_windows_mode() {
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 1);
    snapshot(&mut desk, 2);
    // Pane 2's tile starts 410 pixels in.
    let in_two = [500.0, 100.0];
    let in_one = [100.0, 100.0];
    normal_mode(&mut desk);
    click(&mut desk, in_two, Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Normal);
    key(&mut desk, "i");
    click(&mut desk, in_one, Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Insert, "left in normal mode");
}

#[test]
fn switching_tabs_and_workspaces_arrives_in_the_windows_mode() {
    let mut desk = desk(workspaces(vec![
        workspace(1, "a", vec![tab(1, pane(1)), tab(2, pane(2))]),
        workspace(2, "b", vec![tab(3, pane(3))]),
    ]));
    snapshot(&mut desk, 1);
    // Insert mode: to tab 2, and to workspace b, as its name is clicked.
    ctrl_b(&mut desk, "2");
    assert_eq!(mode(&desk, 2), Mode::Insert);
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "1");
    assert_eq!(mode(&desk, 1), Mode::Normal);
    key(&mut desk, "i");
    desk.show_workspace(WorkspaceId(2));
    answer_all(&mut desk);
    snapshot(&mut desk, 3);
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert_eq!(mode(&desk, 3), Mode::Insert);
    // Normal mode: back to a, at tab 1, then tab 2, left in normal mode
    // and insert mode.
    normal_mode(&mut desk);
    command(&mut desk, "ws a");
    answer_all(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Normal);
    command(&mut desk, "2");
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // A workspace's panes keep their places while it isn't shown, but
    // not the mode: b's pane, left in normal mode, comes back in insert.
    key(&mut desk, "i");
    ctrl_b(&mut desk, "w");
    type_keys(&mut desk, "b");
    enter(&mut desk);
    answer_all(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert_eq!(mode(&desk, 3), Mode::Insert);
}

#[test]
fn jumps_arrive_in_the_windows_mode() {
    let mut desk = three_tabs();
    for terminal in 1..=5 {
        snapshot(&mut desk, terminal);
    }
    // Pane 1 left in normal mode, pane 2 in insert mode.
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "p2");
    key(&mut desk, "i");
    // Ctrl+Shift+O in insert mode: back to pane 1, in insert mode.
    press(&mut desk, Key::Character("O".into()), None, CTRL_SHIFT);
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Insert);
    // Ctrl+I in normal mode: forward to pane 2, in normal mode.
    normal_mode(&mut desk);
    ctrl(&mut desk, "i");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // And Ctrl+O back to pane 1, left in normal mode.
    ctrl(&mut desk, "o");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Normal);
    // Ctrl+Shift+I in insert mode, to pane 2, left in normal mode.
    key(&mut desk, "i");
    press(&mut desk, Key::Character("I".into()), None, CTRL_SHIFT);
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Insert);
}

#[test]
fn a_pane_opened_takes_the_windows_mode() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    // In insert mode, Ctrl+B | opens a terminal in insert mode.
    answer_all(&mut desk);
    ctrl_b(&mut desk, "|");
    assert!(matches!(
        desk.take_outbox()[..],
        [ClientMessage::Request(Request::Split { .. })]
    ));
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), pane(2)])));
    desk.answered(Response::PaneCreated(PaneId(2)));
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Insert);
    // In normal mode, `:|` opens one in normal mode.
    normal_mode(&mut desk);
    answer_all(&mut desk);
    command(&mut desk, "|");
    assert!(matches!(
        desk.take_outbox()[..],
        [ClientMessage::Request(Request::Split { .. })]
    ));
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(1), pane(2), pane(3)])));
    desk.answered(Response::PaneCreated(PaneId(3)));
    assert_eq!(mode(&desk, 3), Mode::Normal);
}

#[test]
fn an_editor_arrives_in_insert_mode_at_its_cursor_and_in_normal_mode_as_it_was() {
    let layout = side_by_side(vec![pane(1), file_pane(2, 7)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 1);
    load(&mut desk, 7, "/t.txt", "abc\nxyz\n");
    // To the editor in normal mode, and its cursor to `y`.
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    key(&mut desk, "jl");
    // Back in insert mode, from the terminal: it types at the cursor.
    ctrl(&mut desk, "h");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "l");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    assert_eq!(bar_mode(&desk), Some(ModeName::Insert));
    key(&mut desk, "Q");
    assert_eq!(editor_text(&desk), "abc\nxQyz\n");
    // And in normal mode again, it leaves insert mode as Esc does.
    ctrl_b(&mut desk, "h");
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert!(note(&desk).ends_with("2:2"), "back onto the Q");
}

/// The daemon does what the window asked, which shows a file in place of
/// what a pane shows: it tells of `workspaces`, then answers.
fn shown_here(desk: &mut Desk, workspaces: Workspaces) {
    let asked = desk.take_outbox();
    assert!(
        asked
            .iter()
            .any(|message| matches!(message, ClientMessage::Request(Request::OpenHere { .. }))),
        "{asked:?}"
    );
    desk.workspaces_changed(workspaces);
    for message in asked {
        if let ClientMessage::Request(_) = message {
            desk.answered(Response::Done);
        }
    }
    answer_all(desk);
}

#[test]
fn a_file_the_finder_opens_or_a_link_opens_takes_the_windows_mode() {
    use clackatoo_protocol::Search;
    // The finder, from an editor in normal mode, opens the file in its
    // place, in normal mode.
    let mut desk = editing("/p/src/main.rs", "fn main() {}\n");
    type_keys(&mut desk, " pf");
    let asked = finds(&mut desk);
    let (id, _) = search_in(&asked);
    found_files(&mut desk, id, &["src/other.rs"]);
    finder_rows(&mut desk);
    enter(&mut desk);
    shown_here(&mut desk, one_tab(file_pane(1, 8)));
    load(&mut desk, 8, "/p/src/other.rs", "fn other() {}\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    // From an editor in insert mode, opened as a binding in insert mode
    // can, in insert mode.
    key(&mut desk, "i");
    desk.act(bindings::Action::Find(crate::finder::Kind::Files));
    let asked = finds(&mut desk);
    assert!(asked.iter().any(|message| matches!(
        message,
        ClientMessage::Find {
            search: Search::Files,
            ..
        }
    )));
    let (id, _) = search_in(&asked);
    found_files(&mut desk, id, &["src/main.rs"]);
    finder_rows(&mut desk);
    enter(&mut desk);
    shown_here(&mut desk, one_tab(file_pane(1, 9)));
    load(&mut desk, 9, "/p/src/main.rs", "fn main() {}\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);

    // A file link Ctrl+clicked in a terminal in insert mode opens in the
    // tab's editor, in insert mode.
    let mut desk = super::desk(one_tab(side_by_side(vec![pane(1), file_pane(2, 7)])));
    load(&mut desk, 7, "/p/src/a.rs", "a\nb\nc\n");
    screen_showing(&mut desk, 1, 40, "\x1b]7;file:///p\x07--> src/a.rs:3:1\r\n");
    answer_all(&mut desk);
    click(&mut desk, cell_point(0, 8), CTRL);
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(editor_mode(&desk), Mode::Insert);
}

#[test]
fn the_configs_lua_moving_the_focus_arrives_in_the_windows_mode() {
    let mut desk = three_side_by_side();
    desk.lua_from(
        r#"
        client.keys.map("ni", "<C-b>m", function(ev) ev.pane.tab.panes[3]:focus() end)
        "#,
    );
    answer_all(&mut desk);
    // Pane 3 left in normal mode.
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "p3");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "m");
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert_eq!(mode(&desk, 3), Mode::Insert);
}

#[test]
fn the_start_page_closing_leaves_the_mode_as_it_was() {
    for insert in [true, false] {
        let mut desk = desk(one_tab(pane(1)));
        snapshot(&mut desk, 1);
        if !insert {
            normal_mode(&mut desk);
        }
        ctrl_b(&mut desk, "a");
        assert!(desk.start.is_some(), "the start page opened");
        escape(&mut desk);
        assert!(desk.start.is_none());
        let expected = if insert { Mode::Insert } else { Mode::Normal };
        assert_eq!(mode(&desk, 1), expected);
    }
}

#[test]
fn a_pane_closing_moves_the_focus_to_one_in_the_windows_mode() {
    let mut desk = three_side_by_side();
    // Pane 1 left in insert mode; pane 2 is closed from normal mode.
    ctrl_b(&mut desk, "l");
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "x");
    answer_all(&mut desk);
    // The daemon focuses the pane before it.
    let mut layout = tab(1, side_by_side(vec![pane(1), pane(3)]));
    layout.focused = PaneId(1);
    desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![layout])]));
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Normal);
    // And from insert mode, to pane 1, left in normal mode.
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(3));
    key(&mut desk, "i");
    ctrl_b(&mut desk, "x");
    answer_all(&mut desk);
    desk.workspaces_changed(one_tab(pane(1)));
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Insert);
}

#[test]
fn panes_with_no_insert_mode_show_normal_mode_and_leave_the_windows_alone() {
    let layout = side_by_side(vec![pane(2), files_pane(1, 1), board_pane(4, 1), pane(3)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    snapshot(&mut desk, 3);
    // Pane 3 left in normal mode, then pane 2 put in insert mode.
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "p4");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    // Through the explorer and the board, which show normal mode...
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(bar_mode(&desk), Some(ModeName::Normal));
    assert_eq!(desk.text_goes_to(), None, "keys are the explorer's");
    assert_eq!(desk.mode(), WindowMode::Insert);
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(4));
    assert_eq!(bar_mode(&desk), Some(ModeName::Normal));
    // ...to pane 3, in insert mode still.
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 3), Mode::Insert);
    // Ctrl+; in one of them puts the window in normal mode.
    ctrl_b(&mut desk, "h");
    normal_mode(&mut desk);
    assert_eq!(desk.mode(), WindowMode::Normal);
    ctrl_b(&mut desk, "h");
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Normal, "left in insert mode");
}

#[test]
fn a_window_opening_on_a_pane_with_no_insert_mode_starts_in_normal_mode() {
    let layout = side_by_side(vec![files_pane(1, 1), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    assert_eq!(desk.mode(), WindowMode::Normal);
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

/// A pane showing a folder, made from explorer `files`.
fn folder_pane(pane: u32, files: u32) -> Layout {
    Layout::Pane(LayoutPane {
        id: PaneId(pane),
        content: Content::Folder(FilesId(files)),
    })
}

/// The daemon roots explorer `files` at `/p`, and lists it.
fn lists_p(desk: &mut Desk, files: u32) {
    desk.files_changed(
        FilesId(files),
        FilesUpdate::Root {
            path: "/p".into(),
            name: "~/p".into(),
        },
    );
    desk.files_changed(
        FilesId(files),
        FilesUpdate::Listed {
            dir: "/p".into(),
            entries: vec![entry("src", true), entry("a.txt", false)],
            cut_short: false,
        },
    );
}

#[test]
fn an_editor_showing_a_folder_follows_the_mode_once_its_listing_comes() {
    for insert in [true, false] {
        let layout = side_by_side(vec![pane(1), folder_pane(2, 1)]);
        let mut desk = desk(one_tab(layout));
        snapshot(&mut desk, 1);
        if !insert {
            normal_mode(&mut desk);
        }
        ctrl_b(&mut desk, "l");
        answer_all(&mut desk);
        // No editor until the listing comes: the window keeps its mode.
        assert!(desk.focused_editor().is_none());
        lists_p(&mut desk, 1);
        assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
        let expected = if insert { Mode::Insert } else { Mode::Normal };
        assert_eq!(editor_mode(&desk), expected);
    }
}

#[test]
fn going_into_a_folder_keeps_the_windows_mode() {
    let layout = side_by_side(vec![pane(1), folder_pane(2, 1)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 1);
    ctrl_b(&mut desk, "l");
    answer_all(&mut desk);
    lists_p(&mut desk, 1);
    assert_eq!(editor_mode(&desk), Mode::Insert);
    // Esc, then Enter on `src/`, below `..`: its listing comes in normal
    // mode.
    escape(&mut desk);
    key(&mut desk, "ggj");
    enter(&mut desk);
    answer_all(&mut desk);
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Listed {
            dir: "/p/src".into(),
            entries: vec![entry("main.rs", false)],
            cut_short: false,
        },
    );
    assert_eq!(listing_shown(&desk), ["..", "main.rs"]);
    assert_eq!(editor_mode(&desk), Mode::Normal);
    // And the terminal, gone to from there, is in normal mode.
    ctrl_b(&mut desk, "h");
    assert_eq!(mode(&desk, 1), Mode::Normal);
}

#[test]
fn leaving_a_terminal_ends_a_selection_a_half_typed_command_and_a_search() {
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 1);
    snapshot(&mut desk, 2);
    normal_mode(&mut desk);
    // A visual selection, left: the window is in normal mode.
    key(&mut desk, "v");
    assert_eq!(mode(&desk, 1), Mode::Visual(Selection::Characters));
    ctrl(&mut desk, "l");
    assert_eq!(mode(&desk, 1), Mode::Normal, "the selection's gone");
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // `y` waiting for its motion, left with a click: a key would be its
    // motion.
    key(&mut desk, "y");
    assert!(desk.pane(TerminalId(2)).unwrap().pending());
    click(&mut desk, [100.0, 100.0], Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert!(!desk.pane(TerminalId(2)).unwrap().pending());
    // A search being typed, which takes every key.
    key(&mut desk, "/ab");
    assert!(matches!(desk.status(), Status::Typing { .. }));
    click(&mut desk, [500.0, 100.0], Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert!(!desk.pane(TerminalId(1)).unwrap().prompting());
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // A command line being typed is the pane's it was begun in.
    key(&mut desk, ":clo");
    assert!(desk.input.is_some());
    click(&mut desk, [100.0, 100.0], Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert!(desk.input.is_none());
    assert!(!matches!(desk.status(), Status::Typing { .. }));
}

#[test]
fn leaving_an_editor_ends_a_selection_and_a_half_typed_command_but_keeps_its_cursor() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    load(&mut desk, 7, "/t.txt", "abc\nxyz\n");
    key(&mut desk, "jlv");
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Characters));
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Normal, "visual is normal mode");
    ctrl(&mut desk, "h");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(note(&desk), "t.txt  2:2", "the cursor where it was");
    // `d` waiting for its motion, left with a click: a key would be its
    // motion.
    key(&mut desk, "d");
    click(&mut desk, [500.0, 100.0], Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(2));
    ctrl(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert!(!desk.focused_editor().unwrap().0.pending());
    // A search being typed, which takes every key.
    key(&mut desk, "/x");
    click(&mut desk, [500.0, 100.0], Modifiers::default());
    assert_eq!(shown_pane(&desk), PaneId(2));
    ctrl(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert!(!desk.focused_editor().unwrap().0.prompting());
    key(&mut desk, "x");
    assert_eq!(editor_text(&desk), "abc\nxz\n", "nothing was half typed");
}

#[test]
fn leaving_a_chat_ends_its_selection_and_arrives_at_its_box_in_insert_mode() {
    use clackatoo_protocol::{ChatNode, ChatState, Item, Node, Outcome, Turn, TurnEnd};
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 2);
    let node = ChatNode {
        state: ChatState::Ready,
        turns: vec![Turn {
            from: None,
            prompt: "how?".into(),
            sent: 0,
            items: vec![Item::Words("Like this.".into())],
            end: Some(TurnEnd {
                at: 1,
                outcome: Outcome::Done,
            }),
            files: None,
            switch: None,
        }],
        ..ChatNode::default()
    };
    desk.view_changed(
        TerminalId(1),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: Node::Chat(Box::new(node)),
        }),
    );
    let mut fonts = crate::chat::fixtures::fonts();
    let paint = |desk: &mut Desk, fonts: &mut crate::fonts::Fonts| {
        let pane = desk.pane_mut(TerminalId(1)).expect("the chat's pane");
        pane.paint_chat(fonts, [0.0; 2], [900.0, 1000.0], true);
    };
    paint(&mut desk, &mut fonts);
    normal_mode(&mut desk);
    key(&mut desk, "kv");
    paint(&mut desk, &mut fonts);
    assert_eq!(mode(&desk, 1), Mode::Visual(Selection::Characters));
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 1), Mode::Normal);
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // Back in insert mode: the box is typed in.
    key(&mut desk, "i");
    ctrl_b(&mut desk, "h");
    assert_eq!(mode(&desk, 1), Mode::Insert);
    assert_eq!(desk.text_goes_to(), Some(TextTarget::Pane));
    // And left in insert mode, it's read in normal mode.
    ctrl_b(&mut desk, "l");
    normal_mode(&mut desk);
    ctrl(&mut desk, "h");
    assert_eq!(mode(&desk, 1), Mode::Normal);
    assert_eq!(desk.text_goes_to(), None);
}

#[test]
fn a_macro_typed_across_panes_goes_on_in_the_windows_mode() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    load(&mut desk, 7, "/t.txt", "abc\n");
    // Recorded in the editor: into insert mode, to the terminal, which
    // is in insert mode too, a key for it, and back.
    key(&mut desk, "qai");
    ctrl_b(&mut desk, "l");
    key(&mut desk, "x");
    ctrl_b(&mut desk, "h");
    normal_mode(&mut desk);
    key(&mut desk, "q");
    let typed = |desk: &mut Desk| -> Vec<(TerminalId, Vec<u8>)> {
        (answer_all(desk).into_iter())
            .filter_map(|request| match request {
                Request::SendInput { terminal, bytes } => Some((terminal, bytes)),
                _ => None,
            })
            .collect()
    };
    assert_eq!(typed(&mut desk), [(TerminalId(2), b"x".to_vec())]);
    // The terminal left in normal mode.
    ctrl(&mut desk, "l");
    ctrl(&mut desk, "h");
    assert_eq!(mode(&desk, 2), Mode::Normal);
    // Typed again, the macro's keys meet the terminal in insert mode, as
    // they did when it was recorded.
    key(&mut desk, "@a");
    assert_eq!(typed(&mut desk), [(TerminalId(2), b"x".to_vec())]);
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(editor_text(&desk), "abc\n");
}

#[test]
fn another_window_showing_the_same_pane_keeps_its_own_mode() {
    let mut one = three_side_by_side();
    let mut two = three_side_by_side();
    normal_mode(&mut one);
    ctrl(&mut one, "l");
    // The other window's pane 2, shown there in insert mode, isn't moved.
    ctrl_b(&mut two, "l");
    assert_eq!(mode(&one, 2), Mode::Normal);
    assert_eq!(mode(&two, 2), Mode::Insert);
    // Nothing the daemon hears says what mode a window is in.
    let heard = one.take_outbox();
    assert!(
        heard.iter().all(|message| matches!(
            message,
            ClientMessage::Request(Request::Focus { .. } | Request::Resize { .. })
                | ClientMessage::FrameDone { .. }
        )),
        "{heard:?}"
    );
}

#[test]
fn reconnecting_keeps_the_windows_mode() {
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 1);
    snapshot(&mut desk, 2);
    normal_mode(&mut desk);
    desk.lost();
    desk.connected(two_side_by_side(), false);
    answer_all(&mut desk);
    assert_eq!(desk.mode(), WindowMode::Normal);
    assert_eq!(mode(&desk, 1), Mode::Normal);
    ctrl(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

/// What `requests` typed into terminals.
fn typed_into(requests: &[Request]) -> Vec<TerminalId> {
    (requests.iter())
        .filter_map(|request| match request {
            Request::SendInput { terminal, .. } => Some(*terminal),
            _ => None,
        })
        .collect()
}

#[test]
fn the_focused_pane_closed_by_another_window_arrives_in_normal_mode() {
    let layout = side_by_side(vec![pane(2), files_pane(1, 1), pane(3)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    snapshot(&mut desk, 3);
    // Terminal 3 left in normal mode, then terminal 2 in insert mode,
    // and on to the explorer.
    normal_mode(&mut desk);
    ctrl_b(&mut desk, "p3");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(desk.mode(), WindowMode::Insert);
    answer_all(&mut desk);
    // Another window closes the explorer; the daemon focuses terminal 3.
    let mut closed = tab(1, side_by_side(vec![pane(2), pane(3)]));
    closed.focused = PaneId(3);
    desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![closed])]));
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert_eq!(mode(&desk, 3), Mode::Normal);
    assert_eq!(desk.mode(), WindowMode::Normal);
    key(&mut desk, "j");
    assert_eq!(
        typed_into(&answer_all(&mut desk)),
        [],
        "the shell heard nothing"
    );
    // Nor does the next pane gone to type.
    ctrl(&mut desk, "h");
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

#[test]
fn a_chat_closed_by_another_window_or_an_agent_arrives_in_normal_mode() {
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 2);
    desk.view_changed(
        TerminalId(1),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::default()),
        }),
    );
    assert_eq!(mode(&desk, 1), Mode::Insert);
    desk.workspaces_changed(one_tab(pane(2)));
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

#[test]
fn a_program_ending_as_its_typed_into_keeps_the_mode_as_does_closing_it_here() {
    let mut desk = three_side_by_side();
    // Terminals 2 and 3 left in normal mode; terminal 1 typed into.
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    ctrl(&mut desk, "l");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    // `exit` and Enter: the daemon says the program ended, and closes its
    // pane.
    key(&mut desk, "exit");
    enter(&mut desk);
    answer_all(&mut desk);
    desk.terminal_exited(TerminalId(1), std::time::Instant::now());
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Insert);
    // Closed from here, with Ctrl+B x, it carries the mode too.
    ctrl_b(&mut desk, "x");
    answer_all(&mut desk);
    desk.workspaces_changed(one_tab(pane(3)));
    assert_eq!(mode(&desk, 3), Mode::Insert);
}

/// Three terminals, 2 and 3 left in normal mode, and terminal 1 focused
/// in insert mode.
fn typing_in_terminal_one() -> Desk {
    let mut desk = three_side_by_side();
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    ctrl(&mut desk, "l");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    answer_all(&mut desk);
    desk
}

#[test]
fn ctrl_d_ending_the_program_keeps_the_mode() {
    let mut desk = typing_in_terminal_one();
    ctrl(&mut desk, "d");
    assert_eq!(typed_into(&answer_all(&mut desk)), [TerminalId(1)]);
    desk.terminal_exited(TerminalId(1), std::time::Instant::now());
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(mode(&desk, 2), Mode::Insert);
}

#[test]
fn a_program_killed_by_an_agent_lands_the_next_pane_in_normal_mode() {
    // Nothing was typed into it: its ending is no one's here.
    let mut desk = typing_in_terminal_one();
    desk.terminal_exited(TerminalId(1), std::time::Instant::now());
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(mode(&desk, 2), Mode::Normal);
    key(&mut desk, "ls");
    assert_eq!(typed_into(&answer_all(&mut desk)), []);
}

#[test]
fn a_program_crashing_long_after_the_last_key_lands_the_next_pane_in_normal_mode() {
    let mut desk = typing_in_terminal_one();
    key(&mut desk, "make");
    enter(&mut desk);
    answer_all(&mut desk);
    // It crashes four seconds later.
    let later = std::time::Instant::now() + std::time::Duration::from_secs(4);
    desk.terminal_exited(TerminalId(1), later);
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

#[test]
fn a_program_ending_on_a_daemon_too_old_to_say_so_lands_the_next_pane_in_normal_mode() {
    // A daemon before protocol 74 never sends Exited (see
    // `wire::EXITED`): the pane just closes.
    let mut desk = typing_in_terminal_one();
    key(&mut desk, "exit");
    enter(&mut desk);
    answer_all(&mut desk);
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

#[test]
fn a_terminal_being_typed_into_closed_by_another_window_arrives_in_normal_mode() {
    let mut desk = three_side_by_side();
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    answer_all(&mut desk);
    // No word that its program ended: someone else closed it.
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(mode(&desk, 2), Mode::Normal);
    key(&mut desk, "ls");
    assert_eq!(typed_into(&answer_all(&mut desk)), []);
}

#[test]
fn a_close_refused_leaves_the_pane_to_arrive_from_in_normal_mode_if_closed_elsewhere() {
    let mut desk = three_side_by_side();
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    ctrl_b(&mut desk, "p1");
    key(&mut desk, "i");
    answer_all(&mut desk);
    ctrl_b(&mut desk, "x");
    for message in desk.take_outbox() {
        if let ClientMessage::Request(request) = message {
            let refused = matches!(request, Request::ClosePane { .. });
            desk.answered(if refused {
                Response::Error(clackatoo_protocol::Error::NoSuchPane(PaneId(1)))
            } else {
                Response::Done
            });
        }
    }
    // Another window closes it later.
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(mode(&desk, 2), Mode::Normal);
}

#[test]
fn a_command_line_begun_in_a_pane_closed_since_is_dropped() {
    let mut desk = three_side_by_side();
    normal_mode(&mut desk);
    key(&mut desk, ":q");
    assert!(desk.input.is_some());
    desk.workspaces_changed(one_tab(side_by_side(vec![pane(2), pane(3)])));
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert!(desk.input.is_none(), "`:q` can't close pane 2");
    enter(&mut desk);
    assert!(
        !(answer_all(&mut desk).iter()).any(|request| matches!(request, Request::ClosePane { .. }))
    );
}

#[test]
fn opening_or_closing_from_an_explorer_by_command_or_key_leaves_normal_mode() {
    let layout = side_by_side(vec![pane(2), files_pane(1, 1), pane(3)]);
    let new_desk = || {
        let mut desk = super::desk(one_tab(layout.clone()));
        snapshot(&mut desk, 2);
        snapshot(&mut desk, 3);
        // Terminal 3 left in insert mode, and on to the explorer.
        ctrl_b(&mut desk, "p3");
        ctrl_b(&mut desk, "p2");
        assert_eq!(desk.mode(), WindowMode::Insert);
        answer_all(&mut desk);
        desk
    };
    // `:e`, typed in it.
    let mut desk = new_desk();
    command(&mut desk, "e /p/new.txt");
    assert!(
        (answer_all(&mut desk).iter()).any(|request| matches!(request, Request::NewTab { .. }))
    );
    assert_eq!(desk.mode(), WindowMode::Normal);
    // `q`, closing it: the pane gone to is in normal mode.
    let mut desk = new_desk();
    key(&mut desk, "q");
    let asked = answer_all(&mut desk);
    assert!(
        asked.contains(&Request::ClosePane { pane: PaneId(1) }),
        "{asked:?}"
    );
    let mut closed = tab(1, side_by_side(vec![pane(2), pane(3)]));
    closed.focused = PaneId(3);
    desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![closed])]));
    assert_eq!(shown_pane(&desk), PaneId(3));
    assert_eq!(mode(&desk, 3), Mode::Normal);
}

#[test]
fn another_windows_e_in_the_focused_pane_arrives_in_normal_mode() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    load(&mut desk, 7, "/t.txt", "abc\n");
    key(&mut desk, "i");
    answer_all(&mut desk);
    // Another window shows another file in the pane.
    desk.workspaces_changed(one_tab(side_by_side(vec![file_pane(1, 8), pane(2)])));
    load(&mut desk, 8, "/u.txt", "uvw\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(desk.mode(), WindowMode::Normal);
    key(&mut desk, "x");
    assert_eq!(editor_text(&desk), "vw\n", "x deleted, not typed");
}

#[test]
fn the_focus_moved_off_a_pane_still_there_leaves_it_and_arrives_in_normal_mode() {
    let mut desk = desk(two_side_by_side());
    snapshot(&mut desk, 1);
    snapshot(&mut desk, 2);
    ctrl_b(&mut desk, "l");
    normal_mode(&mut desk);
    key(&mut desk, "v");
    assert_eq!(mode(&desk, 2), Mode::Visual(Selection::Characters));
    // Pane 1, left in insert mode, gets the focus as the daemon moves
    // pane 2 to a tab of its own.
    desk.workspaces_changed(workspaces(vec![workspace(
        1,
        "w",
        vec![tab(1, pane(1)), tab(2, pane(2))],
    )]));
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 2), Mode::Normal, "its selection ended");
    assert_eq!(mode(&desk, 1), Mode::Normal);
}

#[test]
fn a_file_opened_from_the_explorer_opens_in_normal_mode() {
    let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7), pane(3)]);
    let mut desk = desk(one_tab(layout));
    load(&mut desk, 7, "/p/b.txt", "b\n");
    snapshot(&mut desk, 3);
    // In insert mode, through the editor, to the explorer.
    ctrl(&mut desk, "l");
    ctrl(&mut desk, "l");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "h");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(desk.mode(), WindowMode::Insert);
    answer_all(&mut desk);
    explore_p(&mut desk);
    // Enter on a.txt opens it in the editor, in normal mode.
    type_keys(&mut desk, "j");
    enter(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(editor_mode(&desk), Mode::Normal);
    let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 8), pane(3)]);
    shown_here(&mut desk, one_tab(layout));
    load(&mut desk, 8, "/p/a.txt", "one\ntwo\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    key(&mut desk, "dd");
    assert_eq!(editor_text(&desk), "two\n", "dd deleted, not typed");
    // Ctrl+V, beside it: what opens is in normal mode, though the
    // explorer was gone to in insert mode.
    ctrl(&mut desk, "l");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "h");
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(desk.mode(), WindowMode::Insert);
    answer_all(&mut desk);
    ctrl(&mut desk, "v");
    assert!(matches!(
        desk.take_outbox()[..],
        [ClientMessage::Request(Request::Split { .. })]
    ));
    let layout = side_by_side(vec![
        files_pane(2, 1),
        file_pane(1, 8),
        file_pane(4, 9),
        pane(3),
    ]);
    desk.workspaces_changed(one_tab(layout));
    desk.answered(Response::PaneCreated(PaneId(4)));
    load(&mut desk, 9, "/p/a.txt", "one\n");
    assert_eq!(shown_pane(&desk), PaneId(4));
    assert_eq!(editor_mode(&desk), Mode::Normal);
}

#[test]
fn enter_in_a_review_goes_to_the_editor_in_normal_mode() {
    use clackatoo_protocol::{Made, ReviewFile, ReviewHunk, ReviewId, ReviewList, ReviewScope};
    let mut desk = editing_agents_change();
    command(&mut desk, "Review turn 1");
    assert!(matches!(
        desk.take_outbox()[..],
        [ClientMessage::Request(Request::Split { .. })]
    ));
    let review = Layout::Pane(LayoutPane {
        id: PaneId(2),
        content: Content::Review(ReviewId(1)),
    });
    desk.workspaces_changed(one_tab(side_by_side(vec![file_pane(1, 7), review])));
    desk.answered(Response::PaneCreated(PaneId(2)));
    answer_all(&mut desk);
    let list = ReviewList {
        chat: Some(TerminalId(3)),
        chat_name: "Claude Code".into(),
        scope: Some(ReviewScope::Turn(Some(0))),
        files: vec![ReviewFile {
            path: "/p/a.rs".into(),
            shown: "a.rs".into(),
            buffer: Some(BufferId(7)),
            hunks: vec![ReviewHunk {
                line: 2,
                before: vec!["a".into()],
                base: "b\n".into(),
                text: "B\n".into(),
                after: vec!["c".into()],
                agents: vec![5],
                by: Some(Made {
                    chat: TerminalId(3),
                    turn: 0,
                }),
                reviewed: false,
            }],
        }],
        comments: Vec::new(),
        note: None,
        chats: Vec::new(),
    };
    desk.review_event(clackatoo_protocol::Event::Review {
        review: ReviewId(1),
        list,
    });
    // The editor in insert mode, and on to the review.
    ctrl(&mut desk, "h");
    key(&mut desk, "i");
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert_eq!(desk.mode(), WindowMode::Insert);
    enter(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(desk.mode(), WindowMode::Normal);
}

/// A terminal, in insert mode, and beside it an editor of buffer 7, whose
/// text hasn't come, gone to.
fn at_an_editor_waiting_for_its_text() -> Desk {
    let layout = side_by_side(vec![pane(1), file_pane(2, 7)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 1);
    ctrl_b(&mut desk, "l");
    answer_all(&mut desk);
    assert_eq!(editor_mode(&desk), Mode::Normal, "no text to type into yet");
    assert_eq!(desk.mode(), WindowMode::Insert);
    desk
}

#[test]
fn an_editor_arrived_at_in_insert_mode_before_its_text_holds_what_insert_mode_types() {
    let mut desk = at_an_editor_waiting_for_its_text();
    key(&mut desk, "ab");
    enter(&mut desk);
    key(&mut desk, "c");
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    assert_eq!(editor_text(&desk), "ab\ncxyz\n");
}

#[test]
fn any_other_key_or_a_click_gives_up_waiting_for_an_editors_text_and_is_had_at_once() {
    // Esc: the editor stays in normal mode, and what was held goes.
    let mut desk = at_an_editor_waiting_for_its_text();
    key(&mut desk, "ab");
    escape(&mut desk);
    assert_eq!(desk.mode(), WindowMode::Normal);
    assert!(note(&desk).contains("dropped"), "{}", note(&desk));
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(editor_text(&desk), "xyz\n");
    // Ctrl+B h moves at once, from the editor left in normal mode, so
    // `ls` is normal mode's, then and once the text has come: the shell
    // hears nothing, not even late.
    let mut desk = at_an_editor_waiting_for_its_text();
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Normal);
    key(&mut desk, "ls");
    enter(&mut desk);
    assert_eq!(typed_into(&answer_all(&mut desk)), []);
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(typed_into(&answer_all(&mut desk)), []);
    // Ctrl+; goes to normal mode at once, where `:` opens the command
    // line. (In insert mode, `:` is text, and waits with the rest.)
    let mut desk = at_an_editor_waiting_for_its_text();
    normal_mode(&mut desk);
    assert_eq!(desk.mode(), WindowMode::Normal);
    key(&mut desk, ":");
    assert!(desk.input.is_some());
    // A click, in the editor itself, is had in normal mode.
    let mut desk = at_an_editor_waiting_for_its_text();
    key(&mut desk, "ab");
    click(&mut desk, [500.0, 100.0], Modifiers::default());
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(editor_text(&desk), "xyz\n");
}

#[test]
fn waiting_for_an_editors_text_is_given_up_on_as_waiting_for_the_grammar_is() {
    let mut desk = at_an_editor_waiting_for_its_text();
    key(&mut desk, "ab");
    let deadline = desk.editor_wakes().expect("woken to give up");
    assert!(deadline <= std::time::Instant::now() + crate::editor::PATIENCE);
    desk.tick_editors(deadline);
    assert!(note(&desk).contains("too long"), "{}", note(&desk));
    assert_eq!(desk.mode(), WindowMode::Normal);
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(editor_mode(&desk), Mode::Normal);
    assert_eq!(editor_text(&desk), "xyz\n");
}

#[test]
fn keys_held_for_an_editors_text_only_ever_reach_it() {
    let mut desk = at_an_editor_waiting_for_its_text();
    // Enter, in insert mode, goes to the terminal.
    desk.lua_from(
        r#"
        client.keys.map("i", "<CR>", function(ev) ev.pane.tab.panes[1]:focus() end)
        "#,
    );
    answer_all(&mut desk);
    key(&mut desk, "a");
    enter(&mut desk);
    key(&mut desk, "ls");
    load(&mut desk, 7, "/t.txt", "xyz\n");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(
        typed_into(&answer_all(&mut desk)),
        [],
        "ls never reached it"
    );
    ctrl_b(&mut desk, "l");
    assert_eq!(editor_text(&desk), "axyz\n");
}

#[test]
fn a_chat_asking_something_arrives_in_normal_mode_and_keeps_the_windows() {
    use clackatoo_protocol::{ChatNode, Choice, ChoiceKind, Node, Question};
    let mut desk = desk(one_tab(side_by_side(vec![pane(2), pane(1)])));
    snapshot(&mut desk, 2);
    let node = ChatNode {
        question: Some(Question {
            id: 1,
            title: "Write notes.txt".into(),
            choices: vec![Choice {
                id: "allow".into(),
                name: "Allow".into(),
                kind: ChoiceKind::AllowOnce,
            }],
            own: false,
        }),
        ..ChatNode::default()
    };
    desk.view_changed(
        TerminalId(1),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: Node::Chat(Box::new(node)),
        }),
    );
    ctrl_b(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(1));
    assert_eq!(mode(&desk, 1), Mode::Normal);
    assert_eq!(desk.text_goes_to(), None, "a key can't answer it");
    assert_eq!(desk.mode(), WindowMode::Insert);
    ctrl_b(&mut desk, "h");
    assert_eq!(mode(&desk, 2), Mode::Insert);
    // Put in insert mode by hand, and left in normal mode, it's the
    // window's mode again.
    ctrl_b(&mut desk, "l");
    key(&mut desk, "i");
    assert_eq!(mode(&desk, 1), Mode::Insert);
    normal_mode(&mut desk);
    assert_eq!(desk.mode(), WindowMode::Normal);
    ctrl_b(&mut desk, "h");
    assert_eq!(mode(&desk, 2), Mode::Normal);
}
