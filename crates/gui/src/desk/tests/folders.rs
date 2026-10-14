//! Editors showing folders, as oil.nvim's buffers do, driven through the
//! desk as a window would drive it, with the daemon played by the test.

use clackatoo_protocol::{FileOp, FilesAction, FilesId, FilesUpdate, Search};

use super::*;

const MAIN_RS: &str = "fn main() {}\n";

/// A pane showing a folder, made from explorer `files`.
fn folder_pane(pane: u32, files: u32) -> Layout {
    Layout::Pane(LayoutPane {
        id: PaneId(pane),
        content: Content::Folder(FilesId(files)),
    })
}

/// The daemon says explorer `files` is rooted at `path`.
fn rooted(desk: &mut Desk, files: u32, path: &str) {
    let update = FilesUpdate::Root {
        path: path.into(),
        name: path.replacen("/home/me", "~", 1),
    };
    desk.files_changed(FilesId(files), update);
}

/// The daemon lists `dir`, holding `entries`, for explorer `files`.
fn listed(desk: &mut Desk, files: u32, dir: &str, entries: Vec<clackatoo_protocol::Entry>) {
    let update = FilesUpdate::Listed {
        dir: dir.into(),
        entries,
        cut_short: false,
    };
    desk.files_changed(FilesId(files), update);
}

/// What `/p/src` holds.
fn src() -> Vec<clackatoo_protocol::Entry> {
    vec![entry("util", true), entry("main.rs", false)]
}

/// What `/p` holds.
fn p() -> Vec<clackatoo_protocol::Entry> {
    vec![entry("src", true), entry("a.txt", false)]
}

/// The files and folders asked to show in panes in place of theirs,
/// every request answered.
fn opened_here(desk: &mut Desk) -> Vec<String> {
    (answer_all(desk).into_iter())
        .filter_map(|request| match request {
            Request::OpenHere {
                pane: PaneId(1),
                file,
            } => Some(file.path),
            _ => None,
        })
        .collect()
}

/// Pane 1 shows explorer `files`' folder, `dir`, holding `entries`, once
/// the daemon has made it.
fn shows_folder(desk: &mut Desk, files: u32, dir: &str, entries: Vec<clackatoo_protocol::Entry>) {
    desk.workspaces_changed(one_tab(folder_pane(1, files)));
    answer_all(desk);
    rooted(desk, files, dir);
    listed(desk, files, dir, entries);
}

/// The folders an explorer is asked to be rooted at, in turn.
fn roots(messages: &[ClientMessage]) -> Vec<String> {
    (messages.iter())
        .filter_map(|message| match message {
            ClientMessage::Files {
                action: FilesAction::Root(dir),
                ..
            } => Some(dir.clone()),
            _ => None,
        })
        .collect()
}

/// Where in `messages` the first that `which` picks is.
fn place_of(messages: &[ClientMessage], which: impl Fn(&ClientMessage) -> bool) -> usize {
    (messages.iter())
        .position(which)
        .unwrap_or_else(|| panic!("not in {messages:?}"))
}

/// The rows the folder pane 1 shows, without the blank ones, as what its
/// listings' edits come to is.
fn folder_rows(desk: &mut Desk) -> Vec<String> {
    desk.settle_explorers();
    let (rows, _) = desk.explorer(PaneId(1)).expect("an explorer").scene();
    (rows.iter())
        .map(|row| row.text().trim_end().to_owned())
        .filter(|row| !row.is_empty())
        .collect()
}

/// An editor of `/p/src/main.rs` that has gone to its folder with `:e .`.
fn listing_src() -> Desk {
    let mut desk = editing("/p/src/main.rs", MAIN_RS);
    command(&mut desk, "e .");
    // From the file's folder, as the daemon resolves it.
    assert_eq!(opened_here(&mut desk), ["."]);
    shows_folder(&mut desk, 1, "/p/src", src());
    desk
}

#[test]
fn e_dot_shows_the_folder_and_enter_opens_a_file_in_its_place() {
    let mut desk = listing_src();
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    let (editor, _) = desk.focused_editor().expect("an editor");
    assert_eq!(editor.mode, Mode::Normal);
    assert_eq!(desk.tab_title(desk.shown_tab().unwrap()), Some("/p/src"));
    // In place of the listing, as `:e` would.
    type_keys(&mut desk, "j");
    enter(&mut desk);
    assert_eq!(opened_here(&mut desk), ["/p/src/main.rs"]);
    desk.workspaces_changed(one_tab(file_pane(1, 7)));
    answer_all(&mut desk);
    load(&mut desk, 7, "/p/src/main.rs", MAIN_RS);
    assert_eq!(editor_text(&desk), MAIN_RS);
    assert!(desk.explorer(PaneId(1)).is_none());
}

