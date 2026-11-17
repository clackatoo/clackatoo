//! Context through a whole daemon: what the loop does, and when.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use clackatoo_protocol::{
    ChatAction, ContextKind, ContextMode, ContextRef, ContextSelection, SideAction,
};
use mio::Token;

use super::*;
use crate::server::turn_undo::tests::World;

/// A snapshot of a file called `name` holding `text`.
fn snapshot_of(name: &str, text: &str) -> ContextSnapshot {
    snapshot(
        &ContextSelection {
            source: ContextRef::File {
                path: name.into(),
                lines: None,
            },
            mode: ContextMode::Contents,
            expected: None,
        },
        Excerpt {
            label: name.into(),
            location: name.into(),
            text: text.into(),
            note: String::new(),
            partial: false,
        },
    )
    .unwrap()
}

/// Keeps a message that went with a snapshot, and gives the chat a turn
/// that reads as an agent would tell it again: the words and the quoted
/// snapshot, with no attachment. Returns the turn's index.
fn told_again(world: &mut World, text: &str, file: &str) -> usize {
    let snapshot = snapshot_of(file, "what the file held");
    let root = world
        .daemon
        .chat_ref(world.chat)
        .unwrap()
        .project()
        .to_owned();
    let path = world.daemon.context_store_path();
    let kept = std::slice::from_ref(&snapshot);
    store::remember(&path, &root, text, kept, &BTreeSet::new()).unwrap();
    let words = crate::chat::Prompt {
        text: text.into(),
        images: Vec::new(),
        context: vec![snapshot],
        resolved_context: true,
    }
    .words();
    let chat = world.daemon.chat(world.chat).unwrap();
    chat.act(ChatAction::Send(words));
    let index = chat.node().turns.len() - 1;
    world.end_turn();
    index
}

/// Runs the loop's part until `done`, woken by the workers as the loop is.
fn until(world: &mut World, done: impl Fn(&Daemon) -> bool) {
    let mut events = mio::Events::with_capacity(8);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        // The loop's look at the scheduler, so its next change wakes us.
        world.daemon.work.scheduler.revision();
        world.daemon.take_context_history();
        if done(&world.daemon) {
            return;
        }
        assert!(Instant::now() < deadline, "the lookups never settled");
        let wait = Some(Duration::from_secs(1));
        world.daemon.poll.poll(&mut events, wait).unwrap();
    }
}

fn attached(daemon: &Daemon, chat: TerminalId, turn: usize) -> usize {
    daemon.chat_ref(chat).unwrap().node().turns[turn]
        .context
        .len()
}

#[test]
fn a_chats_turns_get_their_attachments_back_when_it_is_told_again_not_on_every_loop_turn() {
    let mut world = World::new();
    world.end_turn();
    let chat = world.chat;
    let first = told_again(&mut world, "explain this", "a.rs");
    // The chat was marked as it was added, so its turns are looked up.
    until(&mut world, |daemon| attached(daemon, chat, first) == 1);
    let node = world.daemon.chat_ref(chat).unwrap().node();
    assert_eq!(node.turns[first].prompt, "explain this");
    until(&mut world, |daemon| daemon.contexts.history.idle());
    // Looked through, it isn't looked through again: a turn that turns
    // up without the chat being told again is left alone, however often
    // the loop goes round.
    let second = told_again(&mut world, "and this", "b.rs");
    for _ in 0..3 {
        world.daemon.take_context_history();
    }
    assert!(world.daemon.contexts.history.idle());
    assert_eq!(attached(&world.daemon, chat, second), 0);
    // Told again, it is.
    world.daemon.context_history_changed(chat);
    until(&mut world, |daemon| attached(daemon, chat, second) == 1);
    // What's remembered of a chat goes with it.
    assert!(world.daemon.contexts.history.remembers(chat));
    world.daemon.close_terminal(chat).unwrap();
    assert!(!world.daemon.contexts.history.remembers(chat));
}

#[test]
fn a_side_question_is_not_offered_as_a_chat_to_attach() {
    let mut world = World::new();
    world.daemon.next_terminal();
    let parent = world.chat;
    let open = SideAction::Open {
        question: String::new(),
    };
    world.daemon.side_action(parent, open).unwrap();
    let side = world.daemon.side_of(parent).unwrap();
    world
        .daemon
        .chat(side)
        .unwrap()
        .act(ChatAction::Send("a side question".into()));
    let listed = world
        .daemon
        .context_candidates(Token(usize::MAX), parent, ContextKind::Chat, "");
    let chats: Vec<_> = listed
        .items
        .iter()
        .filter_map(|item| match &item.source {
            ContextRef::Chat { chat, .. } => Some(*chat),
            _ => None,
        })
        .collect();
    assert!(chats.contains(&parent), "{chats:?}");
    assert!(!chats.contains(&side), "{chats:?}");
}

#[test]
fn only_a_search_for_symbols_starts_an_index_and_never_of_the_home_directory() {
    let mut world = World::new();
    world.daemon.config.languages = Some(world.path("languages"));
    let project = world.path("project");
    std::fs::create_dir(&project).unwrap();
    // Opening the picker searches everything: no index is loaded for it.
    assert!(
        world
            .daemon
            .search_index(ContextKind::All, &project)
            .unwrap()
            .is_none()
    );
    assert!(world.daemon.outline_loaded(&project).is_none());
    // Asking for symbols loads one, and then the picker reads it too.
    assert!(
        world
            .daemon
            .search_index(ContextKind::Symbol, &project)
            .unwrap()
            .is_some()
    );
    assert!(
        world
            .daemon
            .search_index(ContextKind::All, &project)
            .unwrap()
            .is_some()
    );
    // A chat started in the home directory has it as its project.
    let home = world.daemon.config.home.clone();
    assert_eq!(world.daemon.chat_ref(world.chat).unwrap().project(), home);
    let refused = world.daemon.search_index(ContextKind::Symbol, &home);
    assert!(refused.is_err_and(|why| why.contains("home directory")));
    assert!(
        world
            .daemon
            .search_index(ContextKind::All, &home)
            .unwrap()
            .is_none()
    );
    assert!(world.daemon.outline_loaded(&home).is_none());
}
