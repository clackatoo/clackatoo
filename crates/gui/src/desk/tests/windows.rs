//! Windows of one process, sharing one Lua (see [`crate::client`]): the
//! config runs once, a window opened later goes by what it bound and set,
//! `:set` and `:setp` change every window, `:setlocal` only its own, and
//! a daemon event is heard once, however many windows hear it.

use std::time::{Duration, Instant};

use super::*;
use crate::client::Client;
use crate::lua::Source;

/// A config directory whose `client/init.lua` is `init`.
fn config(init: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("client")).unwrap();
    std::fs::write(dir.path().join("client/init.lua"), init).unwrap();
    dir
}

/// A window of `client`, showing three tabs.
fn window(client: &Client) -> Desk {
    let mut desk = three_tabs();
    snapshot(&mut desk, 1);
    desk.join(client.clone());
    normal_mode(&mut desk);
    desk
}

/// A window of a new client running the config in `dir`, loaded.
fn first_window(dir: &tempfile::TempDir) -> (Client, Desk) {
    let client = Client::default();
    let mut desk = window(&client);
    client.start(Source::Dir(dir.path().into()), Box::new(|| Box::new(|| {})));
    desk.lua_ready();
    (client, desk)
}

/// Lets each of `desks` do what Lua says until `done` says it's done,
/// for a few seconds at most.
fn until(desks: &mut [&mut Desk], done: impl Fn(&[&mut Desk]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(desks) {
        assert!(Instant::now() < deadline, "it never happened");
        std::thread::sleep(Duration::from_millis(2));
        for desk in desks.iter_mut() {
            desk.lua_said();
        }
    }
}

#[test]
fn the_config_runs_once_for_every_window_and_one_opened_later_joins_it() {
    // Each run adds one: run once, it's 5.
    let dir = config(
        r#"
        client.opt.tabstop = client.opt.tabstop + 1
        client.keys.map("n", "<C-b>t", function(ev) ev.pane:close() end)
        client.on("keypress", function(ev)
            if ev.key == "-" then ev.pane:close() ev:stop() end
        end)
        "#,
    );
    let (client, mut one) = first_window(&dir);
    assert_eq!(one.options.tabstop, 5);
    // Opened later, it has what the config bound and set, which didn't
    // run again.
    let mut two = window(&client);
    two.lua_said();
    assert_eq!(two.options.tabstop, 5);
    assert_eq!(one.options.tabstop, 5);
    answer_all(&mut one);
    answer_all(&mut two);
    ctrl_b(&mut two, "t");
    assert!(
        matches!(
            answer_all(&mut two)[..],
            [Request::ClosePane { pane: PaneId(1) }]
        ),
        "the binding runs in the window whose key it was"
    );
    one.lua_said();
    assert!(answer_all(&mut one).is_empty(), "and not in the other");
    // And its keypress handler hears its keys too.
    type_keys(&mut two, "-");
    assert!(matches!(
        answer_all(&mut two)[..],
        [Request::ClosePane { pane: PaneId(1) }]
    ));
    // A reload runs it once more, for both.
    command(&mut one, "reload");
    one.lua_ready();
    two.lua_said();
    assert_eq!((one.options.tabstop, two.options.tabstop), (5, 5));
    ctrl_b(&mut one, "t");
    assert!(matches!(
        answer_all(&mut one)[..],
        [Request::ClosePane { pane: PaneId(1) }]
    ));
}

#[test]
fn set_and_setp_change_every_window_and_setlocal_only_its_own() {
    let dir = config("");
    let (client, mut one) = first_window(&dir);
    let mut two = window(&client);
    // `:setp` saves it, and every window has it.
    command(&mut one, "setp nonu");
    one.said_saved(one.saving.wait());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("client/settings.lua"))
            .unwrap()
            .lines()
            .last(),
        Some("client.opt.number = false")
    );
    two.follow_client();
    assert!(!one.options.number && !two.options.number);
    // `:setlocal` is the window's own.
    command(&mut two, "setlocal nu cc=80");
    one.follow_client();
    assert!(two.options.number && !one.options.number);
    assert_eq!(two.options.colorcolumn, [80]);
    assert!(one.options.colorcolumn.is_empty());
    command(&mut two, "verbose set nu?");
    assert_eq!(
        two.message.as_deref(),
        Some("number · this window's own, set with :set or a key")
    );
    // Every window's value changes under it, not over it.
    command(&mut one, "set rnu nonu");
    two.follow_client();
    assert!(two.options.number && !one.options.number);
    command(&mut two, "setlocal");
    assert_eq!(two.message.as_deref(), Some("number  colorcolumn=80"));
    // `<` takes it away; `:set` in the window does too.
    command(&mut two, "setlocal nu<");
    assert!(!two.options.number);
    command(&mut two, "set cc=100");
    one.follow_client();
    assert_eq!(
        (
            one.options.colorcolumn.clone(),
            two.options.colorcolumn.clone()
        ),
        (vec![100], vec![100])
    );
    // Only an option that can be is.
    command(&mut two, "setlocal ts=8");
    assert_eq!(
        two.message.as_deref(),
        Some("tabstop is the same in every window, so :setlocal can't set it: :set does")
    );
    assert_eq!(two.options.tabstop, 4);
    // A window that closes takes its own with it.
    command(&mut two, "setlocal nonu");
    two.leave_client();
    assert!(
        client
            .options()
            .borrow()
            .local(2, crate::options::find("nu").unwrap())
            .is_none()
    );
}

