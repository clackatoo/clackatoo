//! What one pane leaves for the others, driven through the desk as a
//! window drives it: the window's last `f`, `t`, `F` or `T`, which `;`
//! and `,` find again in any pane.

use super::*;

/// A terminal showing `output` in pane 1, beside an editor of `/t.txt`,
/// holding `text`, in pane 2, focused on the terminal, in normal mode.
fn terminal_beside_editor(output: &str, text: &str) -> (Desk, Terminal) {
    let layout = side_by_side(vec![pane(1), file_pane(2, 7)]);
    let mut desk = desk(one_tab(layout));
    let mut screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
    screen.advance(output.as_bytes());
    desk.view_changed(TerminalId(1), &ViewUpdate::Snapshot(View::capture(&screen)));
    load(&mut desk, 7, "/t.txt", text);
    normal_mode(&mut desk);
    serve(&mut desk, &screen);
    (desk, screen)
}

/// Answers what the window asked, as the daemon would: text from
/// `screen`, and anything else done.
fn serve(desk: &mut Desk, screen: &Terminal) {
    for message in desk.take_outbox() {
        let ClientMessage::Request(request) = message else {
            continue;
        };
        let response = match request {
            Request::Text {
                from,
                to,
                whole_lines,
                ..
            } => Response::Text(screen.text(from, to, whole_lines)),
            Request::BlockText { from, to, .. } => Response::Text(screen.block_text(from, to)),
            _ => Response::Done,
        };
        desk.answered(response);
    }
}

/// The terminal's normal mode cursor, as its line in view and column.
fn terminal_cursor(desk: &Desk) -> (usize, usize) {
    let pane = desk.pane(TerminalId(1)).expect("the terminal");
    let cursor = pane.scene(24).1.expect("a cursor");
    (cursor.line, cursor.col)
}

#[test]
fn a_find_in_an_editor_is_found_again_in_a_terminal_and_back() {
    let (mut desk, _screen) = terminal_beside_editor("axbxcx", "abxcxc\n");
    ctrl(&mut desk, "l");
    type_keys(&mut desk, "fx");
    assert!(note(&desk).ends_with("1:3"), "{}", note(&desk));
    // In the terminal, `;` finds the editor's `x`, and `,` back.
    ctrl(&mut desk, "h");
    type_keys(&mut desk, "0;");
    assert_eq!(terminal_cursor(&desk), (0, 1));
    type_keys(&mut desk, ";;");
    assert_eq!(terminal_cursor(&desk), (0, 5));
    type_keys(&mut desk, ",");
    assert_eq!(terminal_cursor(&desk), (0, 3));
    // And a find in the terminal is the editor's to find again: `tc`, so
    // `;` in the editor goes to just before its next `c`.
    type_keys(&mut desk, "0tc");
    assert_eq!(terminal_cursor(&desk), (0, 3));
    ctrl(&mut desk, "l");
    type_keys(&mut desk, "0;");
    assert!(note(&desk).ends_with("1:3"), "{}", note(&desk));
    type_keys(&mut desk, ";");
    assert!(note(&desk).ends_with("1:5"), "{}", note(&desk));
}
