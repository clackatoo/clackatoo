//! The settings menu's lists and forms: MCP servers, specialists, skills
//! and permission rules. A form's changes are never thrown away without
//! asking; its keys are its own; only a control acts on a click; the foot
//! names keys that work; a lost connection fails what was asked and
//! everything's read again once it's back; and rules this window can't
//! read are never saved over.

use clackatoo_protocol::{
    ChatChoices, McpAction, McpReply, McpScope, McpServerInfo, McpState, OpenFile, OptionSaved,
    PaneTerminal, SpecialistDefinition, SpecialistFile, SpecialistScope,
};

use super::settings_menu::{daemon_said, easy, easy_configured, easy_row, named};
use super::*;
use crate::options::easy::Page;
use crate::settings::view::{ControlView, EasyView, Foot, Part, SettingsHit};

/// A desk with the menu open on `page` of Easy, the daemon having said
/// its settings, and nothing else it was asked for yet.
fn on_page(page: Page) -> (Desk, tempfile::TempDir) {
    let (mut desk, dir) = easy_configured("");
    desk.daemon_settings_came(&daemon_said("desk"), None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, page);
    (desk, dir)
}

/// Shows `page`, by a click on it down the side.
fn go_to(desk: &mut Desk, page: Page) {
    let at = Page::ALL.iter().position(|each| *each == page).unwrap();
    desk.settings_pointer(SettingsHit::Page(u8::try_from(at).unwrap()));
    answer_all(desk);
}

/// A click on the control of the row labelled `label`.
fn click(desk: &mut Desk, label: &str) {
    let (row, _) = easy_row(desk, label);
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
}

/// What the window asked of the daemon since it was last looked at,
/// but for what it reads again every so often while a page is shown.
fn sent(desk: &mut Desk) -> Vec<Request> {
    let read_again = |request: &Request| {
        matches!(
            request,
            Request::DaemonSettings
                | Request::Mcp {
                    action: McpAction::List,
                    ..
                }
        )
    };
    let mut asked = answer_all(desk);
    asked.retain(|request| !read_again(request));
    asked
}

/// The number the MCP page's last request is known by.
fn mcp_asked(desk: &Desk) -> u64 {
    (desk.asked.iter())
        .filter_map(|asker| match asker {
            Asker::Window(Asked::SettingsMcp(number)) => Some(*number),
            _ => None,
        })
        .max()
        .expect("something asked of the MCP page")
}

/// A server a command runs, kept in the project.
fn server(name: &str) -> McpServerInfo {
    McpServerInfo {
        name: name.into(),
        settings: r#"{"command":"search-server","args":["--stdio"],"enabled":true}"#.into(),
        source: "Project".into(),
        path: "/project/.agents/mcp.json".into(),
        revision: "saved".into(),
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

/// A desk with the MCP Servers page shown, listing `servers`.
fn mcp_page(servers: Vec<McpServerInfo>) -> (Desk, tempfile::TempDir) {
    let (mut desk, dir) = easy_configured("");
    desk.daemon_settings_came(&daemon_said("desk"), None);
    command(&mut desk, "Settings");
    let number = mcp_asked(&desk);
    answer_all(&mut desk);
    desk.settings_mcp(
        number,
        Response::Mcp(McpReply {
            servers,
            ..McpReply::default()
        }),
    );
    go_to(&mut desk, Page::Mcp);
    (desk, dir)
}

/// One of the user's own specialists, in a file.
fn specialist(name: &str) -> SpecialistFile {
    SpecialistFile {
        definition: SpecialistDefinition {
            name: name.into(),
            settings: r#"{"description":"Reviews","instructions":"Review.","includes":["checks"]}"#
                .into(),
            saved: Some("the file as it was".into()),
            locked: None,
        },
        path: format!("/project/.agents/specialists/{name}/SPECIALIST.md"),
        scope: SpecialistScope::Project,
        editable: true,
    }
}

/// A desk with the Specialists page shown, listing `files`.
fn specialists_page(files: Vec<SpecialistFile>) -> (Desk, tempfile::TempDir) {
    let (mut desk, dir) = on_page(Page::Specialists);
    desk.settings_specialists(Response::SpecialistFiles(files));
    (desk, dir)
}

/// The daemon's settings, its permission rules being `rules`.
fn with_rules(rules: &str) -> clackatoo_protocol::DaemonSettings {
    let mut settings = daemon_said("desk");
    for option in &mut settings.options {
        if option.name == "permissions.rules" {
            option.value = rules.into();
        }
    }
    settings
}

fn labels(view: &EasyView) -> Vec<String> {
    (view.sections.iter())
        .flat_map(|section| &section.rows)
        .map(|row| row.label.clone())
        .collect()
}

fn hints(view: &EasyView) -> Vec<&'static str> {
    view.hints.iter().map(|(key, _)| *key).collect()
}

#[test]
fn a_form_with_changes_is_never_thrown_away_without_asking() {
    let (mut desk, _dir) = specialists_page(Vec::new());
    desk.settings_pointer(SettingsHit::Primary);
    assert_eq!(easy(&mut desk).title, "New specialist");
    // Nothing typed yet: Esc goes back to the list at once.
    named(&mut desk, NamedKey::Escape);
    assert_eq!(easy(&mut desk).title, "Specialists");

    let changed = |desk: &mut Desk| {
        desk.settings_pointer(SettingsHit::Primary);
        type_keys(desk, "reviewer");
        named(desk, NamedKey::Enter);
        assert!(easy(desk).footer.is_empty());
    };
    // Esc asks, and another key takes the question back.
    changed(&mut desk);
    named(&mut desk, NamedKey::Escape);
    let view = easy(&mut desk);
    assert!(
        view.warning && view.footer.starts_with("Discard"),
        "{}",
        view.footer
    );
    assert_eq!(view.title, "New specialist");
    named(&mut desk, NamedKey::Tab);
    assert!(easy(&mut desk).footer.is_empty());
    named(&mut desk, NamedKey::Escape);
    assert_eq!(easy(&mut desk).title, "New specialist");
    named(&mut desk, NamedKey::Escape);
    assert_eq!(easy(&mut desk).title, "Specialists", "asked twice, it goes");

    // A click off the panel, the close button and the cog ask too.
    for close in [SettingsHit::Backdrop, SettingsHit::Close] {
        changed(&mut desk);
        desk.settings_pointer(close);
        assert!(desk.settings_open(), "{close:?} closed it");
        assert!(easy(&mut desk).footer.starts_with("Discard"));
        desk.settings_pointer(close);
        assert!(!desk.settings_open());
        command(&mut desk, "Settings");
        go_to(&mut desk, Page::Specialists);
        desk.settings_specialists(Response::SpecialistFiles(Vec::new()));
    }
    changed(&mut desk);
    desk.toggle_settings();
    assert!(desk.settings_open(), "the cog closed it");
    desk.toggle_settings();
    assert!(!desk.settings_open());

    // And so does going to another page, or to Advanced.
    command(&mut desk, "Settings");
    go_to(&mut desk, Page::Specialists);
    desk.settings_specialists(Response::SpecialistFiles(Vec::new()));
    changed(&mut desk);
    go_to(&mut desk, Page::Appearance);
    assert_eq!(easy(&mut desk).title, "New specialist");
    named(&mut desk, NamedKey::ArrowDown);
    desk.settings_pointer(SettingsHit::Mode(crate::settings::Mode::Advanced));
    assert_eq!(
        desk.settings_mode_shown(),
        Some(crate::settings::Mode::Easy),
        "a key between, and it asks again"
    );
    go_to(&mut desk, Page::Appearance);
    assert_eq!(easy(&mut desk).title, "Appearance");
    // Back on its page, the form has gone: it was agreed to.
    go_to(&mut desk, Page::Specialists);
    assert_eq!(easy(&mut desk).title, "Specialists");
}

#[test]
fn a_letter_typed_in_a_form_is_text_or_nothing_and_never_a_command() {
    let (mut desk, _dir) = specialists_page(Vec::new());
    desk.settings_pointer(SettingsHit::Primary);
    // On a box of text, typing starts at once; Enter finishes it and
    // goes on to the next row.
    type_keys(&mut desk, "qmru");
    let (name, row) = easy_row(&mut desk, "Name");
    let ControlView::Field(field) = &row.control else {
        panic!("{row:?}")
    };
    assert_eq!((field.text.as_str(), field.caret), ("qmru", Some(4)));
    named(&mut desk, NamedKey::Enter);
    let view = easy(&mut desk);
    assert_eq!(view.focus, Some(usize::from(name) + 1));
    assert_eq!(labels(&view)[usize::from(name) + 1], "Kept in");
    // On a row that isn't text, the letters that act elsewhere do
    // nothing: the menu neither closes, nor goes to Advanced or a search.
    type_keys(&mut desk, "qmru/");
    let view = easy(&mut desk);
    assert_eq!(view.title, "New specialist");
    assert!(!view.typing && view.query.is_empty());
    assert_eq!(
        desk.settings_mode_shown(),
        Some(crate::settings::Mode::Easy)
    );
    // `l` and `h` step its choice, without going round, as on any page.
    let kept_in = |desk: &mut Desk| match easy_row(desk, "Kept in").1.control {
        ControlView::Segmented { picked, .. } => picked,
        other => panic!("{other:?}"),
    };
    type_keys(&mut desk, "l");
    assert_eq!(kept_in(&mut desk), Some(1));
    type_keys(&mut desk, "l");
    assert_eq!(kept_in(&mut desk), Some(1));
    type_keys(&mut desk, "h");
    assert_eq!(kept_in(&mut desk), Some(0));
    // Tab goes on, Shift+Tab back, and typing there is that row's.
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, "Reviews changes");
    named(&mut desk, NamedKey::Tab);
    let (_, described) = easy_row(&mut desk, "Description");
    assert!(
        matches!(&described.control, ControlView::Field(field)
            if field.text == "Reviews changes" && field.caret.is_none()),
        "{described:?}"
    );
    // Esc while typing stops typing, and keeps what was typed: it's the
    // question before the form goes that guards it.
    type_keys(&mut desk, "After a change");
    named(&mut desk, NamedKey::Escape);
    let (_, when) = easy_row(&mut desk, "When to use it");
    assert!(
        matches!(&when.control, ControlView::Field(field)
            if field.text == "After a change" && field.caret.is_none()),
        "{when:?}"
    );
    named(&mut desk, NamedKey::Escape);
    let view = easy(&mut desk);
    assert_eq!(view.title, "New specialist");
    assert!(view.footer.starts_with("Discard"), "{}", view.footer);
    // The keys that edit text start it too, without a letter first.
    press(&mut desk, Key::Character("u".into()), None, CTRL);
    let (_, when) = easy_row(&mut desk, "When to use it");
    assert!(
        matches!(&when.control, ControlView::Field(field)
            if field.text.is_empty() && field.caret == Some(0)),
        "{when:?}"
    );
}

