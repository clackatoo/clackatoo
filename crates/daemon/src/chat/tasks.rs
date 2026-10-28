//! What the agent runs in the background of a chat: shells Claude Code
//! started with `run_in_background`, monitors, and turns it takes by
//! itself, such as to answer a shell that finished (CLACKA-177).
//!
//! The agent says of them in updates of their own (see
//! [`crate::agent::Task`]), which the chat keeps as its view's `tasks`:
//! those still going, and those that ended since the user last sent a
//! message. Nothing is told of one again once its agent has gone, so a
//! task still going then is marked lost, never left running for ever: its
//! process went with the agent's.

use agent_client_protocol_schema::v1::StopReason;
use clackatoo_protocol::{AgentTask, AgentTaskKind, AgentTaskState, ChatPatch};

use super::{Action, HostedChat, now};
use crate::agent::Task;

/// The most tasks a chat keeps: the oldest that ended go first, then the
/// oldest going.
const MOST_TASKS: usize = 16;

impl HostedChat {
    /// Takes what the agent said of a task it runs in the background.
    /// Returns how the turn the agent took by itself ended, if the task
    /// that stands for it did: such a task isn't listed, as the turn shows.
    pub(crate) fn task_said(&mut self, said: Task) -> Option<StopReason> {
        match self.turn_task_said(&said) {
            TurnTask::Ended(how) => return Some(how),
            TurnTask::Taken => return None,
            TurnTask::NotOne => {}
        }
        let mut tasks = self.node.tasks.clone();
        match said {
            Task::Spawned {
                id,
                kind,
                name,
                description,
                can_stop,
                tool,
            } => {
                let kind = task_kind(&kind);
                let command = (tool.as_deref())
                    .filter(|_| kind == AgentTaskKind::Shell)
                    .and_then(|tool| self.tool_anywhere(tool))
                    .map(|tool| tool.title.clone());
                let task = AgentTask {
                    id: id.clone(),
                    kind,
                    title: task_title(&name, description.as_deref()),
                    command,
                    state: AgentTaskState::Running,
                    summary: None,
                    started: now(),
                    ended: None,
                    can_stop,
                };
                if let Some(known) = tasks.iter_mut().find(|task| task.id == id) {
                    *known = task;
                } else {
                    tasks.push(task);
                    bound(&mut tasks);
                }
            }
            Task::Progress { id, description } => {
                let task = tasks.iter_mut().find(|task| task.id == id)?;
                if let Some(description) = description
                    && task.kind != AgentTaskKind::Turn
                {
                    task.title = description;
                }
            }
            Task::State { id, state, summary } => {
                let task = tasks.iter_mut().find(|task| task.id == id)?;
                let state = match state.as_str() {
                    "running" => AgentTaskState::Running,
                    "paused" => AgentTaskState::Paused,
                    "completed" => AgentTaskState::Done,
                    "failed" => AgentTaskState::Failed,
                    "stopped" => AgentTaskState::Stopped,
                    _ => return None,
                };
                // Once it has ended, it stays as it ended.
                if !task.going() {
                    return None;
                }
                // A stop asked for stays in hand until the agent says it ended.
                if !(task.state == AgentTaskState::Stopping && state == AgentTaskState::Running) {
                    task.state = state;
                }
                task.summary = summary.or(task.summary.take());
                if !task.going() {
                    task.ended = Some(now());
                }
            }
        }
        self.set_tasks(tasks);
        None
    }

    /// Takes what the agent said of a task that stands for a turn it takes
    /// by itself, if `said` is of one: one starting opens that turn, if
    /// none is being answered, and one ending ends it.
    fn turn_task_said(&mut self, said: &Task) -> TurnTask {
        match said {
            Task::Spawned {
                id,
                kind,
                description,
                ..
            } if kind == "turn" => {
                if self.wake(description.clone()) {
                    self.woke_by_task = Some(id.clone());
                } else if self.woken() && self.woke_by_task.is_none() {
                    // Its words came first.
                    self.woke_by_task = Some(id.clone());
                    let last = self.node.turns.len() - 1;
                    let turn = &mut self.node.turns[last];
                    if turn.woke.as_deref() == Some("") && description.is_some() {
                        turn.woke.clone_from(description);
                        self.patch_turn(last);
                    }
                }
                TurnTask::Taken
            }
            Task::Progress { id, .. } if self.woke_by_task.as_ref() == Some(id) => TurnTask::Taken,
            Task::State { id, state, .. } if self.woke_by_task.as_ref() == Some(id) => {
                let reason = match state.as_str() {
                    "completed" | "failed" => StopReason::EndTurn,
                    "stopped" => StopReason::Cancelled,
                    _ => return TurnTask::Taken,
                };
                self.woke_by_task = None;
                if self.woken() {
                    TurnTask::Ended(reason)
                } else {
                    TurnTask::Taken
                }
            }
            _ => TurnTask::NotOne,
        }
    }

