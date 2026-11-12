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
/// directory. The menu opens in Advanced, as it was used last.
fn configured(init: &str) -> (Desk, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("client")).unwrap();
    std::fs::write(dir.path().join("client/init.lua"), init).unwrap();
    std::fs::write(dir.path().join("settings-mode"), "advanced\n").unwrap();
    let mut desk = three_tabs();
    desk.settings_mode_file = Some(dir.path().join("settings-mode"));
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
    // Not by a daemon on another machine, which has files of its own.
    desk.remote = true;
    named(&mut desk, NamedKey::Enter);
    assert!(desk.settings.is_none());
    assert_eq!(
        desk.message.as_deref(),
        Some("init.lua is on this machine; the workspace's daemon is on another")
    );
    assert!(answer_all(&mut desk).is_empty());
    desk.remote = false;
    command(&mut desk, "Settings numberwidth");
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
    assert_eq!(
        desk.client().0.keys().bound_by(KeyMode::Normal, &keys),
        crate::options::SetBy::DEFAULT
    );
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

#[test]
fn keys_saved_in_some_modes_change_only_those_and_taking_out_nothing_says_so() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "mapp niv <A-t> <C-b>c");
    saved(&mut desk, 1);
    command(&mut desk, "mapp i <A-t> <C-b>]");
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!(
            "{HEADER}\nclient.keys.map(\"nv\", \"<A-t>\", \"<C-b>c\")\n\
             client.keys.map(\"i\", \"<A-t>\", \"<C-b>]\")\n"
        )
    );
    // Two rows, each its own: taking out one leaves the other.
    command(&mut desk, "Settings <A-t>");
    let (picked, _, preview) = menu(&mut desk);
    assert!(
        picked.starts_with("<A-t>") && picked.contains("types <C-b>c"),
        "{picked}"
    );
    assert!(
        preview.contains(&"Modes     normal, visual".to_owned()),
        "{preview:#?}"
    );
    type_keys(&mut desk, "d");
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!("{HEADER}\nclient.keys.map(\"i\", \"<A-t>\", \"<C-b>]\")\n")
    );
    let keys = parse_keys("<A-t>").unwrap();
    assert!(desk.client().0.keys().get(KeyMode::Normal, &keys).is_none());
    assert!(desk.client().0.keys().get(KeyMode::Insert, &keys).is_some());
    // What isn't in the file any more isn't said to be taken out.
    std::fs::write(
        dir.path().join("client/settings.lua"),
        format!("{HEADER}\n"),
    )
    .unwrap();
    let (picked, _, _) = menu(&mut desk);
    assert!(
        picked.starts_with("<A-t>") && picked.contains("types <C-b>]"),
        "{picked}"
    );
    type_keys(&mut desk, "d");
    saved(&mut desk, 1);
    assert_eq!(
        desk.message.as_deref(),
        Some("<A-t> wasn't taken out: client/settings.lua doesn't bind <A-t> in those modes")
    );
}

#[test]
fn undoing_keys_recorded_over_saved_ones_binds_those_again() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "mapp n <A-t> <C-b>]");
    saved(&mut desk, 1);
    command(&mut desk, "Settings new tab");
    assert!(menu(&mut desk).0.starts_with("<C-b>c"));
    named(&mut desk, NamedKey::Enter);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    press(&mut desk, Key::Character("t".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!(
            "{HEADER}\nclient.keys.map(\"niv\", \"<A-t>\", \"<C-b>c\", {{ desc = \"new tab\" }})\n"
        )
    );
    // Undone, the keys go, and what settings.lua bound them to before is
    // bound and saved again.
    type_keys(&mut desk, "u");
    saved(&mut desk, 2);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!("{HEADER}\nclient.keys.map(\"n\", \"<A-t>\", \"<C-b>]\")\n")
    );
    let keys = parse_keys("<A-t>").unwrap();
    let client = desk.client().0;
    let bindings = client.keys();
    assert_eq!(
        bindings
            .get(KeyMode::Normal, &keys)
            .map(|(binding, _)| binding.clone()),
        Some(crate::desk::Binding::Keys(parse_keys("<C-b>]").unwrap()))
    );
    assert!(bindings.get(KeyMode::Insert, &keys).is_none());
}

#[test]
fn an_option_read_as_a_window_opens_steps_from_what_is_saved() {
    let (mut desk, dir) = configured("");
    // Nothing to change changes nothing, and leaves nothing to undo.
    command(&mut desk, "Settings priority");
    type_keys(&mut desk, "ru");
    assert_eq!(menu(&mut desk).1, "the menu hasn't changed priority");
    named(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, "/renderer");
    named(&mut desk, NamedKey::Enter);
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert_eq!(desk.options.renderer, crate::options::Renderer::Vulkan);
    assert_eq!(read(&dir, "client/startup.toml"), "renderer = \"opengl\"\n");
    let (picked, _, _) = menu(&mut desk);
    assert!(picked.starts_with("renderer"), "{picked}");
    assert!(
        picked.contains(" opengl ") && picked.ends_with("for windows opened after"),
        "{picked}"
    );
    // And back, from what's saved, not from what this window runs on.
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert_eq!(read(&dir, "client/startup.toml"), "renderer = \"vulkan\"\n");
    let (picked, _, _) = menu(&mut desk);
    assert!(
        picked.contains(" vulkan") && !picked.ends_with("after"),
        "{picked}"
    );
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert_eq!(read(&dir, "client/startup.toml"), "renderer = \"opengl\"\n");
}