#[test]
fn a_lost_connection_fails_what_was_asked_and_everything_is_read_again_once_it_is_back() {
    let (mut desk, _dir) = mcp_page(vec![server("search")]);
    click(&mut desk, "search");
    click(&mut desk, "Test");
    assert!(
        matches!(
            &sent(&mut desk)[..],
            [Request::Mcp {
                action: McpAction::Test { .. },
                ..
            }]
        ),
        "the test was asked for"
    );
    assert_eq!(easy(&mut desk).footer, "Testing it…");
    desk.lost();
    let view = easy(&mut desk);
    assert!(
        view.banner
            .as_deref()
            .is_some_and(|said| said.contains("lost")),
        "{view:?}"
    );
    // The page isn't dead: Cancel goes back to the list.
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    assert_eq!(easy(&mut desk).title, "MCP Servers");
    // What's asked while it's out of reach fails at once, saying that
    // nothing was sent, not that it may have been done.
    click(&mut desk, "search");
    click(&mut desk, "Test");
    assert!(sent(&mut desk).is_empty());
    let banner = easy(&mut desk).banner.expect("it says so");
    assert!(banner.contains("nothing was sent"), "{banner}");

    // A specialist's save, lost with the connection, leaves its form
    // open to cancel, or save again.
    go_to(&mut desk, Page::Specialists);
    let workspaces = desk.workspaces.clone();
    desk.connected(workspaces, false);
    answer_all(&mut desk);
    desk.settings_specialists(Response::SpecialistFiles(vec![specialist("reviewer")]));
    click(&mut desk, "reviewer");
    click(&mut desk, "You can start it");
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    assert!(matches!(
        &sent(&mut desk)[..],
        [Request::SaveSpecialistFile { .. }]
    ));
    desk.lost();
    let view = easy(&mut desk);
    assert_eq!(view.title, "reviewer");
    assert!(view.banner.is_some());
    assert_eq!(view.buttons[1].label, "Save", "not saving for ever");
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    assert_eq!(easy(&mut desk).title, "Specialists");

    // Back, every list is asked for again, the page shown's among them.
    go_to(&mut desk, Page::Mcp);
    assert!(
        easy(&mut desk).empty.is_some(),
        "it says it isn't connected"
    );
    let workspaces = desk.workspaces.clone();
    desk.connected(workspaces, false);
    let asked = answer_all(&mut desk);
    let asks = |is: fn(&Request) -> bool| asked.iter().any(is);
    assert!(asks(|request| matches!(
        request,
        Request::SpecialistFiles { .. }
    )));
    assert!(asks(|request| matches!(request, Request::Skills { .. })));
    assert!(asks(|request| matches!(request, Request::DaemonSettings)));
    assert!(asks(|request| matches!(
        request,
        Request::ChatChoices { .. }
    )));
    assert!(
        asks(|request| matches!(
            request,
            Request::Mcp {
                action: McpAction::List,
                ..
            }
        )),
        "{asked:?}"
    );
}

#[test]
fn only_a_control_acts_on_a_click_and_the_rest_of_a_row_takes_the_keys() {
    let (mut desk, _dir) = mcp_page(vec![server("search")]);
    // The list: a click on a row's words opens nothing.
    let (row, _) = easy_row(&mut desk, "Private credential");
    desk.settings_pointer(SettingsHit::Row(row));
    let view = easy(&mut desk);
    assert_eq!(view.title, "MCP Servers");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert_eq!(easy(&mut desk).title, "Private credential");
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));

    // A form: a switch goes over only when it's clicked itself, and
    // nothing is removed by a click on Remove's words.
    click(&mut desk, "search");
    let enabled = |desk: &mut Desk| easy_row(desk, "Enabled").1.control;
    let (row, _) = easy_row(&mut desk, "Enabled");
    desk.settings_pointer(SettingsHit::Row(row));
    assert_eq!(enabled(&mut desk), ControlView::Toggle(true));
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert_eq!(enabled(&mut desk), ControlView::Toggle(false));
    let (row, _) = easy_row(&mut desk, "Remove search");
    desk.settings_pointer(SettingsHit::Row(row));
    assert!(sent(&mut desk).is_empty());
    assert!(!easy(&mut desk).footer.contains("Remove"));
    // A click in a box of text puts the caret where it's clicked.
    let (row, _) = easy_row(&mut desk, "Command");
    desk.settings_pointer(SettingsHit::Control(row, Part::Caret(6)));
    type_keys(&mut desk, "!");
    let (_, command) = easy_row(&mut desk, "Command");
    assert!(
        matches!(&command.control, ControlView::Field(field)
            if field.text == "search!-server" && field.caret == Some(7)),
        "{command:?}"
    );
    // And one on another row's words finishes it, keeping what's typed.
    let (row, _) = easy_row(&mut desk, "Enabled");
    desk.settings_pointer(SettingsHit::Row(row));
    let (_, command) = easy_row(&mut desk, "Command");
    assert!(matches!(&command.control, ControlView::Field(field) if field.caret.is_none()));
    assert_eq!(easy(&mut desk).focus, None, "the mouse's, not the keys'");

    // A skill opens by its button, not by its words.
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    desk.settings_skills(Response::Skills(vec![clackatoo_protocol::Skill {
        name: "migrate-db".into(),
        description: "Check a database migration.".into(),
        path: "/project/.agents/skills/migrate-db/SKILL.md".into(),
        source: "Project".into(),
        problem: None,
        implicit: true,
        user_invocable: true,
    }]));
    go_to(&mut desk, Page::Skills);
    let (row, _) = easy_row(&mut desk, "/migrate-db");
    desk.settings_pointer(SettingsHit::Row(row));
    assert!(desk.settings_open());
    assert!(sent(&mut desk).is_empty());
}

