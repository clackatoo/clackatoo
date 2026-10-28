//! Subagents' panes: the band across the top of each that says what it
//! is, going from it to the chat that started it, and closing its pane,
//! which the daemon answers by sending the chat back to the background,
//! and `:bd`, which closes it for good.

use super::*;

/// A desk with chat 2 in the first tab and chat 3, which chat 2's agent
/// started as `explore`, in the second, shown, with the list of chats
/// saying so.
fn subagent_desk() -> Desk {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, pane(2)), tab(2, pane(3))],
    )]));
    for chat in [2, 3] {
        desk.view_changed(
            TerminalId(chat),
            &ViewUpdate::Snapshot(View {
                title: format!("chat {chat}"),
                cwd: None,
                root: clackatoo_protocol::Node::Chat(Box::default()),
            }),
        );
    }
    desk.show_tab(TabId(2));
    desk.chats_changed(vec![summary(2, 1, ChatState::Ready), subagent(3, 2)]);
    answer_all(&mut desk);
    desk
}

/// Chat `chat`, which chat `parent`'s agent started as `explore`, as the
/// list of chats sums it up.
fn subagent(chat: u32, parent: u32) -> ChatSummary {
    ChatSummary {
        parent: Some(TerminalId(parent)),
        profile: Some("explore".into()),
        ..summary(chat, 1, ChatState::Ready)
    }
}

/// What the band over chat `chat`'s pane says, all of it.
fn band_says(desk: &Desk, chat: u32) -> String {
    let band = desk.subagent_band(TerminalId(chat)).expect("a band");
    band.parts().into_iter().map(|(part, _)| part).collect()
}

#[test]
// Whole numbers of pixels, compared exactly.
#[allow(clippy::float_cmp)]
fn a_subagents_pane_says_what_it_is_and_goes_to_the_chat_that_started_it() {
    let mut desk = subagent_desk();
    assert_eq!(
        band_says(&desk, 3),
        "↳ subagent of chat 2 · explore   ^B u parent chat"
    );
    // Half as tall again as a row, and only over a subagent.
    assert_eq!(desk.band_height(TerminalId(3)), 30.0);
    assert_eq!(desk.band_height(TerminalId(2)), 0.0);
    assert!(desk.subagent_band(TerminalId(2)).is_none());

    // Ctrl+B u goes to the chat that started it, in its other tab.
    ctrl_b(&mut desk, "u");
    assert_eq!(desk.shown().unwrap().pane, PaneId(2));
    // From a chat no agent started, it says why not.
    ctrl_b(&mut desk, "u");
    assert_eq!(desk.shown().unwrap().pane, PaneId(2));
    assert_eq!(
        note(&desk),
        "this pane isn't a subagent's: no agent started it"
    );

    // So does a click on the band, but not one on the chat below it.
    desk.show_tab(TabId(2));
    click(&mut desk, cell_point(1, 3), Modifiers::default());
    assert_eq!(desk.shown().unwrap().pane, PaneId(3), "the chat's own");
    click(&mut desk, cell_point(0, 3), Modifiers::default());
    assert_eq!(desk.shown().unwrap().pane, PaneId(2));

    // The key the band names is the one bound, and with none, none.
    desk.lua_from(r#"client.keys.unmap("niv", "<C-b>u")"#);
    assert_eq!(band_says(&desk, 3), "↳ subagent of chat 2 · explore");
}

#[test]
fn a_subagent_whose_chat_closed_says_so_and_goes_nowhere() {
    let mut desk = subagent_desk();
    desk.chats_changed(vec![subagent(3, 2)]);
    assert_eq!(
        band_says(&desk, 3),
        "↳ subagent of chat 2, which has closed · explore"
    );
    ctrl_b(&mut desk, "u");
    assert_eq!(desk.shown().unwrap().pane, PaneId(3));
    assert_eq!(note(&desk), "chat 2, which started this one, has closed");
    // Its pane closing closes it, so the bar says nothing of a card.
    normal_mode(&mut desk);
    command(&mut desk, "q");
    assert_eq!(
        answer_all(&mut desk),
        [Request::ClosePane { pane: PaneId(3) }]
    );
    desk.chats_changed(Vec::new());
    desk.workspaces_changed(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, pane(2))],
    )]));
    assert!(!note(&desk).contains("background"), "{}", note(&desk));
}

#[test]
fn closing_a_subagents_pane_says_where_it_goes_and_bd_closes_it_for_good() {
    let mut desk = subagent_desk();
    normal_mode(&mut desk);
    command(&mut desk, "q");
    assert_eq!(
        answer_all(&mut desk),
        [Request::ClosePane { pane: PaneId(3) }]
    );
    assert!(!note(&desk).contains("background"), "not before it's gone");
    // The daemon took its pane away, and it goes on: the bar says where,
    // and the window stops following it, as one that closed would have
    // stopped by itself.
    desk.workspaces_changed(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, pane(2))],
    )]));
    assert_eq!(
        note(&desk),
        "chat 3 goes on in the background, as a card in chat 2: :bd closes it"
    );
    assert!(answer_all(&mut desk).contains(&Request::Detach {
        terminal: TerminalId(3)
    }));

    // `:bd` closes a chat itself.
    let mut desk = subagent_desk();
    normal_mode(&mut desk);
    command(&mut desk, "bd");
    assert_eq!(
        answer_all(&mut desk),
        [Request::CloseTerminal {
            terminal: TerminalId(3)
        }]
    );
}

#[test]
fn bd_is_for_chats() {
    let mut desk = desk(one_tab(pane(1)));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    command(&mut desk, "bd");
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(
        note(&desk),
        ":bd closes a chat for good; :q closes this pane"
    );
}