#[test]
fn an_agents_keys_save_no_keys_and_leave_nothing_to_undo() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "mapp n <A-u> u");
    saved(&mut desk, 1);
    command(&mut desk, "Settings new tab");
    desk.automating = true;
    named(&mut desk, NamedKey::Enter);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    press(&mut desk, Key::Character("t".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(menu(&mut desk).1, "an agent's input can't save keys");
    desk.automating = false;
    type_keys(&mut desk, "u");
    assert_eq!(menu(&mut desk).1, "the menu hasn't changed niv <C-b>c");
    let keys = parse_keys("<A-t>").unwrap();
    assert!(desk.client().0.keys().get(KeyMode::Normal, &keys).is_none());
    // Nor take any out.
    named(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, "/<A-u>");
    named(&mut desk, NamedKey::Enter);
    desk.automating = true;
    type_keys(&mut desk, "d");
    assert_eq!(menu(&mut desk).1, "an agent's input can't save keys");
    desk.automating = false;
    assert!(read(&dir, "client/settings.lua").contains("<A-u>"));
    type_keys(&mut desk, "u");
    assert_eq!(menu(&mut desk).1, "the menu hasn't changed n <A-u>");
}

#[test]
fn keys_init_lua_binds_since_are_never_undone() {
    let (mut desk, dir) = configured("");
    command(&mut desk, "Settings new tab");
    named(&mut desk, NamedKey::Enter);
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    press(&mut desk, Key::Character("t".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    named(&mut desk, NamedKey::Escape);
    named(&mut desk, NamedKey::Escape);
    std::fs::write(
        dir.path().join("client/init.lua"),
        "client.keys.map('n', '<A-t>', function() end)\n",
    )
    .unwrap();
    command(&mut desk, "reload");
    desk.lua_ready();
    let file = read(&dir, "client/settings.lua");
    command(&mut desk, "Settings new tab");
    type_keys(&mut desk, "u");
    assert_eq!(
        menu(&mut desk).1,
        "client/init.lua:1 binds <A-t> now, so it can't be undone"
    );
    assert_eq!(read(&dir, "client/settings.lua"), file);
    // Kept, to undo once init.lua doesn't bind them.
    std::fs::write(dir.path().join("client/init.lua"), "").unwrap();
    named(&mut desk, NamedKey::Escape);
    named(&mut desk, NamedKey::Escape);
    command(&mut desk, "reload");
    desk.lua_ready();
    command(&mut desk, "Settings new tab");
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert_eq!(read(&dir, "client/settings.lua"), format!("{HEADER}\n"));
}

/// A desk whose config holds `init.lua`, loaded, with the menu used in no
/// mode yet: it opens in Easy.
fn easy_configured(init: &str) -> (Desk, tempfile::TempDir) {
    let (desk, dir) = configured(init);
    std::fs::remove_file(dir.path().join("settings-mode")).unwrap();
    (desk, dir)
}

/// Easy, as it's drawn.
fn easy(desk: &mut Desk) -> crate::settings::view::EasyView {
    desk.easy_view().expect("Easy")
}

/// The row of Easy labelled `label`, by its place among every section's,
/// and the row.
fn easy_row(desk: &mut Desk, label: &str) -> (u16, crate::settings::view::RowView) {
    let view = easy(desk);
    let rows: Vec<_> = view
        .sections
        .into_iter()
        .flat_map(|section| section.rows)
        .collect();
    let at = (rows.iter())
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no row {label}"));
    (u16::try_from(at).unwrap(), rows[at].clone())
}

#[test]
fn easy_is_how_the_menu_opens_first_and_the_mode_used_last_is_remembered() {
    use crate::settings::Mode;
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Easy));
    assert!(
        desk.finder_view((30, 100), None).is_none(),
        "Easy isn't the finder"
    );
    let view = easy(&mut desk);
    assert_eq!(view.title, "Appearance");
    assert!(view.preview.is_some());
    // `m` switches to Advanced, and the next menu opens there.
    type_keys(&mut desk, "m");
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Advanced));
    assert_eq!(read(&dir, "settings-mode"), "advanced\n");
    named(&mut desk, NamedKey::Escape);
    command(&mut desk, "Settings");
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Advanced));
    // And a click on the header's switch, back.
    desk.settings_pointer(crate::settings::view::SettingsHit::Mode(Mode::Easy));
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Easy));
    assert_eq!(read(&dir, "settings-mode"), "easy\n");
}

#[test]
fn skills_are_searchable_and_open_their_source_on_the_workspaces_machine() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    let _ = answer_all(&mut desk);
    desk.settings_skills(Response::Skills(vec![clackatoo_protocol::Skill {
        name: "migrate-db".into(),
        description: "Check a database migration.".into(),
        path: "/project/.agents/skills/migrate-db/SKILL.md".into(),
        source: "Project".into(),
        problem: None,
        implicit: true,
        user_invocable: true,
    }]));
    let page = crate::options::easy::Page::ALL
        .iter()
        .position(|page| *page == crate::options::easy::Page::Skills)
        .unwrap();
    desk.settings_pointer(SettingsHit::Page(u8::try_from(page).unwrap()));
    assert_eq!(easy(&mut desk).title, "Skills");
    let (_, skill) = easy_row(&mut desk, "/migrate-db");
    assert_eq!(skill.control, ControlView::Open);
    assert!(skill.notes.iter().any(|(note, _)| note == "Project"));
    type_keys(&mut desk, "/migration");
    assert!(
        easy(&mut desk)
            .sections
            .iter()
            .any(|section| section.heading == "Skills")
    );
    let (row, _) = easy_row(&mut desk, "/migrate-db");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert!(desk.settings.is_none());
    let asked = answer_all(&mut desk);
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Split {
        terminal: PaneTerminal::File(OpenFile { path, .. }), on: None, ..
    } if path == "/project/.agents/skills/migrate-db/SKILL.md")),
        "{asked:?}"
    );
}

