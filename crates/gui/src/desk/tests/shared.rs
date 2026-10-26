//! What one pane leaves for the others, driven through the desk as a
//! window drives it: the window's last `f`, `t`, `F` or `T`, which `;`
//! and `,` find again in any pane, and what a terminal yanks, which goes
//! into the registers as an editor's yank does.

use crate::editor::Register;
use crate::options::Clipboard;
use crate::pane::{CopyTo, Yanked};

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

#[test]
fn a_find_in_an_editor_is_found_again_in_a_chat() {
    let layout = side_by_side(vec![file_pane(1, 7), pane(2)]);
    let mut desk = desk(one_tab(layout));
    load(&mut desk, 7, "/t.txt", "a b\n");
    let node = crate::chat::fixtures::talk(&[("one", "alpha beta")]);
    desk.view_changed(
        TerminalId(2),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(node)),
        }),
    );
    normal_mode(&mut desk);
    type_keys(&mut desk, "fb");
    assert!(note(&desk).ends_with("1:3"), "{}", note(&desk));
    ctrl(&mut desk, "l");
    let mut fonts = crate::chat::fixtures::fonts();
    let paint = |desk: &mut Desk, fonts: &mut crate::fonts::Fonts| {
        let chat = desk.chat_mut(TerminalId(2)).expect("the chat's pane");
        chat.paint(fonts, [0.0; 2], [900.0, 1000.0], true);
    };
    paint(&mut desk, &mut fonts);
    type_keys(&mut desk, "G0");
    paint(&mut desk, &mut fonts);
    type_keys(&mut desk, ";");
    let chat = desk.chat_mut(TerminalId(2)).expect("the chat's pane");
    assert_eq!(chat.view_mut().cursor_byte(), Some("alpha ".len()));
}

#[test]
fn a_terminals_yank_is_put_by_p_in_an_editor_and_lines_as_lines() {
    let (mut desk, screen) = terminal_beside_editor("one two\r\nthree", "abc\n");
    desk.take_yanked();
    // `yy` on the terminal's last line, where its cursor starts.
    type_keys(&mut desk, "yy");
    serve(&mut desk, &screen);
    assert_eq!(register(&mut desk, '"').as_deref(), Some("three\n"));
    assert_eq!(register(&mut desk, '0').as_deref(), Some("three\n"));
    // `clipboard` says the unnamed register goes along with the
    // clipboard, as for an editor's yank.
    assert_eq!(
        desk.take_yanked(),
        [("three\n".to_owned(), Copy::Clipboard)]
    );
    ctrl(&mut desk, "l");
    type_keys(&mut desk, "p");
    assert_eq!(editor_text(&desk), "abc\nthree\n", "a line, below");
    // A register named first: `"a`, and by characters.
    ctrl(&mut desk, "h");
    type_keys(&mut desk, "k0\"ayw");
    serve(&mut desk, &screen);
    assert_eq!(register(&mut desk, 'a').as_deref(), Some("one "));
    assert_eq!(register(&mut desk, '0').as_deref(), Some("three\n"), "kept");
    ctrl(&mut desk, "l");
    type_keys(&mut desk, "gg0\"ap");
    assert_eq!(editor_text(&desk), "aone bc\nthree\n");
    // `"A` adds to it, and `p` puts what was last kept, wherever.
    ctrl(&mut desk, "h");
    type_keys(&mut desk, "\"Ayy");
    serve(&mut desk, &screen);
    assert_eq!(register(&mut desk, 'a').as_deref(), Some("one \none two\n"));
}

#[test]
fn a_terminals_yank_keeps_off_the_clipboard_when_clipboard_says_so() {
    let (mut desk, screen) = terminal_beside_editor("one", "abc\n");
    command(&mut desk, "set clipboard=none");
    desk.take_yanked();
    type_keys(&mut desk, "yy");
    serve(&mut desk, &screen);
    assert!(desk.take_yanked().is_empty());
    assert_eq!(register(&mut desk, '"').as_deref(), Some("one\n"));
    // `"+` still goes there.
    type_keys(&mut desk, "\"+yy");
    serve(&mut desk, &screen);
    assert_eq!(desk.take_yanked(), [("one\n".to_owned(), Copy::Clipboard)]);
}

#[test]
fn a_mouse_selection_goes_on_the_primary_selection_alone_as_vims_goes_in_star() {
    let mut desk = desk(one_tab(pane(1)));
    desk.shared_mut().registers.set_sync(Clipboard::UnnamedPlus);
    let yank = |text: &str, to| Yanked {
        register: Register::chars(text.into()),
        to,
    };
    desk.terminal_yanked(yank("kept", CopyTo::Register(None)));
    desk.terminal_yanked(yank("selected", CopyTo::Primary));
    assert_eq!(register(&mut desk, '*').as_deref(), Some("selected"));
    assert_eq!(
        register(&mut desk, '"').as_deref(),
        Some("kept"),
        "not here"
    );
    assert_eq!(
        desk.take_yanked(),
        [
            ("kept".to_owned(), Copy::Clipboard),
            ("selected".to_owned(), Copy::Primary)
        ]
    );
}
