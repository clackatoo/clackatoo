//! What the agent runs in the background, on a shelf above the box a
//! message is typed in: "In the background · 1 running", then a line for
//! each, saying what it runs, how it's going and for how long, with a
//! button to stop one that's running. Claude Code's shells started with
//! `run_in_background` show here, and the turns it takes by itself.
//!
//! While any is going, the shelf counts the seconds.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use clackatoo_protocol::{AgentTask, AgentTaskKind, AgentTaskState};

use super::fold::duration;
use super::{BoxFrame, Canvas, ChatView, Fill, Hit, Measures, Paint, Tone};

/// The most tasks the shelf has a line for: the rest it counts.
const SHOWN: usize = 3;

impl ChatView {
    /// What the agent runs in the background, as the shelf above the box
    /// lists it, line by line.
    pub(in crate::chat) fn shelf(&self) -> Vec<&AgentTask> {
        shelf_order(&self.node.tasks)
    }

    /// Draws the shelf above the box and whatever's above it already,
    /// `above` high, if the agent runs anything in the background or has
    /// since the last message. Returns how high it is.
    pub(super) fn draw_tasks(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        above: f32,
    ) -> f32 {
        let now = self.now();
        let shown: Vec<(String, Tone, Option<String>)> = shelf_order(&self.node.tasks)
            .into_iter()
            .take(SHOWN)
            .map(|task| {
                let stop = (task.can_stop && task.state == AgentTaskState::Running)
                    .then(|| task.id.clone());
                (task_row(task, now), task_tone(task), stop)
            })
            .collect();
        if shown.is_empty() {
            return 0.0;
        }
        let row = m.em(1.9);
        let header = m.em(1.65);
        let pad = m.em(0.45);
        let gap = m.em(0.45);
        let rows = f32::from(u8::try_from(shown.len()).unwrap_or(u8::MAX));
        let panel = header + row * rows + pad * 2.0;
        let mut top = frame.top - above - panel - gap;
        let left = frame.left + m.em(0.65);
        let width = frame.place.column - m.em(1.3);
        canvas.fill(
            Fill::rounded([left, top, width, panel], Paint::Raised, m.em(0.9))
                .bordered(1.0, Paint::RaisedEdge),
        );
        let inner_left = left + m.em(0.8);
        let inner_width = width - m.em(1.6);
        top += pad;
        let small = Measures::of(m.em(0.85));
        let heading = shelf_heading(&self.node.tasks);
        self.queue_text(
            canvas,
            small,
            &heading,
            [inner_left, top, inner_width, header],
            Tone::Muted,
        );
        top += header;
        for (index, (text, tone, stop)) in shown.into_iter().enumerate() {
            if index > 0 {
                canvas.fill(Fill::solid(
                    [inner_left, top, inner_width, 1.0],
                    Paint::RaisedEdge,
                ));
            }
            let button = m.em(1.8);
            let text_width = (inner_width - button - m.em(0.5)).max(0.0);
            self.queue_text(
                canvas,
                small,
                &text,
                [inner_left, top, text_width, row],
                tone,
            );
            if let Some(id) = stop {
                let rect = [
                    inner_left + inner_width - button,
                    top + (row - button) / 2.0,
                    button,
                    button,
                ];
                self.draw_stop_task(canvas, m, frame, rect, id);
            }
            top += row;
        }
        panel + gap
    }

    /// Draws the button that stops task `id`, in `rect`: the square the
    /// chat's own Stop is.
    fn draw_stop_task(
        &mut self,
        canvas: &mut Canvas<'_>,
        m: Measures,
        frame: &BoxFrame,
        rect: [f32; 4],
        id: String,
    ) {
        let side = rect[2];
        if self
            .mouse
            .is_some_and(|p| super::super::contains(frame.place.in_pane(rect), p))
        {
            canvas.fill(Fill::rounded(rect, Paint::ChatHover, side / 2.0));
        }
        let extent = m.em(0.5);
        canvas.fill(Fill::rounded(
            [
                rect[0] + (side - extent) / 2.0,
                rect[1] + (side - extent) / 2.0,
                extent,
                extent,
            ],
            Paint::Icon,
            m.em(0.1),
        ));
        self.hits
            .push((Hit::StopTask(id), frame.place.in_pane(rect)));
    }

