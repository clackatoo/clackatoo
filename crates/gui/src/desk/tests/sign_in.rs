//! A chat's Sign in to a managed MCP server its agent can't use until the
//! user signs in: asked of the daemon as Settings' Sign in is, for the
//! chat's project, and never for an agent at the keys.

use clackatoo_protocol::{ChatNode, McpAction, McpReply, McpSignIn, McpSignInState};

use super::*;

/// A desk showing chat 2, whose agent needs a sign in to "docs".
fn chat_desk() -> Desk {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, pane(2))],
    )]));
    let node = ChatNode {
        agent: "Codex".into(),
        mcp_sign_in: vec![McpSignIn {
            server: "docs".into(),
            state: McpSignInState::Needed,
        }],
        ..ChatNode::default()
    };
    desk.view_changed(
        TerminalId(2),
        &ViewUpdate::Snapshot(View {
            title: "chat 2".into(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(node)),
        }),
    );
    answer_all(&mut desk);
    desk
}

fn click_sign_in(desk: &mut Desk) {
    let chat = desk.chats.get_mut(&TerminalId(2)).unwrap();
    chat.view_mut().sign_ins.push("docs".into());
    let mut fonts = crate::chat::fixtures::fonts();
    desk.settle_chats(&mut fonts);
}

#[test]
fn a_chats_sign_in_asks_the_daemon_to_sign_in_for_the_chats_pane() {
    let mut desk = chat_desk();
    click_sign_in(&mut desk);
    let asked = desk.take_outbox();
    let wanted = ClientMessage::Request(Request::Mcp {
        pane: Some(PaneId(2)),
        action: McpAction::Authenticate {
            name: "docs".into(),
        },
    });
    assert!(asked.contains(&wanted), "{asked:?}");
    // What it answers: the login opens, or why there's none is said.
    assert!(desk.chat_mcp_signed_in(Response::Mcp(McpReply {
        notice: "Sign in is available for HTTP servers".into(),
        ..McpReply::default()
    })));
    assert!(note(&desk).contains("HTTP servers"), "{}", note(&desk));
}

#[test]
fn an_agent_at_the_keys_cant_sign_in_to_a_server() {
    let mut desk = chat_desk();
    desk.automating = true;
    click_sign_in(&mut desk);
    let asked = desk.take_outbox();
    assert!(
        !asked
            .iter()
            .any(|m| matches!(m, ClientMessage::Request(Request::Mcp { .. }))),
        "{asked:?}"
    );
    assert!(note(&desk).contains("Only the user"), "{}", note(&desk));
}