#[test]
fn the_foot_names_keys_that_work_on_the_page_shown() {
    let (mut desk, _dir) = mcp_page(vec![server("search")]);
    // A list: nothing to save, undo or put back.
    let view = easy(&mut desk);
    assert_eq!(hints(&view), ["a", "/", "Esc"], "the keys are on the pages");
    assert!(view.buttons.is_empty());
    assert_eq!(view.primary.as_deref(), Some("Add a server"));
    // Into the page, on a server: Enter opens it.
    named(&mut desk, NamedKey::Enter);
    assert_eq!(hints(&easy(&mut desk)), ["a", "Enter", "/", "Esc"]);
    // `a` is its button.
    type_keys(&mut desk, "a");
    let view = easy(&mut desk);
    assert_eq!(
        (view.within.as_deref(), view.title.as_str()),
        (Some("MCP Servers"), "New server")
    );
    assert_eq!(hints(&view), ["Tab", "Ctrl+S", "Esc"]);
    assert_eq!(view.primary, None);
    let said: Vec<&str> = view.buttons.iter().map(|each| &*each.label).collect();
    assert_eq!(said, ["Back", "Save"]);
    // Typing: Esc is the text's.
    type_keys(&mut desk, "docs");
    let view = easy(&mut desk);
    assert_eq!(view.hints.last(), Some(&("Esc", "done typing")));
    let said: Vec<&str> = view.buttons.iter().map(|each| &*each.label).collect();
    assert_eq!(said, ["Cancel", "Save"], "there's something to lose now");
    named(&mut desk, NamedKey::Escape);
    assert_eq!(easy(&mut desk).hints.last(), Some(&("Esc", "cancel")));
    named(&mut desk, NamedKey::Escape);
    assert!(
        easy(&mut desk).hints.is_empty(),
        "the question says its keys"
    );
    named(&mut desk, NamedKey::Escape);

    // A page of settings: its rows undo and go back as they start.
    go_to(&mut desk, Page::Appearance);
    assert_eq!(hints(&easy(&mut desk)), ["u", "r", "m", "Esc"]);
    // Skills, with none: nothing for Enter to do.
    go_to(&mut desk, Page::Skills);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(hints(&easy(&mut desk)), ["/", "Esc"]);
    // Permissions: who answers is a setting, undone and put back as one,
    // and the foot doesn't say rules save as they're made.
    go_to(&mut desk, Page::Permissions);
    named(&mut desk, NamedKey::Enter);
    let view = easy(&mut desk);
    assert_eq!(hints(&view), ["a", "u", "r", "/", "Esc"]);
    assert_eq!(view.footer, "");
    // The rules save from their form, with Ctrl+S.
    type_keys(&mut desk, "a");
    assert_eq!(hints(&easy(&mut desk)), ["Tab", "Ctrl+S", "Esc"]);
}

#[test]
fn a_click_on_the_search_box_ends_typing_in_a_rule() {
    let (mut desk, _dir) = on_page(Page::Permissions);
    type_keys(&mut desk, "a");
    // Down to what it matches, and into the box, without changing it.
    named(&mut desk, NamedKey::Tab);
    named(&mut desk, NamedKey::Tab);
    named(&mut desk, NamedKey::Enter);
    let (_, tool) = easy_row(&mut desk, "Tool");
    assert!(matches!(&tool.control, ControlView::Field(field) if field.caret == Some(0)));
    desk.settings_pointer(SettingsHit::Search);
    type_keys(&mut desk, "wrap");
    let view = easy(&mut desk);
    assert_eq!((view.query.as_str(), view.typing), ("wrap", true));
    // The rule had nothing typed into it, and has gone.
    named(&mut desk, NamedKey::Escape);
    assert_eq!(easy(&mut desk).within, None);
}

#[test]
fn the_keys_start_at_a_forms_first_row_and_stay_put_as_its_server_changes() {
    let (mut desk, _dir) = on_page(Page::Permissions);
    type_keys(&mut desk, "a");
    let view = easy(&mut desk);
    assert_eq!(view.focus, Some(0));
    assert_eq!(labels(&view)[0], "When it matches");
    named(&mut desk, NamedKey::Escape);

    // A server's switch pressed twice is on again, on the same page.
    let (mut desk, _dir) = mcp_page(vec![server("search")]);
    // From the pages down the side into the page, then into the server.
    named(&mut desk, NamedKey::Enter);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(easy(&mut desk).title, "search");
    let (row, _) = easy_row(&mut desk, "Enabled");
    while easy(&mut desk).focus != Some(usize::from(row)) {
        named(&mut desk, NamedKey::Tab);
    }
    type_keys(&mut desk, "  ");
    let view = easy(&mut desk);
    assert_eq!(view.title, "search");
    assert_eq!(view.focus, Some(usize::from(row)));
    assert_eq!(
        easy_row(&mut desk, "Enabled").1.control,
        ControlView::Toggle(true)
    );
    assert!(sent(&mut desk).is_empty(), "it saves with the form");
    // The list is read again, with a tool more: the keys are on Enabled
    // still, a row further down.
    let number = {
        let menu = desk.settings.as_mut().unwrap();
        menu.mcp_reading()
    };
    let tooled = McpServerInfo {
        tools: vec![clackatoo_protocol::McpToolInfo {
            name: "find".into(),
            description: String::new(),
        }],
        state: McpState::Connected,
        ..server("search")
    };
    desk.settings_mcp(
        number,
        Response::Mcp(McpReply {
            servers: vec![tooled],
            ..McpReply::default()
        }),
    );
    let (moved, _) = easy_row(&mut desk, "Enabled");
    assert_eq!(moved, row + 1, "Reconnect is offered now");
    assert_eq!(easy(&mut desk).focus, Some(usize::from(moved)));
}

#[test]
fn rules_this_window_cannot_read_are_said_to_be_and_never_saved_over() {
    let (mut desk, _dir) = easy_configured("");
    // A newer daemon's rule, of a kind this window doesn't know.
    let rules = r#"[{"decision":"deny","kind":"network","target":"example.com"}]"#;
    desk.daemon_settings_came(&with_rules(rules), None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, Page::Permissions);
    let view = easy(&mut desk);
    let banner = view.banner.clone().expect("it says why");
    assert!(banner.contains("can't read"), "{banner}");
    assert_eq!(view.primary, None, "no rule can be added");
    assert!(!hints(&view).contains(&"a"));
    assert_eq!(
        labels(&view),
        ["Who answers agents' requests"],
        "none is shown as if there were none"
    );
    // Nor by its key.
    type_keys(&mut desk, "a");
    assert_eq!(easy(&mut desk).within, None);
    assert!(sent(&mut desk).is_empty());
}

