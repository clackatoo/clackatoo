//! Selecting with the mouse in editors, driven through the desk as a
//! window drives it: the drag is the editor's wherever the mouse goes,
//! letting go puts the selection on the primary selection, and moving the
//! mouse with no button held costs nothing.

use std::fmt::Write;

use clackatoo_protocol::{FilesId, FilesUpdate};

use super::*;

const LEFT: MouseButton = MouseButton::Left;

/// The cells left of an editor's text by default: git's signs, and line
/// numbers four wide.
const GUTTER: u16 = 5;

/// The point in the middle of the cell at `line` and `col` of the text
/// of an editor in the first pane, right of its gutter.
fn text_point(line: u16, col: u16) -> [f32; 2] {
    cell_point(line, col + GUTTER)
}

fn press(desk: &mut Desk, point: [f32; 2], clicks: u8) -> MouseOutcome {
    desk.mouse(
        point,
        MouseAction::Press(LEFT),
        clicks,
        Modifiers::default(),
    )
}

fn moved(desk: &mut Desk, point: [f32; 2], held: bool) -> MouseOutcome {
    let action = MouseAction::Motion(held.then_some(LEFT));
    desk.mouse(point, action, 1, Modifiers::default())
}

fn release(desk: &mut Desk, point: [f32; 2]) -> MouseOutcome {
    desk.mouse(point, MouseAction::Release(LEFT), 1, Modifiers::default())
}

/// The focused editor's mode.
fn editor_mode(desk: &Desk) -> Mode {
    desk.focused_editor().expect("an editor").0.mode
}

/// What the bottom bar says the mode is.
fn bar_mode(desk: &Desk) -> Option<ModeName> {
    match desk.status() {
        Status::Tabs { mode, .. } => mode,
        Status::Typing { .. } => None,
    }
}

/// A text of `lines` lines, each saying which it is, from 0.
fn numbered(lines: usize) -> String {
    (0..lines).fold(String::new(), |mut text, line| {
        let _ = writeln!(text, "line {line}");
        text
    })
}