#[test]
fn a_chats_turn_ending_is_heard_once_however_many_windows_hear_it() {
    let dir = config(
        r#"
        local heard = 0
        client.on("turnend", function(ev)
            heard += 1
            client.opt.tabstop = heard
            -- In the window that heard it.
            client.cmd(`setlocal cc={ev.chat}`)
        end)
        "#,
    );
    let (client, mut one) = first_window(&dir);
    let mut two = window(&client);
    for desk in [&mut one, &mut two] {
        desk.chats_changed(vec![summary(5, 1, ChatState::Answering)]);
    }
    for desk in [&mut one, &mut two] {
        desk.chats_changed(vec![summary(5, 1, ChatState::Ready)]);
    }
    until(&mut [&mut one, &mut two], |desks| {
        desks.iter().any(|desk| desk.options.colorcolumn == [5])
    });
    // Long enough for a second time to have come.
    std::thread::sleep(Duration::from_millis(50));
    one.lua_said();
    two.lua_said();
    assert_eq!((one.options.tabstop, two.options.tabstop), (1, 1));
    assert!(
        one.options.colorcolumn.is_empty() ^ two.options.colorcolumn.is_empty(),
        "only one window ran it"
    );
    // Another turn of the same chat is another time.
    for desk in [&mut one, &mut two] {
        desk.chats_changed(vec![summary(5, 1, ChatState::Answering)]);
        desk.chats_changed(vec![ChatSummary {
            active: 1,
            ..summary(5, 1, ChatState::Ready)
        }]);
    }
    until(&mut [&mut one, &mut two], |desks| {
        desks.iter().all(|desk| desk.options.tabstop == 2)
    });
}

#[test]
fn a_binding_that_propagates_lets_its_keys_go_on_at_once() {
    let mut desk = editing("/t.txt", "abcdefgh\n");
    desk.lua_from(
        r#"
        client.keys.map("n", "x", function(ev)
            local t = os.clock()
            while os.clock() - t < 0.2 do end
            client.opt.tabstop = ev.count1
            ev:stop()
        end, {propagate = true})
        "#,
    );
    let started = Instant::now();
    type_text(&mut desk, "2x");
    let took = started.elapsed();
    assert_eq!(editor_text(&desk), "cdefgh\n", "its keys went on");
    // Typically well under a millisecond; waiting for Lua, 200.
    assert!(took < Duration::from_millis(50), "{took:?}");
    until(&mut [&mut desk], |desks| desks[0].options.tabstop == 2);
    // Its key was decided before it ran, so it's too late to stop it.
    until(&mut [&mut desk], |desks| {
        (desks[0].message.as_deref()).is_some_and(|said| said.contains("too late to stop"))
    });
}

#[test]
fn a_windows_command_line_font_is_how_it_started() {
    use crate::options::{Decimal, Options, find};
    let client = Client::default();
    let mut desk = window(&client);
    // `--font-size 14`, which every other window doesn't start with.
    let start = Options::drawing_with(&crate::FontOptions {
        family: None,
        size: 14.0,
    });
    desk.start_options(start, vec![find("fontsize").unwrap()]);
    assert_eq!(desk.options.fontsize, Decimal::whole(14));
    assert_eq!(client.options().borrow().fontsize, Decimal::whole(12));
    command(&mut desk, "set");
    assert_eq!(
        desk.message.as_deref(),
        Some("every option is as it started")
    );
    command(&mut desk, "verbose set fsz?");
    assert_eq!(
        desk.message.as_deref(),
        Some(
            "fontsize=14 · this window's own, as the window opened: startup.toml or the command line"
        )
    );
}