#[test]
fn a_rule_reads_as_a_sentence_and_saves_into_the_list_as_it_is_then() {
    let (mut desk, _dir) = easy_configured("");
    let one = r#"[{"decision":"allow","kind":"command","target":"cargo test"}]"#;
    desk.daemon_settings_came(&with_rules(one), None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, Page::Permissions);
    let view = easy(&mut desk);
    assert!(labels(&view).contains(&"Allow the command `cargo test`".to_owned()));
    assert_eq!(
        view.subtitle,
        "Who answers when an agent asks to do something, for every chat on this machine."
    );
    // A new one: deny changing files in the home directory.
    type_keys(&mut desk, "a");
    type_keys(&mut desk, "l");
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, "lll");
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, "notes");
    // Saved as it is, it says what's wrong under its field.
    press(&mut desk, Key::Character("s".into()), None, CTRL);
    let (_, folder) = easy_row(&mut desk, "Folder");
    assert_eq!(folder.notes.len(), 1, "{folder:?}");
    assert!(sent(&mut desk).is_empty());
    // The list changes meanwhile, somewhere else.
    let two = r#"[{"decision":"allow","kind":"command","target":"cargo test"},{"decision":"ask","kind":"tool","target":"docs/search"}]"#;
    desk.daemon_settings_came(&with_rules(two), None);
    named(&mut desk, NamedKey::Home);
    type_keys(&mut desk, "~/");
    press(&mut desk, Key::Character("s".into()), None, CTRL);
    let asked = sent(&mut desk);
    let [Request::SetPermissionRules { rules, expected }] = &asked[..] else {
        panic!("{asked:?}")
    };
    assert_eq!(expected, two, "over the list as it is now");
    let saved: serde_json::Value = serde_json::from_str(rules).unwrap();
    assert_eq!(saved.as_array().unwrap().len(), 3, "nothing is lost");
    assert_eq!(saved[2]["target"], "~/notes");
    assert_eq!(easy(&mut desk).buttons[1].label, "Saving…");
    // The daemon says it's saved: the page says so in its own words.
    desk.daemon_settings_came(
        &with_rules(rules),
        Some(OptionSaved {
            name: "permissions.rules".into(),
            value: Some(rules.clone()),
            refused: None,
        }),
    );
    let view = easy(&mut desk);
    assert_eq!(view.within, None);
    assert!(view.footer.starts_with("Saved."), "{}", view.footer);
    assert!(!view.footer.contains('{'), "no JSON: {}", view.footer);
    assert_eq!(
        labels(&view)[view.focus.unwrap()],
        "Deny changing files in `~/notes`"
    );
}

#[test]
fn rules_the_daemons_config_sets_are_locked_and_an_agents_input_changes_none() {
    let (mut desk, _dir) = easy_configured("");
    let one = r#"[{"decision":"allow","kind":"command","target":"cargo test"}]"#;
    let mut settings = with_rules(one);
    for option in &mut settings.options {
        if option.name == "permissions.rules" {
            option.by = clackatoo_protocol::OptionSetBy {
                source: clackatoo_protocol::OptionSource::Config,
                at: Some(clackatoo_protocol::OptionPlace {
                    file: "/home/u/.config/clackatoo/daemon/init.lua".into(),
                    line: 7,
                }),
                inner: None,
            };
        }
    }
    desk.daemon_settings_came(&settings, None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, Page::Permissions);
    let view = easy(&mut desk);
    assert_eq!(view.primary, None);
    let (row, rule) = easy_row(&mut desk, "Allow the command `cargo test`");
    assert_eq!(rule.lock.as_deref(), Some("init.lua:7"));
    assert_eq!(rule.control, ControlView::None);
    // Its place opens the line that sets them, on the daemon's machine.
    desk.settings_pointer(SettingsHit::Lock(row));
    assert!(!desk.settings_open());
    let asked = sent(&mut desk);
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Split {
            terminal: PaneTerminal::File(OpenFile { path, .. }), ..
        } if path.ends_with("daemon/init.lua"))),
        "{asked:?}"
    );

    // An agent's keys open no rule to change.
    desk.daemon_settings_came(&with_rules(one), None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, Page::Permissions);
    desk.automating = true;
    click(&mut desk, "Allow the command `cargo test`");
    let view = easy(&mut desk);
    assert_eq!(view.within, None);
    assert!(
        view.banner
            .as_deref()
            .is_some_and(|said| said.contains("yours"))
    );
}

#[test]
fn a_search_finds_servers_specialists_and_rules_by_name_and_opens_them() {
    let (mut desk, _dir) = easy_configured("");
    let one = r#"[{"decision":"allow","kind":"command","target":"cargo test"}]"#;
    desk.daemon_settings_came(&with_rules(one), None);
    command(&mut desk, "Settings");
    let number = mcp_asked(&desk);
    answer_all(&mut desk);
    desk.settings_mcp(
        number,
        Response::Mcp(McpReply {
            servers: vec![server("search")],
            ..McpReply::default()
        }),
    );
    desk.settings_specialists(Response::SpecialistFiles(vec![specialist("reviewer")]));
    let found = |desk: &mut Desk, query: &str| {
        type_keys(desk, &format!("/{query}"));
        let view = easy(desk);
        let headings: Vec<String> = (view.sections.iter())
            .map(|section| section.heading.clone())
            .collect();
        named(desk, NamedKey::Enter);
        headings
    };
    assert!(found(&mut desk, "search").contains(&"MCP servers".to_owned()));
    click(&mut desk, "search");
    let view = easy(&mut desk);
    assert_eq!(
        (view.within.as_deref(), &*view.title),
        (Some("MCP Servers"), "search")
    );
    named(&mut desk, NamedKey::Escape);
    assert!(found(&mut desk, "reviewer").contains(&"Specialists".to_owned()));
    click(&mut desk, "reviewer");
    assert_eq!(easy(&mut desk).title, "reviewer");
    named(&mut desk, NamedKey::Escape);
    assert!(found(&mut desk, "cargo test").contains(&"Permission rules".to_owned()));
    click(&mut desk, "Allow the command `cargo test`");
    assert_eq!(easy(&mut desk).title, "Allow the command `cargo test`");
}

#[test]
fn a_list_says_what_it_is_for_and_why_it_is_empty() {
    let (mut desk, _dir) = on_page(Page::Mcp);
    // Not yet said where: this project, and it's being read.
    let view = easy(&mut desk);
    assert_eq!(view.subtitle, "Servers chats can use, for this project.");
    assert_eq!(view.empty.as_deref(), Some("Reading the servers…"));
    assert!(view.sections.iter().all(|section| section.rows.is_empty()));
    assert_eq!(view.primary, None);
    // The daemon says the directory the focused pane works in.
    desk.settings_agents(Response::ChatChoices(ChatChoices {
        agents: Vec::new(),
        default: "claude".into(),
        cwd: "/home/u/notes".into(),
        home: "/home/u".into(),
        unshown: Vec::new(),
    }));
    assert_eq!(
        easy(&mut desk).subtitle,
        "Servers chats can use, for ~/notes."
    );
    // On another machine, whose.
    desk.remote = true;
    desk.daemon_settings_came(&daemon_said("desk"), None);
    for (page, said) in [
        (Page::Mcp, "Servers chats can use, for ~/notes on desk."),
        (
            Page::Specialists,
            "Roles a chat can hand work to, for ~/notes on desk.",
        ),
        (
            Page::Skills,
            "What agents know how to do, for ~/notes on desk.",
        ),
    ] {
        go_to(&mut desk, page);
        let view = easy(&mut desk);
        assert_eq!(view.subtitle, said);
        assert_eq!(view.machine.as_deref(), Some("daemon on desk"));
    }
    // A daemon that can't list them says so, where the rows would be.
    desk.settings_skills(Response::Error(clackatoo_protocol::Error::Other(
        "no such pane".into(),
    )));
    let view = easy(&mut desk);
    let why = view.empty.as_deref().expect("it says why");
    assert!(
        why.starts_with("The skills couldn't be read") && why.contains("no such pane"),
        "{why}"
    );
    // None at all says how to add one; a section with nothing has no
    // heading.
    desk.settings_skills(Response::Skills(Vec::new()));
    let view = easy(&mut desk);
    assert!(
        view.empty
            .as_deref()
            .is_some_and(|said| said.starts_with("No skills yet"))
    );
    go_to(&mut desk, Page::Specialists);
    desk.settings_specialists(Response::SpecialistFiles(Vec::new()));
    let view = easy(&mut desk);
    let yours = &view.sections[0];
    assert!(yours.rows.is_empty() && yours.note.is_some());
    assert!(view.sections[1].rows.is_empty() && view.sections[1].note.is_none());
}