#[test]
fn easy_changes_several_options_as_one_saves_them_and_undoes_them_together() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    let (row, line_numbers) = easy_row(&mut desk, "Line numbers");
    assert_eq!(
        line_numbers.control,
        ControlView::Segmented {
            labels: vec!["Off", "On", "Relative"],
            picked: Some(2),
        }
    );
    // Off, by a click: both options, live and saved.
    desk.settings_pointer(SettingsHit::Control(row, Part::Segment(0)));
    saved(&mut desk, 1);
    assert!(!desk.options.number && !desk.options.relativenumber);
    let file = read(&dir, "client/settings.lua");
    assert!(file.contains("client.opt.number = false\n"), "{file}");
    assert!(
        file.contains("client.opt.relativenumber = false\n"),
        "{file}"
    );
    assert_eq!(
        easy(&mut desk).footer,
        "Saved nonumber norelativenumber in client/settings.lua"
    );
    // `l` steps to On, and `u` undoes it, then Off, as one each.
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert!(desk.options.number && !desk.options.relativenumber);
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(!desk.options.number);
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(desk.options.number && desk.options.relativenumber);
    assert!(!read(&dir, "client/settings.lua").contains("number"));
    // A switch, by the keys: the next row, turned off.
    type_keys(&mut desk, "j");
    let (_, cursorline) = easy_row(&mut desk, "Highlight the cursor's line");
    assert_eq!(cursorline.control, ControlView::Toggle(true));
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert!(!desk.options.cursorline);
    assert_eq!(
        easy_row(&mut desk, "Highlight the cursor's line").1.control,
        ControlView::Toggle(false)
    );
}

#[test]
fn easy_greys_what_init_lua_sets_and_says_what_is_set_since() {
    use crate::settings::view::{ControlView, SettingsHit};
    let (mut desk, dir) = easy_configured("client.opt.wrap = false\n");
    command(&mut desk, "set nocursorline");
    command(&mut desk, "Settings");
    let (row, wrap) = easy_row(&mut desk, "Wrap long lines");
    assert_eq!(wrap.lock.as_deref(), Some("init.lua:1"));
    assert_eq!(wrap.control, ControlView::Toggle(false));
    assert_eq!(
        wrap.help,
        "Your init.lua sets this, and it wins over what's saved here."
    );
    // What's configured shows, and what it is now under it.
    let (_, cursorline) = easy_row(&mut desk, "Highlight the cursor's line");
    assert_eq!(cursorline.control, ControlView::Toggle(true));
    assert_eq!(
        cursorline.notes,
        vec![(
            "Currently off, set since the config loaded.".to_owned(),
            crate::render::Ink::Caution
        )]
    );
    // The locked one doesn't change: its place opens.
    answer_all(&mut desk);
    for _ in 0..row {
        type_keys(&mut desk, "j");
    }
    type_keys(&mut desk, "l");
    assert!(
        easy(&mut desk)
            .footer
            .starts_with("client/init.lua:1 sets it")
    );
    assert!(!dir.path().join("client/settings.lua").exists());
    desk.settings_pointer(SettingsHit::Lock(row));
    assert!(desk.settings.is_none());
    let requests = answer_all(&mut desk);
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::Split {
                terminal: PaneTerminal::File(OpenFile { path, .. }),
                ..
            } if path.ends_with("client/init.lua")
        )),
        "{requests:#?}"
    );
}

#[test]
fn a_search_finds_what_easy_has_then_what_only_advanced_has_and_goes_there() {
    use crate::settings::Mode;
    use crate::settings::view::ControlView;
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    type_keys(&mut desk, "/numberwidth");
    let view = easy(&mut desk);
    assert_eq!(view.title, "Search");
    let headings: Vec<&str> = (view.sections.iter())
        .map(|section| section.heading.as_str())
        .collect();
    assert!(headings.contains(&"Only in Advanced"), "{headings:?}");
    let found = &view.sections.last().unwrap().rows[0];
    assert_eq!(found.label, "numberwidth");
    assert_eq!(found.control, ControlView::Advanced);
    // Enter stops typing; on the row found only in Advanced, it goes
    // there, with it picked.
    named(&mut desk, NamedKey::Enter);
    let rows = view
        .sections
        .iter()
        .map(|section| section.rows.len())
        .sum::<usize>();
    for _ in 1..rows {
        type_keys(&mut desk, "j");
    }
    named(&mut desk, NamedKey::Enter);
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Advanced));
    let (picked, ..) = menu(&mut desk);
    assert!(picked.starts_with("numberwidth"), "{picked}");
    // What's typed is the same search in both.
    type_keys(&mut desk, "m");
    assert_eq!(easy(&mut desk).query, "numberwidth");
}