#[test]
fn minus_goes_up_from_a_file_to_its_line_and_from_a_folder_and_ctrl_o_comes_back() {
    let mut desk = editing("/p/src/main.rs", MAIN_RS);
    // From a file: its folder, with the cursor on it.
    type_keys(&mut desk, "-");
    // As a folder, ending `/`, so one gone since isn't made a file.
    assert_eq!(opened_here(&mut desk), ["/p/src/"]);
    shows_folder(&mut desk, 2, "/p/src", src());
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(listing_line(&desk), 2);
    // From a folder: the folder above, on the folder come from, which
    // the explorer is rooted at, for the finder.
    type_keys(&mut desk, "-");
    let sent = desk.take_outbox();
    assert_eq!(roots(&sent), ["/p"]);
    rooted(&mut desk, 2, "/p");
    listed(&mut desk, 2, "/p", p());
    assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
    assert_eq!(listing_line(&desk), 1);
    // Back into the folder, on the line it was on...
    ctrl(&mut desk, "o");
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(listing_line(&desk), 2);
    assert_eq!(roots(&desk.take_outbox()), ["/p/src"]);
    // ...then to the file, in place of the listing...
    ctrl(&mut desk, "o");
    assert_eq!(opened_here(&mut desk), ["/p/src/main.rs"]);
    desk.workspaces_changed(one_tab(file_pane(1, 7)));
    answer_all(&mut desk);
    load(&mut desk, 7, "/p/src/main.rs", MAIN_RS);
    // ...and forward again, to the folder, made afresh, on the file.
    ctrl(&mut desk, "i");
    assert_eq!(opened_here(&mut desk), ["/p/src/"]);
    shows_folder(&mut desk, 3, "/p/src", src());
    assert_eq!(listing_line(&desk), 2);
}

#[test]
fn the_finder_from_a_listing_is_rooted_at_the_folder_it_shows() {
    let mut desk = listing_src();
    type_keys(&mut desk, "-");
    rooted(&mut desk, 1, "/p");
    listed(&mut desk, 1, "/p", p());
    answer_all(&mut desk);
    // Into `src`, listed already, and the finder at once: the explorer
    // is rooted there before the finder asks, from the pane.
    enter(&mut desk);
    type_keys(&mut desk, " pf");
    let sent = desk.take_outbox();
    let rooted_at = (sent.iter()).position(|message| {
        matches!(message, ClientMessage::Files { action: FilesAction::Root(dir), .. } if dir == "/p/src")
    });
    let found = (sent.iter()).position(|message| {
        matches!(
            message,
            ClientMessage::Find {
                pane: Some(PaneId(1)),
                search: Search::Files,
                ..
            }
        )
    });
    assert!(rooted_at.is_some() && rooted_at < found, "{sent:?}");
}

#[test]
fn a_listing_with_changes_isnt_left_until_theyre_made_or_thrown_away() {
    let mut desk = listing_src();
    type_keys(&mut desk, "dd");
    // Enter on a file, `:e`, and Ctrl+O would lose them.
    enter(&mut desk);
    let refusal = "the listing has changes: :w makes them, :e! throws them away";
    assert_eq!(note(&desk), refusal);
    command(&mut desk, "e /p/a.txt");
    assert_eq!(note(&desk), refusal);
    assert_eq!(opened_here(&mut desk), Vec::<String>::new());
    command(&mut desk, "q");
    assert_eq!(
        note(&desk),
        "the listings have changes: :w makes them, :q! throws them away"
    );
    assert!(answer_all(&mut desk).is_empty());
    // `:q!` closes the pane, as for a file.
    command(&mut desk, "q!");
    assert_eq!(
        answer_all(&mut desk),
        [Request::ClosePane { pane: PaneId(1) }]
    );
}