    /// When the shelf next counts on a second, if anything on it is
    /// going.
    pub(super) fn tasks_live(&self) -> Option<Instant> {
        let now = self.now();
        (shelf_order(&self.node.tasks).into_iter().take(SHOWN))
            .filter(|task| task.going())
            .map(|task| {
                let into = now.saturating_sub(task.started) % 1000;
                Instant::now() + Duration::from_millis(1000 - into)
            })
            .min()
    }
}

/// The tasks in the order the shelf shows them: those going first, then
/// those that ended, newest first in each.
fn shelf_order(tasks: &[AgentTask]) -> Vec<&AgentTask> {
    let mut order: Vec<&AgentTask> = tasks.iter().rev().collect();
    order.sort_by_key(|task| !task.going());
    order
}

/// What the shelf's line says: how many are going, and how many more
/// there are than it has a line for.
fn shelf_heading(tasks: &[AgentTask]) -> String {
    let going = tasks.iter().filter(|task| task.going()).count();
    let mut heading = if going > 0 {
        format!("In the background · {going} running")
    } else {
        "In the background · all ended".to_owned()
    };
    if tasks.len() > SHOWN {
        let _ = write!(heading, " · {} more", tasks.len() - SHOWN);
    }
    heading
}

/// What a task's line says: what it runs, how it's going and for how
/// long. A shell shows its command, and what it's for beside it.
pub(super) fn task_row(task: &AgentTask, now: u64) -> String {
    let took = |end: u64| duration(end.saturating_sub(task.started));
    let (mark, status) = match task.state {
        AgentTaskState::Running => ("●", format!("running {}", took(now))),
        AgentTaskState::Paused => ("‖", "paused".to_owned()),
        AgentTaskState::Stopping => ("●", "stopping…".to_owned()),
        AgentTaskState::Done => ("✓", ended("done", task)),
        AgentTaskState::Failed => ("✗", ended("failed", task)),
        AgentTaskState::Stopped => ("■", ended("stopped", task)),
        AgentTaskState::Lost => ("·", "ended with its agent".to_owned()),
    };
    let what = match (&task.command, task.kind) {
        (Some(command), _) if *command != task.title => format!("{command} · {}", task.title),
        (Some(command), _) => command.clone(),
        (None, AgentTaskKind::Monitor) => format!("Watching: {}", task.title),
        (None, _) => task.title.clone(),
    };
    format!("{mark} {what} — {status}")
}

/// How an ended task's line says it ended, `how`, with how long it took
/// and what the agent said of it.
fn ended(how: &str, task: &AgentTask) -> String {
    let mut said = how.to_owned();
    if let Some(end) = task.ended {
        let _ = write!(said, " in {}", duration(end.saturating_sub(task.started)));
    }
    if let Some(summary) = &task.summary {
        let _ = write!(said, ": {summary}");
    }
    said
}

/// The colour of a task's line: one that failed stands out, and those
/// that ended step back.
fn task_tone(task: &AgentTask) -> Tone {
    match task.state {
        AgentTaskState::Failed => Tone::Warning,
        AgentTaskState::Running | AgentTaskState::Paused | AgentTaskState::Stopping => Tone::Plain,
        AgentTaskState::Done | AgentTaskState::Stopped | AgentTaskState::Lost => Tone::Muted,
    }
}

#[cfg(test)]
mod tests {
    use clackatoo_protocol::{
        AgentTask, AgentTaskKind, AgentTaskState, ChatAction, ChatNode, ChatPatch, ChatState,
        Patch, Subagent, SubagentStatus, TerminalId, Turn, ViewUpdate,
    };

    use crate::chat::fixtures::{fonts, paint};
    use crate::chat::{ChatView, Hit};

    fn task(id: &str, state: AgentTaskState) -> AgentTask {
        AgentTask {
            id: id.into(),
            kind: AgentTaskKind::Shell,
            title: "Serve the docs".into(),
            command: Some("trail serve".into()),
            state,
            summary: None,
            started: 100_000,
            ended: None,
            can_stop: true,
        }
    }

    fn chat(tasks: Vec<AgentTask>) -> ChatNode {
        ChatNode {
            state: ChatState::Ready,
            turns: vec![Turn {
                previews: Vec::new(),
                from: None,
                prompt: "Serve the docs".into(),
                sent: 99_000,
                items: Vec::new(),
                end: None,
                files: None,
                switch: None,
            }],
            tasks,
            ..ChatNode::default()
        }
    }