#[test]
fn easy_s_slider_shows_where_it_is_dragged_and_sets_the_size_let_go() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    let (row, size) = easy_row(&mut desk, "Size");
    assert!(matches!(size.control, ControlView::Slider { ref value, .. } if value == "12 pt"));
    // Half way along 8 to 24 is 16; nothing's set until it's let go.
    desk.settings_pointer(SettingsHit::Control(row, Part::Slider(500)));
    assert!(desk.settings_sliding());
    desk.settings_slide(750);
    let view = easy(&mut desk);
    assert!(
        view.preview.as_ref().unwrap().scale > 1.6,
        "drawn at 20 of 12"
    );
    assert!(
        matches!(easy_row(&mut desk, "Size").1.control, ControlView::Slider { ref value, .. } if value == "20 pt")
    );
    assert_eq!(desk.options.fontsize.to_string(), "12");
    desk.settings_slid();
    saved(&mut desk, 1);
    assert_eq!(desk.options.fontsize.to_string(), "20");
    assert!(read(&dir, "client/startup.toml").contains("font-size = 20"));
    // And `h` steps it back half a point.
    let (_, _) = easy_row(&mut desk, "Size");
    type_keys(&mut desk, "h");
    saved(&mut desk, 1);
    assert_eq!(desk.options.fontsize.to_string(), "19.5");
}

#[test]
fn easy_pages_go_by_tab_braces_and_clicks_and_the_keys_page_records_keys() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    type_keys(&mut desk, "}");
    assert_eq!(easy(&mut desk).title, "Editor");
    type_keys(&mut desk, "{{");
    assert_eq!(easy(&mut desk).title, "Permissions");
    type_keys(&mut desk, "{");
    assert_eq!(easy(&mut desk).title, "MCP Servers");
    desk.settings_pointer(SettingsHit::Page(3));
    let view = easy(&mut desk);
    assert_eq!(view.title, "Keys");
    assert!(!view.on_side, "a click shows no focus");
    // Into the page, then recording keys for a new tab.
    type_keys(&mut desk, "l");
    let (row, new_tab) = easy_row(&mut desk, "New tab");
    assert_eq!(
        new_tab.control,
        ControlView::Keys {
            keys: vec!["<C-b>c".into()],
            recording: None
        }
    );
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let alt = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    press(&mut desk, Key::Character("t".into()), None, alt);
    named(&mut desk, NamedKey::Enter);
    saved(&mut desk, 1);
    assert!(read(&dir, "client/settings.lua").contains("\"<A-t>\""));
    let (_, new_tab) = easy_row(&mut desk, "New tab");
    assert!(
        matches!(&new_tab.control, ControlView::Keys { keys, .. } if keys.contains(&"<A-t>".to_owned())),
        "{new_tab:?}"
    );
    // Undone from Easy, as Advanced would.
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(!read(&dir, "client/settings.lua").contains("<A-t>"));
}

#[test]
fn typing_a_search_never_changes_a_setting_and_tab_leaves_the_box() {
    for advanced in [false, true] {
        let (mut desk, dir) = easy_configured("");
        command(&mut desk, "Settings");
        if advanced {
            type_keys(&mut desk, "m");
        }
        type_keys(&mut desk, "/wrap");
        named(&mut desk, NamedKey::ArrowRight);
        named(&mut desk, NamedKey::ArrowLeft);
        assert!(desk.options.wrap && desk.options.cursorline && desk.options.number);
        assert!(!dir.path().join("client/settings.lua").exists());
        named(&mut desk, NamedKey::Tab);
        assert!(
            !desk.settings.as_ref().unwrap().typing(),
            "Tab leaves the box"
        );
        // Letters act again.
        type_keys(&mut desk, "q");
        assert!(desk.settings.is_none());
    }
}

#[test]
fn a_change_easy_made_to_two_options_is_undone_from_either_mode() {
    use crate::settings::view::{Part, SettingsHit};
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    let (row, _) = easy_row(&mut desk, "Line numbers");
    desk.settings_pointer(SettingsHit::Control(row, Part::Segment(0)));
    saved(&mut desk, 1);
    assert!(!desk.options.number && !desk.options.relativenumber);
    // From Advanced's row of one of them: both come back, in one write.
    type_keys(&mut desk, "m/relativenumber");
    named(&mut desk, NamedKey::Enter);
    assert!(menu(&mut desk).0.starts_with("relativenumber"));
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(desk.options.number && desk.options.relativenumber);
    assert!(!read(&dir, "client/settings.lua").contains("number"));
    // And the other way: Advanced's change, undone from Easy's row.
    named(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, "/relativenumber");
    named(&mut desk, NamedKey::Enter);
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert!(!desk.options.relativenumber);
    named(&mut desk, NamedKey::Escape);
    type_keys(&mut desk, "m");
    desk.settings_pointer(SettingsHit::Row(row));
    type_keys(&mut desk, "u");
    saved(&mut desk, 1);
    assert!(desk.options.relativenumber);
}

#[test]
fn easy_steps_the_size_from_its_value_past_the_sliders_end() {
    use crate::settings::view::SettingsHit;
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "setp fontsize=30");
    saved(&mut desk, 1);
    command(&mut desk, "Settings");
    let (row, _) = easy_row(&mut desk, "Size");
    desk.settings_pointer(SettingsHit::Row(row));
    type_keys(&mut desk, "l");
    saved(&mut desk, 1);
    assert_eq!(desk.options.fontsize.to_string(), "30.5");
    type_keys(&mut desk, "hh");
    saved(&mut desk, 2);
    assert_eq!(desk.options.fontsize.to_string(), "29.5");
}

