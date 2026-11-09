//! The tutor's lessons, driven through the desk as a window would drive
//! it, with the daemon played by the test.

use std::path::{Path, PathBuf};

use clackatoo_protocol::{Answer, Block, BufferUpdate, Construct, Piece, Query, Which, Workspace};

use super::*;
use crate::editor::Daemon;
use crate::tutor::{self, Io, Start, State};
use clackatoo_protocol::TutorFiles;

const SCRATCH: &str = "/scratch/1";
const STATE: &str = "/state/tutor";

/// A desk whose tutor starts remembering `state`, then connects to
/// `workspaces`, as a window opens.
fn tutored(workspaces: Workspaces, state: Option<State>) -> Desk {
    tutored_in(workspaces, state, Path::new(SCRATCH))
}

/// As [`tutored`], with its scratch directory at `scratch`.
fn tutored_in(workspaces: Workspaces, state: Option<State>, scratch: &Path) -> Desk {
    let mut desk = Desk::default();
    desk.set_area(Some(Area {
        rect: crate::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 1210.0,
            height: 480.0,
        },
        cell: [10.0, 20.0],
        gap: 10.0,
        inset: 5.0,
    }));
    desk.start_tutor(Start {
        state,
        state_path: Some(STATE.into()),
        scratch: Some(scratch.into()),
        ..Start::default()
    });
    desk.connected(workspaces, false);
    answer_all(&mut desk);
    desk
}

/// What the tutor remembers, as everything news has said has been seen.
fn seen_everything() -> State {
    let mut state = State::default();
    let keys = tutor::news::NEWS.iter().map(tutor::news::News::key);
    state.noticed.extend(keys.clone());
    state.read.extend(keys);
    state
}

/// One workspace, "w", of one tab with a terminal in normal mode.
fn one_terminal(state: Option<State>) -> Desk {
    let mut desk = tutored(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        state,
    );
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    desk.message = None;
    desk
}

/// The requests waiting, taken.
fn take_requests(desk: &mut Desk) -> Vec<Request> {
    (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::Request(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// The workspace "w": its first tab a terminal, then the lesson's tab of
/// `lesson`, with `more` tabs after.
fn with_lesson(lesson: Layout, more: Vec<Tab>, active: u32) -> Workspace {
    let mut tabs = vec![tab(1, pane(1)), tab(2, lesson)];
    tabs.extend(more);
    Workspace {
        id: WorkspaceId(1),
        name: "w".into(),
        active_tab: TabId(active),
        tabs,
        home: None,
        lent: None,
        background: Vec::new(),
    }
}

/// The step the open lesson is at.
fn step(desk: &Desk) -> usize {
    desk.tutor.open.as_ref().expect("a lesson open").step
}

/// The lesson `id`'s text, as its pane shows it.
fn rendered(id: &str) -> String {
    tutor::lesson(id).unwrap().render().text
}

/// Opens the lesson `id`, as the daemon would: its practice pane, pane 2
/// in tab 2, showing `practice`, then the lesson pane, pane 3, showing
/// buffer 8 on its left. Returns what was asked as the lesson started,
/// answered with "done".
fn open(desk: &mut Desk, id: &str, practice: Layout) -> Vec<Request> {
    command(desk, &format!("Tutor {id}"));
    let asked = take_requests(desk);
    assert!(matches!(asked[..], [Request::NewTab { .. }]), "{asked:?}");
    desk.workspaces_changed(workspaces(vec![with_lesson(practice.clone(), vec![], 2)]));
    desk.answered(Response::PaneCreated(PaneId(2)));
    let asked = take_requests(desk);
    let lesson_file = format!("{SCRATCH}/{id}.txt");
    assert!(
        asked.iter().any(|request| matches!(
            request,
            Request::Split { pane: PaneId(2), side: Side::Left, terminal: PaneTerminal::File(file), .. }
                if file.path == lesson_file
        )),
        "{asked:?}"
    );
    let title = format!("Tutor: {}", tutor::lesson(id).unwrap().title);
    assert!(
        asked.iter().any(
            |request| matches!(request, Request::RenameTab { tab: TabId(2), name } if *name == title)
        ),
        "{asked:?}"
    );
    let lesson_pane = file_pane(3, 8);
    let layout = side_by_side(vec![lesson_pane, practice]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    for request in asked {
        let response = match request {
            Request::Split { .. } => Response::PaneCreated(PaneId(3)),
            _ => Response::Done,
        };
        desk.answered(response);
    }
    let started = answer_all(desk);
    load(desk, 8, &lesson_file, &rendered(id));
    started
}

/// The text of each row the finder shows, as many as there are.
fn all_finder_rows(desk: &mut Desk) -> Vec<String> {
    // Room for every row there is: the lines of what's new are the most.
    let rows = tutor::news::NEWS.len() + 10;
    let view = desk.finder_view((rows, 120), None).expect("the finder");
    view.rows.into_iter().map(|row| row.text).collect()
}

// --- Every lesson opens --------------------------------------------------

#[test]
// The Basics once couldn't open: its practice terminal starts in its
// folder, which only writing practice files made, and it has none.
fn every_lesson_opens_with_its_folder_there_for_its_practice_pane() {
    let scratch = tempfile::tempdir().unwrap();
    let mut desk = tutored_in(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        Some(seen_everything()),
        scratch.path(),
    );
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    take_requests(&mut desk);
    for lesson in tutor::lessons() {
        let id = lesson.id;
        desk.message = None;
        command(&mut desk, &format!("Tutor {id}"));
        assert_eq!(desk.message, None, "{id}");
        // The window does the tutor's files before it sends the request.
        for io in desk.take_tutor_io() {
            io.run();
        }
        let dir = scratch.path().join(id);
        assert!(dir.is_dir(), "{id}");
        assert!(scratch.path().join(format!("{id}.txt")).is_file(), "{id}");
        let asked = take_requests(&mut desk);
        let [Request::NewTab { terminal, .. }] = &asked[..] else {
            panic!("{id}: {asked:?}");
        };
        match terminal {
            PaneTerminal::New(new) => {
                assert!(lesson.terminal(), "{id}");
                let cwd = new.cwd.as_deref().expect("the lesson's folder");
                assert_eq!(Path::new(cwd), dir, "{id}");
                // A program starts there, as the daemon's shell would.
                let started = std::process::Command::new("true").current_dir(cwd).status();
                assert!(started.is_ok_and(|status| status.success()), "{id}");
            }
            PaneTerminal::File(file) => {
                let (name, _) = lesson.practice_file().expect("a practice file");
                assert_eq!(Path::new(&file.path), dir.join(name), "{id}");
                assert!(Path::new(&file.path).is_file(), "{id}");
            }
            other => panic!("{id}: {other:?}"),
        }
    }
}

// --- The basics, through to the end ---------------------------------------

#[test]
// One lesson, through from its start to its end.
#[allow(clippy::too_many_lines)]
fn the_basics_lesson_is_done_from_start_to_end_with_the_keys_it_teaches() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "basics", pane(2));
    // Its scratch directory is made afresh, with the lesson's text.
    let io = desk.take_tutor_io();
    assert_eq!(io[0], Io::Remove(PathBuf::from("/scratch/1/basics")));
    assert_eq!(io[1], Io::MakeDir(PathBuf::from("/scratch/1/basics")));
    assert!(io.contains(&Io::Write {
        path: "/scratch/1/basics.txt".into(),
        text: rendered("basics"),
    }));
    snapshot(&mut desk, 2);
    assert_eq!(
        shown_pane(&desk),
        PaneId(2),
        "the practice pane has the focus"
    );
    assert_eq!(mode(&desk, 2), Mode::Normal);
    assert_eq!(step(&desk), 0);

    // Insert mode, then normal mode.
    type_keys(&mut desk, "i");
    assert_eq!(step(&desk), 1);
    normal_mode(&mut desk);
    assert_eq!(step(&desk), 2);

    // A command run: the tutor asks after the terminal's last block.
    let block = |command: Option<&str>, exit| Block {
        first: 0,
        last: 1,
        output: command.map(|_| 1),
        command: command.map(str::to_owned),
        cwd: None,
        running: false,
        exit,
    };
    let asked = take_requests(&mut desk);
    assert!(
        matches!(
            asked[..],
            [Request::Block {
                terminal: TerminalId(2),
                which: Which::Before,
                ..
            }]
        ),
        "{asked:?}"
    );
    desk.answered(Response::Block(None));
    assert_eq!(step(&desk), 2, "nothing's been run");
    type_text(&mut desk, "iecho hello\r");
    screen_showing(&mut desk, 2, 40, "$ echo hello\r\nhello\r\n$ ");
    let asked = take_requests(&mut desk);
    assert!(
        asked
            .iter()
            .any(|request| matches!(request, Request::Block { .. }))
    );
    for request in asked {
        let response = match request {
            Request::Block { .. } => Response::Block(Some(block(Some("echo hello"), Some(0)))),
            _ => Response::Done,
        };
        desk.answered(response);
    }
    assert_eq!(step(&desk), 3);

    // A line copied in normal mode.
    normal_mode(&mut desk);
    type_keys(&mut desk, "kyy");
    for request in take_requests(&mut desk) {
        let response = match request {
            Request::Text { .. } => Response::Text("hello".into()),
            _ => Response::Done,
        };
        desk.answered(response);
    }
    assert_eq!(step(&desk), 4);

    // Kept in a register named first, as an editor's yank is.
    type_keys(&mut desk, "\"ayy");
    for request in take_requests(&mut desk) {
        let response = match request {
            Request::Text { .. } => Response::Text("hello".into()),
            _ => Response::Done,
        };
        desk.answered(response);
    }
    assert_eq!(step(&desk), 5);

    // A split, gone to and back from, and closed.
    command(&mut desk, "|");
    let asked = take_requests(&mut desk);
    assert!(matches!(
        asked[..],
        [Request::Split {
            pane: PaneId(2),
            side: Side::Right,
            ..
        }]
    ));
    let three = side_by_side(vec![file_pane(3, 8), pane(2), pane(4)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(three, vec![], 2)]));
    desk.answered(Response::PaneCreated(PaneId(4)));
    answer_all(&mut desk);
    snapshot(&mut desk, 4);
    assert_eq!(step(&desk), 6);
    assert_eq!(shown_pane(&desk), PaneId(4));
    ctrl(&mut desk, "h");
    assert_eq!(step(&desk), 7);
    // Back to the new pane in insert mode, which it arrives in.
    type_keys(&mut desk, "i");
    assert_eq!(step(&desk), 7, "the practice terminal doesn't count");
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 4), Mode::Insert);
    assert_eq!(step(&desk), 8);
    ctrl_b(&mut desk, "x");
    let asked = take_requests(&mut desk);
    assert!(
        asked.contains(&Request::ClosePane { pane: PaneId(4) }),
        "{asked:?}"
    );
    for _ in &asked {
        desk.answered(Response::Done);
    }
    let two = side_by_side(vec![file_pane(3, 8), pane(2)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(two.clone(), vec![], 2)]));
    answer_all(&mut desk);
    assert_eq!(step(&desk), 9);
    assert_eq!(
        mode(&desk, 2),
        Mode::Insert,
        "the practice terminal took the mode"
    );

    // Opened again, zoomed and put back, and closed again.
    press(
        &mut desk,
        Key::Character("T".into()),
        None,
        Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::default()
        },
    );
    let asked = take_requests(&mut desk);
    assert!(
        asked.contains(&Request::Reopen {
            beside: Some(PaneId(3))
        }),
        "{asked:?}"
    );
    let again = side_by_side(vec![file_pane(3, 8), pane(2), pane(6)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(again.clone(), vec![], 2)]));
    desk.answered(Response::PaneCreated(PaneId(6)));
    answer_all(&mut desk);
    snapshot(&mut desk, 6);
    assert_eq!(step(&desk), 10);
    ctrl_b(&mut desk, "p1");
    ctrl_b(&mut desk, "z");
    let asked = take_requests(&mut desk);
    let zoom = Request::Zoom {
        tab: TabId(2),
        pane: Some(PaneId(3)),
    };
    assert!(asked.contains(&zoom), "{asked:?}");
    for _ in &asked {
        desk.answered(Response::Done);
    }
    let mut zoomed = workspaces(vec![with_lesson(again, vec![], 2)]);
    zoomed.zoomed = vec![clackatoo_protocol::Zoomed {
        tab: TabId(2),
        pane: PaneId(3),
    }];
    desk.workspaces_changed(zoomed);
    answer_all(&mut desk);
    assert_eq!(step(&desk), 11);
    ctrl_b(&mut desk, "z");
    let asked = take_requests(&mut desk);
    let unzoom = Request::Zoom {
        tab: TabId(2),
        pane: None,
    };
    assert!(asked.contains(&unzoom), "{asked:?}");
    for _ in &asked {
        desk.answered(Response::Done);
    }
    desk.workspaces_changed(workspaces(vec![with_lesson(two.clone(), vec![], 2)]));
    answer_all(&mut desk);
    assert_eq!(step(&desk), 12);

    // The command line: Tab completes an option, and Up goes back to it.
    normal_mode(&mut desk);
    let key = |desk: &mut Desk, key| press(desk, Key::Named(key), None, Modifiers::default());
    type_keys(&mut desk, ":set norel");
    key(&mut desk, NamedKey::Tab);
    enter(&mut desk);
    assert_eq!(step(&desk), 13);
    type_keys(&mut desk, ":set");
    key(&mut desk, NamedKey::ArrowUp);
    ctrl(&mut desk, "w");
    type_keys(&mut desk, "rnu");
    enter(&mut desk);
    assert_eq!(step(&desk), 14);

    // A tab of its own, and back.
    command(&mut desk, "Terminal");
    assert!(matches!(
        take_requests(&mut desk)[..],
        [Request::NewTab { .. }]
    ));
    desk.workspaces_changed(workspaces(vec![with_lesson(
        two.clone(),
        vec![tab(3, pane(5))],
        2,
    )]));
    desk.answered(Response::PaneCreated(PaneId(5)));
    answer_all(&mut desk);
    snapshot(&mut desk, 5);
    assert_eq!(step(&desk), 15);
    command(&mut desk, "2");
    answer_all(&mut desk);
    assert_eq!(step(&desk), 16);

    // A workspace of its own, and back.
    command(&mut desk, "ws tutor");
    assert!(matches!(
        take_requests(&mut desk)[..],
        [Request::NewWorkspace { .. }]
    ));
    let tutor_workspace = workspace(2, "tutor", vec![tab(4, pane(6))]);
    desk.workspaces_changed(workspaces(vec![
        with_lesson(two.clone(), vec![tab(3, pane(5))], 2),
        tutor_workspace,
    ]));
    desk.answered(Response::PaneCreated(PaneId(6)));
    answer_all(&mut desk);
    snapshot(&mut desk, 6);
    assert_eq!(step(&desk), 17);
    ctrl_b(&mut desk, "w");
    type_text(&mut desk, "w");
    enter(&mut desk);
    answer_all(&mut desk);
    assert_eq!(desk.shown().unwrap().tab, TabId(2));
    assert_eq!(step(&desk), 18);

    // What the bar has said.
    command(&mut desk, "messages");
    assert_eq!(step(&desk), 19);
    // The last steps are to read: `]s` goes on, to the end.
    let lesson = tutor::lesson("basics").unwrap();
    assert!(lesson.steps[19].checks.is_empty());
    assert!(lesson.steps[20].checks.is_empty());
    ctrl(&mut desk, "l");
    assert_eq!(shown_pane(&desk), PaneId(2));
    type_keys(&mut desk, "]s");
    assert_eq!(step(&desk), 20);
    type_keys(&mut desk, "]s");
    assert_eq!(step(&desk), lesson.steps.len());
    assert!(
        desk.message
            .as_deref()
            .unwrap()
            .starts_with("that's the end of Basics")
    );
    assert!(desk.tutor.state.done.contains("basics"));
    let saved = desk.take_tutor_io();
    let Some(Io::Save { state, .. }) = saved.last() else {
        panic!("{saved:?}");
    };
    assert!(state.done.contains("basics"), "{state:?}");

    // `:q` ends it, closing both its panes.
    command(&mut desk, "q");
    let asked = take_requests(&mut desk);
    assert!(
        asked.contains(&Request::ClosePane { pane: PaneId(3) }),
        "{asked:?}"
    );
    assert!(
        asked.contains(&Request::ClosePane { pane: PaneId(2) }),
        "{asked:?}"
    );
    assert!(desk.tutor.open.is_none());
    let io = desk.take_tutor_io();
    assert!(io.contains(&Io::Remove("/scratch/1/basics".into())));
    assert!(io.contains(&Io::Remove("/scratch/1/basics.txt".into())));
}

