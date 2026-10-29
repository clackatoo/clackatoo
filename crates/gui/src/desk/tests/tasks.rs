//! Following what an agent's background task printed: `:taskoutput`, or
//! a click on its line, opens a terminal below the chat running `tail` on
//! the task's output file, with no shell, or says why there's nothing to
//! follow (CLACKA-199).

use clackatoo_protocol::{AgentTask, AgentTaskKind, AgentTaskState, ChatNode, NewTerminal};

use super::super::chats::tail_program;
use super::*;

/// A shell the agent runs in the background, `id`, started at `started`,
/// its output going to `output` if it said.
fn shell(id: &str, started: u64, output: Option<&str>) -> AgentTask {
    AgentTask {
        id: id.into(),
        kind: AgentTaskKind::Shell,
        title: format!("Serve {id}"),
        command: Some("npm run dev".into()),
        state: AgentTaskState::Running,
        summary: None,
        started,
        ended: None,
        can_stop: true,
        output: output.map(Into::into),
    }
}

/// Gives chat `chat` its view: its agent runs `tasks` in the background.
fn show_chat(desk: &mut Desk, chat: u32, tasks: Vec<AgentTask>) {
    let node = ChatNode {
        agent: "Claude Code".into(),
        tasks,
        ..ChatNode::default()
    };
    desk.view_changed(
        TerminalId(chat),
        &ViewUpdate::Snapshot(View {
            title: format!("chat {chat}"),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(node)),
        }),
    );
}

/// A desk showing chat 2, whose agent runs `tasks` in the background.
fn chat_desk(tasks: Vec<AgentTask>) -> Desk {
    let mut desk = desk(workspaces(vec![workspace(1, "work", vec![tab(1, pane(2))])]));
    show_chat(&mut desk, 2, tasks);
    answer_all(&mut desk);
    normal_mode(&mut desk);
    desk
}

/// The pane split and the program the new terminal runs, if `asked` is
/// one split below a pane.
fn split_below(asked: &[Request]) -> (PaneId, Vec<String>) {
    let [
        Request::Split {
            pane,
            side: Side::Below,
            terminal: PaneTerminal::New(NewTerminal {
                command: Some(run), ..
            }),
            on: None,
        },
    ] = asked
    else {
        panic!("a terminal split off below: {asked:?}");
    };
    (*pane, run.clone())
}

/// What follows `file`: `tail`, by its path here, and its arguments, each
/// its own, with no shell to read them.
fn tail(file: &str) -> Vec<String> {
    let tail = tail_program(false);
    [tail, "-c", "262144", "-F", "--", file]
        .map(str::to_owned)
        .to_vec()
}

#[test]
fn colon_taskoutput_follows_a_tasks_output_in_a_terminal_below_the_chat() {
    let file = "/tmp/c/tasks/t2.output";
    let mut desk = chat_desk(vec![shell("t1", 100, None), shell("t2", 200, Some(file))]);
    // The newest with output, with none named.
    command(&mut desk, "taskoutput");
    let (pane, run) = split_below(&answer_all(&mut desk));
    assert_eq!(pane, PaneId(2));
    assert_eq!(run, tail(file));

    // The line under it, the older, says nothing of where it prints.
    command(&mut desk, "taskoutput 2");
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(
        note(&desk),
        "the agent didn't say where Serve t1 prints: ask it to show the output"
    );
    command(&mut desk, "taskoutput 3");
    assert_eq!(note(&desk), "there's no line 3 above the box");
}

#[test]
fn colon_taskoutput_with_nothing_to_follow_says_why() {
    let mut desk = chat_desk(Vec::new());
    command(&mut desk, "taskoutput");
    assert_eq!(note(&desk), "the agent runs nothing in the background");
    let mut desk = chat_desk(vec![shell("t1", 100, None)]);
    command(&mut desk, "taskoutput");
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(
        note(&desk),
        "the agent didn't say where any of it prints: ask it to show the output"
    );
}

/// A click on a task's line names the chat and the task, not a line: with
/// two chats side by side and the focus in the other, it opens that task
/// below its own chat, though the focused chat's shelf lists another task
/// first, and though its own shelf moved on since.
#[test]
fn a_click_follows_the_task_it_was_on_below_its_own_chat_whichever_has_the_focus() {
    let mut desk = desk(workspaces(vec![workspace(
        1,
        "work",
        vec![tab(1, side_by_side(vec![pane(2), pane(3)]))],
    )]));
    let mine = "/tmp/c/tasks/a.output";
    show_chat(&mut desk, 2, vec![shell("b", 300, Some("/tmp/c/tasks/b.output"))]);
    show_chat(&mut desk, 3, vec![shell("a", 100, Some(mine))]);
    answer_all(&mut desk);
    normal_mode(&mut desk);
    assert_eq!(desk.shown().unwrap().pane, PaneId(2), "the other chat");
    // A newer task came to chat 3's shelf's first line since the click.
    show_chat(
        &mut desk,
        3,
        vec![
            shell("a", 100, Some(mine)),
            shell("c", 400, Some("/tmp/c/tasks/c.output")),
        ],
    );
    answer_all(&mut desk);
    let chat = desk.chats.get_mut(&TerminalId(3)).unwrap();
    chat.view_mut().follows.push("a".into());
    let mut fonts = crate::chat::fixtures::fonts();
    desk.settle_chats(&mut fonts);
    let (pane, run) = split_below(&answer_all(&mut desk));
    assert_eq!(pane, PaneId(3));
    assert_eq!(run, tail(mine));

    // One gone from the shelf by then is said to be.
    let chat = desk.chats.get_mut(&TerminalId(3)).unwrap();
    chat.view_mut().follows.push("gone".into());
    desk.settle_chats(&mut fonts);
    assert!(answer_all(&mut desk).is_empty());
    assert_eq!(
        note(&desk),
        "that task is no longer listed above the box"
    );
}
