//! The start page driven by keys, with what the daemon says given it as
//! it would come.

use clackatoo_protocol::{SettingChoice, Side, WorkspaceId};
use winit::keyboard::{Key, KeyLocation, NamedKey};

use super::*;
use crate::input::Modifiers;

fn press(page: &mut StartPage, key: &Key, modifiers: Modifiers) -> Outcome {
    let text = match key {
        Key::Character(text) if !modifiers.ctrl => Some(text.to_string()),
        _ => None,
    };
    page.key(&KeyPress {
        key,
        text: text.as_deref(),
        location: KeyLocation::Standard,
        code: None,
        modifiers,
    })
}

fn named(page: &mut StartPage, key: NamedKey) -> Outcome {
    press(page, &Key::Named(key), Modifiers::default())
}

fn type_text(page: &mut StartPage, text: &str) -> Outcome {
    let mut outcome = Outcome::Nothing;
    for ch in text.chars() {
        let key = Key::Character(ch.to_string().into());
        outcome = press(page, &key, Modifiers::default());
    }
    outcome
}

fn agent(name: &str, title: &str, models: ProviderModels) -> Provider {
    Provider {
        agent: name.to_owned(),
        title: title.to_owned(),
        models,
    }
}

fn models(values: &[&str]) -> ProviderModels {
    ProviderModels::Known(
        (values.iter())
            .map(|value| SettingChoice {
                value: (*value).to_owned(),
                name: value.to_uppercase(),
                description: None,
            })
            .collect(),
    )
}

fn choices() -> ChatChoices {
    ChatChoices {
        agents: vec![
            agent("claude", "Claude Code", models(&["haiku", "sonnet"])),
            agent("codex", "Codex", ProviderModels::Unknown),
            agent("gemini", "Gemini", ProviderModels::NotInstalled),
        ],
        default: "claude".to_owned(),
        cwd: "/home/me/project".to_owned(),
        home: "/home/me".to_owned(),
        unshown: Vec::new(),
    }
}

fn chat(id: u32, title: &str) -> ChatSummary {
    ChatSummary {
        chat: TerminalId(id),
        title: title.to_owned(),
        agent: "Claude Code".to_owned(),
        state: ChatState::Ready,
        asking: false,
        unseen: false,
        parent: None,
        workspace: Some(WorkspaceId(1)),
        active: u64::from(id),
    }
}

fn session(id: &str, title: &str, chat: Option<u32>) -> AgentSession {
    AgentSession {
        session: id.to_owned(),
        title: title.to_owned(),
        cwd: "/home/me/project".to_owned(),
        updated: Some("2026-10-12T09:00:00Z".to_owned()),
        chat: chat.map(TerminalId),
    }
}

/// A page told the daemon's choices, with `chats` in no pane, and
/// `sessions` listed.
fn page(last: LastChat, chats: Vec<ChatSummary>, sessions: Vec<AgentSession>) -> StartPage {
    let mut page = StartPage::new(Place::Tab, last, None, Case::default());
    assert_eq!(
        page.choices(choices(), chats),
        Outcome::List {
            agent: "claude".to_owned(),
            cwd: "/home/me/project".to_owned()
        }
    );
    page.listing(1);
    assert!(page.listed(1, Ok(sessions)));
    page
}

/// The text of each row shown.
fn shown(page: &mut StartPage) -> Vec<String> {
    let view = page.view((10, 80), None);
    view.rows.into_iter().map(|row| row.text).collect()
}

#[test]
fn enter_starts_a_new_chat_at_once_even_before_the_daemon_says_what_there_is() {
    let mut page = StartPage::new(
        Place::Beside(Side::Right),
        LastChat::default(),
        Some("codex".to_owned()),
        Case::default(),
    );
    assert_eq!(page.place(), Place::Beside(Side::Right));
    assert_eq!(
        named(&mut page, NamedKey::Enter),
        Outcome::Start(NewChat {
            agent: Some("codex".to_owned()),
            model: None,
            cwd: None,
        })
    );
    // Nothing named leaves it to the daemon.
    let mut page = StartPage::new(Place::Tab, LastChat::default(), None, Case::default());
    assert_eq!(
        type_text(&mut page, "n"),
        Outcome::Start(NewChat::default())
    );
}