#[test]
fn the_lesson_pane_has_no_insert_mode_so_brackets_step_through_it_from_insert_mode() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "basics", pane(2));
    snapshot(&mut desk, 2);
    type_keys(&mut desk, "i");
    assert_eq!(step(&desk), 1);
    // To the lesson in insert mode: it stays in normal mode, and shows it.
    ctrl_b(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(3));
    let (editor, _) = desk.focused_editor().expect("the lesson");
    assert_eq!(editor.mode, Mode::Normal);
    assert_eq!(desk.mode(), WindowMode::Insert);
    type_keys(&mut desk, "]s");
    assert_eq!(step(&desk), 2);
    // And back to the practice terminal, in insert mode still.
    ctrl_b(&mut desk, "l");
    assert_eq!(mode(&desk, 2), Mode::Insert);
}

// --- Text objects, with the grammar played by the test ----------------------

/// The pieces of `construct` in `text`, as a grammar would find them in
/// the practice file: functions and structs at a line's start, with their
/// bodies inside the braces, and the parameters of functions.
fn pieces(text: &str, construct: Construct) -> Vec<Piece> {
    let keyword = match construct {
        Construct::Class => "struct ",
        Construct::Function | Construct::Argument => "fn ",
    };
    let mut items = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with(keyword) {
            let close = start + text[start..].find("\n}").expect("a closing brace");
            let open = start + text[start..].find("{\n").expect("an opening brace") + 2;
            let body = open + text[open..].len() - text[open..].trim_start().len();
            items.push(Piece {
                start,
                end: close + 2,
                inner: Some((body, close)),
            });
        }
        start += line.len();
    }
    if construct != Construct::Argument {
        return items;
    }
    let mut arguments = Vec::new();
    for item in items {
        let header = &text[item.start..];
        let (open, close) = (header.find('(').unwrap() + 1, header.find(')').unwrap());
        let mut at = item.start + open;
        let list: Vec<&str> = header[open..close]
            .split(", ")
            .filter(|a| !a.is_empty())
            .collect();
        for (index, argument) in list.iter().enumerate() {
            let (first, end) = (at, at + argument.len());
            let outer = if index > 0 {
                (first - 2, end)
            } else if list.len() > 1 {
                (first, end + 2)
            } else {
                (first, end)
            };
            arguments.push(Piece {
                start: outer.0,
                end: outer.1,
                inner: Some((first, end)),
            });
            at = end + 2;
        }
    }
    arguments
}