    /// What the shelf's lines say, as laid out for the last paint, in
    /// order of their text.
    fn shelf(chat: &ChatView) -> Vec<String> {
        let mut lines: Vec<String> = (chat.cache.labels.now.keys())
            .map(|(text, ..)| text.clone())
            .filter(|text| text.starts_with("In the background") || text.contains(" — "))
            .collect();
        lines.sort();
        lines
    }

    #[test]
    fn a_shell_in_the_background_shows_above_the_box_counting_and_stops_from_there() {
        let mut fonts = fonts();
        let mut chat = ChatView::new(
            String::new(),
            chat(vec![task("t1", AgentTaskState::Running)]),
        );
        chat.set_clock(112_400);
        paint(&mut chat, &mut fonts);
        assert_eq!(
            shelf(&chat),
            [
                "In the background · 1 running",
                "● trail serve · Serve the docs — running 12s",
            ]
        );
        assert!(chat.live().is_some(), "it counts the seconds");
        let rect = (chat.hits.iter())
            .find(|(hit, _)| *hit == Hit::StopTask("t1".into()))
            .map(|(_, rect)| *rect)
            .expect("a stop button");
        chat.press([rect[0] + 2.0, rect[1] + 2.0], 1, false);
        chat.release();
        let settled = chat.settle(&mut fonts);
        assert_eq!(settled.actions, [ChatAction::StopTask { id: "t1".into() }]);
    }

    #[test]
    fn ended_tasks_say_how_and_have_no_stop_button() {
        let mut fonts = fonts();
        let mut done = task("t1", AgentTaskState::Done);
        done.ended = Some(106_000);
        done.summary = Some("exit code 0".into());
        let mut lost = task("t2", AgentTaskState::Lost);
        lost.command = None;
        let mut chat = ChatView::new(String::new(), chat(vec![done, lost]));
        chat.set_clock(112_400);
        paint(&mut chat, &mut fonts);
        assert_eq!(
            shelf(&chat),
            [
                "In the background · all ended",
                "· Serve the docs — ended with its agent",
                "✓ trail serve · Serve the docs — done in 6s: exit code 0",
            ]
            .map(str::to_owned)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert!(!(chat.hits.iter()).any(|(hit, _)| matches!(hit, Hit::StopTask(_))));
        assert!(chat.live().is_none(), "nothing counts");
    }

    #[test]
    fn a_subagents_card_says_what_it_runs_in_the_background() {
        let mut fonts = fonts();
        let mut node = chat(Vec::new());
        node.state = ChatState::Answering;
        node.subagents = vec![Subagent {
            chat: TerminalId(5),
            agent: "Claude Code".into(),
            profile: None,
            model: None,
            task: "Serve the docs".into(),
            status: SubagentStatus::Done,
            started: 100_000,
            ended: Some(101_000),
            turn: 0,
            shown: false,
            tasks: vec![task("t1", AgentTaskState::Running)],
        }];
        let mut chat = ChatView::new(String::new(), node);
        chat.set_clock(112_400);
        chat.toggle_agents(0);
        paint(&mut chat, &mut fonts);
        let card = chat.cache.turns[0].agents.as_ref().expect("a card");
        assert_eq!(
            card.header.source,
            "1 subagent finished in 1s · 1 in the background  ⌄"
        );
        let rows: Vec<&str> = (card.rows.iter())
            .map(|(_, laid)| laid.source.as_str())
            .collect();
        assert_eq!(
            rows,
            [
                "✓ Claude Code · Serve the docs — done in 1s",
                "  ● trail serve · Serve the docs — running 12s",
            ]
        );
        // The task's line is the chat's: Enter there opens it.
        assert_eq!(chat.agent_on(0, 2), Some(TerminalId(5)));
        assert!(chat.live().is_some(), "the card counts its seconds");
        // Once it ends, the card says nothing of it.
        let mut ended = chat.node.subagents.clone();
        ended[0].tasks.clear();
        chat.apply(&ViewUpdate::Patch(vec![Patch::Chat(ChatPatch::Subagents(
            ended,
        ))]));
        paint(&mut chat, &mut fonts);
        let card = chat.cache.turns[0].agents.as_ref().expect("a card");
        assert_eq!(card.header.source, "1 subagent finished in 1s  ⌄");
    }
}