#[test]
fn leaving_a_chat_for_a_bar_takes_away_its_copy_button() {
    use clackatoo_protocol::{ChatNode, ChatState, Item, Node, Outcome, Turn, TurnEnd};
    let mut desk = desk(one_tab(pane(1)));
    let node = ChatNode {
        state: ChatState::Ready,
        turns: vec![Turn {
            woke: None,
            previews: Vec::new(),
            from: None,
            prompt: "how?".into(),
            sent: 0,
            items: vec![Item::Words("Like this.".into())],
            end: Some(TurnEnd {
                at: 1,
                outcome: Outcome::Done,
                raw: None,
                spent: None,
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
    let pane = desk.chat_mut(TerminalId(1)).expect("the chat's pane");
    pane.paint(&mut fonts, [0.0; 2], [800.0, 470.0], true);
    // Over its one turn: its button shows.
    assert!(moved(&mut desk, [100.0, 60.0], false).redraw);
    // Up onto the top bar, over no pane: it goes.
    assert!(moved(&mut desk, [100.0, -10.0], false).redraw);
    assert_eq!(desk.mouse_cell, None);
    // Nothing more to take away.
    assert!(!moved(&mut desk, [100.0, -12.0], false).redraw);
}

#[test]
fn a_drag_selects_in_visual_mode_and_letting_go_puts_it_on_the_primary_selection() {
    let mut desk = editing("/t.txt", "hello world\nsecond line\n");
    normal_mode(&mut desk);
    desk.take_yanked();
    assert!(press(&mut desk, text_point(0, 1), 1).redraw);
    assert_eq!(editor_mode(&desk), Mode::Normal, "a press selects nothing");
    assert!(moved(&mut desk, text_point(0, 6), true).redraw);
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Characters));
    assert_eq!(bar_mode(&desk), Some(ModeName::Visual));
    assert_eq!(desk.mode(), WindowMode::Normal);
    // Within the same cell, nothing to draw.
    let near = [text_point(0, 6)[0] + 2.0, text_point(0, 6)[1] + 3.0];
    assert!(!moved(&mut desk, near, true).redraw);
    // Out of the pane to the left: the line's start, still this drag's.
    assert!(moved(&mut desk, [-40.0, text_point(1, 0)[1]], true).redraw);
    release(&mut desk, text_point(0, 6));
    assert_eq!(
        desk.take_yanked(),
        [("ello w".to_owned(), Copy::Primary)],
        "where it was let go"
    );
    assert_eq!(desk.held_on(Copy::Primary), "ello w");
    // It stays selected, for an operator.
    type_keys(&mut desk, "d");
    assert_eq!(editor_text(&desk), "horld\nsecond line\n");
}

#[test]
fn a_triple_click_drag_selects_lines() {
    let mut desk = editing("/t.txt", "one\ntwo\nthree\n");
    normal_mode(&mut desk);
    desk.take_yanked();
    press(&mut desk, text_point(0, 1), 3);
    assert_eq!(bar_mode(&desk), Some(ModeName::VisualLine));
    moved(&mut desk, text_point(1, 0), true);
    release(&mut desk, text_point(1, 0));
    assert_eq!(
        desk.take_yanked(),
        [("one\ntwo\n".to_owned(), Copy::Primary)]
    );
}

#[test]
fn a_double_click_in_an_empty_file_leaves_the_primary_selection_as_it_was() {
    let mut desk = editing("/t.txt", "");
    normal_mode(&mut desk);
    desk.take_yanked();
    press(&mut desk, text_point(0, 0), 2);
    release(&mut desk, text_point(0, 0));
    assert_eq!(desk.take_yanked(), []);
}

#[test]
fn moving_the_mouse_with_no_button_held_draws_nothing() {
    let mut desk = editing("/t.txt", "hello world\nsecond line\n");
    normal_mode(&mut desk);
    for col in 0..8 {
        assert!(!moved(&mut desk, text_point(0, col), false).redraw);
    }
    press(&mut desk, text_point(0, 0), 1);
    moved(&mut desk, text_point(1, 3), true);
    release(&mut desk, text_point(1, 3));
    // Over it again, and over its selection: still nothing.
    for col in 0..8 {
        assert!(!moved(&mut desk, text_point(1, col), false).redraw);
    }
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Characters));
}

#[test]
fn with_the_unnamed_register_on_the_primary_selection_letting_go_leaves_it() {
    let mut desk = editing("/t.txt", "hello world\n");
    normal_mode(&mut desk);
    command(&mut desk, "set clipboard=unnamed");
    type_keys(&mut desk, "yiw");
    desk.take_yanked();
    press(&mut desk, text_point(0, 6), 2);
    release(&mut desk, text_point(0, 6));
    assert_eq!(desk.take_yanked(), []);
    // So `p` puts what was yanked over the word selected.
    type_keys(&mut desk, "p");
    assert_eq!(editor_text(&desk), "hello hello\n");
}

#[test]
fn past_the_bottom_the_view_scrolls_by_itself_until_let_go_even_off_the_window() {
    let text = numbered(100);
    let mut desk = editing("/t.txt", &text);
    normal_mode(&mut desk);
    let before = desk.editor_wakes();
    press(&mut desk, text_point(2, 1), 1);
    // Far below the window.
    assert!(moved(&mut desk, [50.0, 900.0], true).redraw);
    let wake = desk.editor_wakes().expect("the view to scroll on");
    assert!(before.is_none_or(|before| wake < before));
    let note_before = note(&desk);
    assert!(desk.tick_editors(wake + std::time::Duration::from_millis(50)));
    assert_ne!(note(&desk), note_before, "the cursor went on down");
    // Let go off the window, where the window can't say where.
    assert!(desk.editor_released());
    assert_eq!(desk.editor_wakes(), before);
    assert!(!desk.editor_released(), "once");
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Characters));
}

#[test]
fn losing_the_focus_ends_a_drag() {
    let text = numbered(100);
    let mut desk = editing("/t.txt", &text);
    normal_mode(&mut desk);
    let before = desk.editor_wakes();
    press(&mut desk, text_point(2, 1), 1);
    moved(&mut desk, [50.0, 900.0], true);
    desk.window_focus(false);
    assert_eq!(desk.editor_wakes(), before);
    // The mouse moving on no longer selects.
    desk.window_focus(true);
    assert!(!moved(&mut desk, text_point(1, 0), true).redraw);
    assert!(!desk.editor_released());
}