/// Answers the edits and grammar questions sent about buffer 7, with
/// `daemon` and the grammar played by [`pieces`], until there are none,
/// and everything else with "done".
fn serve_lesson(desk: &mut Desk, daemon: &mut Daemon) {
    loop {
        let messages = desk.take_outbox();
        if messages.is_empty() {
            return;
        }
        for message in messages {
            match message {
                ClientMessage::Edit {
                    buffer: BufferId(7),
                    ..
                } => {
                    let (answer, _) = daemon.take(message);
                    desk.buffer_changed(BufferId(7), answer);
                }
                ClientMessage::Query {
                    buffer: BufferId(7),
                    id,
                    at,
                    query,
                    ..
                } => {
                    let text = daemon.text().to_string();
                    let found = match query {
                        Query::Pieces(construct) => {
                            let all = pieces(&text, construct);
                            let around: Vec<Piece> = (all.iter())
                                .filter(|piece| (piece.start..piece.end).contains(&at))
                                .copied()
                                .collect();
                            if around.is_empty() {
                                all.into_iter()
                                    .filter(|piece| piece.start > at)
                                    .take(1)
                                    .collect()
                            } else {
                                around
                            }
                        }
                        Query::Starts {
                            construct,
                            backward,
                            count,
                        } => {
                            let mut starts: Vec<Piece> = (pieces(&text, construct).into_iter())
                                .filter(|piece| {
                                    if backward {
                                        piece.start < at
                                    } else {
                                        piece.start > at
                                    }
                                })
                                .map(|piece| Piece {
                                    inner: None,
                                    ..piece
                                })
                                .collect();
                            if backward {
                                starts.reverse();
                            }
                            starts.truncate(count as usize);
                            starts
                        }
                        other => panic!("asked {other:?}"),
                    };
                    let answer = BufferUpdate::Answer {
                        id,
                        version: daemon.version(),
                        answer: Answer::Pieces(found),
                    };
                    desk.buffer_changed(BufferId(7), answer);
                }
                ClientMessage::Request(_) => {
                    desk.answered(Response::Done);
                }
                _ => {}
            }
        }
    }
}

/// Goes to the line of the focused editor's text that starts with
/// `start`, with `:N`.
fn go_to_line(desk: &mut Desk, start: &str) {
    let text = editor_text(desk);
    let line = text
        .lines()
        .position(|line| line.starts_with(start))
        .expect("the line");
    command(desk, &(line + 1).to_string());
}

#[test]
fn the_text_objects_lesson_is_done_from_start_to_end_with_the_grammar_answering() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "text-objects", file_pane(2, 7));
    let lesson = tutor::lesson("text-objects").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    // Written to the scratch directory before the daemon opens it.
    assert!(desk.take_tutor_io().contains(&Io::Write {
        path: PathBuf::from(format!("{SCRATCH}/text-objects/{name}")),
        text: practice.clone(),
    }));
    load(
        &mut desk,
        7,
        &format!("{SCRATCH}/text-objects/{name}"),
        practice,
    );
    let mut daemon = Daemon::new(practice);
    assert_eq!(shown_pane(&desk), PaneId(2));
    // The lesson pane shows the lesson as prose, the first step lit.
    let editor = desk.editor(PaneId(3)).unwrap();
    assert!(editor.shows_lesson());

    go_to_line(&mut desk, "fn first");
    type_keys(&mut desk, "daf");
    serve_lesson(&mut desk, &mut daemon);
    assert!(!editor_text(&desk).contains("fn first"));
    assert_eq!(step(&desk), 1);

    go_to_line(&mut desk, "    width * 2");
    type_keys(&mut desk, "cif");
    serve_lesson(&mut desk, &mut daemon);
    type_text(&mut desk, "width * 3");
    escape(&mut desk);
    serve_lesson(&mut desk, &mut daemon);
    assert!(
        editor_text(&desk).contains("    width * 3\n"),
        "{}",
        editor_text(&desk)
    );
    assert_eq!(step(&desk), 2);

    go_to_line(&mut desk, "fn area");
    type_keys(&mut desk, "2fhdaa");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 3, "{}", editor_text(&desk));

    type_keys(&mut desk, "ggdac");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 4, "{}", editor_text(&desk));

    // `]f` from the top, as the step puts the cursor there, until `last`.
    for _ in 0..3 {
        type_keys(&mut desk, "]f");
        serve_lesson(&mut desk, &mut daemon);
    }
    assert_eq!(step(&desk), 5, "{}", editor_text(&desk));
    // Growing a selection is to read.
    type_keys(&mut desk, "]s");
    assert_eq!(step(&desk), 6);
    go_to_line(&mut desk, "fn area");
    type_keys(&mut desk, "dap");
    serve_lesson(&mut desk, &mut daemon);
    assert!(!editor_text(&desk).contains("fn area"));
    assert_eq!(step(&desk), 7);

    go_to_line(&mut desk, "/// Doubles");
    type_keys(&mut desk, "fWdas");
    serve_lesson(&mut desk, &mut daemon);
    assert!(editor_text(&desk).contains("/// Doubles a width.\n"));
    assert_eq!(step(&desk), 8);

    go_to_line(&mut desk, "    println!(\"<b>the end");
    type_keys(&mut desk, "fecit");
    serve_lesson(&mut desk, &mut daemon);
    type_text(&mut desk, "done");
    escape(&mut desk);
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 9, "{}", editor_text(&desk));

    go_to_line(&mut desk, "fn double");
    type_keys(&mut desk, "5lvjY");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 10);

    type_keys(&mut desk, "0f(Y");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 11);

    // `u` puts the cursor back where `dk` began, a line up.
    serve_lesson(&mut desk, &mut daemon);
    type_keys(&mut desk, "f:dk");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 11, "{}", editor_text(&desk));
    type_keys(&mut desk, "u");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), lesson.steps.len(), "{}", editor_text(&desk));
    assert_eq!(daemon.text().to_string(), editor_text(&desk));
}

#[test]
fn the_blocks_lesson_is_done_from_start_to_end_with_the_keys_it_teaches() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "blocks", file_pane(2, 7));
    let lesson = tutor::lesson("blocks").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    load(&mut desk, 7, &format!("{SCRATCH}/blocks/{name}"), practice);
    let mut daemon = Daemon::new(practice);
    // Ctrl+V and Esc as `^V` and `Esc`, typing as `>` and the text.
    let mut keys = |desk: &mut Desk, parts: &[&str]| {
        for &part in parts {
            match part {
                "^V" => ctrl(desk, "v"),
                "Esc" => escape(desk),
                part => match part.strip_prefix('>') {
                    Some(text) => type_text(desk, text),
                    None => type_keys(desk, part),
                },
            }
        }
        serve_lesson(desk, &mut daemon);
    };
    keys(&mut desk, &["^V", "3jl"]);
    assert_eq!(step(&desk), 1);
    keys(&mut desk, &["U"]);
    assert_eq!(step(&desk), 2, "{}", editor_text(&desk));
    keys(&mut desk, &["^V", "3jI", ">- ", "Esc"]);
    assert_eq!(step(&desk), 3, "{}", editor_text(&desk));
    keys(&mut desk, &["^V", "3j$A", ">;", "Esc"]);
    assert_eq!(step(&desk), 4, "{}", editor_text(&desk));
    keys(&mut desk, &["0", "^V", "3jld"]);
    assert_eq!(step(&desk), 5, "{}", editor_text(&desk));
    keys(&mut desk, &["P"]);
    assert_eq!(step(&desk), 6, "{}", editor_text(&desk));
    assert_eq!(
        editor_text(&desk),
        "- APple;\n- BAnana;\n- CHerry;\n- DAmson;\n"
    );
    assert_eq!(daemon.text().to_string(), editor_text(&desk));
}

// --- Steps --------------------------------------------------------------------

#[test]
fn a_step_gone_back_to_ticks_off_only_once_its_done_again() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "basics", pane(2));
    snapshot(&mut desk, 2);
    type_keys(&mut desk, "i");
    normal_mode(&mut desk);
    assert_eq!(step(&desk), 2);
    // Back to "back to normal mode", which holds already: it waits.
    type_keys(&mut desk, "[s");
    assert_eq!(step(&desk), 1);
    type_keys(&mut desk, "j");
    assert_eq!(step(&desk), 1);
    type_keys(&mut desk, "i");
    normal_mode(&mut desk);
    assert_eq!(step(&desk), 2);
    // Where it's left is remembered.
    assert_eq!(desk.tutor.state.steps.get("basics"), Some(&2));
    // `[s` at the first step, and `]s` past the end, say so.
    type_keys(&mut desk, "[s[s[s");
    assert_eq!(step(&desk), 0);
    assert_eq!(desk.message.as_deref(), Some("that's the first step"));
}