#[test]
fn a_slider_dragged_stays_its_row_whatever_the_keys_do() {
    use crate::settings::view::{Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    let (row, _) = easy_row(&mut desk, "Size");
    desk.settings_pointer(SettingsHit::Control(row, Part::Slider(500)));
    type_keys(&mut desk, "jj");
    desk.settings_slide(1000);
    desk.settings_slid();
    saved(&mut desk, 1);
    assert_eq!(desk.options.fontsize.to_string(), "24");
    assert!(
        desk.options.number && desk.options.relativenumber,
        "nothing else"
    );
}

#[test]
fn choices_alike_light_together_and_others_step_from_the_nearest() {
    use crate::settings::view::{ControlView, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "setp noignorecase");
    command(&mut desk, "setp norelativenumber");
    command(&mut desk, "setp nonumber");
    command(&mut desk, "setp relativenumber");
    saved(&mut desk, 4);
    command(&mut desk, "Settings");
    // Without ignorecase, smartcase does nothing: it's Match.
    type_keys(&mut desk, "}");
    let (_, case) = easy_row(&mut desk, "Letter case");
    assert!(
        matches!(
            case.control,
            ControlView::Segmented {
                picked: Some(2),
                ..
            }
        ),
        "{case:?}"
    );
    // nonumber relativenumber is no choice, but nearest Relative.
    type_keys(&mut desk, "{");
    let (row, numbers) = easy_row(&mut desk, "Line numbers");
    assert!(
        matches!(numbers.control, ControlView::Segmented { picked: None, .. }),
        "{numbers:?}"
    );
    desk.settings_pointer(SettingsHit::Row(row));
    type_keys(&mut desk, "h");
    saved(&mut desk, 1);
    assert!(desk.options.number && !desk.options.relativenumber, "On");
}

#[test]
fn o_on_a_row_half_locked_opens_the_lock() {
    use crate::settings::view::SettingsHit;
    let (mut desk, _dir) = easy_configured("client.opt.rnu = true\n");
    command(&mut desk, "Settings");
    let (row, numbers) = easy_row(&mut desk, "Line numbers");
    assert_eq!(numbers.lock.as_deref(), Some("init.lua:1"));
    answer_all(&mut desk);
    desk.settings_pointer(SettingsHit::Row(row));
    type_keys(&mut desk, "o");
    assert!(desk.settings.is_none());
    let requests = answer_all(&mut desk);
    assert!(
        requests.iter().any(|request| matches!(
            request,
            Request::Split {
                terminal: PaneTerminal::File(OpenFile { path, .. }),
                ..
            } if path.ends_with("client/init.lua")
        )),
        "{requests:#?}"
    );
}

#[test]
fn a_jump_to_advanced_from_a_search_isnt_remembered_and_a_click_off_it_closes_it() {
    use crate::settings::Mode;
    use crate::settings::view::SettingsHit;
    let (mut desk, dir) = easy_configured("");
    command(&mut desk, "Settings");
    type_keys(&mut desk, "/numberwidth");
    named(&mut desk, NamedKey::Enter);
    let found = easy(&mut desk)
        .sections
        .iter()
        .map(|section| section.rows.len())
        .sum::<usize>();
    for _ in 1..found {
        type_keys(&mut desk, "j");
    }
    named(&mut desk, NamedKey::Enter);
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Advanced));
    assert!(!dir.path().join("settings-mode").exists());
    desk.settings_pointer(SettingsHit::Backdrop);
    assert!(desk.settings.is_none());
    command(&mut desk, "Settings");
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Easy));
}

#[test]
fn what_easy_shows_is_the_same_until_something_changes_it() {
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    let key = desk.settings_shown_key();
    let _ = easy(&mut desk);
    let _ = desk.finder_view((30, 100), None);
    assert_eq!(desk.settings_shown_key(), key, "looking changes nothing");
    type_keys(&mut desk, "j");
    assert_ne!(desk.settings_shown_key(), key);
    let key = desk.settings_shown_key();
    command(&mut desk, "set nowrap");
    assert_ne!(desk.settings_shown_key(), key, "an option set elsewhere");
}

#[test]
fn a_save_that_fails_puts_back_what_easy_counted_on() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, dir) = easy_configured("");
    // A line clackatoo didn't write, which it won't save over.
    std::fs::write(dir.path().join("client/settings.lua"), "local x = 1\n").unwrap();
    command(&mut desk, "Settings");
    let (row, _) = easy_row(&mut desk, "Highlight the cursor's line");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    // Counted on at once...
    let (_, cursorline) = easy_row(&mut desk, "Highlight the cursor's line");
    assert_eq!(cursorline.control, ControlView::Toggle(false));
    // ...and put back when the save fails.
    saved(&mut desk, 1);
    let view = easy(&mut desk);
    assert!(view.warning, "{}", view.footer);
    let (_, cursorline) = easy_row(&mut desk, "Highlight the cursor's line");
    assert_eq!(cursorline.control, ControlView::Toggle(true));
}

#[test]
fn the_cog_opens_the_menu_in_the_mode_used_last_and_closes_it() {
    use crate::settings::Mode;
    let (mut desk, _dir) = configured("");
    desk.toggle_settings();
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Advanced));
    desk.toggle_settings();
    assert!(desk.settings.is_none());
    let (mut desk, _dir) = easy_configured("");
    desk.toggle_settings();
    assert_eq!(desk.settings_mode_shown(), Some(Mode::Easy));
}

