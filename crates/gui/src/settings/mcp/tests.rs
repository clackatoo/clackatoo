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
    McpServerInfo {name:"search".into(),settings:json!({"command":"search-server","args":["--stdio"],"env":{"KEY":{"secret":"search-key"}},"enabled":true,"timeout_seconds":30}).to_string(),source:"Project".into(),path:"/project/.agents/mcp.json".into(),revision:"saved-revision".into(),editable:true,scope:McpScope::Project,trusted:false,enabled:true,state:McpState::Disconnected,detail:String::new(),tools:Vec::new()}
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
