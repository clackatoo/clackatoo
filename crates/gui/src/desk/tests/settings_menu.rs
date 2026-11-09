//! The settings menu, `:Settings`: searched, its options changed live and
//! saved in their files, put back as they start, and undone; locked where
//! the user's own config sets them, opening that line; and keys recorded,
//! saved in settings.lua as keys bound to keys, which a reload reads back.
//! And `:mappersist`, which saves keys as the menu does.

use clackatoo_protocol::{OpenFile, PaneTerminal};

use super::*;
use crate::keymap::{KeyMode, parse_keys};
use crate::lua::Source;
use crate::options::provenance::Source as SetFrom;
use crate::options::settings_lua::HEADER;

/// A desk whose config directory holds `init.lua`, loaded, and the
/// directory.
fn configured(init: &str) -> (Desk, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("client")).unwrap();
    std::fs::write(dir.path().join("client/init.lua"), init).unwrap();
    let mut desk = three_tabs();
    snapshot(&mut desk, 1);
    desk.lua_in(Source::Dir(dir.path().into()));
    normal_mode(&mut desk);
    (desk, dir)
}

/// Waits for `count` settings to have been saved, and says so as the
/// window does.
fn saved(desk: &mut Desk, count: usize) {
    for _ in 0..count {
        let saved = desk.saving.wait();
        desk.said_saved(saved);
    }
}

fn read(dir: &tempfile::TempDir, file: &str) -> String {
    std::fs::read_to_string(dir.path().join(file)).unwrap_or_default()
}

fn named(desk: &mut Desk, key: NamedKey) {
    press(desk, Key::Named(key), None, Modifiers::default());
}

/// The menu, as it's drawn: its picked row, what it says in place of the
/// count, and the lines of its preview.
fn menu(desk: &mut Desk) -> (String, String, Vec<String>) {
    let view = desk
        .finder_view((30, 100), Some((30, 70)))
        .expect("the menu");
    let picked = view
        .selected
        .map(|at| view.rows[at].text.clone())
        .unwrap_or_default();
    let preview = (view.preview.into_iter())
        .flat_map(|preview| preview.lines)
        .map(|line| line.text)
        .collect();
    (picked, view.count, preview)
}

#[test]
fn the_menu_finds_an_option_changes_and_saves_it_resets_and_undoes_it() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "Settings");
    assert!(desk.settings.is_some());
    // `/` searches; Enter stops typing, keeping what's found.
    type_keys(&mut desk, "/wrap");
    named(&mut desk, NamedKey::Enter);
    let (picked, _, preview) = menu(&mut desk);
    assert!(picked.starts_with("wrap "), "{picked}");
    assert!(
        preview
            .iter()
            .any(|line| line.starts_with("Saved in  client/settings.lua"))
    );
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert!(!desk.options.wrap, "it applies at once");
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!("{HEADER}\nclient.opt.wrap = false\n")
    );
    assert_eq!(
        desk.message.as_deref(),
        Some("saved nowrap in client/settings.lua")
    );
    // Back as it starts, and out of the file.
    type_keys(&mut desk, "r");
    saved(&mut desk, 1);
    assert!(desk.options.wrap);
    assert_eq!(read(&dir, "client/settings.lua"), format!("{HEADER}\n"));
    // Undone, a change at a time.
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(!desk.options.wrap);
    assert!(read(&dir, "client/settings.lua").ends_with("client.opt.wrap = false\n"));
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(desk.options.wrap);
    assert!(!read(&dir, "client/settings.lua").contains("wrap"));
    type_keys(&mut desk, "u");
    assert_eq!(menu(&mut desk).1, "the menu hasn't changed wrap");
    // Esc clears the search, then closes the menu.
    named(&mut desk, NamedKey::Escape);
    assert!(desk.settings.is_some());
    named(&mut desk, NamedKey::Escape);
    assert!(desk.settings.is_none());
}

#[test]
fn an_option_set_since_says_both_values_and_one_of_this_windows_own_can_go() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "set nuw=7");
    command(&mut desk, "setlocal nonu");
    command(&mut desk, "Settings numberwidth");
    let (picked, _, preview) = menu(&mut desk);
    assert!(picked.starts_with("numberwidth"), "{picked}");
    assert!(picked.ends_with("not saved"), "{picked}");
    assert!(
        preview.contains(&"Now       configured 4, currently 7".to_owned()),
        "{preview:#?}"
    );
    // Changing it changes what it's configured to.
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert_eq!(desk.options.numberwidth, 8);
    assert!(read(&dir, "client/settings.lua").ends_with("client.opt.numberwidth = 8\n"));
    let (picked, _, preview) = menu(&mut desk);
    assert!(!picked.ends_with("not saved"), "{picked}");
    assert!(
        !preview.iter().any(|line| line.starts_with("Now")),
        "{preview:#?}"
    );
    // A value typed, as `:set` takes it.
    named(&mut desk, NamedKey::Enter);
    named(&mut desk, NamedKey::Backspace);
    type_keys(&mut desk, "12");
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert_eq!(desk.options.numberwidth, 12);
    // `:setlocal`'s is this window's own, which `x` takes away.
    named(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, "/number");
    named(&mut desk, NamedKey::Enter);
    let (picked, _, preview) = menu(&mut desk);
    assert!(picked.starts_with("number "), "{picked}");
    assert!(picked.ends_with("this window's own"), "{picked}");
    assert!(
        preview.contains(&"Here      this window's own: off".to_owned()),
        "{preview:#?}"
    );
    assert!(!desk.options.number);
    type_keys(&mut desk, "x");
    assert!(desk.options.number);
}