#[test]
fn a_drag_from_insert_mode_shows_visual_and_goes_back_to_insert_after() {
    let mut desk = editing("/t.txt", "hello world\n");
    normal_mode(&mut desk);
    type_keys(&mut desk, "i");
    assert_eq!(desk.mode(), WindowMode::Insert);
    press(&mut desk, text_point(0, 0), 1);
    assert_eq!(editor_mode(&desk), Mode::Insert, "a click types there");
    moved(&mut desk, text_point(0, 4), true);
    release(&mut desk, text_point(0, 4));
    assert_eq!(bar_mode(&desk), Some(ModeName::InsertVisual));
    assert_eq!(desk.mode(), WindowMode::Normal);
    type_keys(&mut desk, "y");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    assert_eq!(bar_mode(&desk), Some(ModeName::Insert));
    assert_eq!(desk.mode(), WindowMode::Insert);
    type_keys(&mut desk, ">");
    assert_eq!(editor_text(&desk), ">hello world\n");
}

/// A desk with one editor showing the folder `/p`, of `src/` and
/// `a.txt`, in normal mode.
fn showing_a_folder() -> Desk {
    let layout = Layout::Pane(LayoutPane {
        id: PaneId(1),
        content: Content::Folder(FilesId(1)),
        home: None,
        key: None,
    });
    let mut desk = desk(one_tab(layout));
    normal_mode(&mut desk);
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Root {
            path: "/p".into(),
            name: "~/p".into(),
        },
    );
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Listed {
            dir: "/p".into(),
            entries: vec![entry("src", true), entry("a.txt", false)],
            cut_short: false,
        },
    );
    answer_all(&mut desk);
    assert!(desk.focused_editor().is_some());
    desk
}

/// The point on the name `name` in the folder's listing, as drawn.
fn on_entry(desk: &mut Desk, name: &str) -> [f32; 2] {
    desk.settle_editors();
    let (rows, _) = desk.editor(PaneId(1)).expect("the folder's editor").scene();
    let (line, col) = (rows.iter().enumerate())
        .find_map(|(line, row)| Some((line, row.text().find(name)?)))
        .expect("the entry drawn");
    cell_point(
        u16::try_from(line).unwrap(),
        u16::try_from(col).unwrap() + 1,
    )
}

#[test]
fn an_editor_showing_a_folder_drags_as_any_editor_does() {
    let mut desk = showing_a_folder();
    let (first, second) = (on_entry(&mut desk, "src/"), on_entry(&mut desk, "a.txt"));
    press(&mut desk, first, 1);
    moved(&mut desk, second, true);
    release(&mut desk, second);
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Characters));
    press(&mut desk, first, 3);
    moved(&mut desk, second, true);
    assert_eq!(editor_mode(&desk), Mode::Visual(Selection::Lines));
}

#[test]
fn a_double_click_in_a_folders_listing_opens_the_entry() {
    let mut desk = showing_a_folder();
    let file = on_entry(&mut desk, "a.txt");
    press(&mut desk, file, 1);
    release(&mut desk, file);
    press(&mut desk, file, 2);
    release(&mut desk, file);
    assert_eq!(files_opened(answer_all(&mut desk)), ["in 1 /p/a.txt"]);
    assert_eq!(editor_mode(&desk), Mode::Normal, "nothing selected");
}

#[test]
fn a_double_click_on_dot_dot_in_a_folders_listing_goes_up() {
    let mut desk = showing_a_folder();
    let up = on_entry(&mut desk, "..");
    press(&mut desk, up, 1);
    release(&mut desk, up);
    press(&mut desk, up, 2);
    release(&mut desk, up);
    let rooted = (desk.take_outbox().into_iter()).any(|message| {
        matches!(
            message,
            ClientMessage::Files { action: clackatoo_protocol::FilesAction::Root(dir), .. } if dir == "/"
        )
    });
    assert!(rooted, "rooted at the folder above");
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Root {
            path: "/".into(),
            name: "/".into(),
        },
    );
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Listed {
            dir: "/".into(),
            entries: vec![entry("etc", true), entry("p", true)],
            cut_short: false,
        },
    );
    // At `/`, there's no `..`, and the cursor's on the folder come from.
    assert_eq!(listing_shown(&desk), ["etc/", "p/"]);
    assert_eq!(listing_line(&desk), 1);
    assert_eq!(editor_mode(&desk), Mode::Normal, "nothing selected");
}

