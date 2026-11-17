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
    // The outline of one loose file there needs no walk: it gets an index
    // that holds only what's asked about, which a search never reads.
    assert!(world.daemon.outline_for_file(&home).is_ok());
    let all = world.daemon.search_index(ContextKind::All, &home);
    assert!(all.unwrap().is_none());
    assert!(
        world
            .daemon
            .search_index(ContextKind::Symbol, &home)
            .is_err()
    );
    world.daemon.take_outlines();
    let listed = world.daemon.work.scheduler.snapshot().1;
    let scope = Some(home.to_string_lossy().into_owned());
    assert!(
        listed.iter().all(|job| job.scope != scope),
        "home is never scanned: {listed:?}"
    );
}

#[test]
fn a_home_reached_through_a_link_is_still_the_home_that_is_never_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    // The environment names home through the link, as where `/home` is
    // one; a chat's project has its links followed.
    let (mut daemon, chat) = crate::server::agent_files::tests::daemon_with_chat(&link);
    daemon.config.languages = Some(real.join("languages"));
    assert_eq!(daemon.chat_ref(chat).unwrap().project(), real);
    let refused = daemon.search_index(ContextKind::Symbol, &real);
    assert!(refused.is_err_and(|why| why.contains("home directory")));
}

#[test]
fn jobs_that_open_the_store_take_turns_but_a_plain_preview_does_not_wait_for_them() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    let mut scheduler = crate::work::Scheduler::new(|| {});
    let mut asked: Asked<(), bool> = Asked::default();
    let question = |scope: Option<&str>| Question {
        name: "test",
        scope: scope.map(str::to_owned),
        wait: Duration::from_hours(1),
        priority: Priority::Interactive,
    };
    let (started, start) = mpsc::channel();
    let (release, wait) = mpsc::channel::<()>();
    let (told, heard) = mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let first = Arc::clone(&finished);
    asked
        .ask(
            &mut scheduler,
            question(Some(store::SCOPE)),
            (),
            move |_| {
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(20)).unwrap();
                first.store(true, Ordering::Release);
                true
            },
        )
        .unwrap();
    start.recv_timeout(Duration::from_secs(20)).unwrap();
    // A second store job, and a job that opens no store, queued behind.
    let (second, tell) = (Arc::clone(&finished), told.clone());
    asked
        .ask(
            &mut scheduler,
            question(Some(store::SCOPE)),
            (),
            move |_| {
                tell.send(("store", second.load(Ordering::Acquire)))
                    .unwrap();
                true
            },
        )
        .unwrap();
    let plain = Arc::clone(&finished);
    asked
        .ask(&mut scheduler, question(None), (), move |_| {
            told.send(("plain", plain.load(Ordering::Acquire))).unwrap();
            true
        })
        .unwrap();
    // The plain one runs while the first store job still holds its worker.
    let wait = Duration::from_secs(20);
    assert_eq!(heard.recv_timeout(wait).unwrap(), ("plain", false));
    release.send(()).unwrap();
    // The second store job ran only once the first had finished.
    assert_eq!(heard.recv_timeout(wait).unwrap(), ("store", true));
    // And each kind of job says truly whether it opens the store.
    let preview = |keep| capture::Capture {
        prepared: Vec::new(),
        languages: None,
        keep,
    };
    assert!(!preview(Keep::Nothing).opens_store());
    let draft = Keep::Draft {
        store: PathBuf::new(),
        in_use: BTreeSet::new(),
    };
    assert!(preview(draft).opens_store());
}

#[test]
fn a_lookup_that_finds_the_store_busy_is_asked_again_not_taken_for_looked() {
    let mut world = World::new();
    world.end_turn();
    let chat = world.chat;
    until(&mut world, |daemon| daemon.contexts.history.idle());
    let turn = told_again(&mut world, "explain this", "a.rs");
    // Another connection holds the store, as a daemon taking over might
    // while it brings an older layout up to date.
    let path = world.daemon.context_store_path();
    let holder = rusqlite::Connection::open(&path).unwrap();
    holder
        .execute_batch("PRAGMA user_version=0; BEGIN IMMEDIATE")
        .unwrap();
    world.daemon.context_history_changed(chat);
    until(&mut world, |daemon| daemon.contexts.history.retried > 0);
    assert_eq!(attached(&world.daemon, chat, turn), 0);
    // Let go of, the store answers, and the turn gets its attachment.
    holder.execute_batch("COMMIT").unwrap();
    until(&mut world, |daemon| attached(daemon, chat, turn) == 1);
}