#[test]
fn brackets_outside_a_lesson_are_the_panes_as_ever_and_in_one_go_on_past_it() {
    let mut desk = editing("/p/a.rs", FUNCTIONS);
    // Outside a lesson, `]` waits for nothing, and leaves the bar be.
    desk.message = Some("written".into());
    type_keys(&mut desk, "]");
    assert!(desk.keymap.waiting().is_empty());
    assert!(desk.tutor.bracket.is_none());
    type_keys(&mut desk, "s");
    assert_ne!(
        desk.message.as_deref(),
        Some("that's the first step"),
        "no lesson to step through"
    );
    type_keys(&mut desk, "]f");
    let asked = (desk.take_outbox().into_iter()).any(|message| {
        matches!(
            message,
            ClientMessage::Query {
                query: Query::Starts {
                    backward: false,
                    ..
                },
                ..
            }
        )
    });
    assert!(asked, "]f asked where the next function starts");
    // In a lesson's practice terminal, `]` waits for `s`, and `]]` goes on
    // to the terminal as ever.
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "shell", pane(2));
    snapshot(&mut desk, 2);
    desk.message = Some("kept".into());
    type_keys(&mut desk, "]");
    assert!(desk.tutor.bracket.is_some());
    assert_eq!(desk.message.as_deref(), Some("kept"), "the bar's left be");
    type_keys(&mut desk, "]");
    let asked = take_requests(&mut desk);
    assert!(
        asked.iter().any(|request| matches!(
            request,
            Request::Block {
                terminal: TerminalId(2),
                which: Which::After,
                ..
            }
        )),
        "{asked:?}"
    );
}

#[test]
fn a_lesson_opens_at_the_step_it_was_left_at_with_that_steps_text() {
    let mut state = seen_everything();
    // "Every match on the line", which sets its text afresh.
    state.steps.insert("substitute".into(), 1);
    let mut desk = one_terminal(Some(state));
    open(&mut desk, "substitute", file_pane(2, 7));
    let lesson = tutor::lesson("substitute").unwrap();
    assert_eq!(step(&desk), 1);
    let text = lesson.practice_at(1).unwrap();
    assert!(desk.take_tutor_io().contains(&Io::Write {
        path: format!("{SCRATCH}/substitute/pets.txt").into(),
        text: text.to_owned(),
    }));
}

#[test]
fn a_step_that_sets_the_practice_text_puts_it_in_as_it_starts() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "substitute", file_pane(2, 7));
    let lesson = tutor::lesson("substitute").unwrap();
    let path = format!("{SCRATCH}/substitute/pets.txt");
    load(&mut desk, 7, &path, lesson.practice_at(0).unwrap());
    let mut daemon = Daemon::new(lesson.practice_at(0).unwrap());
    command(&mut desk, "s/cat/fish/");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 1);
    // The next step's text, afresh, its cursor on the line it says.
    assert_eq!(editor_text(&desk), lesson.practice_at(1).unwrap());
    assert_eq!(cursor_line(&desk), 0);
    command(&mut desk, "s/cat/fish/g");
    serve_lesson(&mut desk, &mut daemon);
    assert_eq!(step(&desk), 2);
    assert_eq!(daemon.text().to_string(), editor_text(&desk));
}

#[test]
fn a_change_to_the_lesson_is_put_back() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "text-objects", file_pane(2, 7));
    ctrl(&mut desk, "h");
    assert_eq!(shown_pane(&desk), PaneId(3));
    // The lesson pane shows no line numbers, as prose.
    type_keys(&mut desk, "dd");
    let asked = take_requests(&mut desk);
    assert!(
        asked.contains(&Request::Discard {
            buffer: BufferId(8)
        }),
        "{asked:?}"
    );
    assert!(
        desk.message
            .as_deref()
            .unwrap()
            .contains("can't be changed")
    );
    // Written again first, in case it was written over.
    assert!(desk.take_tutor_io().contains(&Io::Write {
        path: format!("{SCRATCH}/text-objects.txt").into(),
        text: rendered("text-objects"),
    }));
}

#[test]
fn links_and_explorers_never_open_files_in_the_lesson_pane() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "shell", pane(2));
    assert!(!desk.takes_files(PaneId(3)));
    assert!(desk.is_lesson_pane(PaneId(3)));
}

#[test]
fn a_setup_command_is_typed_into_the_practice_terminal() {
    let mut desk = one_terminal(Some(seen_everything()));
    let asked = open(&mut desk, "shell", pane(2));
    let typed: Vec<&[u8]> = (asked.iter())
        .filter_map(|request| match request {
            Request::SendInput {
                terminal: TerminalId(2),
                bytes,
            } => Some(&bytes[..]),
            _ => None,
        })
        .collect();
    assert_eq!(typed, [&b"printf 'one\\ntwo\\nthree\\n'\r"[..], b"ls\r"]);
}

#[test]
fn a_lesson_closing_with_the_window_closes_its_panes_and_throws_away_its_changes() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "text-objects", file_pane(2, 7));
    let lesson = tutor::lesson("text-objects").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    load(
        &mut desk,
        7,
        &format!("{SCRATCH}/text-objects/{name}"),
        practice,
    );
    type_keys(&mut desk, "x");
    desk.take_outbox();
    desk.take_tutor_io();
    desk.tutor_exiting();
    let asked = take_requests(&mut desk);
    assert_eq!(
        asked,
        [
            Request::ClosePane { pane: PaneId(3) },
            Request::Discard {
                buffer: BufferId(7)
            },
            Request::ClosePane { pane: PaneId(2) },
        ]
    );
    let io = desk.take_tutor_io();
    assert!(io.contains(&Io::Remove(SCRATCH.into())), "{io:?}");
}

#[test]
fn a_lesson_pane_closed_ends_the_lesson_and_closes_the_other() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "basics", pane(2));
    desk.workspaces_changed(workspaces(vec![with_lesson(pane(2), vec![], 2)]));
    assert!(desk.tutor.open.is_none());
    assert!(take_requests(&mut desk).contains(&Request::ClosePane { pane: PaneId(2) }));
}

#[test]
fn a_command_that_finished_before_the_step_began_doesnt_do_it() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "basics", pane(2));
    snapshot(&mut desk, 2);
    type_keys(&mut desk, "]s]s");
    assert_eq!(step(&desk), 2);
    let block = |running, exit| {
        Response::Block(Some(Block {
            first: 0,
            last: 1,
            output: Some(1),
            command: Some("true".into()),
            cwd: None,
            running,
            exit,
        }))
    };
    // The last command exited 0, before the step: not done.
    let asked = take_requests(&mut desk);
    assert!(matches!(asked[..], [Request::Block { .. }]), "{asked:?}");
    desk.answered(block(false, Some(0)));
    assert_eq!(step(&desk), 2);
    // One run now, which runs, then exits 0: done.
    screen_showing(&mut desk, 2, 40, "$ true\r\n$ echo hello\r\n");
    assert!(matches!(
        take_requests(&mut desk)[..],
        [Request::Block { .. }]
    ));
    desk.answered(block(true, None));
    assert_eq!(step(&desk), 2);
    screen_showing(&mut desk, 2, 40, "$ true\r\n$ echo hello\r\nhello\r\n$ ");
    assert!(matches!(
        take_requests(&mut desk)[..],
        [Request::Block { .. }]
    ));
    desk.answered(block(false, Some(0)));
    assert_eq!(step(&desk), 3);
}

#[test]
fn a_pane_made_for_a_lesson_ended_since_is_closed_not_taken_for_the_next() {
    let mut desk = one_terminal(Some(seen_everything()));
    command(&mut desk, "Tutor basics");
    command(&mut desk, "Tutor wrap");
    let asked = take_requests(&mut desk);
    assert!(
        matches!(asked[..], [Request::NewTab { .. }, Request::NewTab { .. }]),
        "{asked:?}"
    );
    desk.workspaces_changed(workspaces(vec![with_lesson(
        pane(2),
        vec![tab(3, file_pane(4, 7))],
        2,
    )]));
    desk.answered(Response::PaneCreated(PaneId(2)));
    let later = take_requests(&mut desk);
    assert!(
        later.contains(&Request::ClosePane { pane: PaneId(2) }),
        "{later:?}"
    );
    // The second lesson's tab, asked for before those.
    desk.answered(Response::PaneCreated(PaneId(4)));
    for _ in later {
        desk.answered(Response::Done);
    }
    assert_eq!(desk.tutor.open.as_ref().unwrap().lesson.id, "wrap");
    let asked = take_requests(&mut desk);
    assert!(
        asked.iter().any(|request| matches!(
            request,
            Request::Split {
                pane: PaneId(4),
                ..
            }
        )),
        "{asked:?}"
    );
}

#[test]
fn the_tutor_says_why_it_cant_open_a_lesson() {
    let mut desk = Desk::default();
    desk.start_tutor(Start {
        unavailable: Some("the tutor needs the daemon on this machine, not another".into()),
        ..Start::default()
    });
    desk.connected(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        false,
    );
    answer_all(&mut desk);
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    command(&mut desk, "Tutor basics");
    assert!(desk.message.as_deref().unwrap().contains("on this machine"));
    command(&mut desk, "Tutor nothing");
    assert_eq!(desk.message.as_deref(), Some("no lesson ‘nothing’"));
    command(&mut desk, "Tutor basic");
    assert_eq!(
        desk.message.as_deref(),
        Some("no lesson ‘basic’: did you mean ‘:Tutor basics’?")
    );
}

// --- Where the files are made ---------------------------------------------

/// The ID of the window's daemon's machine.
const OWN: clackatoo_protocol::MachineId = clackatoo_protocol::MachineId([0xa1; 16]);

/// The folder a daemon on another machine made a lesson's files in.
const MADE: &str = "/run/user/1000/clackatoo/tutor/20261109-101500-1";