#[test]
fn the_new_chat_starts_as_the_last_did_and_its_pickers_change_it() {
    let last = LastChat {
        agent: Some("claude".to_owned()),
        model: Some("sonnet".to_owned()),
        cwd: Some("/home/me/other".to_owned()),
    };
    let mut page = page(last, Vec::new(), Vec::new());
    assert_eq!(
        shown(&mut page)[0],
        "New chat · Claude Code · SONNET · ~/project"
    );
    // The model picker, back to the agent's own choice.
    named(&mut page, NamedKey::Tab);
    type_text(&mut page, "l");
    assert_eq!(
        shown(&mut page)[0],
        "New chat · Claude Code · its own choice · ~/project"
    );
    // The folder: the last one's, then home.
    named(&mut page, NamedKey::Tab);
    let outcome = type_text(&mut page, "l");
    assert_eq!(
        outcome,
        Outcome::List {
            agent: "claude".to_owned(),
            cwd: "/home/me/other".to_owned()
        },
        "a new folder lists its sessions"
    );
    assert_eq!(
        type_text(&mut page, "n"),
        Outcome::Start(NewChat {
            agent: Some("claude".to_owned()),
            model: None,
            cwd: Some("/home/me/other".to_owned()),
        })
    );
    // Another agent: its own model, its sessions; one not installed lists
    // none, but starts, to offer installing it.
    named(&mut page, NamedKey::Tab);
    assert!(matches!(type_text(&mut page, "l"), Outcome::List { agent, .. } if agent == "codex"));
    assert_eq!(type_text(&mut page, "l"), Outcome::Changed);
    assert!(
        page.view((10, 80), None)
            .count
            .contains("Gemini isn't installed")
    );
    assert!(matches!(
        named(&mut page, NamedKey::Enter),
        Outcome::Start(NewChat { agent: Some(agent), .. }) if agent == "gemini"
    ));
}

#[test]
fn chats_in_no_pane_come_first_then_the_agents_sessions_less_those_chats() {
    let chats = vec![chat(3, "Older chat"), chat(9, "Newest chat")];
    let sessions = vec![
        session("a", "Fix the parser", None),
        session("b", "Newest chat's session", Some(9)),
        session("c", "Open in a pane", Some(4)),
    ];
    let mut page = page(LastChat::default(), chats, sessions);
    let rows = shown(&mut page);
    assert_eq!(rows.len(), 5, "{rows:#?}");
    assert!(rows[1].starts_with("Newest chat  Claude Code, waiting"));
    assert!(rows[2].starts_with("Older chat"));
    assert!(rows[3].starts_with("Fix the parser  "), "{rows:#?}");
    assert!(rows[4].ends_with(", open"), "{rows:#?}");

    // j and k move; Enter acts on what's picked.
    type_text(&mut page, "jj");
    assert_eq!(
        named(&mut page, NamedKey::Enter),
        Outcome::Show(TerminalId(3))
    );
    type_text(&mut page, "j");
    assert_eq!(
        named(&mut page, NamedKey::Enter),
        Outcome::Resume {
            agent: "claude".to_owned(),
            session: session("a", "Fix the parser", None),
        }
    );
    // One open in a pane is gone to.
    type_text(&mut page, "j");
    assert_eq!(
        named(&mut page, NamedKey::Enter),
        Outcome::Show(TerminalId(4))
    );
    type_text(&mut page, "kkkk");
    assert!(matches!(
        named(&mut page, NamedKey::Enter),
        Outcome::Start(_)
    ));
}