#[test]
fn a_specialist_is_made_saved_and_its_file_opened_for_its_instructions() {
    let (mut desk, _dir) = specialists_page(Vec::new());
    type_keys(&mut desk, "a");
    type_keys(&mut desk, "reviewer");
    named(&mut desk, NamedKey::Enter);
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, "Reviews changes for bugs");
    named(&mut desk, NamedKey::Enter);
    assert!(sent(&mut desk).is_empty(), "nothing's saved until Save");
    // Its instructions are its file's: saved first, then opened.
    let (_, instructions) = easy_row(&mut desk, "Instructions");
    assert!(
        matches!(&instructions.control, ControlView::Button(button)
            if button.label == "Save, then open its file"),
        "{instructions:?}"
    );
    click(&mut desk, "Instructions");
    let asked = sent(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist: saved,
            scope,
            path,
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}")
    };
    assert_eq!(
        (saved.name.as_str(), *scope, path),
        ("reviewer", SpecialistScope::Personal, &None)
    );
    let settings: serde_json::Value = serde_json::from_str(&saved.settings).unwrap();
    assert_eq!(settings["description"], "Reviews changes for bugs");
    assert!(
        settings["instructions"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert_eq!(settings["limits"]["access"], "none");
    // Refused, the form is kept, and says why in full.
    let saved = saved.clone();
    desk.settings_specialist_saved(
        &saved,
        Response::Error(clackatoo_protocol::Error::Other("disk full".into())),
    );
    let view = easy(&mut desk);
    assert_eq!(view.title, "New specialist");
    assert!(
        view.banner
            .as_deref()
            .is_some_and(|said| said.contains("disk full"))
    );
    // Saved: once the list says where, its file opens beside the work.
    click(&mut desk, "Instructions");
    sent(&mut desk);
    desk.settings_specialist_saved(&saved, Response::SpecialistSaved);
    let asked = sent(&mut desk);
    assert!(
        matches!(&asked[..], [Request::SpecialistFiles { .. }]),
        "{asked:?}"
    );
    let mut file = specialist("reviewer");
    file.scope = SpecialistScope::Personal;
    file.path = "/home/u/.agents/specialists/reviewer/SPECIALIST.md".into();
    desk.settings_specialists(Response::SpecialistFiles(vec![file]));
    assert!(!desk.settings_open());
    let asked = sent(&mut desk);
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Split {
            terminal: PaneTerminal::File(OpenFile { path, .. }), ..
        } if path.ends_with("reviewer/SPECIALIST.md"))),
        "{asked:?}"
    );
}

#[test]
fn a_specialist_that_is_not_yours_is_copied_and_yours_is_saved_where_it_is() {
    let builtin = SpecialistFile {
        path: String::new(),
        scope: SpecialistScope::Personal,
        editable: false,
        ..specialist("explore")
    };
    let (mut desk, _dir) = specialists_page(vec![builtin, specialist("reviewer")]);
    let (_, explore) = easy_row(&mut desk, "explore");
    assert!(
        matches!(&explore.control, ControlView::Button(button) if button.label == "Copy to change")
    );
    assert_eq!(
        explore.status.map(|(said, _)| said).as_deref(),
        Some("Built in")
    );
    click(&mut desk, "explore");
    let view = easy(&mut desk);
    assert_eq!(view.title, "Copy of explore");
    // It says a copy of the same name takes the built-in's place.
    let (_, name) = easy_row(&mut desk, "Name");
    assert!(
        name.notes
            .iter()
            .any(|(note, _)| note.contains("in place of")),
        "{name:?}"
    );
    // Kept in the project: clicking the choice picked keeps it picked.
    let (scope, _) = easy_row(&mut desk, "Kept in");
    desk.settings_pointer(SettingsHit::Control(scope, Part::Segment(1)));
    desk.settings_pointer(SettingsHit::Control(scope, Part::Segment(1)));
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    let asked = sent(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist: saved,
            scope: SpecialistScope::Project,
            path: None,
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}")
    };
    assert_eq!((saved.name.as_str(), &saved.saved), ("explore", &None));
    let settings: serde_json::Value = serde_json::from_str(&saved.settings).unwrap();
    assert_eq!(
        settings["includes"],
        serde_json::json!(["checks"]),
        "what isn't shown is kept"
    );
    assert_eq!(settings["instructions"], "Review.");
    desk.settings_specialist_saved(&saved.clone(), Response::SpecialistSaved);
    sent(&mut desk);

    // One of the user's own is saved over its file, as that was.
    click(&mut desk, "reviewer");
    click(&mut desk, "Agents can hand work to it");
    desk.controlled = true;
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    assert!(sent(&mut desk).is_empty(), "an agent's input saves none");
    assert!(
        easy(&mut desk)
            .banner
            .as_deref()
            .is_some_and(|said| said.contains("yours"))
    );
    desk.controlled = false;
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    let asked = sent(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist: saved,
            path: Some(path),
            ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}")
    };
    assert!(path.ends_with("reviewer/SPECIALIST.md"));
    assert_eq!(saved.saved.as_deref(), Some("the file as it was"));
    // Another menu's answer closes no form of this one's.
    let saved = saved.clone();
    desk.settings_pointer(SettingsHit::Close);
    desk.settings_pointer(SettingsHit::Close);
    command(&mut desk, "Settings");
    go_to(&mut desk, Page::Specialists);
    desk.settings_specialists(Response::SpecialistFiles(vec![specialist("reviewer")]));
    click(&mut desk, "reviewer");
    desk.settings_specialist_saved(&saved, Response::SpecialistSaved);
    assert_eq!(easy(&mut desk).title, "reviewer");
}

