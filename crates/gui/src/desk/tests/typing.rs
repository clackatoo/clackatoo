//! The command line's keys: Tab completing, Up and Down going back
//! through what was typed before, Ctrl+U, Ctrl+W and Ctrl+R, and the
//! panes' searches going back through theirs.

use super::*;

fn key(desk: &mut Desk, key: NamedKey) {
    press(desk, Key::Named(key), None, Modifiers::default());
}

fn shift_tab(desk: &mut Desk) {
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    press(desk, Key::Named(NamedKey::Tab), None, shift);
}

/// What's typed in the bar, its caret where it is, and the choices under
/// it, with the one picked.
fn typed(desk: &Desk) -> (String, Vec<(String, bool)>) {
    match desk.status() {
        Status::Typing { text, choices, .. } => (text, choices),
        Status::Tabs { .. } => panic!("nothing's typed"),
    }
}

fn text(desk: &Desk) -> String {
    typed(desk).0
}

/// A desk with a terminal, in normal mode.
fn terminal_desk() -> Desk {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    desk
}

#[test]
fn tab_completes_the_word_and_goes_through_the_choices() {
    let mut desk = terminal_desk();
    type_keys(&mut desk, ":set nu");
    key(&mut desk, NamedKey::Tab);
    let (text, choices) = typed(&desk);
    assert_eq!(text, ":set number▏");
    assert_eq!(
        choices,
        [("number".into(), true), ("numberwidth".into(), false)]
    );
    key(&mut desk, NamedKey::Tab);
    assert_eq!(self::text(&desk), ":set numberwidth▏");
    // Down and Up go through them too.
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(self::text(&desk), ":set number▏");
    // Back past the first, to what was typed.
    shift_tab(&mut desk);
    assert_eq!(self::text(&desk), ":set nu▏");
    // Any other key ends it, and it's typed.
    type_keys(&mut desk, "m");
    assert_eq!(typed(&desk), (":set num▏".into(), Vec::new()));
    // One choice goes in at once, with nothing listed.
    type_keys(&mut desk, " wr");
    key(&mut desk, NamedKey::Tab);
    assert_eq!(typed(&desk), (":set num wrap▏".into(), Vec::new()));
    // The command's name, and a kind of pane after a split's mark.
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":Ex");
    key(&mut desk, NamedKey::Tab);
    assert_eq!(self::text(&desk), ":Explore▏");
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":|Ter");
    key(&mut desk, NamedKey::Tab);
    assert_eq!(self::text(&desk), ":|Terminal▏");
}

#[test]
fn tab_completes_workspaces_and_files_from_the_focused_editors_folder() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().to_owned();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "").unwrap();
    std::fs::write(dir.join("notes.md"), "").unwrap();
    let path = dir.join("a.rs");
    let mut desk = editing(path.to_str().unwrap(), "fn a() {}\n");
    type_keys(&mut desk, ":e s");
    key(&mut desk, NamedKey::Tab);
    // The folder is listed off the window's thread, and nothing's put in
    // till it is.
    assert_eq!(text(&desk), ":e s▏");
    assert!(list(&mut desk));
    assert_eq!(text(&desk), ":e src/▏");
    key(&mut desk, NamedKey::Tab);
    assert!(list(&mut desk));
    assert_eq!(text(&desk), ":e src/main.rs▏");
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":ws ");
    key(&mut desk, NamedKey::Tab);
    assert_eq!(text(&desk), ":ws w▏", "the one workspace");
    // On another machine's daemon, files aren't the window's to list.
    key(&mut desk, NamedKey::Escape);
    desk.remote = true;
    type_keys(&mut desk, ":e s");
    key(&mut desk, NamedKey::Tab);
    assert_eq!(text(&desk), ":e s▏");
    key(&mut desk, NamedKey::Escape);
    assert!(
        matches!(desk.status(), Status::Tabs { note: Some(note), .. }
            if note.contains("this machine"))
    );
}