#[test]
fn w_and_y_make_a_listings_changes_then_its_folder_is_listed_afresh() {
    let mut desk = listing_src();
    type_keys(&mut desk, "jcwlib");
    escape(&mut desk);
    command(&mut desk, "w");
    assert_eq!(note(&desk), "make this change? (y/n)");
    type_keys(&mut desk, "y");
    let moved = FileOp::Move {
        from: "/p/src/main.rs".into(),
        to: "/p/src/lib.rs".into(),
        overwrite: None,
        expect: Some(file_expected()),
    };
    assert_eq!(file_ops(&mut desk), [(0, moved)]);
    desk.file_op_done(0, Ok(()));
    assert_eq!(note(&desk), "made the change");
    // A listing again, not a tree, made afresh as the folder changes.
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    listed(
        &mut desk,
        1,
        "/p/src",
        vec![entry("util", true), entry("lib.rs", false)],
    );
    assert_eq!(listing_shown(&desk), ["..", "util/", "lib.rs"]);
    // Not while a change is being typed, though nothing's changed yet.
    type_keys(&mut desk, "jA");
    listed(&mut desk, 1, "/p/src", src());
    assert_eq!(listing_shown(&desk), ["..", "util/", "lib.rs"]);
    let (editor, _) = desk.focused_editor().expect("an editor");
    assert_eq!(editor.mode, Mode::Insert);
    escape(&mut desk);
    // Only while its text is as it was made.
    type_keys(&mut desk, "dd");
    listed(
        &mut desk,
        1,
        "/p/src",
        vec![entry("util", true), entry("lib.rs", false)],
    );
    assert_eq!(listing_shown(&desk), ["..", "util/"]);
}

#[test]
fn g_dot_shows_hidden_files_as_the_explorer_does_and_dot_still_repeats() {
    let mut desk = editing("/p/src/main.rs", MAIN_RS);
    command(&mut desk, "e /p");
    answer_all(&mut desk);
    let entries = vec![
        entry(".git", true),
        entry("src", true),
        entry("a.txt", false),
    ];
    shows_folder(&mut desk, 1, "/p", entries);
    assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
    type_keys(&mut desk, "jg.");
    assert_eq!(note(&desk), "hidden files show");
    assert_eq!(listing_shown(&desk), ["..", ".git/", "src/", "a.txt"]);
    assert_eq!(listing_line(&desk), 3, "still on a.txt");
    type_keys(&mut desk, "g.");
    assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
    // `.` alone is Vim's: it does the last change again.
    type_keys(&mut desk, "x.");
    assert_eq!(listing_shown(&desk), ["..", "src/", "txt"]);
    // Not while it has changes, which a listing made afresh would lose.
    type_keys(&mut desk, "g.");
    assert_eq!(listing_shown(&desk), ["..", "src/", "txt"]);
}

#[test]
fn colon_editor_with_no_path_shows_the_focused_panes_folder() {
    let mut desk = editing("/p/src/main.rs", MAIN_RS);
    command(&mut desk, "Editor");
    let asked = answer_all(&mut desk);
    assert!(
        matches!(
            asked.as_slice(),
            [Request::NewTab { terminal: PaneTerminal::File(file), .. }] if file.path == "."
        ),
        "{asked:?}"
    );
}

#[test]
fn the_root_follows_the_folder_shown_however_quickly_it_moves_and_goes_first() {
    let mut desk = listing_src();
    // Up, then back into src before the daemon says it's rooted above.
    type_keys(&mut desk, "-");
    listed(&mut desk, 1, "/p", p());
    enter(&mut desk);
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(roots(&desk.take_outbox()), ["/p", "/p/src"]);
    // The finder's search goes after it is rooted where it shows, though
    // the daemon said otherwise since, as another window's moves do.
    rooted(&mut desk, 1, "/p");
    type_keys(&mut desk, " pf");
    let sent = desk.take_outbox();
    assert_eq!(roots(&sent).last().map(String::as_str), Some("/p/src"));
    let rooted_at = (sent.iter()).rposition(|message| {
        matches!(
            message,
            ClientMessage::Files {
                action: FilesAction::Root(_),
                ..
            }
        )
    });
    let found = place_of(&sent, |message| {
        matches!(message, ClientMessage::Find { .. })
    });
    assert!(rooted_at.is_some_and(|at| at < found), "{sent:?}");
    escape(&mut desk);
    answer_all(&mut desk);
    // And a relative path is from the folder about to show, rooted there
    // before it's asked to open.
    type_keys(&mut desk, "-");
    command(&mut desk, "e foo");
    let sent = desk.take_outbox();
    let rooted_up = place_of(
        &sent,
        |message| matches!(message, ClientMessage::Files { action: FilesAction::Root(dir), .. } if dir == "/p"),
    );
    let opened = place_of(
        &sent,
        |message| matches!(message, ClientMessage::Request(Request::OpenHere { file, .. }) if file.path == "foo"),
    );
    assert!(rooted_up < opened, "{sent:?}");
}

