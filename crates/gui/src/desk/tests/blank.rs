//! `:e` alone: an explorer of the focused pane's folder beside a buffer
//! with no file yet, and `:w NAME`, which names one, driven through the
//! desk as a window would drive it, with the daemon played by the test.

use clackatoo_protocol::{BufferUpdate, FilesAction, FilesId, OpenBlank, OpenFiles};

use super::*;

/// A new tab's pane, asked for by `:e` alone, showing a buffer with no
/// file yet, whose name is from `cwd`.
fn new_blank_tab(cwd: Option<&str>) -> Request {
    Request::NewTab {
        workspace: WorkspaceId(1),
        terminal: PaneTerminal::Blank(OpenBlank {
            cwd: cwd.map(str::to_owned),
        }),
    }
}

/// An explorer down the left of tab `tab`, rooted at `dir`, as `:e` alone
/// asks for one where there's none.
fn explorer_down_the_left(tab: u32, dir: Option<&str>) -> Request {
    Request::SplitTab {
        tab: TabId(tab),
        side: Side::Left,
        terminal: PaneTerminal::Files(OpenFiles {
            dir: dir.map(str::to_owned),
        }),
        share: Some(250),
    }
}

/// The requests waiting but those moving the focus, each answered in
/// turn: a pane asked for, as made, numbered `made`.
fn requests(desk: &mut Desk, made: u32) -> Vec<Request> {
    let asked: Vec<Request> = (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .collect();
    for request in &asked {
        desk.answered(match request {
            Request::NewTab { .. } | Request::SplitTab { .. } => {
                Response::PaneCreated(PaneId(made))
            }
            _ => Response::Done,
        });
    }
    (asked.into_iter())
        .filter(|request| !matches!(request, Request::Focus { .. }))
        .collect()
}

/// The requests waiting but those moving the focus, unanswered.
fn asked(desk: &mut Desk) -> Vec<Request> {
    (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .filter(|request| !matches!(request, Request::Focus { .. }))
        .collect()
}

/// What `messages` ask explorer `files` to be rooted at, in turn.
fn roots_of(messages: &[ClientMessage], files: u32) -> Vec<String> {
    (messages.iter())
        .filter_map(|message| match message {
            ClientMessage::Files {
                files: id,
                action: FilesAction::Root(dir),
            } if *id == FilesId(files) => Some(dir.clone()),
            _ => None,
        })
        .collect()
}

/// The terminal `terminal`'s shell is in `cwd`, as it reported.
fn terminal_in(desk: &mut Desk, terminal: u32, cwd: Option<&str>) {
    let screen = Terminal::new(Size::new(40, 24).unwrap(), &Config::default());
    let mut view = View::capture(&screen);
    view.cwd = cwd.map(str::to_owned);
    desk.view_changed(TerminalId(terminal), &ViewUpdate::Snapshot(view));
}

/// The daemon makes the new tab 2, showing buffer 9 in pane 5, after
/// tab 1 laid out as `first`, as `:e` alone asked, telling of it before
/// answering that it did.
fn blank_tab_made(desk: &mut Desk, first: Layout) {
    let both = workspaces(vec![workspace(
        1,
        "w",
        vec![tab(1, first), tab(2, file_pane(5, 9))],
    )]);
    desk.workspaces_changed(both);
    desk.answered(Response::PaneCreated(PaneId(5)));
}

/// Gives buffer `buffer` the file at `path`, or none if it's empty, as
/// the daemon sends a blank one, with `text`, changed if `modified`.
fn loaded(desk: &mut Desk, buffer: u32, path: &str, text: &str, modified: bool) {
    let snapshot = clackatoo_protocol::BufferSnapshot {
        path: path.into(),
        text: text.into(),
        version: 0,
        modified,
        applied: None,
        landed: None,
        daemon: 0,
    };
    desk.buffer_changed(BufferId(buffer), BufferUpdate::Snapshot(snapshot));
}

/// A desk showing the explorer `/p` in pane 2, beside pane 1, which shows
/// buffer 7, a file at `path` or with no file if it's empty, focused.
fn editor_beside_explorer(path: &str, text: &str, modified: bool) -> Desk {
    let layout = side_by_side(vec![files_pane(2, 1), file_pane(1, 7)]);
    let mut desk = desk(one_tab(layout));
    loaded(&mut desk, 7, path, text, modified);
    explore_p(&mut desk);
    normal_mode(&mut desk);
    ctrl(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(1));
    answer_all(&mut desk);
    desk
}

/// A desk showing terminal 3, its shell in `cwd`, in normal mode.
fn desk_with_terminal_in(cwd: Option<&str>) -> Desk {
    let mut desk = desk(one_tab(pane(3)));
    terminal_in(&mut desk, 3, cwd);
    normal_mode(&mut desk);
    answer_all(&mut desk);
    desk
}

/// Whether the bar's message says `words`.
fn said(desk: &Desk, words: &str) -> bool {
    (desk.message.as_deref()).is_some_and(|said| said.contains(words))
}

#[test]
fn e_alone_in_a_terminal_opens_a_tab_with_a_blank_buffer_and_an_explorer_of_its_folder() {
    let mut desk = desk_with_terminal_in(Some("/home/me/src"));
    command(&mut desk, "e");
    // Where `:e PATH` would open the file: a tab of its own.
    assert_eq!(asked(&mut desk), [new_blank_tab(Some("/home/me/src"))]);
    blank_tab_made(&mut desk, pane(3));
    assert_eq!(shown_pane(&desk), PaneId(5));
    loaded(&mut desk, 9, "", "", false);
    assert_eq!(desk.editor_title(PaneId(5)), Some("[No Name]"));
    // Then the explorer, down its left, at the terminal's folder, which
    // the daemon tells of before answering.
    let layout = side_by_side(vec![files_pane(6, 1), file_pane(5, 9)]);
    desk.workspaces_changed(workspaces(vec![workspace(
        1,
        "w",
        vec![tab(1, pane(3)), tab(2, layout)],
    )]));
    let made = requests(&mut desk, 6);
    assert!(
        made.contains(&explorer_down_the_left(2, Some("/home/me/src"))),
        "{made:?}"
    );
    // The explorer has the focus, to choose a file from.
    assert_eq!(shown_pane(&desk), PaneId(6));
}

#[test]
fn e_alone_beside_a_folder_the_window_doesnt_know_leaves_it_to_the_daemon() {
    // A chat's folder is only shown, `~` and all, and is never a path.
    let (mut desk, _) = working_chat_desk();
    desk.show_tab_numbered(WorkspaceId(1), 2);
    normal_mode(&mut desk);
    answer_all(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(2));
    command(&mut desk, "e");
    assert_eq!(asked(&mut desk), [new_blank_tab(None)]);
    // Nor has a terminal's shell always said.
    let mut desk = desk_with_terminal_in(None);
    command(&mut desk, "e");
    assert_eq!(asked(&mut desk), [new_blank_tab(None)]);
    blank_tab_made(&mut desk, pane(3));
    assert!(asked(&mut desk).contains(&explorer_down_the_left(2, None)));
}

#[test]
fn e_alone_in_an_editor_puts_a_blank_buffer_in_its_place_and_roots_the_tabs_explorer_there() {
    let mut desk = editor_beside_explorer("/p/src/main.rs", "fn main() {}\n", false);
    command(&mut desk, "e");
    let messages = desk.take_outbox();
    let asked: Vec<&Request> = (messages.iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .filter(|request| !matches!(request, Request::Focus { .. }))
        .collect();
    assert_eq!(
        asked,
        [&Request::BlankHere {
            pane: PaneId(1),
            blank: OpenBlank {
                cwd: Some("/p/src".into())
            },
        }],
        "no new explorer: the tab has one"
    );
    assert_eq!(roots_of(&messages, 1), ["/p/src"]);
    assert_eq!(shown_pane(&desk), PaneId(2));
}

#[test]
fn e_alone_keeps_a_files_changes_unseen_as_e_path_does() {
    let mut desk = editor_beside_explorer("/p/a.txt", "a\n", true);
    command(&mut desk, "e");
    let made = requests(&mut desk, 0);
    assert!(
        (made.iter()).any(|request| matches!(request, Request::BlankHere { .. })),
        "{made:?}"
    );
    assert!(said(&desk, "/p/a.txt has changes not written, kept unseen"));
}

#[test]
fn a_blank_buffer_with_changes_isnt_left_without_a_name_or_a_bang() {
    let mut desk = editor_beside_explorer("", "draft\n", true);
    let refused = |desk: &mut Desk, line: &str| {
        command(desk, line);
        let made = asked(desk);
        assert!(made.is_empty(), ":{line} asked {made:?}");
        assert!(said(desk, ":w NAME"), ":{line} said {:?}", desk.message);
        assert_eq!(shown_pane(desk), PaneId(1));
        desk.message = None;
    };
    refused(&mut desk, "e");
    refused(&mut desk, "e other.txt");
    refused(&mut desk, "close");
    refused(&mut desk, "q");
    // Nor does its tab or workspace close with it, from the bars.
    desk.close_tab(TabId(1));
    assert_eq!(asked(&mut desk), []);
    assert!(said(&desk, ":w NAME"));
    desk.close_workspace(WorkspaceId(1));
    assert_eq!(asked(&mut desk), []);
    // `:q!` drops it, as for any file.
    command(&mut desk, "q!");
    assert_eq!(
        asked(&mut desk),
        [
            Request::Discard {
                buffer: BufferId(7)
            },
            Request::ClosePane { pane: PaneId(1) }
        ]
    );
    // `:e!` empties it, which can be undone.
    command(&mut desk, "e!");
    assert_eq!(
        asked(&mut desk),
        [Request::Discard {
            buffer: BufferId(7)
        }]
    );
}

#[test]
fn a_blank_buffer_typed_in_has_changes_before_the_daemon_says_so() {
    // `ihello<Esc>:q<CR>`, faster than the daemon answers the edits.
    let mut desk = editor_beside_explorer("", "", false);
    type_text(&mut desk, "ihello");
    normal_mode(&mut desk);
    command(&mut desk, "q");
    let made = asked(&mut desk);
    assert!(
        !(made.iter()).any(|request| matches!(request, Request::ClosePane { .. })),
        "{made:?}"
    );
    assert!(said(&desk, ":w NAME writes them"), "{:?}", desk.message);
    command(&mut desk, "e");
    assert!(!(asked(&mut desk).iter()).any(|request| matches!(request, Request::BlankHere { .. })));
    assert!(said(&desk, ":e! drops them"), "{:?}", desk.message);
}

#[test]
fn e_alone_in_a_blank_buffer_with_no_changes_needs_no_other() {
    let mut desk = editor_beside_explorer("", "", false);
    command(&mut desk, "e");
    let messages = desk.take_outbox();
    assert!(
        !(messages.iter()).any(|message| matches!(
            message,
            ClientMessage::Request(request) if !matches!(request, Request::Focus { .. })
        )),
        "{messages:?}"
    );
    // Its folder isn't known, so the explorer stays where it's rooted.
    assert!(roots_of(&messages, 1).is_empty());
    assert_eq!(shown_pane(&desk), PaneId(2));
}

#[test]
fn e_alone_in_an_explorer_opens_a_tab_at_its_root_unless_it_edits_folders_as_text() {
    let layout = side_by_side(vec![files_pane(2, 1), pane(3)]);
    let mut desk = desk(one_tab(layout.clone()));
    explore_p(&mut desk);
    normal_mode(&mut desk);
    answer_all(&mut desk);
    assert_eq!(shown_pane(&desk), PaneId(2));
    command(&mut desk, "e");
    assert_eq!(asked(&mut desk), [new_blank_tab(Some("/p"))]);
    blank_tab_made(&mut desk, layout.clone());
    assert!(asked(&mut desk).contains(&explorer_down_the_left(2, Some("/p"))));

    // Editing its folders as text, it's left only by :w or :q!.
    let mut desk = super::desk(one_tab(layout));
    explore_p(&mut desk);
    normal_mode(&mut desk);
    command(&mut desk, "Explore!");
    answer_all(&mut desk);
    command(&mut desk, "e");
    assert_eq!(asked(&mut desk), []);
    assert!(said(&desk, "editing folders as text"), "{:?}", desk.message);
}

#[test]
fn e_alone_needs_a_daemon_that_makes_blank_buffers() {
    let mut desk = desk_with_terminal_in(Some("/home/me"));
    desk.daemon_is(
        clackatoo_protocol::wire::BLANK - 1,
        Some("2026.10.50".into()),
    );
    command(&mut desk, "e");
    assert_eq!(asked(&mut desk), []);
    assert!(
        said(&desk, ":e alone needs a newer daemon"),
        "{:?}",
        desk.message
    );
}

#[test]
fn w_in_a_blank_buffer_asks_for_its_name_and_writes_it_there() {
    let mut desk = editor_beside_explorer("", "draft\n", true);
    command(&mut desk, "w");
    assert_eq!(asked(&mut desk), []);
    assert!(
        matches!(desk.status(), Status::Typing { text, .. } if text == ":w ▏"),
        "the command line asks"
    );
    type_text(&mut desk, "notes.md");
    enter(&mut desk);
    assert_eq!(
        asked(&mut desk),
        [Request::SaveAs {
            buffer: BufferId(7),
            path: "notes.md".into(),
            force: false,
        }]
    );
    // Named, as the daemon tells before answering, it's written as any
    // file is.
    desk.buffer_changed(BufferId(7), BufferUpdate::Moved("/p/notes.md".into()));
    desk.answered(Response::Done);
    assert_eq!(desk.message.as_deref(), Some("notes.md written"));
    assert_eq!(desk.editor_title(PaneId(1)), Some("notes.md"));

    // `:wq!` asks as itself, and `:w NAME` needs no asking.
    let mut desk = editor_beside_explorer("", "draft\n", true);
    command(&mut desk, "wq!");
    assert!(matches!(desk.status(), Status::Typing { text, .. } if text == ":wq! ▏"));
    escape(&mut desk);
    command(&mut desk, "w! a\\ b.md");
    assert_eq!(
        asked(&mut desk),
        [Request::SaveAs {
            buffer: BufferId(7),
            path: "a b.md".into(),
            force: true,
        }]
    );
}

#[test]
fn w_name_in_a_files_editor_is_refused_as_it_has_one() {
    let mut desk = editor_beside_explorer("/p/a.txt", "a\n", false);
    command(&mut desk, "w b.txt");
    assert_eq!(asked(&mut desk), []);
    assert!(said(&desk, "this is ‘a.txt’"), "{:?}", desk.message);
}
