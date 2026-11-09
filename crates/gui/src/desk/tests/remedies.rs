//! What a failed turn's Sign in and New chat buttons, and `gl` and `gC`,
//! do in the window (CLACKA-233): ask the daemon, which opens the tab on
//! the chat's machine, and show the pane it made.

use clackatoo_protocol::{ChatAction, ChatNode};

use super::*;

/// A desk showing chat 2.
fn chat_desk() -> Desk {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, pane(2))],
    )]));
    let node = ChatNode {
        agent: "Codex".into(),
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

#[test]
fn a_remedy_asked_for_goes_to_the_daemon_for_the_chats_turn() {
    let mut desk = chat_desk();
    let chat = desk.chats.get_mut(&TerminalId(2)).unwrap();
    chat.view_mut().remedies.push(7);
    let mut fonts = crate::chat::fixtures::fonts();
    desk.settle_chats(&mut fonts);
    let asked = desk.take_outbox();
    let wanted = ClientMessage::Request(Request::Chat {
        terminal: TerminalId(2),
        action: ChatAction::Remedy { turn: 7 },
    });
    assert!(asked.contains(&wanted), "{asked:?}");
}