/// The daemon's settings as a daemon on `machine` says them: each option
/// at its default, but `subagents.working`, which its init.lua sets at
/// line 2, and `images.total_mb`, saved in its settings.lua, which it
/// goes by once it restarts.
fn daemon_said(machine: &str) -> clackatoo_protocol::DaemonSettings {
    use clackatoo_daemon::settings::options;
    use clackatoo_protocol::{DaemonOption, OptionPlace, OptionSetBy, OptionSource};
    let dir = "/home/u/.config/clackatoo";
    let by = |source, file: &str, line| OptionSetBy {
        source,
        at: Some(OptionPlace {
            file: format!("{dir}/daemon/{file}"),
            line,
        }),
        inner: None,
    };
    let options = (options::ALL.iter())
        .map(|info| {
            let mut option = DaemonOption {
                name: info.name.to_owned(),
                value: options::default_of(info).text(),
                running: None,
                restart: info.restart,
                by: OptionSetBy {
                    source: OptionSource::Default,
                    at: None,
                    inner: None,
                },
            };
            match info.name {
                "subagents.working" => {
                    option.value = "8".into();
                    option.by = by(OptionSource::Config, "init.lua", 2);
                }
                "images.total_mb" => {
                    option.value = "512".into();
                    option.running = Some("256".into());
                    option.by = by(OptionSource::Settings, "settings.lua", 3);
                }
                _ => {}
            }
            option
        })
        .collect();
    clackatoo_protocol::DaemonSettings {
        machine: machine.into(),
        dir: dir.into(),
        options,
        failure: None,
        permission_history: Vec::new(),
    }
}