#[test]
fn typing_filters_and_letters_type_once_typing() {
    let sessions = vec![
        session("a", "Fix the parser", None),
        session("b", "Write the docs", None),
        session("c", "jump table", None),
    ];
    let mut page = page(LastChat::default(), Vec::new(), sessions);
    // A letter that isn't a key starts the filter, and picks the best.
    type_text(&mut page, "docs");
    assert_eq!(page.view((10, 80), None).query, "docs▏");
    let rows = shown(&mut page);
    assert_eq!(rows.len(), 2, "the new chat stays: {rows:#?}");
    assert!(matches!(
        named(&mut page, NamedKey::Enter),
        Outcome::Resume { session, .. } if session.session == "b"
    ));
    // Esc clears what's typed, and then closes.
    assert_eq!(named(&mut page, NamedKey::Escape), Outcome::Changed);
    assert_eq!(shown(&mut page).len(), 4);
    // `/` types a letter that's a key: j, here.
    type_text(&mut page, "/");
    type_text(&mut page, "jump");
    let rows = shown(&mut page);
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows[1].starts_with("jump table"));
    // Backspace to nothing keeps typing.
    for _ in 0..4 {
        named(&mut page, NamedKey::Backspace);
    }
    type_text(&mut page, "n");
    assert_eq!(page.view((10, 80), None).query, "n▏", "n typed");
    named(&mut page, NamedKey::Escape);
    assert_eq!(named(&mut page, NamedKey::Escape), Outcome::Close);
}

#[test]
fn a_listing_no_longer_wanted_is_dropped_and_one_that_failed_says_why() {
    let mut page = StartPage::new(Place::Tab, LastChat::default(), None, Case::default());
    page.choices(choices(), Vec::new());
    page.listing(1);
    // The agent changed meanwhile.
    named(&mut page, NamedKey::Tab);
    named(&mut page, NamedKey::Tab);
    named(&mut page, NamedKey::Tab);
    type_text(&mut page, "l");
    page.listing(2);
    assert!(!page.listed(1, Ok(vec![session("a", "Old", None)])));
    assert!(page.view((10, 80), None).count.ends_with('…'));
    assert!(page.listed(2, Err("it stopped".to_owned())));
    assert_eq!(page.view((10, 80), None).count, "it stopped");
    assert_eq!(shown(&mut page).len(), 1);
}

#[test]
fn how_long_ago_reads_in_words() {
    let now: jiff::Timestamp = "2026-10-13T12:00:00Z".parse().unwrap();
    let ago_of = |then: &str| ago(then, now).unwrap();
    assert_eq!(ago_of("2026-10-13T11:59:30Z"), "just now");
    assert_eq!(ago_of("2026-10-13T11:15:00Z"), "45 min ago");
    assert_eq!(ago_of("2026-10-13T09:00:00Z"), "3 h ago");
    assert_eq!(ago_of("2026-10-12T09:00:00Z"), "a day ago");
    assert_eq!(ago_of("2026-10-08T12:00:00Z"), "5 days ago");
    assert_eq!(ago_of("2026-08-01T12:00:00Z"), "2026-08-01");
    // In the future, as with a clock behind, it's now.
    assert_eq!(ago_of("2026-10-13T13:00:00Z"), "just now");
    assert_eq!(ago("yesterday", now), None);
    assert_eq!(shown_folder("/home/me", "/home/me"), "~");
    assert_eq!(shown_folder("/home/me/x", "/home/me"), "~/x");
    assert_eq!(shown_folder("/home/meg", "/home/me"), "/home/meg");
}

#[test]
fn a_session_another_workspaces_chat_has_open_isnt_taken_from_there() {
    let mut page = StartPage::new(Place::Tab, LastChat::default(), None, Case::default());
    let mut theirs = chat(5, "Theirs");
    theirs.workspace = Some(WorkspaceId(2));
    let choices = ChatChoices {
        unshown: vec![theirs],
        ..choices()
    };
    page.choices(choices, Vec::new());
    page.listing(1);
    page.listed(1, Ok(vec![session("a", "Theirs", Some(5))]));
    type_text(&mut page, "j");
    assert_eq!(named(&mut page, NamedKey::Enter), Outcome::Changed);
    let count = page.view((10, 80), None).count;
    assert!(count.contains("in another workspace"), "{count}");
}