#[test]
fn a_specialists_agent_and_model_are_picked_from_what_the_daemon_lists() {
    use clackatoo_protocol::{Provider, ProviderModels, SettingChoice};
    let (mut desk, _dir) = specialists_page(vec![specialist("reviewer")]);
    let model = |value: &str, name: &str| SettingChoice {
        value: value.into(),
        name: name.into(),
        description: None,
    };
    desk.settings_agents(Response::ChatChoices(ChatChoices {
        agents: vec![
            Provider {
                agent: "claude".into(),
                title: "Claude Code".into(),
                models: ProviderModels::Known(vec![model("opus", "Opus"), model("haiku", "Haiku")]),
            },
            Provider {
                agent: "codex".into(),
                title: "Codex".into(),
                models: ProviderModels::Unknown,
            },
        ],
        default: "claude".into(),
        cwd: "/home/u/notes".into(),
        home: "/home/u".into(),
        unshown: Vec::new(),
    }));
    click(&mut desk, "reviewer");
    let chosen = |desk: &mut Desk, label: &str| match easy_row(desk, label).1.control {
        ControlView::Choice(said) => said,
        other => panic!("{other:?}"),
    };
    assert_eq!(chosen(&mut desk, "Agent"), "The chat's");
    // Its list opens under it: the chat's, then each agent.
    click(&mut desk, "Agent");
    let view = easy(&mut desk);
    let listed: Vec<String> = (view.fonts.as_ref().expect("its list").rows.iter())
        .map(|row| row.text.trim_end().to_owned())
        .collect();
    assert_eq!(listed, ["The chat's", "Claude Code", "Codex"]);
    assert_eq!(hints(&view), ["Enter", "Esc"]);
    named(&mut desk, NamedKey::ArrowDown);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(chosen(&mut desk, "Agent"), "Claude Code");
    // Then its models, and the two that are any agent's.
    click(&mut desk, "Model");
    let view = easy(&mut desk);
    let listed: Vec<String> = (view.fonts.as_ref().expect("its list").rows.iter())
        .map(|row| row.text.trim_end().to_owned())
        .collect();
    assert_eq!(listed, ["The agent's usual", "The chat's", "Opus", "Haiku"]);
    desk.settings_pointer(SettingsHit::Font(3));
    assert_eq!(chosen(&mut desk, "Model"), "Haiku");
    // Another agent's model isn't this one's.
    click(&mut desk, "Agent");
    named(&mut desk, NamedKey::ArrowDown);
    named(&mut desk, NamedKey::Enter);
    assert_eq!(chosen(&mut desk, "Agent"), "Codex");
    assert_eq!(chosen(&mut desk, "Model"), "The agent's usual");
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    let asked = sent(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist: saved, ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}")
    };
    let settings: serde_json::Value = serde_json::from_str(&saved.settings).unwrap();
    assert_eq!(settings["agent"], "codex");
    assert_eq!(settings.get("model"), None);
}

#[test]
fn the_agents_page_says_what_each_choice_of_an_agents_own_servers_does() {
    let (mut desk, _dir) = on_page(Page::Agents);
    let view = easy(&mut desk);
    let section = (view.sections.iter())
        .find(|section| section.heading == "Servers from the agent's own configuration")
        .expect("its section");
    let said: Vec<&str> = section.rows.iter().map(|row| &*row.label).collect();
    assert_eq!(
        said,
        [
            "Let agents use their own MCP servers",
            "Claude Code's own servers",
            "Codex's own servers"
        ]
    );
    assert!(matches!(
        &section.rows[1].control,
        ControlView::Segmented { labels, .. } if labels == &["As above", "Never", "Always"]
    ));
    // And the servers' own page has none of that over its list.
    go_to(&mut desk, Page::Mcp);
    assert!(
        !labels(&easy(&mut desk))
            .iter()
            .any(|label| label.contains("own"))
    );
}

#[test]
fn every_way_of_closing_the_menu_asks_before_a_changed_form_goes() {
    let (mut desk, _dir) = specialists_page(Vec::new());
    let changed = |desk: &mut Desk| {
        desk.settings_pointer(SettingsHit::Primary);
        type_keys(desk, "reviewer");
        named(desk, NamedKey::Enter);
    };
    // The key that goes back to normal mode from anywhere.
    changed(&mut desk);
    normal_mode(&mut desk);
    assert!(desk.settings_open(), "Ctrl+; closed it");
    assert!(easy(&mut desk).footer.starts_with("Discard"));
    // Another key between, and it asks again.
    named(&mut desk, NamedKey::Tab);
    normal_mode(&mut desk);
    assert!(desk.settings_open());
    normal_mode(&mut desk);
    assert!(!desk.settings_open(), "asked twice, it goes");

    // `:Settings` again, as a script or an agent's command runs it, and
    // the cog by a chat's specialists, leave the form as it is.
    command(&mut desk, "Settings");
    go_to(&mut desk, Page::Specialists);
    desk.settings_specialists(Response::SpecialistFiles(Vec::new()));
    changed(&mut desk);
    desk.open_settings("wrap");
    let view = easy(&mut desk);
    assert_eq!(
        (view.title.as_str(), view.query.as_str()),
        ("New specialist", "")
    );
    named(&mut desk, NamedKey::Tab);
    desk.open_specialist_settings();
    assert_eq!(easy(&mut desk).title, "New specialist");
    let (_, name) = easy_row(&mut desk, "Name");
    assert!(matches!(&name.control, ControlView::Field(field) if field.text == "reviewer"));
    // And so does anything else that opens over the menu.
    named(&mut desk, NamedKey::Tab);
    desk.close_finder();
    assert!(desk.settings_open());
    // A form with no changes is no reason to stay.
    named(&mut desk, NamedKey::Escape);
    named(&mut desk, NamedKey::Escape);
    desk.settings_pointer(SettingsHit::Primary);
    normal_mode(&mut desk);
    assert!(!desk.settings_open());
}

#[test]
fn enter_never_goes_on_to_delete_and_any_key_takes_back_its_first_press() {
    let (mut desk, _dir) = easy_configured("");
    let one = r#"[{"decision":"allow","kind":"command","target":"cargo test"}]"#;
    desk.daemon_settings_came(&with_rules(one), None);
    command(&mut desk, "Settings");
    answer_all(&mut desk);
    go_to(&mut desk, Page::Permissions);
    click(&mut desk, "Allow the command `cargo test`");
    // To the command, more typed, and Enter: the keys stay on it, as
    // Delete is next. Enter again only starts typing again.
    named(&mut desk, NamedKey::Tab);
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, " --all");
    let (command_row, _) = easy_row(&mut desk, "Command");
    for _ in 0..3 {
        named(&mut desk, NamedKey::Enter);
        assert_eq!(easy(&mut desk).focus, Some(usize::from(command_row)));
    }
    assert!(sent(&mut desk).is_empty(), "nothing was deleted");
    named(&mut desk, NamedKey::Escape);
    // Tab reaches Delete; its first press arms it, and says so.
    named(&mut desk, NamedKey::Tab);
    let delete = |desk: &mut Desk| match easy_row(desk, "Delete this rule").1.control {
        ControlView::Button(button) => button.label,
        other => panic!("{other:?}"),
    };
    named(&mut desk, NamedKey::Enter);
    assert_eq!(delete(&mut desk), "Really delete");
    assert!(easy(&mut desk).footer.starts_with("Delete this rule?"));
    // Any other key takes that back, and the question with it.
    named(&mut desk, NamedKey::ArrowUp);
    assert_eq!(delete(&mut desk), "Delete");
    assert_eq!(easy(&mut desk).footer, "");
    // And so does a click on anything but its button.
    named(&mut desk, NamedKey::ArrowDown);
    named(&mut desk, NamedKey::Enter);
    let (row, _) = easy_row(&mut desk, "Command");
    desk.settings_pointer(SettingsHit::Row(row));
    assert_eq!(delete(&mut desk), "Delete");
    // Pressed twice, it deletes.
    click(&mut desk, "Delete this rule");
    click(&mut desk, "Delete this rule");
    let asked = sent(&mut desk);
    assert!(
        matches!(&asked[..], [Request::SetPermissionRules { rules, .. }] if rules == "[]"),
        "{asked:?}"
    );
}