/// The config changed on disk is run again beside the one the windows go
/// by, for its plain options only: a keypress handler or key it adds isn't
/// taken, nor a command run, nor `osc52` loosened; and the bar says
/// :reload does the rest (CLACKA-237 review).
#[test]
fn a_config_changed_on_disk_takes_its_plain_options_and_nothing_else() {
    use crate::lua::FromLua;
    let (mut desk, dir) = configured("client.opt.wrap = false\n");
    std::fs::write(
        dir.path().join("client/init.lua"),
        "client.opt.wrap = true\nclient.opt.osc52 = \"copy-paste\"\n\
         client.on(\"keypress\", function() end)\n\
         client.keys.map(\"n\", \"<A-q>\", function() end)\nclient.cmd(\"bg make\")\n",
    )
    .unwrap();
    let (client, window) = desk.client();
    let osc52 = crate::options::find("osc52").unwrap();
    let before = client.options().borrow().get(osc52);
    assert!(client.run_trial());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut said = Vec::new();
    while !said.iter().any(|(_, message)| {
        matches!(message, FromLua::Error(text) if text.contains(":reload turns on what's held"))
    }) {
        assert!(std::time::Instant::now() < deadline, "{said:?}");
        std::thread::sleep(std::time::Duration::from_millis(10));
        said.extend(client.take(window));
    }
    // What :reload would turn on is said first (CLACKA-237 review, M2).
    let held = (said.iter())
        .find_map(|(_, message)| match message {
            FromLua::Error(text) if text.contains("what's held") => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        held.ends_with("what's held: osc52=copy-paste, client.cmd(bg make), 1 key map, 1 handler"),
        "{held}"
    );
    // Nothing but what's said reaches the window.
    assert!(
        said.iter()
            .all(|(_, message)| matches!(message, FromLua::Error(_))),
        "{said:?}"
    );
    let options = client.options();
    assert_eq!(
        options.borrow().get(crate::options::find("wrap").unwrap()),
        crate::options::Value::Bool(true)
    );
    assert_eq!(options.borrow().get(osc52), before);
    let alt_q = crate::keymap::parse_keys("<A-q>").unwrap();
    assert!(
        !(client.keys().bound(KeyMode::Normal)).any(|(keys, _, _)| *keys == alt_q),
        "a key the trial bound"
    );
    assert!(client.keypress_handlers(window, None).is_empty());
}

/// What the daemon's config changed outside clackatoo, of what runs
/// programs, is said in the bar, what it was and what it is, cleaned of
/// what wouldn't show; nothing waits on it (CLACKA-237).
#[test]
fn a_change_outside_clackatoo_is_said() {
    let (mut desk, _dir) = configured("");
    assert!(!desk.config_changed_outside(&[]), "nothing to say");
    let outside = [
        clackatoo_protocol::ConfigChange {
            name: "agents.claude command".into(),
            before: "{ \"claude\" }".into(),
            after: "{ \"other\" }\u{1b}[31m".into(),
        },
        clackatoo_protocol::ConfigChange {
            name: "agents.mine".into(),
            before: String::new(),
            after: "{ command = { \"mine\" } }".into(),
        },
    ];
    assert!(desk.config_changed_outside(&outside));
    let said = desk.message.clone().unwrap_or_default();
    assert!(
        said.starts_with(
            "config changed outside clackatoo: agents.claude command: { \"claude\" } → \
             { \"other\" }"
        ),
        "{said}"
    );
    assert!(said.contains("agents.mine: none → { command"), "{said}");
    assert!(!said.contains('\u{1b}'), "{said:?}");
}

/// An agent's input, or a chat's with Control, never changes the daemon's
/// settings, nor a window's that runs programs, in the menu or with
/// `:set`; the window's others it may (CLACKA-237 review, H2).
#[test]
fn an_agents_input_changes_no_setting_that_runs_programs() {
    use crate::settings::view::{Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    desk.daemon_settings_came(&daemon_said("desk"), None);
    desk.settings_pointer(SettingsHit::Page(2));
    let (row, _) = easy_row(&mut desk, "Keep what terminals showed");
    for (automating, controlled) in [(true, false), (false, true)] {
        (desk.automating, desk.controlled) = (automating, controlled);
        desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
        assert_eq!(answer_all(&mut desk), [], "the daemon's isn't saved");
    }
    named(&mut desk, NamedKey::Escape);
    let osc52 = crate::options::find("osc52").unwrap();
    let wrap = crate::options::find("wrap").unwrap();
    let (before, wrapped) = (desk.options.get(osc52), desk.options.get(wrap));
    command(&mut desk, "set osc52=copy-paste");
    command(&mut desk, "setp osc52=copy-paste");
    assert_eq!(desk.options.get(osc52), before, "osc52 is the user's");
    command(&mut desk, "set wrap!");
    assert_ne!(desk.options.get(wrap), wrapped, "wrap is anyone's");
    // The user may.
    (desk.automating, desk.controlled) = (false, false);
    command(&mut desk, "set osc52=copy-paste");
    assert_ne!(desk.options.get(osc52), before);
}

/// Easy shows the daemon's rows once the daemon has said its settings,
/// and a change goes to the daemon to save, shown at once; what its
/// init.lua sets is locked; one it reads only as it starts says it waits
/// for that; and a remote window says whose they are (CLACKA-237).
#[test]
fn easy_and_advanced_show_the_daemons_settings_and_the_daemon_saves_them() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    command(&mut desk, "Settings");
    let asked = answer_all(&mut desk);
    assert!(asked.contains(&Request::DaemonSettings), "{asked:#?}");
    desk.settings_pointer(SettingsHit::Page(2));
    assert!(
        !(easy(&mut desk).sections.iter()).any(|section| section.heading == "After a restart"),
        "the daemon hasn't said them yet"
    );
    desk.daemon_settings_came(&daemon_said("desk"), None);
    let (row, keep) = easy_row(&mut desk, "Keep what terminals showed");
    assert_eq!(keep.control, ControlView::Toggle(true));
    assert_eq!(
        keep.notes,
        vec![(
            "Takes effect at the daemon's next upgrade or restart.".to_owned(),
            crate::render::Ink::Muted
        )]
    );
    assert_eq!(easy(&mut desk).machine, None);
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let asked = answer_all(&mut desk);
    assert_eq!(
        asked,
        [Request::SetDaemonOption {
            name: "history.keep".into(),
            value: Some("false".into()),
        }]
    );
    // Counted on at once, waiting for a restart.
    let (_, keep) = easy_row(&mut desk, "Keep what terminals showed");
    assert_eq!(keep.control, ControlView::Toggle(false));
    assert_eq!(
        keep.notes,
        vec![(
            "Takes effect at the daemon's next upgrade or restart.".to_owned(),
            crate::render::Ink::Caution
        )]
    );
    // The daemon says it couldn't: as it was, and why.
    desk.daemon_settings_came(
        &daemon_said("desk"),
        Some(clackatoo_protocol::OptionSaved {
            name: "history.keep".into(),
            value: Some("false".into()),
            refused: Some("daemon/settings.lua's line 4 isn't one clackatoo wrote".into()),
        }),
    );
    let view = easy(&mut desk);
    assert!(
        view.warning && view.footer.contains("line 4"),
        "{}",
        view.footer
    );
    // Another window's change, which this one didn't ask for, isn't said.
    desk.daemon_settings_came(
        &daemon_said("desk"),
        Some(clackatoo_protocol::OptionSaved {
            name: "history.lines".into(),
            value: Some("1000".into()),
            refused: None,
        }),
    );
    assert!(easy(&mut desk).footer.contains("line 4"));
    let (_, keep) = easy_row(&mut desk, "Keep what terminals showed");
    assert_eq!(keep.control, ControlView::Toggle(true));

    // What its init.lua sets is locked, at its place.
    desk.settings_pointer(SettingsHit::Page(5));
    let (_, working) = easy_row(&mut desk, "Working at once");
    assert_eq!(working.lock.as_deref(), Some("init.lua:2"));
    assert_eq!(working.control, ControlView::Stepper("8".into()));

    // Advanced lists them all, after the keys, saying where each is kept.
    type_keys(&mut desk, "m");
    type_keys(&mut desk, "/images.total_mb");
    named(&mut desk, NamedKey::Enter);
    let (picked, _, preview) = menu(&mut desk);
    assert!(picked.starts_with("images.total_mb"), "{picked}");
    assert!(picked.ends_with("restart"), "{picked}");
    assert!(
        preview
            .iter()
            .any(|line| line == "Saved in  daemon/settings.lua"),
        "{preview:#?}"
    );
    assert!(
        (preview.iter()).any(|line| line.contains("daemon's next upgrade or restart")),
        "{preview:#?}"
    );

    // A remote window says whose they are.
    desk.remote = true;
    desk.daemon_settings_came(&daemon_said("desk"), None);
    let view = desk.finder_view((30, 100), None).expect("the menu");
    assert_eq!(view.place.as_deref(), Some("daemon on desk"));
}

#[test]
fn specialist_settings_creates_multiline_drafts_saves_explicitly_and_keeps_failed_edits() {
    use crate::settings::view::{ControlView, Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    desk.open_specialist_settings();
    answer_all(&mut desk);
    desk.settings_specialists(Response::Specialists(vec![]));
    assert_eq!(easy(&mut desk).title, "Specialists");
    let (row, _) = easy_row(&mut desk, "Create specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let (row, _) = easy_row(&mut desk, "Name");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    desk.paste_into_settings("reviewer");
    named(&mut desk, NamedKey::Enter);
    let (row, _) = easy_row(&mut desk, "Instructions");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    desk.paste_into_settings("Review code.\nReport concrete bugs.");
    named(&mut desk, NamedKey::ArrowLeft);
    desk.paste_into_settings("!");
    named(&mut desk, NamedKey::Enter);
    assert!(
        answer_all(&mut desk).is_empty(),
        "editing a draft never saves it"
    );
    let (row, _) = easy_row(&mut desk, "Save specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let requests = answer_all(&mut desk);
    let [Request::SaveSpecialistFile { specialist, .. }] = &requests[..] else {
        panic!("{requests:?}")
    };
    assert_eq!(specialist.name, "reviewer");
    let value: serde_json::Value = serde_json::from_str(&specialist.settings).unwrap();
    assert_eq!(
        value["instructions"],
        "Review code.\nReport concrete bugs!."
    );
    let expected = specialist.clone();
    desk.settings_specialist_saved(
        &expected,
        Response::Error(clackatoo_protocol::Error::Other("disk full".into())),
    );
    let view = easy(&mut desk);
    assert!(view.warning && view.footer.contains("disk full"));
    let (_, instructions) = easy_row(&mut desk, "Instructions");
    assert!(
        matches!(instructions.control,ControlView::Paragraph {text,..} if text.contains("bugs!"))
    );
    let (row, _) = easy_row(&mut desk, "Save specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert!(matches!(
        &answer_all(&mut desk)[..],
        [Request::SaveSpecialistFile { .. }]
    ));
    desk.settings_specialist_saved(&expected, Response::SpecialistSaved);
    assert!(easy(&mut desk).footer.contains("saved"));
    easy_row(&mut desk, "Create specialist");
}

#[test]
fn specialist_settings_prevents_agent_saves_and_ignores_an_old_menus_save_reply() {
    use crate::settings::view::{Part, SettingsHit};
    let (mut desk, _dir) = easy_configured("");
    desk.open_specialist_settings();
    answer_all(&mut desk);
    desk.settings_specialists(Response::Specialists(vec![
        clackatoo_protocol::SpecialistDefinition {
            name: "reviewer".into(),
            settings: r#"{"instructions":"Review","limits":{"access":"read-only"}}"#.into(),
            saved: None,
            locked: None,
        },
    ]));
    let (row, _) = easy_row(&mut desk, "reviewer");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    desk.controlled = true;
    let (row, _) = easy_row(&mut desk, "Save specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert!(answer_all(&mut desk).is_empty());
    assert!(easy(&mut desk).footer.contains("yours"));
    desk.controlled = false;
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let requests = answer_all(&mut desk);
    let [Request::SaveSpecialistFile { specialist, .. }] = &requests[..] else {
        panic!("{requests:?}")
    };
    desk.open_specialist_settings();
    answer_all(&mut desk);
    desk.settings_specialists(Response::Specialists(vec![]));
    desk.settings_pointer(SettingsHit::Control(0, Part::Whole));
    desk.settings_specialist_saved(specialist, Response::SpecialistSaved);
    easy_row(&mut desk, "Instructions"); // previous menu's reply cannot discard this draft
}

#[test]
fn file_specialist_settings_copies_builtins_and_keeps_scope_source_and_dependencies() {
    use crate::settings::view::{Part, SettingsHit};
    use clackatoo_protocol::{SpecialistDefinition, SpecialistFile, SpecialistScope};
    let (mut desk, _dir) = easy_configured("");
    desk.open_specialist_settings();
    answer_all(&mut desk);
    let definition = SpecialistDefinition {
        name: "explore".into(),
        settings: r#"{"instructions":"Explore carefully","includes":["checks"]}"#.into(),
        saved: None,
        locked: None,
    };
    desk.settings_specialists(Response::SpecialistFiles(vec![SpecialistFile {
        definition: definition.clone(),
        path: String::new(),
        scope: SpecialistScope::Personal,
        editable: false,
    }]));
    let (row, view) = easy_row(&mut desk, "explore");
    assert_eq!(
        view.control,
        crate::settings::view::ControlView::Button("Make a custom copy")
    );
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let (scope, _) = easy_row(&mut desk, "Scope");
    desk.settings_pointer(SettingsHit::Control(scope, Part::Segment(1)));
    // Clicking the selected segment keeps the selection, rather than toggling it.
    desk.settings_pointer(SettingsHit::Control(scope, Part::Segment(1)));
    let (row, _) = easy_row(&mut desk, "Save specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let requests = answer_all(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist,
            scope: SpecialistScope::Project,
            path: None,
            ..
        },
    ] = &requests[..]
    else {
        panic!("{requests:?}")
    };
    assert_eq!(specialist.name, "explore");
    assert_eq!(specialist.saved, None);
    assert!(
        serde_json::from_str::<serde_json::Value>(&specialist.settings).unwrap()["includes"]
            .as_array()
            .is_some()
    );
    desk.open_specialist_settings();
    answer_all(&mut desk);
    let mut local = definition;
    local.saved = Some("original file".into());
    desk.settings_specialists(Response::SpecialistFiles(vec![SpecialistFile {
        definition: local,
        path: "/repo/.agents/specialists/explore/SPECIALIST.md".into(),
        scope: SpecialistScope::Project,
        editable: true,
    }]));
    let (row, _) = easy_row(&mut desk, "explore");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let (row, _) = easy_row(&mut desk, "Save specialist");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    let requests = answer_all(&mut desk);
    assert!(
        matches!(&requests[..],[Request::SaveSpecialistFile {path:Some(path),specialist,..}] if path.ends_with("SPECIALIST.md") && specialist.saved.as_deref()==Some("original file"))
    );
}
