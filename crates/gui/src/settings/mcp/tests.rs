use clackatoo_protocol::{McpScope, McpToolInfo};
use serde_json::{Value, json};

use super::server::{worded, words};
use super::*;
use crate::settings::view::Look;
use crate::vim::Stroke;

/// A server a command runs, kept in the project.
fn server() -> McpServerInfo {
    McpServerInfo {
        name: "search".into(),
        settings: json!({
            "command": "search-server",
            "args": ["--stdio"],
            "env": {"KEY": {"secret": "search-key"}},
            "url": null,
            "enabled": true,
            "timeout_seconds": 30,
        })
        .to_string(),
        source: "Project".into(),
        path: "/project/.agents/mcp.json".into(),
        revision: "saved-revision".into(),
        editable: true,
        scope: McpScope::Project,
        trusted: true,
        enabled: true,
        state: McpState::Disconnected,
        detail: String::new(),
        tools: Vec::new(),
        signing_in: false,
    }
}

/// A server at an address, which needs a sign in.
fn remote() -> McpServerInfo {
    McpServerInfo {
        name: "docs".into(),
        settings: json!({"url": "https://example.com/mcp", "enabled": true}).to_string(),
        state: McpState::NeedsLogin,
        ..server()
    }
}

/// The page, its list read as `servers`.
fn listing(servers: Vec<McpServerInfo>) -> Mcp {
    let mut mcp = Mcp::new();
    let number = mcp.reading();
    mcp.came(number, Ok(reply(servers)));
    mcp
}

fn reply(servers: Vec<McpServerInfo>) -> McpReply {
    McpReply {
        servers,
        ..McpReply::default()
    }
}

/// Where the row labelled `label` is on the page.
fn row(mcp: &Mcp, label: &str) -> usize {
    (mcp.drawn().iter())
        .position(|row| row.label == label)
        .unwrap_or_else(|| panic!("no row {label}"))
}

/// The labels of the rows the page shows.
fn labels(mcp: &Mcp) -> Vec<String> {
    mcp.drawn().into_iter().map(|row| row.label).collect()
}

/// What pressing the row labelled `label` asks of the daemon, and its
/// number.
fn asked(mcp: &mut Mcp, label: &str) -> (McpAction, u64) {
    let at = row(mcp, label);
    match mcp.press(at, None) {
        Did::Outcome(Outcome::Mcp { action, id }) => (action, id),
        other => panic!("{label} asked nothing: {other:?}"),
    }
}

/// Types `text` into the row labelled `label`, and leaves it.
fn typed(mcp: &mut Mcp, label: &str, text: &str) {
    let mut at = row(mcp, label);
    assert!(form::paste(mcp, at, text), "{label} takes no text");
    form::key(mcp, &mut at, Stroke::Enter);
}

/// The buttons of server `name`'s row in the list, by their words, and
/// the one the keys are on.
fn buttons(mcp: &Mcp, name: &str) -> (Vec<(String, bool)>, usize) {
    match &mcp.drawn()[row(mcp, name)].control {
        ControlView::Buttons { buttons, at } => (
            (buttons.iter())
                .map(|button| (button.label.clone(), button.enabled))
                .collect(),
            *at,
        ),
        other => panic!("not buttons: {other:?}"),
    }
}