/// Lists the folders the desk asked for, as the window does on threads,
/// and hands them back: returns whether it was drawn again for them.
fn list(desk: &mut Desk) -> bool {
    let listings = desk.take_listings();
    assert!(!listings.is_empty(), "a folder to list");
    (listings.into_iter()).fold(false, |redraw, listing| desk.listed(listing.run()) | redraw)
}

#[test]
fn files_listed_are_offered_only_if_the_line_is_as_it_was_and_one_at_a_time() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().to_owned();
    std::fs::write(dir.join("one.md"), "").unwrap();
    std::fs::write(dir.join("two.md"), "").unwrap();
    let mut desk = editing(dir.join("a.rs").to_str().unwrap(), "\n");
    type_keys(&mut desk, ":e o");
    key(&mut desk, NamedKey::Tab);
    // Tab again, while it's listed, asks for no other.
    key(&mut desk, NamedKey::Tab);
    let listings = desk.take_listings();
    assert_eq!(listings.len(), 1);
    // Typed on meanwhile: what comes is too late.
    type_keys(&mut desk, "n");
    assert!(!desk.listed(listings[0].run()));
    assert_eq!(text(&desk), ":e on▏");
    // And another may be listed now.
    key(&mut desk, NamedKey::Tab);
    assert!(list(&mut desk));
    assert_eq!(text(&desk), ":e one.md▏");
    // Several are listed under the line.
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":e ");
    key(&mut desk, NamedKey::Tab);
    list(&mut desk);
    let (text, choices) = typed(&desk);
    assert_eq!(text, ":e one.md▏");
    assert_eq!(choices, [("one.md".into(), true), ("two.md".into(), false)]);
    // A paste ends it.
    desk.paste("x");
    assert_eq!(typed(&desk), (":e one.mdx▏".into(), Vec::new()));
}

#[test]
fn files_listed_for_one_pane_are_never_offered_in_another() {
    let scratch = tempfile::tempdir().unwrap();
    let path = |rest: &str| scratch.path().join(rest).to_str().unwrap().to_owned();
    for (dir, file) in [("one", "apple"), ("two", "avocado")] {
        std::fs::create_dir(path(dir)).unwrap();
        std::fs::write(path(&format!("{dir}/{file}")), "").unwrap();
    }
    let layout = side_by_side(vec![file_pane(1, 7), file_pane(2, 8)]);
    let mut desk = desk(one_tab(layout));
    load(&mut desk, 7, &path("one/x.rs"), "\n");
    load(&mut desk, 8, &path("two/y.rs"), "\n");
    type_keys(&mut desk, ":e a");
    key(&mut desk, NamedKey::Tab);
    let first = desk.take_listings();
    // The same line, typed from the other pane, while pane 1's folder is
    // listed: that's let go, and pane 2's is listed.
    key(&mut desk, NamedKey::Escape);
    desk.show_pane(PaneId(2));
    type_keys(&mut desk, ":e a");
    key(&mut desk, NamedKey::Tab);
    let second = desk.take_listings();
    assert_eq!(second.len(), 1, "not waiting on pane 1's");
    assert!(!desk.listed(first[0].run()), "pane 1's folder");
    assert_eq!(text(&desk), ":e a▏");
    assert!(desk.listed(second[0].run()));
    assert_eq!(text(&desk), ":e avocado▏");
    // And back in pane 1, pane 2's would be too late.
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":e a");
    key(&mut desk, NamedKey::Tab);
    let third = desk.take_listings();
    key(&mut desk, NamedKey::Escape);
    desk.show_pane(PaneId(1));
    type_keys(&mut desk, ":e a");
    assert!(!desk.listed(third[0].run()));
    assert_eq!(text(&desk), ":e a▏");
}