/// `list`, from a daemon that says it's on [`OWN`].
fn on_own(list: Vec<Workspace>) -> Workspaces {
    let mut workspaces = workspaces(list);
    workspaces.machine = Some(clackatoo_protocol::Machine {
        id: OWN,
        name: "desk".into(),
        run: 1,
    });
    workspaces
}

/// A desk connected to a daemon on another machine of protocol `wire`,
/// whose tutor has no scratch directory, as such a window's hasn't, in
/// one workspace, "w", of one tab with a terminal in normal mode.
fn remote(wire: u32) -> Desk {
    let mut desk = Desk::default();
    desk.start_tutor(Start {
        state: Some(seen_everything()),
        state_path: Some(STATE.into()),
        unavailable: Some("the tutor needs the daemon on this machine, not another".into()),
        ..Start::default()
    });
    desk.daemon_is(wire, None);
    desk.connected(on_own(vec![workspace(1, "w", vec![tab(1, pane(1))])]), true);
    answer_all(&mut desk);
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    desk.message = None;
    take_requests(&mut desk);
    desk
}

#[test]
// jack's laptop's window, on its desktop's daemon: the daemon makes the
// lesson's files, where its panes are.
fn a_daemon_on_another_machine_makes_the_lessons_files_and_its_panes_open_on_them() {
    let mut desk = remote(clackatoo_protocol::PROTOCOL_VERSION);
    let lesson = tutor::lesson("text-objects").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    command(&mut desk, "Tutor text-objects");
    assert_eq!(desk.message, None);
    let asked = take_requests(&mut desk);
    assert_eq!(
        asked,
        [Request::Tutor(TutorFiles::Make {
            lesson: "text-objects".into(),
            text: rendered("text-objects"),
            files: vec![(name.clone(), practice.clone())],
        })]
    );
    // Nothing written here.
    assert_eq!(desk.take_tutor_io(), []);
    desk.answered(Response::Text(MADE.into()));
    let asked = take_requests(&mut desk);
    let practice_file = format!("{MADE}/text-objects/{name}");
    assert!(
        matches!(
            &asked[..],
            [Request::NewTab { terminal: PaneTerminal::File(file), on: Some(OWN), .. }]
                if file.path == practice_file
        ),
        "{asked:?}"
    );
    desk.workspaces_changed(on_own(vec![with_lesson(file_pane(2, 7), vec![], 2)]));
    desk.answered(Response::PaneCreated(PaneId(2)));
    let asked = take_requests(&mut desk);
    let lesson_file = format!("{MADE}/text-objects.txt");
    assert!(
        asked.iter().any(|request| matches!(
            request,
            Request::Split { pane: PaneId(2), terminal: PaneTerminal::File(file), on: Some(OWN), .. }
                if file.path == lesson_file
        )),
        "{asked:?}"
    );
    let layout = side_by_side(vec![file_pane(3, 8), file_pane(2, 7)]);
    desk.workspaces_changed(on_own(vec![with_lesson(layout, vec![], 2)]));
    for request in asked {
        desk.answered(match request {
            Request::Split { .. } => Response::PaneCreated(PaneId(3)),
            _ => Response::Done,
        });
    }
    answer_all(&mut desk);
    load(&mut desk, 8, &lesson_file, &rendered("text-objects"));
    load(&mut desk, 7, &practice_file, practice);
    take_requests(&mut desk);

    // A change to the lesson is put back: written again there first.
    desk.show_pane(PaneId(3));
    type_keys(&mut desk, "dd");
    let asked = take_requests(&mut desk);
    let rewrite = Request::Tutor(TutorFiles::Rewrite {
        dir: MADE.into(),
        lesson: "text-objects".into(),
        text: rendered("text-objects"),
    });
    let at = |wanted: &Request| asked.iter().position(|request| request == wanted);
    let discard = Request::Discard {
        buffer: BufferId(8),
    };
    assert!(
        at(&rewrite).is_some() && at(&rewrite) < at(&discard),
        "{asked:?}"
    );
    assert!(
        !(desk.take_tutor_io().iter()).any(|io| matches!(io, Io::Write { .. })),
        "nothing's written here"
    );
    answer_all(&mut desk);

    // Ending it removes them there.
    command(&mut desk, "q");
    let asked = take_requests(&mut desk);
    let remove = Request::Tutor(TutorFiles::Remove { dir: MADE.into() });
    assert!(asked.contains(&remove), "{asked:?}");
    assert!(
        asked.contains(&Request::ClosePane { pane: PaneId(2) }),
        "{asked:?}"
    );
    assert!(
        !(desk.take_tutor_io().iter()).any(|io| matches!(io, Io::Remove(_))),
        "nothing's removed here"
    );
}

#[test]
fn a_daemon_on_another_machine_makes_a_terminal_lessons_folder_to_start_it_in() {
    let mut desk = remote(clackatoo_protocol::PROTOCOL_VERSION);
    command(&mut desk, "Tutor basics");
    let asked = take_requests(&mut desk);
    assert!(
        matches!(
            &asked[..],
            [Request::Tutor(TutorFiles::Make { lesson, files, .. })]
                if lesson == "basics" && files.is_empty()
        ),
        "{asked:?}"
    );
    desk.answered(Response::Text(MADE.into()));
    let asked = take_requests(&mut desk);
    let folder = format!("{MADE}/basics");
    assert!(
        matches!(
            &asked[..],
            [Request::NewTab { terminal: PaneTerminal::New(new), on: Some(OWN), .. }]
                if new.cwd.as_deref() == Some(folder.as_str())
        ),
        "{asked:?}"
    );
}

#[test]
fn a_lesson_ended_before_its_daemon_says_where_its_files_are_has_them_removed() {
    let mut desk = remote(clackatoo_protocol::PROTOCOL_VERSION);
    command(&mut desk, "Tutor basics");
    command(&mut desk, "Tutor wrap");
    let asked = take_requests(&mut desk);
    assert!(
        matches!(asked[..], [Request::Tutor(_), Request::Tutor(_)]),
        "{asked:?}"
    );
    // The first lesson's, which has ended.
    desk.answered(Response::Text(MADE.into()));
    let asked = take_requests(&mut desk);
    assert_eq!(
        asked,
        [Request::Tutor(TutorFiles::Remove { dir: MADE.into() })]
    );
    // The second's, asked for before that.
    let second = "/run/user/1000/clackatoo/tutor/20261109-101500-2";
    desk.answered(Response::Text(second.into()));
    desk.answered(Response::Done);
    let asked = take_requests(&mut desk);
    assert!(
        matches!(&asked[..], [Request::NewTab { terminal: PaneTerminal::File(file), .. }]
            if file.path.starts_with(&format!("{second}/wrap/"))),
        "{asked:?}"
    );
    assert_eq!(desk.tutor.open.as_ref().unwrap().lesson.id, "wrap");
}

#[test]
fn a_daemon_on_another_machine_too_old_to_make_a_lessons_files_is_named_and_asked_nothing() {
    let mut desk = remote(clackatoo_protocol::wire::TUTOR_FILES - 1);
    command(&mut desk, "Tutor basics");
    let said = desk.message.clone().unwrap_or_default();
    assert!(
        said.starts_with(":Tutor on another machine needs a newer daemon")
            && said.ends_with("clackatoo update there updates it"),
        "{said}"
    );
    assert_eq!(take_requests(&mut desk), []);
    assert_eq!(desk.take_tutor_io(), []);
    assert!(desk.tutor.open.is_none());
}

#[test]
// A workspace of another machine's, or one whose focused pane is another
// machine's, would have the lesson's panes made there, away from its
// files: they're made on the window's daemon's own.
fn a_lessons_panes_are_made_on_the_machine_its_files_are_on() {
    let elsewhere = clackatoo_protocol::MachineId([0xd5; 16]);
    let mut theirs = workspace(1, "w", vec![tab(1, pane(1))]);
    theirs.home = Some(elsewhere);
    let mut desk = tutored(on_own(vec![theirs]), Some(seen_everything()));
    snapshot(&mut desk, 1);
    normal_mode(&mut desk);
    take_requests(&mut desk);
    command(&mut desk, "Tutor explorer");
    // Written here, by the window.
    assert!(
        (desk.take_tutor_io().iter())
            .any(|io| *io == Io::MakeDir(format!("{SCRATCH}/explorer").into()))
    );
    let asked = take_requests(&mut desk);
    assert!(
        matches!(asked[..], [Request::NewTab { on: Some(OWN), .. }]),
        "{asked:?}"
    );
    let mut lesson = with_lesson(file_pane(2, 7), vec![], 2);
    lesson.home = Some(elsewhere);
    desk.workspaces_changed(on_own(vec![lesson]));
    desk.answered(Response::PaneCreated(PaneId(2)));
    let asked = take_requests(&mut desk);
    assert!(
        (asked.iter()).any(|request| matches!(request, Request::Split { on: Some(OWN), .. })),
        "{asked:?}"
    );
}

// --- The picker, and what's new -------------------------------------------------

#[test]
fn colon_tutor_picks_a_lesson_in_the_finder() {
    let mut desk = one_terminal(Some(seen_everything()));
    command(&mut desk, "Tutor");
    let rows = all_finder_rows(&mut desk);
    assert_eq!(rows.len(), tutor::lessons().len());
    assert!(rows[0].starts_with("Basics  "), "{rows:?}");
    type_text(&mut desk, "text obj");
    enter(&mut desk);
    let asked = take_requests(&mut desk);
    assert!(
        matches!(&asked[..], [Request::NewTab { terminal: PaneTerminal::File(file), .. }] if file.path.ends_with("text-objects/shapes.rs")),
        "{asked:?}"
    );
}