#[test]
fn a_read_of_the_list_keeps_what_is_typed_and_an_old_answer_changes_nothing() {
    let mut mcp = listing(vec![server()]);
    mcp.press(row(&mcp, "search"), None);
    typed(&mut mcp, "Command", "é");
    // The list is read again while the form is open.
    let number = mcp.reading();
    // An answer to something no longer waited for.
    mcp.came(number + 100, Ok(reply(Vec::new())));
    assert_eq!(mcp.servers().len(), 1);
    assert!(mcp.due().is_none(), "still being read");
    let mut tested = server();
    tested.state = McpState::Connected;
    mcp.came(number, Ok(reply(vec![tested])));
    assert!(mcp.due().is_some(), "to be read again");
    let drawn = mcp.drawn();
    let field = |label: &str| match &drawn[row(&mcp, label)].control {
        ControlView::Field(field) => field.text.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(field("Command"), "search-serveré", "what's typed is kept");
    let status = drawn[row(&mcp, "Connection")].status.clone();
    assert_eq!(status.map(|(said, _)| said).as_deref(), Some("Connected"));
    assert!(mcp.form().unwrap().changed);
}

#[test]
fn a_save_that_fails_keeps_the_form_and_cancel_works_while_one_is_asked() {
    let mut mcp = listing(vec![server()]);
    mcp.press(row(&mcp, "search"), None);
    typed(&mut mcp, "Command", "-changed");
    let Did::Outcome(Outcome::Mcp { action, id }) = mcp.save() else {
        panic!("not saved")
    };
    let McpAction::Save {
        settings,
        expected,
        scope,
        ..
    } = &action
    else {
        panic!("{action:?}")
    };
    assert_eq!(expected.as_deref(), Some("saved-revision"));
    assert_eq!(*scope, McpScope::Project);
    let settings: Value = serde_json::from_str(settings).unwrap();
    assert_eq!(settings["command"], "search-server-changed");
    assert_eq!(settings["env"], json!({"KEY": {"secret": "search-key"}}));
    assert_eq!(mcp.form().unwrap().save, "Saving…");
    // One thing is asked at a time.
    assert_eq!(mcp.save(), Did::Nothing);
    mcp.came(id, Err("changed outside Settings".into()));
    assert_eq!(
        mcp.notice(),
        Some(&Notice::Failed("changed outside Settings".into()))
    );
    assert!(mcp.form().unwrap().changed, "the form is kept");
    // Asked again, then cancelled: its answer closes nothing else.
    let Did::Outcome(Outcome::Mcp { id, .. }) = mcp.save() else {
        panic!("not saved")
    };
    mcp.cancel();
    assert!(mcp.form().is_none());
    mcp.add();
    mcp.came(id, Ok(reply(vec![server()])));
    assert_eq!(mcp.form().unwrap().title, "New server");
    assert!(matches!(mcp.notice(), Some(Notice::Done(_))));
}

#[test]
fn a_lost_connection_fails_what_was_asked_and_the_list_is_read_again_once_it_is_back() {
    let mut mcp = listing(vec![server()]);
    mcp.press(row(&mcp, "search"), None);
    let (_, id) = asked(&mut mcp, "Test");
    mcp.reading();
    mcp.lost();
    assert_eq!(
        mcp.notice(),
        Some(&Notice::Failed(crate::settings::LOST_ASKING.into()))
    );
    assert!(mcp.due().is_none(), "nothing's read while it's lost");
    // The page isn't dead: something else can be asked, and cancelled.
    mcp.read_now();
    assert!(mcp.due().is_some());
    let (action, again) = asked(&mut mcp, "Test");
    assert!(matches!(action, McpAction::Test { .. }));
    // The lost one's answer, were it to come, is nothing's.
    mcp.came(id, Ok(reply(Vec::new())));
    assert_eq!(mcp.servers().len(), 1);
    mcp.came(again, Err("no route".into()));
    assert_eq!(mcp.notice(), Some(&Notice::Failed("no route".into())));
}

#[test]
fn a_test_changes_its_server_alone_and_is_shown_until_the_server_changes() {
    let mut mcp = listing(vec![remote(), server()]);
    mcp.press(row(&mcp, "search"), None);
    let (_, id) = asked(&mut mcp, "Test");
    // The daemon answers a test with the one server.
    let tested = McpServerInfo {
        state: McpState::Ready,
        detail: "Connection test passed.".into(),
        tools: vec![McpToolInfo {
            name: "find".into(),
            description: "Finds things.".into(),
        }],
        ..server()
    };
    mcp.came(id, Ok(reply(vec![tested])));
    assert_eq!(mcp.servers().len(), 2, "the other is still listed");
    assert_eq!(
        mcp.notice(),
        Some(&Notice::Done("search answered, with 1 tool.".into()))
    );
    assert!(labels(&mcp).contains(&"find".to_owned()));
    // The daemon doesn't keep a test: the next read says nothing of it.
    let number = mcp.reading();
    mcp.came(number, Ok(reply(vec![remote(), server()])));
    assert_eq!(mcp.server("search").unwrap().state, McpState::Ready);
    assert_eq!(mcp.server("search").unwrap().tools.len(), 1);
    // Saved as something else, it's no longer what was tested.
    let number = mcp.reading();
    let changed = McpServerInfo {
        revision: "another".into(),
        ..server()
    };
    mcp.came(number, Ok(reply(vec![remote(), changed])));
    assert_eq!(mcp.server("search").unwrap().state, McpState::Disconnected);
}

#[test]
fn a_form_shows_and_saves_only_what_its_way_of_connecting_uses() {
    let mut mcp = listing(Vec::new());
    assert_eq!(mcp.add(), Did::Went(0));
    let shown = labels(&mcp);
    for label in ["Command", "Arguments", "Environment"] {
        assert!(shown.contains(&label.to_owned()), "{shown:?}");
    }
    for label in ["Address", "Headers", "Client ID", "Connection", "Test"] {
        assert!(!shown.contains(&label.to_owned()), "{shown:?}");
    }
    typed(&mut mcp, "Name", "docs");
    typed(&mut mcp, "Command", "docs-server");
    typed(&mut mcp, "Arguments", "--root \"/my notes\"");
    // Over to an address: the command's fields go, and aren't saved.
    let transport = row(&mcp, "Connects by");
    assert_eq!(mcp.step(transport, 1), Did::Changed);
    assert_eq!(mcp.step(transport, 1), Did::Nothing, "it doesn't go round");
    let shown = labels(&mcp);
    assert!(!shown.contains(&"Command".to_owned()), "{shown:?}");
    assert!(!shown.contains(&"Client ID".to_owned()), "{shown:?}");
    // Saved without an address, it says so under the field, and isn't.
    let Did::Went(at) = mcp.save() else {
        panic!("saved with no address")
    };
    assert_eq!(at, row(&mcp, "Address"));
    assert_eq!(mcp.drawn()[at].notes.len(), 1);
    typed(&mut mcp, "Address", "https://example.com/mcp");
    assert!(mcp.drawn()[at].notes.is_empty());
    mcp.press(row(&mcp, "Sign in with a browser"), None);
    typed(&mut mcp, "Scopes", "read write");
    // A call can be given ten minutes at most.
    typed(&mut mcp, "Give up on a call after", "00");
    assert!(matches!(mcp.save(), Did::Went(_)), "the timeout is checked");
    let timeout = row(&mcp, "Give up on a call after");
    form::begin(&mut mcp, timeout, None);
    let mut focus = timeout;
    form::key(&mut mcp, &mut focus, Stroke::Backspace);
    form::key(&mut mcp, &mut focus, Stroke::Backspace);
    form::key(&mut mcp, &mut focus, Stroke::char('9'));
    form::key(&mut mcp, &mut focus, Stroke::Enter);
    let Did::Outcome(Outcome::Mcp { action, .. }) = mcp.save() else {
        panic!("not saved")
    };
    let McpAction::Save {
        name,
        settings,
        expected: None,
        scope: McpScope::Personal,
    } = action
    else {
        panic!("{action:?}")
    };
    assert_eq!(name, "docs");
    assert_eq!(
        serde_json::from_str::<Value>(&settings).unwrap(),
        json!({
            "url": "https://example.com/mcp",
            "headers": {},
            "oauth": {"client_id": null, "scopes": ["read", "write"]},
            "enabled": true,
            "read_only": false,
            "timeout_seconds": 309,
        })
    );
}

#[test]
fn a_server_shows_what_can_be_done_about_it_as_it_is_and_forgetting_asks_first() {
    // A command needs no sign in, and has none to forget.
    let mut mcp = listing(vec![remote(), server()]);
    mcp.press(row(&mcp, "search"), None);
    let shown = labels(&mcp);
    for label in ["Sign in", "Forget the sign in", "Review and enable"] {
        assert!(!shown.contains(&label.to_owned()), "{shown:?}");
    }
    assert!(shown.contains(&"Test".to_owned()));
    mcp.cancel();
    // One from the project runs nothing until it's enabled.
    let unreviewed = McpServerInfo {
        trusted: false,
        ..remote()
    };
    let mut mcp = listing(vec![unreviewed]);
    assert_eq!(mcp.press(row(&mcp, "docs"), None), Did::Went(1));
    let shown = labels(&mcp);
    assert!(shown.contains(&"Review and enable".to_owned()));
    assert!(!shown.contains(&"Test".to_owned()), "{shown:?}");
    let (action, id) = asked(&mut mcp, "Review and enable");
    assert!(matches!(action, McpAction::Trust { .. }));
    let signed_in = McpServerInfo {
        state: McpState::Ready,
        ..remote()
    };
    mcp.came(id, Ok(reply(vec![signed_in])));
    let shown = labels(&mcp);
    assert!(!shown.contains(&"Review and enable".to_owned()));
    // Forgetting its sign in takes two presses, of that button.
    let forget = row(&mcp, "Forget the sign in");
    assert_eq!(mcp.press(forget, None), Did::Changed);
    assert!(matches!(mcp.notice(), Some(Notice::Confirm(_))));
    let ControlView::Button(button) = &mcp.drawn()[forget].control else {
        panic!("no button")
    };
    assert_eq!(
        (button.label.as_str(), button.look),
        ("Really forget", Look::Danger)
    );
    // Something else pressed between, and it asks again.
    mcp.press(row(&mcp, "Enabled"), None);
    assert_eq!(mcp.press(forget, None), Did::Changed);
    let (action, _) = asked(&mut mcp, "Forget the sign in");
    assert!(matches!(action, McpAction::ForgetAuth { .. }));
}

#[test]
fn a_private_credential_and_a_sign_ins_address_are_drawn_as_dots() {
    let mut mcp = listing(Vec::new());
    mcp.press(row(&mcp, "Private credential"), None);
    typed(&mut mcp, "Name", "key");
    typed(&mut mcp, "Value", "private-value");
    let ControlView::Field(field) = &mcp.drawn()[row(&mcp, "Value")].control else {
        panic!("no field")
    };
    assert!(field.masked);
    let Did::Outcome(Outcome::Mcp { action, .. }) = mcp.save() else {
        panic!("not saved")
    };
    assert!(
        action
            == McpAction::Secret {
                name: "key".into(),
                value: "private-value".into()
            }
    );
    assert!(!format!("{action:?} {mcp:?}").contains("private-value"));
}

#[test]
fn servers_are_copied_over_only_once_some_are_picked() {
    let mut mcp = listing(vec![server()]);
    let (action, id) = asked(&mut mcp, "Copy servers from Claude Code");
    assert!(matches!(&action, McpAction::Import { names, .. } if names.is_empty()));
    let candidate = |name: &str| McpImport {
        name: name.into(),
        description: "A server at an address".into(),
    };
    mcp.came(
        id,
        Ok(McpReply {
            imports: vec![candidate("one"), candidate("two")],
            ..McpReply::default()
        }),
    );
    assert_eq!(mcp.form().unwrap().title, "Copy from Claude Code");
    assert_eq!(mcp.save(), Did::Changed, "none is picked");
    assert!(matches!(mcp.notice(), Some(Notice::Failed(_))));
    mcp.press(row(&mcp, "two"), None);
    let Did::Outcome(Outcome::Mcp { action, id }) = mcp.save() else {
        panic!("not copied")
    };
    assert!(matches!(&action, McpAction::Import { names, .. } if names == &["two"]));
    mcp.came(id, Ok(reply(vec![server()])));
    assert!(mcp.form().is_none(), "back at the list");
}

#[test]
fn a_server_needing_a_login_has_a_sign_in_button_that_says_how_it_goes() {
    let mut mcp = listing(vec![remote(), server()]);
    assert_eq!(buttons(&mcp, "search").0, [("Open".to_owned(), true)]);
    // The keys are on Open, which Enter presses, whatever else it has.
    assert_eq!(
        buttons(&mcp, "docs"),
        (
            vec![("Sign in".to_owned(), true), ("Open".to_owned(), true)],
            1
        )
    );
    // Its Sign in button, clicked, signs in.
    let at = row(&mcp, "docs");
    let Did::Outcome(Outcome::Mcp { action, id }) = mcp.press(at, Some(Part::Segment(0))) else {
        panic!("no sign in")
    };
    assert!(
        action
            == McpAction::Authenticate {
                name: "docs".into()
            }
    );
    assert_eq!(
        buttons(&mcp, "docs").0[0],
        ("Signing in…".to_owned(), false)
    );
    let url = Some("https://example.com/authorize".to_owned());
    let answer = McpReply {
        login_url: url.clone(),
        ..McpReply::default()
    };
    assert_eq!(mcp.came(id, Ok(answer.clone())).0, url);
    let signing_in = McpServerInfo {
        signing_in: true,
        ..remote()
    };
    let number = mcp.reading();
    mcp.came(number, Ok(reply(vec![signing_in, server()])));
    assert_eq!(
        buttons(&mcp, "docs").0[0],
        ("Signing in…".to_owned(), false)
    );
    assert_eq!(mcp.press(at, Some(Part::Segment(0))), Did::Nothing);
    // Then what came of it: failed, and why, so it can be tried again...
    let failed = McpServerInfo {
        detail: "The browser closed".into(),
        ..remote()
    };
    let number = mcp.reading();
    mcp.came(number, Ok(reply(vec![failed, server()])));
    assert_eq!(buttons(&mcp, "docs").0[0], ("Sign in".to_owned(), true));
    assert_eq!(
        mcp.drawn()[at].notes,
        [("The browser closed".to_owned(), Ink::Warning)]
    );
    // ...with `h` to its button, and Enter.
    assert_eq!(mcp.step(at, -1), Did::Changed);
    assert_eq!(mcp.step(at, -1), Did::Nothing);
    let Did::Outcome(Outcome::Mcp { id, .. }) = mcp.press(at, None) else {
        panic!("Enter doesn't sign in")
    };
    assert_eq!(mcp.came(id, Ok(answer)).0, url);
    // ...or signed in.
    let ready = McpServerInfo {
        state: McpState::Ready,
        ..remote()
    };
    let number = mcp.reading();
    mcp.came(number, Ok(reply(vec![ready, server()])));
    assert_eq!(buttons(&mcp, "docs").0[0], ("Signed in".to_owned(), false));
    assert!(mcp.drawn()[at].notes.is_empty());
}

#[test]
fn the_keys_stay_on_a_servers_button_wherever_the_list_puts_it() {
    let mut mcp = listing(vec![remote()]);
    let at = row(&mcp, "docs");
    assert_eq!(mcp.step(at, -1), Did::Changed);
    assert_eq!(buttons(&mcp, "docs").1, 0);
    // Another server is listed before it: its button is still its own.
    let first = McpServerInfo {
        name: "another".into(),
        ..remote()
    };
    let number = mcp.reading();
    mcp.came(number, Ok(reply(vec![first, remote()])));
    assert_eq!(buttons(&mcp, "docs").1, 0);
    assert_eq!(buttons(&mcp, "another").1, 1);
    // Enter on the other opens it, as Enter does.
    assert!(matches!(
        mcp.press(row(&mcp, "another"), None),
        Did::Went(_)
    ));
    assert_eq!(mcp.form().unwrap().title, "another");
}

#[test]
fn a_sign_in_the_daemon_refuses_says_why_on_its_row() {
    let mut mcp = listing(vec![remote()]);
    let at = row(&mcp, "docs");
    let Did::Outcome(Outcome::Mcp { id, .. }) = mcp.press(at, Some(Part::Segment(0))) else {
        panic!("no sign in")
    };
    let why = "Sign in is available for HTTP servers";
    let refused = McpReply {
        servers: vec![remote()],
        notice: why.into(),
        ..McpReply::default()
    };
    assert_eq!(mcp.came(id, Ok(refused)), (None, None));
    assert_eq!(mcp.drawn()[at].notes, [(why.to_owned(), Ink::Warning)]);
}

#[test]
fn a_sign_in_asked_while_the_list_is_read_is_answered_by_its_own_number() {
    let mut mcp = listing(vec![remote()]);
    let reading = mcp.reading();
    let at = row(&mcp, "docs");
    let Did::Outcome(Outcome::Mcp { id, .. }) = mcp.press(at, Some(Part::Segment(0))) else {
        panic!("the read stopped it")
    };
    // The read's answer comes first, and is the list; then the sign in's.
    mcp.came(reading, Ok(reply(vec![remote()])));
    assert_eq!(
        buttons(&mcp, "docs").0[0],
        ("Signing in…".to_owned(), false)
    );
    let url = Some("https://example.com/authorize".to_owned());
    let answer = McpReply {
        login_url: url.clone(),
        ..McpReply::default()
    };
    assert_eq!(mcp.came(id, Ok(answer)).0, url);
    assert!(mcp.asked.is_none());
}

#[test]
fn what_is_wrong_with_the_servers_files_is_said_with_the_list() {
    let mut mcp = listing(vec![server()]);
    assert_eq!(mcp.problems(), None);
    let number = mcp.reading();
    let said = McpReply {
        notice: "mcp.json: expected a table".into(),
        ..reply(vec![server()])
    };
    mcp.came(number, Ok(said));
    assert_eq!(mcp.problems(), Some("mcp.json: expected a table"));
    // A list that can't be read says why, in place of its rows.
    let number = mcp.reading();
    mcp.came(number, Err("no such pane".into()));
    assert_eq!(*mcp.listing(), Listing::Unavailable("no such pane".into()));
    assert!(mcp.drawn().is_empty());
}

#[test]
fn arguments_are_words_read_back_as_they_were_typed() {
    let arguments = |typed: &str| words(typed).unwrap();
    assert_eq!(arguments("--stdio  --port 80"), ["--stdio", "--port", "80"]);
    assert_eq!(arguments("--root \"/my notes\""), ["--root", "/my notes"]);
    assert_eq!(arguments("\"\" a\"b c\"d"), ["", "ab cd"]);
    assert!(words("\"open").is_err());
    for list in [
        vec!["--stdio".to_owned()],
        vec![String::new(), "a b".into(), "say \"hi\"".into()],
        vec!["back\\slash".into(), "two\nlines".into()],
    ] {
        assert_eq!(words(&worded(&list)).unwrap(), list, "{}", worded(&list));
    }
}