#[test]
fn browsing_many_folders_keeps_only_those_with_changes_listed() {
    let mut desk = editing("/p/a.txt", "a\n");
    command(&mut desk, "e /p");
    answer_all(&mut desk);
    let dirs: Vec<clackatoo_protocol::Entry> = (0..50)
        .map(|index| entry(&format!("d{index}"), true))
        .collect();
    shows_folder(&mut desk, 1, "/p", dirs.clone());
    let mut sent = desk.take_outbox();
    for index in 0..50 {
        if index > 0 {
            type_keys(&mut desk, "j");
        }
        enter(&mut desk);
        let dir = format!("/p/d{index}");
        listed(&mut desk, 1, &dir, vec![entry("x.txt", false)]);
        assert_eq!(listing_shown(&desk), ["..", "x.txt"]);
        type_keys(&mut desk, "-");
        listed(&mut desk, 1, "/p", dirs.clone());
        assert_eq!(listing_line(&desk), index + 1, "on d{index}");
        sent.extend(desk.take_outbox());
    }
    // Each folder left as it was made is no longer listed, nor watched.
    let count = |expand: bool| {
        (sent.iter())
            .filter(|message| match message {
                ClientMessage::Files { action, .. } => {
                    matches!(action, FilesAction::Expand(_)) == expand
                        && matches!(action, FilesAction::Expand(_) | FilesAction::Collapse(_))
                }
                _ => false,
            })
            .count()
    };
    assert!(count(true) - count(false) <= 1, "{sent:?}");
    // One changed is kept, and its changes are made with the rest.
    enter(&mut desk);
    listed(&mut desk, 1, "/p/d49", vec![entry("x.txt", false)]);
    type_keys(&mut desk, "cwy");
    escape(&mut desk);
    type_keys(&mut desk, "-");
    listed(&mut desk, 1, "/p", dirs.clone());
    command(&mut desk, "w");
    assert_eq!(
        folder_rows(&mut desk),
        ["to do", "  rename d49/x.txt to y.txt"]
    );
}

#[test]
fn a_jump_isnt_gone_to_while_a_listings_changes_are_shown_to_be_made() {
    let mut desk = listing_src();
    type_keys(&mut desk, "-");
    listed(&mut desk, 1, "/p", p());
    type_keys(&mut desk, "jdd");
    command(&mut desk, "w");
    assert_eq!(note(&desk), "make this change? (y/n)");
    ctrl(&mut desk, "o");
    assert!(desk.focused_editor().is_none(), "still the changes");
    assert_eq!(
        note(&desk),
        "y makes the changes, n goes back to the listing"
    );
}

#[test]
fn enter_on_dot_dot_goes_up_as_minus_does_and_its_going_is_no_change() {
    let mut desk = listing_src();
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(listing_line(&desk), 1, "on the first entry, not `..`");
    // `l` moves along it, as along any text: it opens nothing.
    type_keys(&mut desk, "ggl");
    assert_eq!(roots(&desk.take_outbox()), Vec::<String>::new());
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    // Deleted, it's no change: nothing keeps the editor from going
    // elsewhere, and `:e!` puts it back.
    type_keys(&mut desk, "dd");
    assert_eq!(listing_shown(&desk), ["util/", "main.rs"]);
    assert_eq!(desk.keeps_its_folder(PaneId(1)), None);
    command(&mut desk, "e!");
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    // Enter on it goes up, rooted at the folder above, on the folder
    // come from.
    desk.take_outbox();
    type_keys(&mut desk, "gg");
    enter(&mut desk);
    assert_eq!(roots(&desk.take_outbox()), ["/p"]);
    rooted(&mut desk, 1, "/p");
    listed(&mut desk, 1, "/p", p());
    assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
    assert_eq!(listing_line(&desk), 1, "on src/");
    // And back, as a jump, to `..`, where the cursor was.
    ctrl(&mut desk, "o");
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(listing_line(&desk), 0);
    // `:e!` there, listing it afresh, keeps it on `..` too.
    command(&mut desk, "e!");
    rooted(&mut desk, 1, "/p/src");
    listed(&mut desk, 1, "/p/src", src());
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs"]);
    assert_eq!(listing_line(&desk), 0);
}