    /// Asks the agent to stop task `id`, if it's going and can be stopped.
    pub(super) fn stop_task(&mut self, id: &str, actions: &mut Vec<Action>) {
        let mut tasks = self.node.tasks.clone();
        let Some(task) = (tasks.iter_mut())
            .find(|task| task.id == id && task.can_stop && task.state == AgentTaskState::Running)
        else {
            return;
        };
        task.state = AgentTaskState::Stopping;
        self.set_tasks(tasks);
        actions.push(Action::StopTask(id.to_owned()));
    }

    /// The agent answered asking it to stop task `id`: whether it did.
    /// One that ended meanwhile is as it ended; one it wouldn't stop goes
    /// on running, and the chat says why.
    pub(crate) fn task_stop_answered(&mut self, id: &str, result: Result<bool, String>) {
        let mut tasks = self.node.tasks.clone();
        let Some(task) = (tasks.iter_mut()).find(|task| task.id == id && task.going()) else {
            return;
        };
        if result == Ok(true) {
            task.state = AgentTaskState::Stopped;
            task.ended = Some(now());
        } else {
            task.state = AgentTaskState::Running;
            let why = (result.err()).unwrap_or_else(|| "it couldn't be stopped now".to_owned());
            let title = task.title.clone();
            self.set_notice(Some(format!(
                "{} didn't stop {}: {why}",
                self.node.agent,
                clackatoo_protocol::code_span(&title)
            )));
        }
        self.set_tasks(tasks);
    }

    /// The process that ran the chat's tasks has gone: those still going
    /// are lost, as nothing more will be told of them.
    pub(super) fn tasks_lost(&mut self) {
        if !self.node.tasks.iter().any(AgentTask::going) {
            return;
        }
        let mut tasks = self.node.tasks.clone();
        let ended = now();
        for task in tasks.iter_mut().filter(|task| task.going()) {
            task.state = AgentTaskState::Lost;
            task.ended = Some(ended);
        }
        self.set_tasks(tasks);
    }

    /// Lets go of the tasks that have ended, as the user sends a message.
    pub(super) fn forget_ended_tasks(&mut self) {
        if self.node.tasks.iter().all(AgentTask::going) {
            return;
        }
        let tasks = (self.node.tasks.iter())
            .filter(|task| task.going())
            .cloned();
        self.set_tasks(tasks.collect());
    }

    /// Whether the agent runs anything in the background for the chat,
    /// which letting its session go would end.
    pub(crate) fn has_tasks_going(&self) -> bool {
        self.node.tasks.iter().any(AgentTask::going)
    }

    /// The tasks going, for the card of the chat that started this one.
    pub(crate) fn tasks_going(&self) -> Vec<AgentTask> {
        (self.node.tasks.iter())
            .filter(|task| task.going())
            .cloned()
            .collect()
    }

    fn set_tasks(&mut self, tasks: Vec<AgentTask>) {
        if self.node.tasks != tasks {
            self.node.tasks.clone_from(&tasks);
            self.journal.push(ChatPatch::Tasks(tasks));
        }
    }
}

/// What a task the agent said something of was to a turn it takes by
/// itself.
enum TurnTask {
    /// It's no such task.
    NotOne,
    /// It's one, and what was said of it is taken.
    Taken,
    /// It's one, and the turn it stands for ended, as this says.
    Ended(StopReason),
}

/// The kind of task the agent calls `kind`.
fn task_kind(kind: &str) -> AgentTaskKind {
    match kind {
        "shell" => AgentTaskKind::Shell,
        "monitor" => AgentTaskKind::Monitor,
        "workflow" => AgentTaskKind::Workflow,
        "turn" => AgentTaskKind::Turn,
        _ => AgentTaskKind::Other,
    }
}

/// What a task is shown as: its name, and what it's for if that says
/// more, as for a turn the agent took by itself: "Claude is working: a
/// background command completed".
fn task_title(name: &str, description: Option<&str>) -> String {
    match description.filter(|description| *description != name) {
        Some(description) if name.is_empty() => description.to_owned(),
        Some(description) => format!("{name}: {description}"),
        None if name.is_empty() => "Background task".to_owned(),
        None => name.to_owned(),
    }
}

/// Keeps at most [`MOST_TASKS`] of `tasks`, letting the oldest that ended
/// go first.
fn bound(tasks: &mut Vec<AgentTask>) {
    while tasks.len() > MOST_TASKS {
        let at = tasks.iter().position(|task| !task.going()).unwrap_or(0);
        tasks.remove(at);
    }
}
