//! Options saved with `:setpersist`, where they're kept: the window's in
//! `client/settings.lua`, which a reload reads back, and startup.toml's
//! in `client/startup.toml`; refused while the user's own `init.lua`
//! sets them; and what set each option, as `:verbose set` says it, and
//! each binding.

use super::*;
use crate::keymap::{KeyMode, parse_keys};
use crate::lua::Source;
use crate::options::{Decimal, Renderer, SetBy};

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

#[test]
fn setpersist_saves_in_settings_lua_which_a_reload_reads_back() {
    let (mut desk, dir) = configured("client.opt.rnu = false\n");
    command(&mut desk, "setp nonu cc=80,120 sbr=↪\\ +");
    saved(&mut desk, 3);
    assert_eq!(
        desk.message.as_deref(),
        Some("saved showbreak=↪\\ + in client/settings.lua")
    );
    assert!(!desk.options.number);
    let header = crate::options::settings_lua::HEADER;
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!(
            "{header}\nclient.opt.number = false\nclient.opt.colorcolumn = {{ 80, 120 }}\n\
             client.opt.showbreak = \"↪ +\"\n"
        )
    );
    // `&` puts it back as it starts, and out of the file.
    command(&mut desk, "setp nu&");
    saved(&mut desk, 1);
    assert!(desk.options.number);
    assert_eq!(
        desk.message.as_deref(),
        Some("number is saved in client/settings.lua no more")
    );
    command(&mut desk, "set cc= sbr=");
    command(&mut desk, "reload");
    desk.lua_ready();
    assert_eq!(desk.options.colorcolumn, [80, 120]);
    assert_eq!(desk.options.showbreak, "↪ +");
    assert!(desk.options.number && !desk.options.relativenumber);
    command(&mut desk, "verbose set cc? rnu?");
    assert_eq!(
        desk.message.as_deref(),
        Some(
            "colorcolumn=80,120 · saved in client/settings.lua:4  \
             norelativenumber · set in client/init.lua:1"
        )
    );
}

#[test]
fn setpersist_is_for_now_only_while_init_lua_sets_the_option() {
    let (mut desk, dir) = configured("client.opt.nuw = 6\nclient.cmd('set nowrap')\n");
    command(&mut desk, "set nuw=7");
    command(&mut desk, "setp nuw=8");
    assert_eq!(desk.options.numberwidth, 8);
    assert_eq!(
        desk.message.as_deref(),
        Some("client/init.lua:1 sets numberwidth, so it's set for now and not saved")
    );
    // A `:set` the config runs is the config's.
    command(&mut desk, "setp wrap");
    assert!(desk.options.wrap);
    assert_eq!(
        desk.message.as_deref(),
        Some("client/init.lua:2 sets wrap, so it's set for now and not saved")
    );
    command(&mut desk, "setp nocul");
    saved(&mut desk, 1);
    assert_eq!(
        read(&dir, "client/settings.lua"),
        format!(
            "{}\nclient.opt.cursorline = false\n",
            crate::options::settings_lua::HEADER
        )
    );
}

#[test]
fn startup_options_are_saved_in_startup_toml() {
    let (mut desk, dir) = configured("");
    std::fs::write(
        dir.path().join("client/startup.toml"),
        "# Mine.\nfont = \"Iosevka\"\n",
    )
    .unwrap();
    command(&mut desk, "setp fsz=14");
    saved(&mut desk, 1);
    assert_eq!(desk.options.fontsize, Decimal::whole(14));
    assert_eq!(
        desk.message.as_deref(),
        Some("saved fontsize=14 in client/startup.toml, for windows opened after too")
    );
    // Read only as a window opens: saved for the next, never set now.
    command(&mut desk, "set renderer=opengl");
    assert!(
        (desk.message.as_deref())
            .is_some_and(|said| said.starts_with("renderer is read as a window opens")),
        "{:?}",
        desk.message
    );
    command(&mut desk, "setp renderer=opengl");
    saved(&mut desk, 1);
    assert_eq!(desk.options.renderer, Renderer::Vulkan);
    assert_eq!(
        desk.message.as_deref(),
        Some("saved renderer=opengl in client/startup.toml, for windows opened after")
    );
    assert_eq!(
        read(&dir, "client/startup.toml"),
        "# Mine.\nfont = \"Iosevka\"\nfont-size = 14\nrenderer = \"opengl\"\n"
    );
    assert!(!dir.path().join("client/settings.lua").exists());
}

#[test]
fn verbose_set_says_what_set_each_option_and_bindings_know_theirs() {
    let (mut desk, _dir) = configured(
        "client.keys.map('n', '<C-b>t', function() end)\n\
         client.keys.map('n', 'q', function() client.opt.ts = 8 end)\n",
    );
    command(&mut desk, "verbose set wrap?");
    assert_eq!(desk.message.as_deref(), Some("wrap · as it starts"));
    command(&mut desk, "set nowrap");
    command(&mut desk, "verbose set wrap?");
    assert_eq!(
        desk.message.as_deref(),
        Some("nowrap · set with :set or a key")
    );
    // As the wrap lesson has it: back as it starts, nothing set it.
    command(&mut desk, "set wrap&");
    command(&mut desk, "verbose set wrap?");
    assert_eq!(desk.message.as_deref(), Some("wrap · as it starts"));
    press(
        &mut desk,
        Key::Character("q".into()),
        None,
        Modifiers::default(),
    );
    let ts = crate::options::find("ts").unwrap();
    let by = desk.options.set_by(ts);
    assert_eq!((by.loading, by.at.map(|at| at.line)), (false, Some(2)));
    let keys = |keys| parse_keys(keys).unwrap();
    let bound = desk
        .client()
        .0
        .keys()
        .bound_by(KeyMode::Normal, &keys("<C-b>t"));
    assert!(bound.loading && bound.at.is_some_and(|at| at.line == 1));
    assert_eq!(
        desk.client()
            .0
            .keys()
            .bound_by(KeyMode::Normal, &keys("<C-b>c")),
        SetBy::DEFAULT
    );
    assert_eq!(
        desk.client()
            .0
            .keys()
            .bound_by(KeyMode::Insert, &keys("<C-b>t")),
        SetBy::DEFAULT
    );
}
