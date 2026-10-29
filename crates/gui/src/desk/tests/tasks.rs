//! Following what an agent's background task printed: `:taskoutput`
//! opens a terminal below the chat running the program the task gives to
//! follow its output file, or says why there's nothing to follow
//! (CLACKA-199).

use clackatoo_protocol::{AgentTask, AgentTaskKind, AgentTaskState, ChatNode, NewTerminal};

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

/// A desk showing chat 2, whose agent runs `tasks` in the background.
fn chat_desk(tasks: Vec<AgentTask>) -> Desk {
    let mut desk = desk(workspaces(vec![workspace(1, "work", vec![tab(1, pane(2))])]));
    let chat = ChatNode {
        agent: "Claude Code".into(),
        tasks,
        ..ChatNode::default()
    };
    desk.view_changed(
        TerminalId(2),
        &ViewUpdate::Snapshot(View {
            title: "chat".into(),
            cwd: None,
            root: clackatoo_protocol::Node::Chat(Box::new(chat)),
        }),
    );
    answer_all(&mut desk);
    normal_mode(&mut desk);
    desk
}

#[test]
fn colon_taskoutput_follows_a_tasks_output_in_a_terminal_below_the_chat() {
    let file = "/tmp/c/tasks/t2.output";
    let mut desk = chat_desk(vec![shell("t1", 100, None), shell("t2", 200, Some(file))]);
    // The newest with output, with none named.
    command(&mut desk, "taskoutput");
    let asked = answer_all(&mut desk);
    let [Request::Split {
        pane,
        side,
        terminal: PaneTerminal::New(NewTerminal { command: run, .. }),
        on: None,
    }] = asked.as_slice()
    else {
        panic!("a terminal split off: {asked:?}");
    };
    assert_eq!((*pane, *side), (PaneId(2), Side::Below));
    assert_eq!(
        run.as_deref(),
        Some(["tail", "-c", "262144", "-F", "--", file].map(str::to_owned).as_slice())
    );

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