#[test]
fn tabs_pressed_while_files_are_listed_go_through_them_once_they_come() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().to_owned();
    for file in ["a1", "a2", "a3"] {
        std::fs::write(dir.join(file), "").unwrap();
    }
    let mut desk = editing(dir.join("x.rs").to_str().unwrap(), "\n");
    type_keys(&mut desk, ":e a");
    key(&mut desk, NamedKey::Tab);
    key(&mut desk, NamedKey::Tab);
    assert_eq!(typed(&desk), (":e a▏".into(), Vec::new()), "nothing yet");
    // Slow to come, it says so.
    let asked = desk.listings.asked.as_mut().unwrap();
    asked.at -= std::time::Duration::from_millis(200);
    assert_eq!(typed(&desk).1, [("listing…".to_owned(), false)]);
    assert!(list(&mut desk));
    let (text, choices) = typed(&desk);
    assert_eq!(text, ":e a2▏", "the second, for Tab Tab");
    assert_eq!(choices[1], ("a2".to_owned(), true));
    // Shift+Tab, while it's listed, is the last.
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":e a");
    shift_tab(&mut desk);
    list(&mut desk);
    assert_eq!(text_of(&desk), ":e a3▏");
}

/// What's typed in the bar.
fn text_of(desk: &Desk) -> String {
    typed(desk).0
}

#[test]
fn a_count_alone_leaves_the_next_key_to_the_bindings() {
    let mut desk = terminal_desk();
    type_keys(&mut desk, "5");
    assert!(
        matches!(desk.status(), Status::Tabs { note: Some(note), .. } if note.starts_with('5')),
        "shown, as Vim's showcmd"
    );
    type_keys(&mut desk, ":");
    assert_eq!(text(&desk), ":▏", "the command line opens");
    key(&mut desk, NamedKey::Escape);
    assert!(
        !matches!(desk.status(), Status::Tabs { note: Some(note), .. } if note.starts_with('5')),
        "and the count's gone"
    );
    // In an editor, it starts with as many lines, as Vim's does.
    let mut editor = editing("/p/a.rs", "a\nb\nc\n");
    type_keys(&mut editor, "5");
    assert!(editor_label(&editor).starts_with("5  "));
    type_keys(&mut editor, ":");
    assert_eq!(text(&editor), ":.,.+4▏");
    key(&mut editor, NamedKey::Escape);
    type_keys(&mut editor, "1:");
    assert_eq!(text(&editor), ":.▏");
    key(&mut editor, NamedKey::Escape);
    // From a selection, its lines, count or not, as Neovim's `Vj3:`.
    type_keys(&mut editor, "Vj3:");
    assert_eq!(text(&editor), ":'<,'>▏");
    key(&mut editor, NamedKey::Escape);
    // A count before a key the editor takes is still the editor's.
    type_keys(&mut editor, "2j");
    assert!(
        editor_label(&editor).ends_with("3:1"),
        "{}",
        editor_label(&editor)
    );
}

#[test]
fn huge_counts_neither_hang_nor_overflow() {
    let mut desk = terminal_desk();
    type_keys(&mut desk, "99999999999999999999999H");
    type_keys(&mut desk, "99999999999999999999999L");
    type_keys(&mut desk, "99999999999w99999999999b");
    let mut editor = editing("/p/a.rs", "a b c\nd e\n");
    type_keys(&mut editor, "99999999999999999999w");
    type_keys(&mut editor, "99999999999cw");
    key(&mut editor, NamedKey::Escape);
    type_keys(&mut editor, "99999999999u");
}

#[test]
fn up_and_down_go_back_through_the_commands_that_start_with_what_is_typed() {
    let mut desk = terminal_desk();
    desk.start_recall(crate::cmdline::recall::Start {
        recalled: crate::cmdline::recall::Recalled::default(),
        path: Some("/nowhere/history".into()),
    });
    for line in ["set wrap", "messages", "set number"] {
        command(&mut desk, line);
        key(&mut desk, NamedKey::Escape);
    }
    // Neither of these is kept: one starts with a space, as bash's
    // ignorespace has it, and one has a secret in it.
    for line in [" messages", "Devices allow 1234"] {
        command(&mut desk, line);
        key(&mut desk, NamedKey::Escape);
    }
    type_keys(&mut desk, ":se");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), ":set number▏");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), ":set wrap▏");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), ":set wrap▏", "the oldest");
    key(&mut desk, NamedKey::ArrowDown);
    key(&mut desk, NamedKey::ArrowDown);
    assert_eq!(text(&desk), ":se▏", "back to what was typed");
    // With nothing typed, the last.
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), ":set number▏");
    // What was run goes to the file, for windows opened later.
    let save = desk.take_recall_save().expect("lines to keep");
    assert_eq!(save.added.len(), 3);
    assert_eq!(save.added[2].1, "set number");
    assert!(desk.take_recall_save().is_none(), "once");
}

