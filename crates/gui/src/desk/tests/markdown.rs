//! `:markdown` and `<Space>tm`, which show an editor's Markdown rendered,
//! and the links in it, opened as links elsewhere in the window are.

use super::*;

/// Whether the focused editor shows its Markdown rendered.
fn rendered(desk: &Desk) -> bool {
    desk.focused_editor()
        .is_some_and(|(editor, _)| editor.rendered())
}

#[test]
fn markdown_renders_a_markdown_file_and_says_why_not_for_others() {
    let mut desk = editing("/p/main.rs", "fn main() {}\n");
    command(&mut desk, "Markdown");
    assert!(!rendered(&desk));
    assert_eq!(desk.message.as_deref(), Some("‘main.rs’ isn't Markdown"));
    let mut desk = editing("/p/notes.md", "# Notes\n");
    command(&mut desk, "Markdown");
    assert!(
        rendered(&desk),
        "{:?} {:?}",
        desk.message,
        desk.focused_editor()
            .map(|(e, c)| (e.mode, c.loaded(), c.path().to_owned(), e.label(c)))
    );
    // `<Space>tm` turns it back, as `:markdown` does.
    type_keys(&mut desk, " tm");
    assert!(!rendered(&desk));
    type_keys(&mut desk, " tm");
    assert!(rendered(&desk));
}

#[test]
fn the_focus_arriving_in_insert_mode_leaves_it_rendered_in_normal_mode() {
    let mut desk = editing("/p/notes.md", "# Notes\n");
    command(&mut desk, "Markdown");
    desk.mode = WindowMode::Insert;
    desk.arrive();
    assert!(rendered(&desk));
    let (editor, _) = desk.focused_editor().unwrap();
    assert_eq!(editor.mode, Mode::Normal);
    // The window's mode is as it was, for the pane after, as a pane with
    // no insert mode leaves it.
    assert_eq!(desk.mode, WindowMode::Insert);
    // `i` is for typing in the text.
    type_keys(&mut desk, "i");
    assert!(!rendered(&desk));
    assert_eq!(desk.focused_editor().unwrap().0.mode, Mode::Insert);
}

#[test]
fn a_click_on_a_link_in_a_pane_without_the_focus_focuses_it_and_opens_the_link() {
    use clackatoo_protocol::{Layout, Pane as LayoutPane};
    let file = Layout::Pane(LayoutPane {
        id: PaneId(2),
        content: Content::Buffer(BufferId(7)),
        home: None,
        key: None,
    });
    let layout = Layout::Split {
        axis: Axis::LeftToRight,
        shares: vec![1, 1],
        parts: vec![pane(1), file],
    };
    let mut desk = desk(workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]));
    desk.buffer_changed(
        BufferId(7),
        clackatoo_protocol::BufferUpdate::Snapshot(clackatoo_protocol::BufferSnapshot {
            path: "/p/notes.md".into(),
            text: "[the docs](https://x.org)\n".into(),
            version: 0,
            modified: false,
            applied: None,
            landed: None,
            daemon: 0,
        }),
    );
    answer_all(&mut desk);
    desk.show_pane(PaneId(2));
    desk.editor_to_mode(PaneId(2), false);
    command(&mut desk, "Markdown");
    assert!(rendered(&desk));
    // Back in the terminal, typing into it.
    desk.show_pane(PaneId(1));
    desk.mode = WindowMode::Insert;
    desk.arrive();
    let (_, tile) = *(desk.placement().unwrap().panes.iter())
        .find(|(pane, _)| *pane == PaneId(2))
        .unwrap();
    let mut fonts = crate::chat::fixtures::fonts();
    let size = tile.cells_size([10.0, 20.0]);
    assert!(
        desk.paint_rendered(PaneId(2), &mut fonts, tile.origin, size)
            .is_some()
    );
    let link = (0..100_u16)
        .flat_map(|x| (0..12_u16).map(move |y| [f32::from(x) * 4.0, f32::from(y) * 4.0]))
        .map(|at| [tile.origin[0] + at[0], tile.origin[1] + at[1]])
        .find(|&at| desk.rendered_pointer(at) == Some(crate::chat::Pointer::Hand))
        .expect("the link, drawn");
    desk.mouse(
        link,
        MouseAction::Press(MouseButton::Left),
        1,
        Modifiers::default(),
    );
    assert_eq!(desk.shown.map(|shown| shown.pane), Some(PaneId(2)));
    assert!(rendered(&desk));
    assert_eq!(desk.take_opened(), ["https://x.org"]);
}

#[test]
fn links_open_as_links_elsewhere_do_and_files_from_the_markdown_files_folder() {
    let mut desk = editing("/p/docs/notes.md", "# Notes\n");
    command(&mut desk, "Markdown");
    desk.take_outbox();
    desk.open_rendered_link(PaneId(1), "https://x.org/a?b#c");
    assert_eq!(desk.take_opened(), ["https://x.org/a?b#c"]);
    // A file, from the folder, its heading left aside; in place of this
    // one, as `:e` would.
    desk.open_rendered_link(PaneId(1), "../My%20Notes.md#setup");
    let opened: Vec<String> = (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(Request::OpenHere { file, .. }) => Some(file.path),
            _ => None,
        })
        .collect();
    assert_eq!(opened, ["/p/My Notes.md"]);
    // This file, and a heading of it, is scrolled to, not opened again.
    desk.open_rendered_link(PaneId(1), "notes.md#nowhere");
    assert_eq!(desk.message.as_deref(), Some("no heading ‘nowhere’"));
    desk.open_rendered_link(PaneId(1), "./notes.md");
    assert_eq!(
        desk.message.as_deref(),
        Some("nothing opened: it's the file shown")
    );
    // A link with nowhere to go goes nowhere, not to the folder.
    desk.open_rendered_link(PaneId(1), "");
    assert_eq!(desk.message.as_deref(), Some("the link goes nowhere"));
    assert!(
        !(desk.take_outbox().iter())
            .any(|message| matches!(message, ClientMessage::Request(Request::OpenHere { .. })))
    );
    // A heading there isn't one.
    desk.open_rendered_link(PaneId(1), "#nowhere");
    assert_eq!(desk.message.as_deref(), Some("no heading ‘nowhere’"));
    // A scheme the desktop isn't trusted to open.
    desk.open_rendered_link(PaneId(1), "javascript:alert(1)");
    assert_eq!(
        desk.message.as_deref(),
        Some("‘javascript:alert(1)’ isn't a link clackatoo opens")
    );
    assert!(desk.take_opened().is_empty());
}