#[test]
fn an_option_init_lua_sets_is_locked_and_enter_opens_its_line() {
    let (mut desk, dir) = configured("client.opt.ts = 2\nclient.opt.nuw = 6\n");
    command(&mut desk, "Settings numberwidth");
    let (picked, _, preview) = menu(&mut desk);
    assert!(picked.ends_with("init.lua:2"), "{picked}");
    assert!(
        preview.contains(
            &"Locked: set in client/init.lua:2, which outdoes what the menu saves".to_owned()
        ),
        "{preview:#?}"
    );
    type_keys(&mut desk, "l");
    assert_eq!(
        menu(&mut desk).1,
        "client/init.lua:2 sets numberwidth, which outdoes what the menu saves: Enter opens it"
    );
    type_keys(&mut desk, "r");
    assert_eq!(desk.options.numberwidth, 6, "nothing changed");
    assert!(!dir.path().join("client/settings.lua").exists());
    answer_all(&mut desk);
    named(&mut desk, NamedKey::Enter);
    assert!(desk.settings.is_none());
    let init = dir.path().join("client/init.lua");
    let requests = answer_all(&mut desk);
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::Split {
                terminal: PaneTerminal::File(OpenFile { path, .. }),
                ..
            } if *path == init.to_string_lossy()
        )),
        "{requests:?}"
    );
}

#[test]
fn keys_recorded_are_saved_as_a_binding_that_a_reload_reads_back() {
    let (mut desk, dir) = configured("client.keys.map('n', '<A-y>', function() end)\n");
    command(&mut desk, "Settings new tab");
    let (picked, _, _) = menu(&mut desk);
    assert!(picked.starts_with("<C-b>c"), "{picked}");
    // Keys the user's own config binds are refused.
    named(&mut desk, NamedKey::Enter);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    press(&mut desk, Key::Character("y".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(
        menu(&mut desk).1,
        "client/init.lua:1 binds <A-y>, which outdoes what the menu saves"
    );
    // Others are saved, and bound at once.
    named(&mut desk, NamedKey::Enter);
    press(&mut desk, Key::Character("t".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!(
            "{HEADER}\nclient.keys.map(\"niv\", \"<A-t>\", \"<C-b>c\", {{ desc = \"new tab\" }})\n"
        )
    );
    assert_eq!(
        desk.message.as_deref(),
        Some("saved <A-t> in client/settings.lua")
    );
    named(&mut desk, NamedKey::Escape);
    named(&mut desk, NamedKey::Escape);
    assert!(desk.settings.is_none());
    answer_all(&mut desk);
    press(&mut desk, Key::Character("t".into()), None, alt);
    assert!(matches!(
        answer_all(&mut desk)[..],
        [Request::NewTab { .. }]
    ));
    // A reload reads it back, from settings.lua.
    command(&mut desk, "reload");
    desk.lua_ready();
    let keys = parse_keys("<A-t>").unwrap();
    let by = desk.client().0.keys().bound_by(KeyMode::Insert, &keys);
    assert_eq!((by.source, by.loading), (SetFrom::Settings, true));
    answer_all(&mut desk);
    press(&mut desk, Key::Character("t".into()), None, alt);
    assert!(matches!(
        answer_all(&mut desk)[..],
        [Request::NewTab { .. }]
    ));
    // `d` takes them out again, and they do nothing.
    command(&mut desk, "Settings <A-t>");
    assert!(menu(&mut desk).0.starts_with("<A-t>"));
    type_keys(&mut desk, "d");
    saved(&mut desk, 1);
    assert_eq!(read(&dir, "client/settings.lua"), format!("{HEADER}\n"));
    assert!(desk.client().0.keys().get(KeyMode::Normal, &keys).is_none());
}

#[test]
fn mappersist_saves_keys_unless_init_lua_binds_them() {
    let (mut desk, dir) = configured("client.keys.map('n', '<A-y>', function() end)\n");
    command(&mut desk, "mappersist n <A-y> <C-b>c");
    assert_eq!(
        desk.message.as_deref(),
        Some("client/init.lua:1 binds <A-y>, which outdoes settings.lua: change it there")
    );
    command(&mut desk, "mapp nv <A-u> <C-b>]");
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!("{HEADER}\nclient.keys.map(\"nv\", \"<A-u>\", \"<C-b>]\")\n")
    );
    let keys = parse_keys("<A-u>").unwrap();
    assert!(desk.client().0.keys().get(KeyMode::Visual, &keys).is_some());
}