#[test]
fn ctrl_u_ctrl_w_the_arrows_and_ctrl_r_edit_the_command_line() {
    let mut desk = terminal_desk();
    type_keys(&mut desk, ":Ex");
    ctrl(&mut desk, "u");
    type_keys(&mut desk, "set wr");
    assert_eq!(text(&desk), ":set wr▏");
    ctrl(&mut desk, "w");
    assert_eq!(text(&desk), ":set ▏");
    type_keys(&mut desk, "nu");
    key(&mut desk, NamedKey::Home);
    key(&mut desk, NamedKey::ArrowRight);
    key(&mut desk, NamedKey::Delete);
    assert_eq!(text(&desk), ":s▏t nu");
    key(&mut desk, NamedKey::End);
    assert_eq!(text(&desk), ":st nu▏");
    // Ctrl+R `:` puts in the last command.
    key(&mut desk, NamedKey::Escape);
    command(&mut desk, "messages");
    key(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, ":");
    ctrl(&mut desk, "r");
    type_keys(&mut desk, ":");
    assert_eq!(text(&desk), ":messages▏");
}

#[test]
fn ctrl_r_puts_in_a_register_or_the_word_under_the_editors_cursor() {
    let mut desk = editing("/p/a.rs", "fn alpha() {}\n");
    type_keys(&mut desk, "wyiw:s/");
    ctrl(&mut desk, "r");
    assert_eq!(text(&desk), ":s/\"", "waiting for the register's name");
    type_keys(&mut desk, "0");
    assert_eq!(text(&desk), ":s/alpha▏");
    // An empty one puts nothing in.
    ctrl(&mut desk, "r");
    type_keys(&mut desk, "z");
    assert_eq!(text(&desk), ":s/alpha▏");
    ctrl(&mut desk, "u");
    ctrl(&mut desk, "r");
    ctrl(&mut desk, "w");
    assert_eq!(text(&desk), ":alpha▏");
}

#[test]
fn searches_go_back_through_those_typed_in_terminals_and_editors() {
    let mut desk = terminal_desk();
    for search in ["/foo", "/bar"] {
        type_keys(&mut desk, search);
        key(&mut desk, NamedKey::Enter);
    }
    type_keys(&mut desk, "/");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), "/bar▏");
    key(&mut desk, NamedKey::ArrowUp);
    assert_eq!(text(&desk), "/foo▏");
    // The terminal's search line edits as the command line does.
    ctrl(&mut desk, "w");
    assert_eq!(text(&desk), "/▏");
    key(&mut desk, NamedKey::Escape);
    // An editor's searches are the same searches.
    let mut editor = editing("/p/a.rs", "foo bar\n");
    editor.recall = std::mem::take(&mut desk.recall);
    type_keys(&mut editor, "?b");
    key(&mut editor, NamedKey::ArrowUp);
    assert_eq!(text(&editor), "?bar▏");
    key(&mut editor, NamedKey::Enter);
    // From the top, backward, it went round to the bottom, and says so
    // until the next key.
    assert_eq!(editor_label(&editor), crate::pane::WRAPPED_TO_BOTTOM);
    type_keys(&mut editor, "l");
    assert!(editor_label(&editor).ends_with("1:6"), "found");
}

#[test]
fn the_bar_keeps_the_mode_and_the_tabs_while_something_is_typed() {
    let mut desk = terminal_desk();
    type_keys(&mut desk, ":");
    let Status::Typing {
        under: Some(under), ..
    } = desk.status()
    else {
        panic!("typing, over the bar");
    };
    assert!(matches!(
        *under,
        Status::Tabs {
            mode: Some(ModeName::Normal),
            panes: 1,
            ..
        }
    ));
}
