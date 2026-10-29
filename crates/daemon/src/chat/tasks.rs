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
use crate::handover;

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
                output,
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
                    output,
                };
                self.tasks_session.clone_from(&self.session);
                if let Some(known) = tasks.iter_mut().find(|task| task.id == id) {
                    *known = task;
                } else {
                    tasks.push(task);
                    bound(&mut tasks);
                }
            }
            Task::Progress {
                id,
                description,
                output,
            } => {
                let task = tasks.iter_mut().find(|task| task.id == id)?;
                task.output = output.or(task.output.take());
                if let Some(description) = description
                    && task.kind != AgentTaskKind::Turn
                {
                    task.title = description;
                }
            }
            Task::State {
                id,
                state,
                summary,
                output,
            } => {
                let task = tasks.iter_mut().find(|task| task.id == id)?;
                // Where its output went can come after it ended, as the
                // result of the tool call that started it can.
                task.output = output.or(task.output.take());
                let state = match state.as_str() {
                    "running" => Some(AgentTaskState::Running),
                    "paused" => Some(AgentTaskState::Paused),
                    "completed" => Some(AgentTaskState::Done),
                    "failed" => Some(AgentTaskState::Failed),
                    "stopped" => Some(AgentTaskState::Stopped),
                    _ => None,
                };
                // Once it has ended, it stays as it ended.
                if let Some(state) = state.filter(|_| task.going()) {
                    // A stop asked for stays in hand until the agent says
                    // it ended.
                    if !(task.state == AgentTaskState::Stopping && state == AgentTaskState::Running)
                    {
                        task.state = state;
                    }
                    task.summary = summary.or(task.summary.take());
                    if !task.going() {
                        task.ended = Some(now());
                    }
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
        // One whose start went missing is known by its name.
        let unseen = |id: &str, chat: &Self| {
            id.starts_with(TURN_TASK) && chat.woke_by_task.as_deref() != Some(id)
        };
        match said {
            Task::Progress {
                id, description, ..
            } if unseen(id, self) => {
                if !self.woken() {
                    self.wake(description.clone());
                }
                if self.woken() && self.woke_by_task.is_none() {
                    self.woke_by_task = Some(id.clone());
                }
                return TurnTask::Taken;
            }
            Task::State { id, .. } if unseen(id, self) => {
                if !self.woken() || self.woke_by_task.is_some() {
                    return TurnTask::Taken;
                }
                self.woke_by_task = Some(id.clone());
            }
            _ => {}
        }
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
                    _ if self.woken_stopping() => StopReason::Cancelled,
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
        // One of a session the chat has left is no longer the agent's to
        // stop.
        if self.tasks_session != self.session {
            return;
        }
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

/// What a task ended by the daemon's upgrade says, as its agent went with
/// it.
pub(crate) const ENDED_BY_UPGRADE: &str = "ended by the daemon's upgrade";

impl HostedChat {
    /// What the chat's agent runs in the background, for the next build.
    pub(crate) fn hand_over_tasks(&self) -> Option<handover::ChatTasks> {
        (!self.node.tasks.is_empty()).then(|| handover::ChatTasks {
            chat: self.id.0,
            session: self.tasks_session.clone(),
            tasks: (self.node.tasks.iter()).map(task_out).collect(),
        })
    }

    /// Takes back what the chat's agent ran in the background, `kept`, as
    /// the last build handed it over. The upgrade ended the agent, and
    /// with it all it ran: those still going are lost, and ended by the
    /// upgrade. Returns what each of those was, to tell the agent.
    ///
    /// Unless the agent went on through the upgrade, `live`: then so did
    /// all it ran, and those going are still going.
    pub(crate) fn resume_tasks(&mut self, kept: handover::ChatTasks, live: bool) -> Vec<String> {
        self.tasks_session = kept.session;
        let ended = now();
        let mut lost = Vec::new();
        let mut tasks: Vec<AgentTask> = kept.tasks.into_iter().filter_map(task_in).collect();
        for task in (tasks.iter_mut()).filter(|task| !live && task.going()) {
            task.state = AgentTaskState::Lost;
            task.summary = Some(ENDED_BY_UPGRADE.to_owned());
            task.ended = Some(ended);
            lost.push(match &task.command {
                Some(command) if *command != task.title => {
                    format!("{} ({})", task.title, command)
                }
                _ => task.title.clone(),
            });
        }
        bound(&mut tasks);
        self.set_tasks(tasks);
        lost
    }
}

/// `task`, as the handover keeps it.
fn task_out(task: &AgentTask) -> handover::ChatTask {
    handover::ChatTask {
        id: task.id.clone(),
        kind: match task.kind {
            AgentTaskKind::Shell => "shell",
            AgentTaskKind::Monitor => "monitor",
            AgentTaskKind::Workflow => "workflow",
            AgentTaskKind::Turn => "turn",
            AgentTaskKind::Other => "other",
        }
        .to_owned(),
        title: task.title.clone(),
        command: task.command.clone(),
        state: match task.state {
            AgentTaskState::Running => "running",
            AgentTaskState::Paused => "paused",
            AgentTaskState::Stopping => "stopping",
            AgentTaskState::Done => "done",
            AgentTaskState::Failed => "failed",
            AgentTaskState::Stopped => "stopped",
            AgentTaskState::Lost => "lost",
        }
        .to_owned(),
        summary: task.summary.clone(),
        started: task.started,
        ended: task.ended,
        can_stop: task.can_stop,
    }
}

/// A task the handover kept, unless it's in a state this build doesn't
/// know.
fn task_in(task: handover::ChatTask) -> Option<AgentTask> {
    let state = match task.state.as_str() {
        "running" => AgentTaskState::Running,
        "paused" => AgentTaskState::Paused,
        "stopping" => AgentTaskState::Stopping,
        "done" => AgentTaskState::Done,
        "failed" => AgentTaskState::Failed,
        "stopped" => AgentTaskState::Stopped,
        "lost" => AgentTaskState::Lost,
        _ => return None,
    };
    Some(AgentTask {
        id: task.id,
        kind: task_kind(&task.kind),
        title: task.title,
        command: task.command,
        state,
        summary: task.summary,
        started: task.started,
        ended: task.ended,
        can_stop: task.can_stop,
        // Not handed over: a task the upgrade ended is lost, and one going
        // on is told of again only when its output moves.
        output: None,
    })
}

/// How Claude Code's adapter names the task that stands for a turn it
/// takes by itself, which tells one apart whose start went missing.
const TURN_TASK: &str = "autonomous-turn-";

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
