//! `:Markdown` and `<Space>tm`, which show an editor's Markdown rendered,
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
    assert!(rendered(&desk));
    // `<Space>tm` turns it back, as `:Markdown` does.
    type_keys(&mut desk, " tm");
    assert!(!rendered(&desk));
    type_keys(&mut desk, " tm");
    assert!(rendered(&desk));
}

#[test]
fn the_focus_arriving_in_insert_mode_shows_the_text_to_type_in() {
    let mut desk = editing("/p/notes.md", "# Notes\n");
    command(&mut desk, "Markdown");
    desk.editor_to_mode(PaneId(1), true);
    assert!(!rendered(&desk));
    let (editor, _) = desk.focused_editor().unwrap();
    assert_eq!(editor.mode, Mode::Insert);
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