#[test]
fn a_press_after_a_missed_release_is_had_as_any_press() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    load(&mut desk, 7, "/t.txt", "hello world\nsecond line\n");
    normal_mode(&mut desk);
    press(&mut desk, text_point(0, 0), 1);
    moved(&mut desk, text_point(0, 4), true);
    // No release came: a press on the terminal goes to it.
    press(&mut desk, [500.0, 100.0], 1);
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert!(!desk.editor_released(), "the drag ended");
}

#[test]
fn leaving_an_editor_mid_drag_gives_the_mouse_back() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    snapshot(&mut desk, 2);
    load(&mut desk, 7, "/t.txt", "hello world\nsecond line\n");
    normal_mode(&mut desk);
    press(&mut desk, text_point(0, 0), 1);
    moved(&mut desk, text_point(0, 4), true);
    ctrl(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(2));
    assert!(!desk.editor_released());
}

#[test]
fn a_selection_from_insert_mode_ended_by_a_command_goes_back_to_insert_mode() {
    let mut desk = editing("/t.txt", "hello world\n");
    normal_mode(&mut desk);
    type_keys(&mut desk, "A!");
    press(&mut desk, text_point(0, 1), 2);
    release(&mut desk, text_point(0, 1));
    // As a binding or the config's Lua runs it, with no command line.
    desk.run(crate::command::parse("s/hello/howdy/").unwrap());
    assert_eq!(editor_text(&desk), "howdy world!\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    assert_eq!(bar_mode(&desk), Some(ModeName::Insert));
}

#[test]
fn a_macro_ending_a_selection_from_insert_mode_goes_back_to_it_once_done() {
    let mut desk = editing("/t.txt", "hello world\nsecond\nyjx\n");
    normal_mode(&mut desk);
    // The macro `yjx`, kept in `"a`.
    type_keys(&mut desk, "G\"ay$gg");
    type_keys(&mut desk, "i");
    press(&mut desk, text_point(0, 0), 1);
    moved(&mut desk, text_point(0, 4), true);
    release(&mut desk, text_point(0, 4));
    type_keys(&mut desk, "@a");
    // `y`, then `j` and `x` in normal mode, and insert mode once it's done.
    assert_eq!(editor_text(&desk), "hello world\necond\nyjx\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
}

/// A desk editing "hello world", with `"yiw` of `world` done, the
/// config's visual-mode bindings, and `hello` selected by a drag from
/// insert mode.
fn hello_dragged_from_insert_mode(bindings: &str) -> Desk {
    let mut desk = editing("/t.txt", "hello world\n");
    desk.lua_from(bindings);
    normal_mode(&mut desk);
    type_keys(&mut desk, "wyiw0i");
    press(&mut desk, text_point(0, 0), 1);
    moved(&mut desk, text_point(0, 4), true);
    release(&mut desk, text_point(0, 4));
    assert_eq!(bar_mode(&desk), Some(ModeName::InsertVisual));
    desk
}

#[test]
fn a_binding_typing_keys_on_a_selection_from_insert_mode_goes_back_once_its_done() {
    // Put over it, keeping what was yanked: insert mode only after `P`.
    let mut desk = hello_dragged_from_insert_mode(r#"client.keys.map("v", ",", '"_dP')"#);
    type_keys(&mut desk, ",");
    assert_eq!(editor_text(&desk), "world world\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    // And with what the selection yanks itself.
    let mut desk = hello_dragged_from_insert_mode(r#"client.keys.map("v", ",", "yP")"#);
    type_keys(&mut desk, ",");
    assert_eq!(editor_text(&desk), "hellohello world\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
}

#[test]
fn a_binding_copying_selecting_again_and_deleting_ends_in_insert_mode() {
    let mut desk = hello_dragged_from_insert_mode(r#"client.keys.map("v", ",", "ygvd")"#);
    type_keys(&mut desk, ",");
    assert_eq!(editor_text(&desk), " world\n");
    assert_eq!(editor_mode(&desk), Mode::Insert);
    assert_eq!(bar_mode(&desk), Some(ModeName::Insert));
}