#[test]
fn steps_out_of_the_folder_shown_say_where_they_go_in_full() {
    let mut desk = listing_src();
    // `/p/srcx` starts as `/p/src` does, but isn't in it.
    type_keys(&mut desk, "ggcc../srcx");
    escape(&mut desk);
    type_keys(&mut desk, "Gcc../srcold/main.rs");
    escape(&mut desk);
    command(&mut desk, "w");
    assert_eq!(
        folder_rows(&mut desk),
        [
            "to do",
            "  make   /p/srcx",
            "  move   main.rs to /p/srcold/main.rs"
        ]
    );
}

#[test]
fn e_bang_in_a_folder_throws_away_every_listings_changes() {
    let mut desk = listing_src();
    type_keys(&mut desk, "dd-");
    listed(&mut desk, 1, "/p", p());
    type_keys(&mut desk, "j");
    enter(&mut desk);
    assert_eq!(
        note(&desk),
        "the listing has changes: :w makes them, :e! throws them away"
    );
    command(&mut desk, "e!");
    assert_eq!(listing_shown(&desk), ["..", "src/", "a.txt"]);
    type_keys(&mut desk, "j");
    enter(&mut desk);
    assert_eq!(opened_here(&mut desk), ["/p/a.txt"]);
}

#[test]
fn a_folder_with_changes_gone_from_disk_isnt_asked_for_again_and_again() {
    let mut desk = listing_src();
    type_keys(&mut desk, "dd-");
    listed(&mut desk, 1, "/p", p());
    desk.take_outbox();
    let gone = FilesUpdate::Unlisted {
        dir: "/p/src".into(),
        reason: "it's gone".into(),
    };
    desk.files_changed(FilesId(1), gone);
    let sent = desk.take_outbox();
    let asked_again = (sent.iter()).any(|message| {
        matches!(message, ClientMessage::Files { action: FilesAction::Expand(dir), .. } if dir == "/p/src")
    });
    assert!(!asked_again, "{sent:?}");
}

#[test]
fn a_folder_with_changes_gone_from_disk_keeps_every_change_from_being_made() {
    let mut desk = listing_src();
    type_keys(&mut desk, "dd-");
    listed(&mut desk, 1, "/p", p());
    type_keys(&mut desk, "jcwb");
    escape(&mut desk);
    let gone = FilesUpdate::Unlisted {
        dir: "/p/src".into(),
        reason: "it's gone".into(),
    };
    desk.files_changed(FilesId(1), gone);
    command(&mut desk, "w");
    assert_eq!(note(&desk), "make these 2 changes? (y/n)");
    type_keys(&mut desk, "y");
    assert_eq!(file_ops(&mut desk), [], "nothing done");
    assert!(note(&desk).starts_with("nothing was done: a folder has changed"));
}

#[test]
fn a_folder_gone_from_disk_without_changes_settles_and_keeps_nothing_from_being_made() {
    let mut desk = listing_src();
    type_keys(&mut desk, "-");
    listed(&mut desk, 1, "/p", p());
    // Left as it was made, it settled; gone, it stays so.
    let gone = FilesUpdate::Unlisted {
        dir: "/p/src".into(),
        reason: "it's gone".into(),
    };
    desk.files_changed(FilesId(1), gone);
    type_keys(&mut desk, "jcwb");
    escape(&mut desk);
    command(&mut desk, "w");
    type_keys(&mut desk, "y");
    assert_eq!(file_ops(&mut desk).len(), 1, "the rename goes ahead");
}

#[test]
fn a_line_yanked_before_its_folder_is_listed_afresh_still_pastes_as_a_copy() {
    let mut desk = listing_src();
    type_keys(&mut desk, "jyy");
    // A file made on disk: the listing, unchanged, is made afresh.
    let more = vec![
        entry("util", true),
        entry("main.rs", false),
        entry("new.rs", false),
    ];
    listed(&mut desk, 1, "/p/src", more);
    assert_eq!(listing_shown(&desk), ["..", "util/", "main.rs", "new.rs"]);
    type_keys(&mut desk, "pcwz");
    escape(&mut desk);
    command(&mut desk, "w");
    assert_eq!(note(&desk), "make this change? (y/n)");
    assert_eq!(
        folder_rows(&mut desk),
        ["to do", "  copy   main.rs to z.rs"]
    );
}
