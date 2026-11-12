use super::*;
use crate::options::{Case, Store};
use crate::settings::{Facts, Keys};

fn menu() -> SettingsMenu {
    let (store, keys) = (Store::default(), Keys::default());
    SettingsMenu::new(
        "",
        Case::default(),
        Mode::Easy,
        &Facts {
            store: &store,
            window: 1,
            keys: &keys,
            dir: None,
            automating: false,
            daemon: None,
        },
        0,
    )
}
fn server() -> McpServerInfo {
    McpServerInfo {name:"search".into(),settings:json!({"command":"search-server","args":["--stdio"],"env":{"KEY":{"secret":"search-key"}},"enabled":true,"timeout_seconds":30}).to_string(),source:"Project".into(),path:"/project/.agents/mcp.json".into(),revision:"saved-revision".into(),editable:true,scope:McpScope::Project,trusted:false,enabled:true,state:McpState::Disconnected,detail:String::new(),tools:Vec::new(),signing_in:false}
}
#[test]
fn polling_preserves_a_unicode_draft_and_stale_window_responses_are_ignored() {
    let mut menu = menu();
    let saved = server();
    menu.mcp.entries = vec![saved.clone()];
    menu.mcp.view = View::Edit(Editor::new(Some(saved.clone())));
    menu.mcp_act_part(3, None);
    menu.mcp_paste("é");
    menu.mcp_loading();
    let id = menu.mcp_id();
    menu.mcp_came(id + 1, Ok(McpReply::default()));
    assert!(menu.mcp.pending.is_some());
    menu.mcp_came(
        id,
        Ok(McpReply {
            servers: vec![saved],
            ..McpReply::default()
        }),
    );
    assert!(menu.mcp.editor().unwrap().fields[3].ends_with('é'));
    menu.mcp_key(Stroke::Backspace);
    assert_eq!(menu.mcp.editor().unwrap().fields[3], "search-server");
    assert!(
        !menu
            .mcp
            .editor()
            .unwrap()
            .fields
            .iter()
            .any(|s| s.contains("secret-value"))
    );
}
#[test]
fn save_failures_preserve_drafts_pending_mutations_freeze_fields_and_secrets_are_masked() {
    let mut menu = menu();
    menu.mcp.view = View::Edit(Editor::new(Some(server())));
    menu.mcp_act_part(3, None);
    menu.mcp_paste("-changed");
    menu.finish_mcp_field();
    let Outcome::Mcp { action, id } = menu.mcp_act(FIELDS.len()) else {
        panic!("save not requested")
    };
    assert!(
        matches!(&action,McpAction::Save {expected:Some(revision),scope:McpScope::Project,..} if revision=="saved-revision")
    );
    menu.mcp_act(3);
    menu.mcp_paste("must-not-change");
    assert!(!menu.mcp.editor().unwrap().fields[3].contains("must-not-change"));
    menu.mcp_came(id, Err("changed outside Settings".into()));
    assert!(menu.mcp.editor().unwrap().fields[3].ends_with("-changed"));
    menu.mcp.view = View::Secret(Editor::plain(2));
    menu.mcp_act(1);
    menu.mcp_paste("private-value");
    assert!(!display(menu.mcp.editor().unwrap(), 1, true).contains("private-value"));
    assert!(
        !format!(
            "{:?}",
            McpAction::Secret {
                name: "key".into(),
                value: "private-value".into()
            }
        )
        .contains("private-value")
    );
    menu.mcp.view = View::Callback("search".into(), Editor::plain(1));
    menu.mcp_act(0);
    menu.mcp_paste("http://localhost/callback?code=private-code");
    assert!(!display(menu.mcp.editor().unwrap(), 0, true).contains("private-code"));
}
#[test]
fn provider_import_requires_selection_and_keeps_unsupported_entries_unselected() {
    let mut menu = menu();
    let Outcome::Mcp { id, .. } = menu.mcp_request(McpAction::Import {
        provider: "claude".into(),
        names: vec![],
    }) else {
        panic!("preview not requested")
    };
    menu.mcp_came(
        id,
        Ok(McpReply {
            imports: vec![
                McpImport {
                    name: "supported".into(),
                    description: "HTTP server".into(),
                },
                McpImport {
                    name: "unsupported".into(),
                    description: "Cannot import policy".into(),
                },
            ],
            ..McpReply::default()
        }),
    );
    assert!(matches!(menu.mcp_act(3), Outcome::Changed));
    assert!(menu.mcp.pending.is_none());
    menu.mcp_act(2);
    menu.mcp_act(1);
    let Outcome::Mcp {
        action: McpAction::Import { names, .. },
        ..
    } = menu.mcp_act(3)
    else {
        panic!("import not requested")
    };
    assert_eq!(names, ["supported"]);
}
#[test]
fn a_server_needing_a_login_has_a_sign_in_button_that_says_how_it_goes() {
    let mut menu = menu();
    let locked = McpServerInfo {
        name: "docs".into(),
        state: McpState::NeedsLogin,
        ..server()
    };
    menu.mcp.entries = vec![server(), locked.clone()];
    // The list's five buttons, then the servers, by name.
    let (fine, at) = (5, 6);
    assert_eq!(menu.mcp_row(fine).control, ControlView::Button("Manage"));
    let buttons = |menu: &SettingsMenu| match menu.mcp_row(at).control {
        ControlView::Buttons { buttons, at } => (buttons, at),
        other => panic!("not buttons: {other:?}"),
    };
    assert_eq!(
        buttons(&menu),
        (vec![("Sign in", true), ("Manage", true)], 0)
    );
    // Its button, clicked or with Enter, signs in, as Login does.
    let Outcome::Mcp { action, id } = menu.mcp_act_part(at, Some(Part::Segment(0))) else {
        panic!("no sign in")
    };
    assert!(
        action
            == McpAction::Authenticate {
                name: "docs".into()
            }
    );
    assert_eq!(buttons(&menu).0[0], ("Signing in…", false));
    menu.mcp_came(
        id,
        Ok(McpReply {
            login_url: Some("https://example.com/authorize".into()),
            ..McpReply::default()
        }),
    );
    menu.mcp.entries[1].signing_in = true;
    assert_eq!(buttons(&menu).0[0], ("Signing in…", false));
    assert_eq!(
        menu.mcp_act_part(at, Some(Part::Segment(0))),
        Outcome::Nothing
    );
    // Then what came of it: failed, and why, so it can be tried again...
    menu.mcp.entries[1] = McpServerInfo {
        detail: "The browser closed".into(),
        ..locked.clone()
    };
    assert_eq!(buttons(&menu).0[0], ("Sign in", true));
    assert_eq!(
        menu.mcp_row(at).notes,
        [("The browser closed".to_owned(), Ink::Warning)]
    );
    let Outcome::Mcp { id, .. } = menu.mcp_enter(at) else {
        panic!("Enter doesn't sign in")
    };
    let url = Some("https://example.com/authorize".to_owned());
    assert_eq!(
        menu.mcp_came(
            id,
            Ok(McpReply {
                login_url: url.clone(),
                ..McpReply::default()
            })
        ),
        url
    );
    // ...or signed in.
    menu.mcp.entries[1].state = McpState::Ready;
    menu.mcp.entries[1].detail.clear();
    assert_eq!(buttons(&menu).0[0], ("Signed in", false));
    assert!(menu.mcp_row(at).notes.is_empty());
    // `l` takes the keys to Manage, which Enter opens.
    menu.mcp.entries[1].state = McpState::NeedsLogin;
    assert_eq!(menu.mcp_step(at, 1), Outcome::Changed);
    assert_eq!(buttons(&menu).1, 1);
    assert_eq!(menu.mcp_step(at, 1), Outcome::Nothing);
    assert_eq!(menu.mcp_enter(at), Outcome::Changed);
    assert_eq!(menu.mcp_title(), "MCP · docs");
}
#[test]
fn a_sign_in_the_daemon_refuses_says_why_on_its_row() {
    let mut menu = menu();
    menu.mcp.entries = vec![McpServerInfo {
        state: McpState::NeedsLogin,
        ..server()
    }];
    let Outcome::Mcp { id, .. } = menu.mcp_act_part(5, Some(Part::Segment(0))) else {
        panic!("no sign in")
    };
    let why = "Sign in is available for HTTP servers";
    menu.mcp_came(
        id,
        Ok(McpReply {
            servers: menu.mcp.entries.clone(),
            notice: why.into(),
            ..McpReply::default()
        }),
    );
    assert_eq!(menu.mcp_row(5).notes, [(why.to_owned(), Ink::Warning)]);
}