#[test]
fn whats_new_is_noticed_once_for_each_build_with_something_new() {
    // Nothing remembered: clackatoo's first run, which welcomes you.
    let mut desk = tutored(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        None,
    );
    assert_eq!(
        desk.message.as_deref(),
        Some("Welcome to clackatoo · :Tutor to start")
    );
    // Once a window at most, even if it couldn't be written down, as in a
    // directory the window may only read: connecting again says nothing.
    desk.message = None;
    desk.connected(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        false,
    );
    assert_eq!(desk.message, None);
    let io = desk.take_tutor_io();
    let [Io::Save { path, state }] = &io[..] else {
        panic!("{io:?}");
    };
    assert_eq!(path, std::path::Path::new(STATE));
    // The next window, of the same build, says nothing.
    let remembered = state.clone();
    let desk = tutored(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        Some(remembered.clone()),
    );
    assert_eq!(desk.message, None);
    // A build with a line more says so again.
    let mut older = remembered;
    let newest = tutor::news::NEWS.last().unwrap().key();
    older.noticed.remove(&newest);
    let desk = tutored(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        Some(older),
    );
    assert_eq!(
        desk.message.as_deref(),
        Some("clackatoo updated · :Tutor new")
    );
}

#[test]
fn whats_new_has_the_bar_until_a_key_then_a_release_out_says_so() {
    let mut desk = Desk::default();
    desk.set_notice(Some("clackatoo 2026.10.3 is out: :update".into()));
    let mut older = seen_everything();
    older.noticed.remove(tutor::news::NEWS.last().unwrap().id);
    desk.start_tutor(Start {
        state: Some(older),
        scratch: Some(SCRATCH.into()),
        ..Start::default()
    });
    desk.connected(
        workspaces(vec![workspace(1, "w", vec![tab(1, pane(1))])]),
        false,
    );
    answer_all(&mut desk);
    snapshot(&mut desk, 1);
    assert_eq!(note(&desk), "clackatoo updated · :Tutor new");
    normal_mode(&mut desk);
    assert_eq!(note(&desk), "clackatoo 2026.10.3 is out: :update");
}

#[test]
fn colon_tutor_new_lists_whats_new_unread_first_and_goes_to_its_lesson() {
    let mut state = seen_everything();
    let newest = tutor::news::NEWS.last().unwrap();
    state.read.remove(&newest.key());
    let mut desk = one_terminal(Some(state));
    command(&mut desk, "Tutor new");
    let rows = all_finder_rows(&mut desk);
    assert_eq!(rows.len(), tutor::news::NEWS.len());
    assert!(
        rows[0].starts_with(&format!("new  {}", newest.what)),
        "{rows:?}"
    );
    assert!(!rows[1].starts_with("new"), "{rows:?}");
    // Read from now on.
    assert!(desk.tutor.state.read.contains(&newest.key()));
    enter(&mut desk);
    let asked = take_requests(&mut desk);
    assert!(matches!(asked[..], [Request::NewTab { .. }]), "{asked:?}");
    assert_eq!(desk.tutor.open.as_ref().unwrap().lesson.id, newest.lesson);
}

#[test]
fn leftover_lesson_panes_of_windows_gone_are_closed() {
    let mut desk = Desk::default();
    desk.start_tutor(Start {
        state: Some(seen_everything()),
        scratch: Some("/scratch/2".into()),
        leftovers: vec![PathBuf::from("/scratch/1")],
        ..Start::default()
    });
    let layout = side_by_side(vec![file_pane(3, 8), pane(2)]);
    desk.connected(
        workspaces(vec![workspace(1, "w", vec![tab(1, layout)])]),
        false,
    );
    answer_all(&mut desk);
    load(&mut desk, 8, "/scratch/1/basics.txt", "the lesson\n");
    let asked = take_requests(&mut desk);
    assert_eq!(asked, [Request::ClosePane { pane: PaneId(3) }]);
    // Once nothing shown is theirs, they're forgotten, and keys cost
    // nothing more.
    desk.workspaces_changed(workspaces(vec![workspace(1, "w", vec![tab(1, pane(2))])]));
    answer_all(&mut desk);
    assert!(desk.tutor.leftovers.is_empty());
    snapshot(&mut desk, 2);
    normal_mode(&mut desk);
    type_keys(&mut desk, "jk");
    assert!(take_requests(&mut desk).is_empty());
}