#[test]
fn a_rule_is_never_saved_into_a_list_that_became_unreadable_under_its_form() {
    let (mut desk, _dir) = on_page(Page::Permissions);
    type_keys(&mut desk, "a");
    named(&mut desk, NamedKey::Tab);
    named(&mut desk, NamedKey::Tab);
    type_keys(&mut desk, "clackatoo/list_panes");
    // Another window, of a newer build, saves a rule this one can't read.
    let newer = r#"[{"decision":"deny","kind":"network","target":"example.com"}]"#;
    desk.daemon_settings_came(&with_rules(newer), None);
    press(&mut desk, Key::Character("s".into()), None, CTRL);
    assert!(
        sent(&mut desk).is_empty(),
        "it would save over the rule it can't read"
    );
    let view = easy(&mut desk);
    assert_eq!(view.title, "New rule", "the form is kept");
    assert!(
        view.banner
            .as_deref()
            .is_some_and(|said| said.contains("can't read"))
    );
}

#[test]
fn a_form_being_saved_changes_no_more_and_a_list_of_choices_goes_with_its_form() {
    let (mut desk, _dir) = specialists_page(vec![specialist("reviewer")]);
    click(&mut desk, "reviewer");
    click(&mut desk, "You can start it");
    desk.settings_pointer(SettingsHit::Foot(Foot::Save));
    let asked = sent(&mut desk);
    let [
        Request::SaveSpecialistFile {
            specialist: saved, ..
        },
    ] = &asked[..]
    else {
        panic!("{asked:?}")
    };
    // While the daemon has it, neither keys nor clicks change the form.
    let described = |desk: &mut Desk| match easy_row(desk, "Description").1.control {
        ControlView::Field(field) => field.text,
        other => panic!("{other:?}"),
    };
    let (row, _) = easy_row(&mut desk, "Description");
    desk.settings_pointer(SettingsHit::Control(row, Part::Caret(0)));
    type_keys(&mut desk, "late ");
    desk.paste_into_settings("later");
    click(&mut desk, "Agents can hand work to it");
    assert_eq!(described(&mut desk), "Reviews");
    assert_eq!(
        easy_row(&mut desk, "Agents can hand work to it").1.control,
        ControlView::Toggle(true)
    );
    // Saved: what's saved is what was sent, and the form goes.
    desk.settings_specialist_saved(&saved.clone(), Response::SpecialistSaved);
    assert_eq!(easy(&mut desk).title, "Specialists");

    // A list of choices open under a row goes when its form is cancelled.
    click(&mut desk, "reviewer");
    click(&mut desk, "Agent");
    assert!(easy(&mut desk).fonts.is_some());
    desk.settings_pointer(SettingsHit::Foot(Foot::Cancel));
    let view = easy(&mut desk);
    assert_eq!(view.title, "Specialists");
    assert_eq!(view.fonts, None, "no list left over the page");
    type_keys(&mut desk, "a");
    assert_eq!(
        easy(&mut desk).title,
        "New specialist",
        "and the keys are the page's"
    );
}

#[test]
fn a_built_in_specialist_looked_at_closes_without_a_question() {
    let builtin = SpecialistFile {
        path: String::new(),
        scope: SpecialistScope::Personal,
        editable: false,
        ..specialist("explore")
    };
    // One of the user's own has its name: each row opens its own.
    let (mut desk, _dir) = specialists_page(vec![builtin, specialist("explore")]);
    let view = easy(&mut desk);
    let rows: Vec<(String, Option<String>)> = (view.sections.iter())
        .flat_map(|section| &section.rows)
        .map(|row| (row.label.clone(), row.status.clone().map(|(said, _)| said)))
        .collect();
    assert_eq!(
        rows,
        [
            ("explore".to_owned(), Some("This project".to_owned())),
            ("explore".to_owned(), Some("Built in".to_owned()))
        ]
    );
    desk.settings_pointer(SettingsHit::Control(1, Part::Whole));
    assert_eq!(easy(&mut desk).title, "Copy of explore");
    named(&mut desk, NamedKey::Escape);
    let view = easy(&mut desk);
    assert_eq!(
        view.title, "Specialists",
        "nothing was changed, so nothing's asked"
    );
    assert_eq!(view.focus, Some(1), "back on the built-in's row");
    desk.settings_pointer(SettingsHit::Control(0, Part::Whole));
    assert_eq!(easy(&mut desk).title, "explore");
}

#[test]
fn a_paste_loses_its_line_breaks_and_one_too_long_says_so() {
    let (mut desk, _dir) = mcp_page(Vec::new());
    type_keys(&mut desk, "a");
    // A name copied as a line.
    desk.paste_into_settings("docs\n");
    let name = |desk: &mut Desk| match easy_row(desk, "Name").1.control {
        ControlView::Field(field) => field.text,
        other => panic!("{other:?}"),
    };
    assert_eq!(name(&mut desk), "docs");
    // More than the box holds: none of it goes in, and the foot says so.
    desk.paste_into_settings(&"x".repeat(80));
    assert_eq!(name(&mut desk), "docs");
    let view = easy(&mut desk);
    assert!(
        view.warning && view.footer.contains("more than this box holds"),
        "{}",
        view.footer
    );
}

/// A skill the daemon lists, with why it can't be used, if it can't.
fn skill(name: &str, path: &str, problem: Option<&str>) -> clackatoo_protocol::Skill {
    clackatoo_protocol::Skill {
        name: name.into(),
        description: "Does something.".into(),
        path: path.into(),
        source: "Project".into(),
        problem: problem.map(str::to_owned),
        implicit: true,
        user_invocable: true,
    }
}

#[test]
fn a_skill_is_switched_off_by_its_switch_and_what_is_not_loaded_says_why() {
    let (mut desk, _dir) = on_page(Page::Skills);
    let untrusted = "not loaded: this folder isn't trusted (clackatoo trust, or :trust, trusts it)";
    desk.settings_skills(Response::Skills(vec![
        skill("deploy", "/p/.agents/skills/deploy/SKILL.md", None),
        skill("migrate", "/p/.agents/skills/migrate/SKILL.md", None),
        skill(
            "broken",
            "/p/.agents/skills/broken/SKILL.md",
            Some("no front matter"),
        ),
        skill("auditor", "/p/.agents/specialists/auditor", Some(untrusted)),
    ]));
    let view = easy(&mut desk);
    let headings: Vec<&str> = view.sections.iter().map(|each| &*each.heading).collect();
    assert_eq!(headings, ["", "Not loaded"]);
    assert!(
        view.sections[1]
            .note
            .as_deref()
            .is_some_and(|said| said.contains(":trust"))
    );
    // What can be used has a switch, on, and a button for its file.
    let (_, deploy) = easy_row(&mut desk, "/deploy");
    assert!(
        matches!(&deploy.control, ControlView::Switched { on: true, button }
            if button.label == "Open its file"),
        "{deploy:?}"
    );
    // What can't says why, as the daemon does; a broken skill's file can
    // still be opened, a specialist's folder can't.
    let (_, broken) = easy_row(&mut desk, "/broken");
    assert!(
        matches!(&broken.control, ControlView::Button(_)),
        "{broken:?}"
    );
    assert!(
        broken
            .notes
            .iter()
            .any(|(note, _)| note.contains("no front matter"))
    );
    let (_, auditor) = easy_row(&mut desk, "auditor");
    assert_eq!(auditor.control, ControlView::None);
    assert!(
        auditor
            .notes
            .iter()
            .any(|(note, _)| note.contains("isn't trusted"))
    );

    // Enter on a skill switches it off: the daemon's option names it.
    named(&mut desk, NamedKey::Enter);
    let view = easy(&mut desk);
    assert_eq!(hints(&view), ["Enter", "o", "/", "Esc"]);
    named(&mut desk, NamedKey::Enter);
    let asked = sent(&mut desk);
    let off = |asked: &[Request]| match asked {
        [Request::SetDaemonOption { name, value }] if name == "skills.disabled" => value.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(off(&asked).as_deref(), Some("deploy"));
    let (_, deploy) = easy_row(&mut desk, "/deploy");
    assert!(
        matches!(&deploy.control, ControlView::Switched { on: false, .. }),
        "{deploy:?}"
    );
    assert!(
        deploy
            .notes
            .iter()
            .any(|(note, _)| note.starts_with("Switched off"))
    );
    // A click on another's switch adds it; its button opens its file.
    let (row, _) = easy_row(&mut desk, "/migrate");
    desk.settings_pointer(SettingsHit::Control(row, Part::Whole));
    assert_eq!(off(&sent(&mut desk)).as_deref(), Some("deploy, migrate"));
    // The daemon lists a skill switched off with that as its problem: it
    // stays among the skills, with its switch.
    desk.settings_skills(Response::Skills(vec![
        skill(
            "deploy",
            "/p/.agents/skills/deploy/SKILL.md",
            Some("switched off"),
        ),
        skill(
            "migrate",
            "/p/.agents/skills/migrate/SKILL.md",
            Some("switched off"),
        ),
    ]));
    assert_eq!(easy(&mut desk).sections.len(), 1);
    // `l` switches the one the keys are on, the one last clicked, on
    // again, as it steps any switch.
    type_keys(&mut desk, "l");
    assert_eq!(off(&sent(&mut desk)).as_deref(), Some("deploy"));
    // `o`, or its button, opens its file.
    type_keys(&mut desk, "o");
    assert!(!desk.settings_open());
    let asked = sent(&mut desk);
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Split {
            terminal: PaneTerminal::File(OpenFile { path, .. }), ..
        } if path.ends_with("migrate/SKILL.md"))),
        "{asked:?}"
    );
}