#[test]
fn a_file_of_yours_gone_to_in_the_practice_pane_keeps_its_changes_when_the_lesson_ends() {
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "text-objects", file_pane(2, 7));
    let lesson = tutor::lesson("text-objects").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    load(
        &mut desk,
        7,
        &format!("{SCRATCH}/text-objects/{name}"),
        practice,
    );
    // `:e` a real file in the practice pane, and change it.
    let layout = side_by_side(vec![file_pane(3, 8), file_pane(2, 9)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    load(&mut desk, 9, "/home/me/notes.txt", "mine\n");
    type_keys(&mut desk, "x");
    desk.take_outbox();
    command(&mut desk, "q");
    let asked = take_requests(&mut desk);
    // Only the lesson pane goes.
    assert_eq!(asked, [Request::ClosePane { pane: PaneId(3) }]);
    assert!(desk.tutor.open.is_none());
    assert_eq!(editor_text(&desk), "ine\n", "the change is kept");
}

#[test]
fn a_lessons_explorer_stays_in_the_lessons_folder() {
    use clackatoo_protocol::{FileOp, FilesId, FilesUpdate};
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "explorer", file_pane(2, 7));
    let dir = format!("{SCRATCH}/explorer");
    let layout = side_by_side(vec![files_pane(4, 1), file_pane(3, 8), file_pane(2, 7)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    desk.files_changed(
        FilesId(1),
        FilesUpdate::Root {
            path: dir.clone(),
            name: "explorer".into(),
        },
    );
    assert!(desk.explorer(PaneId(4)).unwrap().fenced());
    desk.take_outbox();
    // Nothing outside the folder is deleted from it…
    desk.file_op(
        PaneId(4),
        FileOp::Delete {
            path: "/home/me/notes.txt".into(),
            trash: true,
            expect: None,
        },
    );
    assert!(desk.take_outbox().is_empty());
    assert_eq!(
        desk.explorer(PaneId(4)).unwrap().label(),
        crate::explorer::FENCED
    );
    // …though what's in it is.
    desk.file_op(
        PaneId(4),
        FileOp::Delete {
            path: format!("{dir}/plans.txt"),
            trash: true,
            expect: None,
        },
    );
    assert!(matches!(
        desk.take_outbox()[..],
        [ClientMessage::FileOp { .. }]
    ));
}

#[test]
fn the_explorer_lesson_roots_its_explorer_at_a_folder_and_goes_back_up_with_dot_dot() {
    use clackatoo_protocol::{FilesAction, FilesId, FilesUpdate};
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "explorer", file_pane(2, 7));
    let dir = format!("{SCRATCH}/explorer");
    let drafts = format!("{dir}/drafts");
    load(&mut desk, 7, &format!("{dir}/done.txt"), "Things to do.\n");
    let layout = side_by_side(vec![files_pane(4, 1), file_pane(3, 8), file_pane(2, 7)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    let rooted = |desk: &mut Desk, path: &str| {
        let root = FilesUpdate::Root {
            path: path.to_owned(),
            name: path.rsplit('/').next().unwrap().to_owned(),
        };
        desk.files_changed(FilesId(1), root);
        let listed = FilesUpdate::Listed {
            dir: path.to_owned(),
            entries: Vec::new(),
            cut_short: false,
        };
        desk.files_changed(FilesId(1), listed);
    };
    rooted(&mut desk, &dir);
    let lesson = tutor::lesson("explorer").unwrap();
    let root_it = (lesson.steps.iter())
        .position(|step| step.title == "Root it at a folder")
        .expect("the step");
    desk.tutor.open.as_mut().unwrap().step = root_it;
    desk.show_pane(PaneId(4));
    desk.settle_lesson();
    let root_asked = |desk: &mut Desk, path: &str| {
        (desk.take_outbox().iter()).any(|message| {
            matches!(message, ClientMessage::Files { action: FilesAction::Root(root), .. } if root == path)
        })
    };
    // At the lesson's folder, which its explorer stays in, there's no `..`.
    assert_eq!(
        desk.explorer(PaneId(4)).unwrap().label(),
        "explorer  g? keys",
        "no line at the cursor: the folder's empty, and has no .."
    );
    type_keys(&mut desk, ":Explore drafts");
    enter(&mut desk);
    assert!(root_asked(&mut desk, &drafts));
    rooted(&mut desk, &drafts);
    assert_eq!(step(&desk), root_it + 1);
    // In drafts, `..` goes back up to the lesson's folder.
    type_keys(&mut desk, "gg");
    assert_eq!(
        desk.explorer(PaneId(4)).unwrap().label(),
        "drafts  ..  g? keys"
    );
    enter(&mut desk);
    assert!(root_asked(&mut desk, &dir));
    rooted(&mut desk, &dir);
    assert_eq!(step(&desk), root_it + 2);
}

#[test]
fn the_explorer_lessons_editor_goes_to_its_folder_with_minus_and_opens_a_file_from_it() {
    use clackatoo_protocol::{FilesId, FilesUpdate};
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "explorer", file_pane(2, 7));
    let dir = format!("{SCRATCH}/explorer");
    load(&mut desk, 7, &format!("{dir}/done.txt"), "Things to do.\n");
    let lesson = tutor::lesson("explorer").unwrap();
    let browse = (lesson.steps.iter())
        .position(|step| step.title == "Browse a folder in an editor")
        .expect("the step");
    desk.tutor.open.as_mut().unwrap().step = browse;
    desk.show_pane(PaneId(2));
    desk.settle_lesson();
    // `-` in the practice pane's file: its folder, the lesson's.
    type_keys(&mut desk, "-");
    let asked = answer_all(&mut desk);
    assert!(
        asked.iter().any(
            |request| matches!(request, Request::OpenHere { pane: PaneId(2), file } if *file.path == format!("{dir}/"))
        ),
        "{asked:?}"
    );
    let folder = Layout::Pane(LayoutPane {
        id: PaneId(2),
        content: Content::Folder(FilesId(5)),
        home: None,
        key: None,
    });
    let layout = side_by_side(vec![file_pane(3, 8), folder]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    let root = FilesUpdate::Root {
        path: dir.clone(),
        name: "explorer".into(),
    };
    desk.files_changed(FilesId(5), root);
    let entries = vec![
        entry("drafts", true),
        entry("done.txt", false),
        entry("notes.txt", false),
    ];
    let listed = |desk: &mut Desk, dir: &str, entries: Vec<clackatoo_protocol::Entry>| {
        let listed = FilesUpdate::Listed {
            dir: dir.into(),
            entries,
            cut_short: false,
        };
        desk.files_changed(FilesId(5), listed);
    };
    let rooted = |desk: &mut Desk, path: &str| {
        let root = FilesUpdate::Root {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
        };
        desk.files_changed(FilesId(5), root);
    };
    let enter = |desk: &mut Desk| {
        press(
            desk,
            Key::Named(NamedKey::Enter),
            None,
            Modifiers::default(),
        );
    };
    listed(&mut desk, &dir, entries.clone());
    assert_eq!(step(&desk), browse + 1);
    // Kept in the lesson's folder, as its explorer is: no `..`, and `-`
    // goes nowhere.
    assert_eq!(listing_shown(&desk), ["drafts/", "done.txt", "notes.txt"]);
    type_keys(&mut desk, "-");
    assert!(answer_all(&mut desk).is_empty());
    // Into drafts, which is empty but for `..`.
    let drafts = format!("{dir}/drafts");
    type_keys(&mut desk, "gg");
    enter(&mut desk);
    rooted(&mut desk, &drafts);
    listed(&mut desk, &drafts, Vec::new());
    assert_eq!(listing_shown(&desk), [".."]);
    assert_eq!(step(&desk), browse + 2);
    // Enter on `..` goes back up, onto drafts/.
    enter(&mut desk);
    rooted(&mut desk, &dir);
    listed(&mut desk, &dir, entries);
    assert_eq!(listing_shown(&desk), ["drafts/", "done.txt", "notes.txt"]);
    assert_eq!(step(&desk), browse + 3);
    // Enter on notes.txt opens it in the listing's place.
    type_keys(&mut desk, "jj");
    enter(&mut desk);
    let notes = format!("{dir}/notes.txt");
    let asked = answer_all(&mut desk);
    assert!(
        asked.iter().any(
            |request| matches!(request, Request::OpenHere { pane: PaneId(2), file } if file.path == notes)
        ),
        "{asked:?}"
    );
    let layout = side_by_side(vec![file_pane(3, 8), file_pane(2, 9)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    load(&mut desk, 9, &notes, "Notes for the explorer lesson.\n");
    assert_eq!(step(&desk), browse + 4);
}

#[test]
fn the_explorer_lessons_e_alone_opens_the_explorer_beside_a_blank_file_which_w_names() {
    use clackatoo_protocol::{FilesAction, FilesId, FilesUpdate, OpenBlank};
    let mut desk = one_terminal(Some(seen_everything()));
    let dir = format!("{SCRATCH}/explorer");
    let explorer = Layout::Pane(LayoutPane {
        id: PaneId(4),
        content: Content::Files(FilesId(5)),
        home: None,
        key: None,
    });
    open(&mut desk, "explorer", file_pane(2, 9));
    let layout = side_by_side(vec![file_pane(3, 8), explorer.clone(), file_pane(2, 9)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    let root = FilesUpdate::Root {
        path: dir.clone(),
        name: "explorer".into(),
    };
    desk.files_changed(FilesId(5), root);
    load(&mut desk, 9, &format!("{dir}/notes.txt"), "Notes.\n");
    let lesson = tutor::lesson("explorer").unwrap();
    let start = (lesson.steps.iter())
        .position(|step| step.title == "Start a new file")
        .expect("the step");
    desk.tutor.open.as_mut().unwrap().step = start;
    desk.show_pane(PaneId(2));
    desk.settle_lesson();
    answer_all(&mut desk);
    assert_eq!(step(&desk), start, "not done before it's started");

    // `:e` alone: a blank file in place of notes.txt, from its folder, the
    // lesson's, which the tab's explorer, kept in it, stays at.
    command(&mut desk, "e");
    let messages = desk.take_outbox();
    assert!(
        messages.iter().any(|message| matches!(
            message,
            ClientMessage::Request(Request::BlankHere { pane: PaneId(2), blank: OpenBlank { cwd } })
                if cwd.as_deref() == Some(dir.as_str())
        )),
        "{messages:?}"
    );
    assert!(messages.iter().any(|message| matches!(
        message,
        ClientMessage::Files { files: FilesId(5), action: FilesAction::Root(root) } if *root == dir
    )));
    let layout = side_by_side(vec![file_pane(3, 8), explorer.clone(), file_pane(2, 10)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    assert_eq!(step(&desk), start, "its text hasn't come");
    let blank = clackatoo_protocol::BufferSnapshot {
        path: String::new(),
        text: String::new(),
        version: 0,
        modified: false,
        applied: None,
        landed: None,
        daemon: 0,
    };
    desk.buffer_changed(BufferId(10), BufferUpdate::Snapshot(blank));
    assert_eq!(step(&desk), start + 1);

    // Named, from the editor, as the explorer lists it.
    desk.show_pane(PaneId(2));
    desk.buffer_changed(
        BufferId(10),
        BufferUpdate::Moved(format!("{dir}/later.txt")),
    );
    let listed = |dir: &str, entries| FilesUpdate::Listed {
        dir: dir.to_owned(),
        entries,
        cut_short: false,
    };
    let root = vec![
        entry("drafts", true),
        entry("later.txt", false),
        entry("notes.txt", false),
    ];
    desk.files_changed(FilesId(5), listed(&dir, root));
    desk.settle_lesson();
    assert_eq!(step(&desk), start + 2);

    // Dragged onto `drafts`, the line above it: no `..` in a lesson's
    // explorer, kept in the lesson's folder.
    let to = drag_row(&mut desk, PaneId(4), 2, 1);
    assert_eq!(to.1, format!("{dir}/drafts/later.txt"));
    desk.file_op_done(to.0, Ok(()));
    desk.files_changed(
        FilesId(5),
        listed(&dir, vec![entry("drafts", true), entry("notes.txt", false)]),
    );
    let drafts = format!("{dir}/drafts");
    desk.files_changed(FilesId(5), listed(&drafts, vec![entry("later.txt", false)]));
    desk.settle_lesson();
    assert_eq!(step(&desk), start + 3);
}

/// Drags what's on row `from` of the explorer `pane` onto row `to`, and
/// returns the move asked for: its number, and where to.
fn drag_row(desk: &mut Desk, pane: PaneId, from: u16, to: u16) -> (u64, String) {
    desk.take_outbox();
    let tile = (desk.placement().unwrap().panes.into_iter())
        .find(|(id, _)| *id == pane)
        .unwrap()
        .1;
    let row = |row: u16| {
        [
            tile.origin[0] + 25.0,
            tile.origin[1] + f32::from(row) * 20.0 + 10.0,
        ]
    };
    let left = MouseButton::Left;
    let none = Modifiers::default();
    desk.mouse(row(from), MouseAction::Press(left), 1, none);
    desk.mouse(row(to), MouseAction::Motion(Some(left)), 1, none);
    desk.mouse(row(to), MouseAction::Release(left), 1, none);
    let moves: Vec<(u64, String)> = (desk.take_outbox().into_iter())
        .filter_map(|message| match message {
            ClientMessage::FileOp {
                id,
                op: clackatoo_protocol::FileOp::Move { to, .. },
                ..
            } => Some((id, to)),
            _ => None,
        })
        .collect();
    let [asked] = &moves[..] else {
        panic!("one move: {moves:?}");
    };
    asked.clone()
}

#[test]
fn the_explorer_lessons_search_step_is_done_by_searching_the_names() {
    use clackatoo_protocol::{FilesId, FilesUpdate};
    let mut desk = one_terminal(Some(seen_everything()));
    open(&mut desk, "explorer", file_pane(2, 7));
    let dir = format!("{SCRATCH}/explorer");
    load(
        &mut desk,
        7,
        &format!("{dir}/notes.txt"),
        "Notes for the explorer lesson.\n",
    );
    let layout = side_by_side(vec![files_pane(4, 1), file_pane(3, 8), file_pane(2, 7)]);
    desk.workspaces_changed(workspaces(vec![with_lesson(layout, vec![], 2)]));
    answer_all(&mut desk);
    let root = FilesUpdate::Root {
        path: dir.clone(),
        name: "explorer".to_owned(),
    };
    desk.files_changed(FilesId(1), root);
    let file = |name: &str| clackatoo_protocol::Entry {
        name: name.to_owned(),
        kind: clackatoo_protocol::EntryKind::File { executable: false },
        ignored: false,
        stamp: None,
    };
    let listed = FilesUpdate::Listed {
        dir,
        entries: vec![file("notes.txt"), file("todo.txt")],
        cut_short: false,
    };
    desk.files_changed(FilesId(1), listed);
    let lesson = tutor::lesson("explorer").unwrap();
    let search = (lesson.steps.iter())
        .position(|step| step.title == "Move and search as in Vim")
        .expect("the step");
    desk.tutor.open.as_mut().unwrap().step = search;
    desk.show_pane(PaneId(4));
    desk.settle_lesson();
    assert_eq!(step(&desk), search, "not done before it's started");
    type_keys(&mut desk, "/todo");
    enter(&mut desk);
    desk.settle_lesson();
    assert_eq!(step(&desk), search + 1);
    // The next is done as the search typed shows where it goes, before
    // Enter.
    desk.settle_lesson();
    assert_eq!(step(&desk), search + 1, "not done before it's started");
    type_keys(&mut desk, "?not");
    desk.settle_lesson();
    assert_eq!(step(&desk), search + 1, "not shown yet");
    desk.preview_searches();
    desk.settle_lesson();
    assert_eq!(step(&desk), search + 2);
}

// --- Against a daemon whose files the window can't see ---------------------

/// A daemon on a thread of its own, with its home and socket in a folder
/// of its own, as a daemon on another machine has: the window has no
/// scratch directory there.
struct Elsewhere {
    socket: PathBuf,
    shutdown: clackatoo_daemon::ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Elsewhere {
    fn start(dir: &Path) -> Self {
        use clackatoo_daemon::{AgentConfig, Daemon, DaemonConfig};
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let config = DaemonConfig {
            shell: "/bin/sh".into(),
            home: dir.to_owned(),
            shell_integration: None,
            agents: AgentConfig::default(),
            mcp: None,
            udp_ports: None,
            languages: None,
            servers: None,
            declined: None,
            trusted: clackatoo_daemon::Trusting::All,
            lsp_trace: None,
            boards: None,
            recovery: None,
            browser: None,
            web: None,
            trash: None,
            workspaces: None,
            keep_gate: None,
            // Never the machine of the user running the tests.
            machine: None,
            machine_name: None,
            display: None,
            links: None,
            logind: None,
            pastes: None,
            ..DaemonConfig::from_env()
        };
        let socket = run.join("daemon.sock");
        let mut daemon = Daemon::bind(&socket, config).unwrap();
        let shutdown = daemon.shutdown_handle();
        let thread = std::thread::spawn(move || drop(daemon.run().unwrap()));
        Self {
            socket,
            shutdown,
            thread: Some(thread),
        }
    }
}

impl Drop for Elsewhere {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

/// Sends what the desk has for the daemon, and gives it what the daemon
/// says back, the workspaces and buffers changing and the answers, until
/// it has nothing more to send.
fn pump(desk: &mut Desk, client: &mut clackatoo_daemon::client::Client) {
    use clackatoo_protocol::Event;
    let heard = |desk: &mut Desk, client: &mut clackatoo_daemon::client::Client| {
        for event in client.take_events() {
            match event {
                Event::Workspaces(workspaces) => desk.workspaces_changed(workspaces),
                Event::Buffer { buffer, update } => {
                    desk.buffer_changed(buffer, update);
                }
                _ => {}
            }
        }
    };
    for _ in 0..100 {
        let outbox = desk.take_outbox();
        if outbox.is_empty() {
            return;
        }
        for message in outbox {
            match message {
                ClientMessage::Request(request) => {
                    let response = client.request(&request).unwrap();
                    heard(desk, client);
                    desk.answered(response);
                }
                ClientMessage::Edit {
                    buffer,
                    seq,
                    change,
                } => client.edit(buffer, seq, change).unwrap(),
                // Frames drawn, places: nothing a lesson waits on.
                _ => {}
            }
        }
        heard(desk, client);
    }
    panic!("the desk never stopped asking");
}

/// Waits for the daemon's thread to remove `folder`, a lesson's that's
/// ended.
fn wait_until_gone(folder: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while folder.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "{} never removed",
            folder.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The text of buffer `buffer`, as the desk has it.
fn copy_text(desk: &Desk, buffer: Option<BufferId>) -> String {
    let copy = buffer.and_then(|buffer| desk.buffer_copy(buffer));
    copy.expect("the buffer").text().to_string()
}

#[test]
// jack's laptop's window on his desktop's daemon: the lesson's practice
// terminal couldn't start, in a folder made on the laptop.
fn lessons_open_heal_and_end_on_a_daemon_whose_files_the_window_cant_see() {
    let there = tempfile::tempdir().unwrap();
    let daemon = Elsewhere::start(there.path());
    let mut client = clackatoo_daemon::client::Client::connect(&daemon.socket).unwrap();
    let shell = PaneTerminal::New(NewTerminal {
        size: Size::new(40, 24).unwrap(),
        command: None,
        cwd: None,
        record_to: None,
    });
    client.new_workspace(Some("w".into()), shell).unwrap();
    let workspaces = client.watch_workspaces().unwrap();
    let terminal = workspaces.list[0].tabs[0].layout.panes()[0]
        .content
        .terminal()
        .unwrap();
    // A window with no scratch directory, as on another machine.
    let mut desk = Desk::default();
    desk.set_area(Some(Area {
        rect: crate::layout::Rect {
            x: 0.0,
            y: 0.0,
            width: 1210.0,
            height: 480.0,
        },
        cell: [10.0, 20.0],
        gap: 10.0,
        inset: 5.0,
    }));
    desk.start_tutor(Start {
        state: Some(seen_everything()),
        unavailable: Some("the tutor needs the daemon on this machine, not another".into()),
        ..Start::default()
    });
    desk.daemon_is(client.wire(), None);
    desk.connected(workspaces, true);
    pump(&mut desk, &mut client);
    snapshot(&mut desk, terminal.0);
    normal_mode(&mut desk);
    pump(&mut desk, &mut client);

    // A terminal lesson: its shell starts in its folder there.
    command(&mut desk, "Tutor basics");
    pump(&mut desk, &mut client);
    assert_eq!(desk.message, None);
    let lessons = std::fs::canonicalize(there.path().join("run/tutor")).unwrap();
    let open = desk.tutor.open.as_ref().expect("the lesson open");
    assert!(open.dir.starts_with(&lessons), "{}", open.dir.display());
    assert!(open.dir.is_dir());
    let text = std::fs::read_to_string(&open.lesson_file).unwrap();
    assert_eq!(text, rendered("basics"));
    let practice = open.practice.expect("the practice pane");
    let (_, _, pane) = desk.workspaces.find_pane(practice).unwrap();
    let shell = pane.content.terminal().expect("a terminal");
    let info = client.list_terminals().unwrap();
    let info = info.iter().find(|info| info.id == shell).unwrap();
    let pid = info.pid.expect("its shell runs");
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).unwrap();
    assert_eq!(cwd, open.dir);
    let folder = open.dir.parent().unwrap().to_owned();
    command(&mut desk, "q");
    pump(&mut desk, &mut client);
    assert!(desk.tutor.open.is_none());
    wait_until_gone(&folder);

    // An editor lesson: its practice file is there, and a change to the
    // lesson is put back there.
    command(&mut desk, "Tutor text-objects");
    pump(&mut desk, &mut client);
    assert_eq!(desk.message, None);
    let lesson = tutor::lesson("text-objects").unwrap();
    let (name, practice) = lesson.practice_file().unwrap();
    let open = desk.tutor.open.as_ref().expect("the lesson open");
    assert!(open.dir.starts_with(&lessons), "{}", open.dir.display());
    assert_eq!(
        std::fs::read_to_string(open.dir.join(name)).unwrap(),
        *practice
    );
    let (lesson_buffer, practice_buffer) = (open.lesson_buffer, open.practice_buffer);
    let (lesson_file, lesson_pane) = (open.lesson_file.clone(), open.pane.unwrap());
    pump(&mut desk, &mut client);
    assert_eq!(copy_text(&desk, practice_buffer), *practice);
    assert_eq!(copy_text(&desk, lesson_buffer), rendered("text-objects"));
    std::fs::write(&lesson_file, "written over\n").unwrap();
    desk.show_pane(lesson_pane);
    type_keys(&mut desk, "dd");
    pump(&mut desk, &mut client);
    assert!(
        (desk.message.as_deref()).is_some_and(|said| said.contains("can't be changed")),
        "{:?}",
        desk.message
    );
    assert_eq!(copy_text(&desk, lesson_buffer), rendered("text-objects"));
    assert_eq!(
        std::fs::read_to_string(&lesson_file).unwrap(),
        rendered("text-objects")
    );
    let folder = lesson_file.parent().unwrap().to_owned();
    command(&mut desk, "q");
    pump(&mut desk, &mut client);
    assert!(desk.tutor.open.is_none());
    wait_until_gone(&folder);
    // Nothing was written where the window is.
    assert!(
        !(desk.take_tutor_io().iter())
            .any(|io| matches!(io, Io::Write { .. } | Io::MakeDir(_) | Io::Remove(_))),
    );
}