/// One of the user's own servers, and a project's nobody has reviewed
/// that has its name.
fn pair() -> (McpServerInfo, McpServerInfo) {
    let yours = McpServerInfo {
        source: "Personal".into(),
        path: "/home/u/.config/clackatoo/mcp.json".into(),
        scope: McpScope::Personal,
        ..server("search")
    };
    let projects = McpServerInfo {
        settings: r#"{"command":"other-search","args":["--all"],"enabled":true}"#.into(),
        trusted: false,
        revision: "unreviewed".into(),
        ..server("search")
    };
    (yours, projects)
}

#[test]
fn a_projects_unreviewed_server_is_listed_beside_yours_of_its_name_and_only_shown() {
    let (yours, projects) = pair();
    let (mut desk, _dir) = mcp_page(vec![projects, yours]);
    // Two rows of one name, told apart by where they're kept: yours first.
    let rows = |desk: &mut Desk| -> Vec<(String, String)> {
        (easy(desk).sections[0].rows.iter())
            .map(|row| {
                let status = row.status.clone().map(|(said, _)| said).unwrap_or_default();
                (row.label.clone(), status)
            })
            .collect()
    };
    assert_eq!(
        rows(&mut desk),
        [
            ("search".to_owned(), "Not in use".to_owned()),
            ("search".to_owned(), "Needs review".to_owned())
        ]
    );
    let view = easy(&mut desk);
    let waiting = &view.sections[0].rows[1];
    assert!(waiting.help.starts_with("This project"), "{waiting:?}");
    assert!(
        waiting
            .notes
            .iter()
            .any(|(note, _)| note.contains("takes its place"))
    );
    // Yours opens as any server does, to change, test and remove.
    desk.settings_pointer(SettingsHit::Control(0, Part::Segment(0)));
    let view = easy(&mut desk);
    assert_eq!(view.title, "search");
    assert!(labels(&view).contains(&"Test".to_owned()));
    assert!(labels(&view).contains(&"Remove search".to_owned()));
    assert!(matches!(
        easy_row(&mut desk, "Command").1.control,
        ControlView::Field(_)
    ));
    named(&mut desk, NamedKey::Escape);
    // The project's is only shown, with its one action.
    desk.settings_pointer(SettingsHit::Control(1, Part::Segment(0)));
    let view = easy(&mut desk);
    assert_eq!(view.title, "search, as this project has it");
    assert!(
        view.subtitle.contains("your own server called search"),
        "{}",
        view.subtitle
    );
    let shown = labels(&view);
    assert!(shown.contains(&"Review and enable".to_owned()));
    for label in ["Test", "Remove search", "Forget the sign in"] {
        assert!(!shown.contains(&label.to_owned()), "{shown:?}");
    }
    let (_, command) = easy_row(&mut desk, "Command");
    assert_eq!(command.control, ControlView::None, "nothing to type into");
    assert!(command.notes.iter().any(|(note, _)| note == "other-search"));
    let said: Vec<&str> = view.buttons.iter().map(|each| &*each.label).collect();
    assert_eq!(said, ["Back"], "and nothing to save");
    assert_eq!(hints(&view), ["Tab", "Esc"]);
    press(&mut desk, Key::Character("s".into()), None, CTRL);
    type_keys(&mut desk, "x");
    assert!(sent(&mut desk).is_empty());
    // The keys start on its one button, which reviews the project's.
    named(&mut desk, NamedKey::Enter);
    let asked = sent(&mut desk);
    assert!(
        matches!(&asked[..], [Request::Mcp {
            action: McpAction::Trust { name, expected },
            ..
        }] if name == "search" && expected == "unreviewed"),
        "{asked:?}"
    );
}

#[test]
fn the_keys_and_a_search_tell_two_servers_of_one_name_apart() {
    let (yours, projects) = pair();
    let (mut desk, _dir) = mcp_page(vec![projects.clone(), yours.clone()]);
    // On the project's row, the keys stay on it when the list is read
    // again with another server before it.
    named(&mut desk, NamedKey::Enter);
    named(&mut desk, NamedKey::ArrowDown);
    assert_eq!(easy(&mut desk).focus, Some(1));
    let number = desk.settings.as_mut().unwrap().mcp_reading();
    desk.settings_mcp(
        number,
        Response::Mcp(McpReply {
            servers: vec![yours.clone(), server("alpha"), projects.clone()],
            ..McpReply::default()
        }),
    );
    assert_eq!(easy(&mut desk).focus, Some(2));
    named(&mut desk, NamedKey::Enter);
    assert_eq!(easy(&mut desk).title, "search, as this project has it");
    named(&mut desk, NamedKey::Escape);

    // A search finds both, and each opens its own.
    type_keys(&mut desk, "/search");
    named(&mut desk, NamedKey::Enter);
    let view = easy(&mut desk);
    let found: Vec<u16> = (view.sections.iter().flat_map(|section| &section.rows))
        .enumerate()
        .filter(|(_, row)| row.label == "search" && row.help.starts_with("MCP Servers"))
        .map(|(at, _)| u16::try_from(at).unwrap())
        .collect();
    assert_eq!(found.len(), 2, "{view:?}");
    assert!(
        view.sections
            .iter()
            .flat_map(|section| &section.rows)
            .any(|row| {
                row.label == "search"
                    && row.help == "MCP Servers · This project, waiting to be reviewed"
            })
    );
    desk.settings_pointer(SettingsHit::Control(found[1], Part::Whole));
    assert_eq!(easy(&mut desk).title, "search, as this project has it");

    // Enabled, the daemon lists the project's alone: its form is one to
    // change, as any other.
    let enabled = McpServerInfo {
        trusted: true,
        ..projects
    };
    let number = desk.settings.as_mut().unwrap().mcp_reading();
    desk.settings_mcp(
        number,
        Response::Mcp(McpReply {
            servers: vec![enabled],
            ..McpReply::default()
        }),
    );
    let view = easy(&mut desk);
    assert_eq!(view.title, "search");
    assert!(matches!(
        easy_row(&mut desk, "Command").1.control,
        ControlView::Field(_)
    ));
}
