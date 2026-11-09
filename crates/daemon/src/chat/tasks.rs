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
//!
//! # Commands left running in the background
//!
//! Codex's adapter says nothing of what Codex runs in the background (its
//! `/ps`) but to a client that asks as `JetBrains`' AIR does, which it then
//! sends no command's output (CLACKA-232). It shows anyway: a command
//! still running as Codex ends its turn is one Codex left running, as the
//! tool call that ran it stays open until the command ends, saying so
//! then, and its output goes on coming as it runs. So a command the
//! agent said runs in a terminal of its own (`_meta.terminal_info`, its
//! output streamed as a terminal's, or a `terminal` block in what it
//! shows), still running as a turn ends well,
//! is listed as a task, `tool:` and its tool call's ID, which can't be
//! stopped from here, and its row goes on running where it is, rather
//! than failing with the turn: what the agent says of it later goes to
//! that row, wherever the conversation has got to, and ends the task as
//! the command ends; once the chat lists it no more (it ended and the user
//! wrote, or it was let go to keep to [`MOST_TASKS`]), nothing more said of
//! it goes anywhere. Claude Code's adapter finishes the row of a command
//! it moves to the background, so it never takes this path.

use agent_client_protocol_schema::v1::{StopReason, ToolCallContent, ToolCallUpdate};
use clackatoo_protocol::{
    AgentTask, AgentTaskKind, AgentTaskState, ChatPatch, Item, Outcome, Tool, ToolContent,
    ToolKind, ToolStatus,
};
use serde_json::Value;

use super::{Action, HostedChat, now};
use crate::agent::Task;
use crate::handover;

/// The most tasks a chat keeps: the oldest that ended go first, then the
/// oldest going.
pub(super) const MOST_TASKS: usize = 16;

/// What the ID of a task that stands for a command the agent left running
/// in the background starts with, before its tool call's ID.
const FROM_TOOL: &str = "tool:";

/// The most tool calls of a turn the chat remembers run in a terminal of
/// their own: far more than a turn leaves running.
const MOST_TERMINALS: usize = 256;

/// The most commands left running in the background the chat remembers
/// having let go of.
const MOST_LET_GO: usize = 64;

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
                    let gone = bound(&mut tasks);
                    self.set_tasks(tasks);
                    self.let_tasks_go(gone);
                    return None;
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
        self.lose_tasks(|_| true);
    }

    /// The branch shown is left, and its session with it: what it runs
    /// in the background that the agent can't be asked to stop is lost to
    /// the chat, as nothing more of it comes here. Those it can be asked
    /// to stop are, as the branch is left (see `branches.rs`).
    pub(super) fn tasks_left_behind(&mut self) {
        self.lose_tasks(|task| !task.can_stop);
    }

    /// Marks the tasks going that `which` picks lost, and the rows of the
    /// commands among them failed.
    fn lose_tasks(&mut self, which: impl Fn(&AgentTask) -> bool) {
        if !self
            .node
            .tasks
            .iter()
            .any(|task| task.going() && which(task))
        {
            return;
        }
        let mut tasks = self.node.tasks.clone();
        let ended = now();
        let mut lost = Vec::new();
        for task in (tasks.iter_mut()).filter(|task| task.going() && which(task)) {
            task.state = AgentTaskState::Lost;
            task.ended = Some(ended);
            lost.push(task.id.clone());
        }
        self.set_tasks(tasks);
        self.background_rows_ended(&lost);
    }

    /// Lets go of the tasks that have ended, as the user sends a message.
    pub(super) fn forget_ended_tasks(&mut self) {
        if self.node.tasks.iter().all(AgentTask::going) {
            return;
        }
        let (going, ended): (Vec<AgentTask>, Vec<AgentTask>) =
            (self.node.tasks.iter().cloned()).partition(AgentTask::going);
        self.set_tasks(going);
        self.let_tasks_go(ended);
    }

    /// The chat lists `gone` no more, having let them go to keep to
    /// [`MOST_TASKS`], or as they ended: the rows of commands among them
    /// still running fail, as nothing more of them shows, and what the
    /// agent says of them later goes nowhere.
    fn let_tasks_go(&mut self, gone: Vec<AgentTask>) {
        let going: Vec<String> = (gone.iter())
            .filter(|task| task.going())
            .map(|task| task.id.clone())
            .collect();
        self.background_rows_ended(&going);
        for task in gone {
            let Some(row) = task.id.strip_prefix(FROM_TOOL) else {
                continue;
            };
            if self.let_go.len() >= MOST_LET_GO {
                self.let_go.pop_front();
            }
            self.let_go.push_back(row.to_owned());
        }
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

/// Commands the agent left running in the background (see the module
/// docs).
impl HostedChat {
    /// Notes that tool call `id` of the turn being answered runs in a
    /// terminal of its own, if the agent says so: in `meta`'s
    /// `terminal_info`, or streaming its output there as a terminal's
    /// (`terminal_output`, `terminal_output_delta`), as Codex's adapter
    /// does (and Zed's convention), or with a `terminal` block in
    /// `content`, what it shows. One told again
    /// ran long ago: the last turn a load tells, ending, leaves nothing
    /// running.
    pub(super) fn note_terminal(
        &mut self,
        id: &str,
        meta: Option<&serde_json::Map<String, Value>>,
        content: &[ToolCallContent],
    ) {
        if self.told_again() {
            return;
        }
        let says = meta.is_some_and(|meta| {
            ["terminal_info", "terminal_output", "terminal_output_delta"]
                .iter()
                .any(|key| meta.contains_key(*key))
        }) || (content.iter())
            .any(|content| matches!(content, ToolCallContent::Terminal(_)));
        if !says || self.terminals.iter().any(|known| known == id) {
            return;
        }
        if self.terminals.len() >= MOST_TERMINALS {
            self.terminals.pop_front();
        }
        self.terminals.push_back(id.to_owned());
    }

    /// Notes again, after an upgrade the agent went on through, which
    /// commands of the turn being answered run in a terminal of their own,
    /// as far as the turn shows: those running that have shown what they
    /// printed as a terminal's output (see [`note_terminal`]). One that
    /// has printed nothing yet is taken for one once it does.
    ///
    /// [`note_terminal`]: Self::note_terminal
    pub(super) fn terminals_again(&mut self) {
        let Some(index) = self.answering_turn() else {
            return;
        };
        let running: Vec<String> = (self.node.turns[index].items.iter())
            .filter_map(|item| match item {
                Item::Tool(tool)
                    if tool.status == ToolStatus::Running
                        && tool.kind == ToolKind::Execute
                        && (tool.content.iter())
                            .any(|content| matches!(content, ToolContent::Output(_))) =>
                {
                    Some(tool.id.clone())
                }
                _ => None,
            })
            .collect();
        let skip = running.len().saturating_sub(MOST_TERMINALS);
        self.terminals = running.into_iter().skip(skip).collect();
    }

    /// The commands of turn `index`, ending as `outcome` says, that the
    /// agent left running in the background: those still running in a
    /// terminal of their own as it ends well. None of a turn stopped or
    /// failed, whose commands may be ending with it, or of one told again,
    /// which ran long ago. A subagent's are its own, under its card.
    pub(super) fn left_running(&self, index: usize, outcome: &Outcome) -> Vec<Tool> {
        if *outcome != Outcome::Done || self.told_again() || self.terminals.is_empty() {
            return Vec::new();
        }
        (self.node.turns[index].items.iter())
            .filter_map(|item| match item {
                Item::Tool(tool) => Some(tool),
                _ => None,
            })
            .filter(|tool| {
                tool.status == ToolStatus::Running
                    && tool.kind == ToolKind::Execute
                    && self.terminals.contains(&tool.id)
            })
            .cloned()
            .collect()
    }

    /// Lists `left`, the commands the agent left running in the background
    /// as turn `index` ended, as tasks of the chat's: each going until the
    /// agent says its command has ended (see
    /// [`background_said`](Self::background_said)).
    pub(super) fn left_in_background(&mut self, index: usize, left: Vec<Tool>) {
        if left.is_empty() {
            return;
        }
        let turn = self.turn_uid(index);
        let mut tasks = self.node.tasks.clone();
        let mut gone = Vec::new();
        for tool in left {
            log::info!(
                "chat {}: {} left `{}` running in the background",
                self.id,
                self.agent,
                tool.title
            );
            if let Some(turn) = turn {
                self.remember_row(&tool.id, turn);
            }
            let task = AgentTask {
                id: format!("{FROM_TOOL}{}", tool.id),
                kind: AgentTaskKind::Shell,
                title: tool.title.clone(),
                command: Some(tool.title),
                state: AgentTaskState::Running,
                summary: None,
                started: now(),
                ended: None,
                can_stop: false,
                output: None,
            };
            if let Some(known) = tasks.iter_mut().find(|known| known.id == task.id) {
                *known = task;
            } else {
                tasks.push(task);
                gone.extend(bound(&mut tasks));
            }
        }
        self.tasks_session.clone_from(&self.session);
        self.set_tasks(tasks);
        self.let_tasks_go(gone);
    }

    /// Takes `update`, of tool call `id`, which isn't the turn answering's,
    /// if it's of a command the agent left running in the background:
    /// what it printed goes on its row, wherever that is now, and once the
    /// agent says the command ended, so does the row, with its exit code,
    /// and the task. Returns whether it took it. An end that brings output
    /// brings all of it: Codex's adapter sends again what the command
    /// printed as it ends, once a later message has been sent, as it no
    /// longer knows what it streamed; so it takes the place of what the row
    /// shows.
    pub(super) fn background_said(&mut self, id: &str, update: &ToolCallUpdate) -> bool {
        if self.let_go.iter().any(|row| row == id) {
            return true;
        }
        let task = format!("{FROM_TOOL}{id}");
        let Some(at_task) = self.node.tasks.iter().position(|known| known.id == task) else {
            return false;
        };
        // Once it has ended, as it ended: a later word of it changes
        // nothing.
        if !self.node.tasks[at_task].going() {
            return true;
        }
        let status = update.fields.status.map(super::tool_status);
        let ended = status.filter(|status| *status != ToolStatus::Running);
        let raw = update.fields.raw_output.as_ref();
        let exit = super::facts::exit_code(update.meta.as_ref(), raw, ToolKind::Execute);
        let output = super::streamed_output(update.meta.as_ref());
        if let Some((turn, at)) = self.background_row(id) {
            let item = &mut self.node.turns[turn].items[at];
            if let Item::Tool(tool) = item {
                let mut changed = false;
                if let Some(status) = ended.filter(|status| *status != tool.status) {
                    tool.status = status;
                    changed = true;
                }
                if let Some(code) = exit.filter(|code| tool.exit != Some(*code)) {
                    tool.exit = Some(code);
                    changed = true;
                }
                if let (Some(data), Some(_)) = (output, ended) {
                    tool.content
                        .retain(|content| !matches!(content, ToolContent::Output(_)));
                    let text = super::clean_text(data);
                    tool.content
                        .push(ToolContent::Output(super::tail(&text, super::TOOL_TEXT)));
                    changed = true;
                }
                if changed {
                    let item = item.clone();
                    self.patch_item(turn, at, item);
                } else if let Some(data) = output {
                    self.append_output(turn, at, data);
                }
            }
        }
        if let Some(status) = ended {
            let mut tasks = self.node.tasks.clone();
            let task = &mut tasks[at_task];
            task.state = if status == ToolStatus::Done {
                AgentTaskState::Done
            } else {
                AgentTaskState::Failed
            };
            task.ended = Some(now());
            task.summary = exit.map(|code| format!("exit code {code}"));
            self.set_tasks(tasks);
        }
        true
    }

    /// Where the row of tool call `id`, a command left running in the
    /// background, is now: its turn and item, if the conversation shown
    /// still has it.
    fn background_row(&mut self, id: &str) -> Option<(usize, usize)> {
        let in_turn = |items: &[Item]| {
            (items.iter()).position(|item| matches!(item, Item::Tool(tool) if tool.id == id))
        };
        let kept = (self.background_rows.iter())
            .find(|(row, _)| row == id)
            .and_then(|&(_, uid)| self.turn_index(uid))
            .and_then(|turn| Some((turn, in_turn(&self.node.turns[turn].items[..])?)));
        if kept.is_some() {
            return kept;
        }
        let (turn, at) = (self.node.turns.iter().enumerate().rev())
            .find_map(|(turn, shown)| Some((turn, in_turn(&shown.items[..])?)))?;
        if let Some(uid) = self.turn_uid(turn) {
            self.remember_row(id, uid);
        }
        Some((turn, at))
    }

    /// Remembers that the row of tool call `id` is in turn `uid`.
    fn remember_row(&mut self, id: &str, uid: super::TurnUid) {
        self.background_rows.retain(|(row, _)| row != id);
        if self.background_rows.len() >= MOST_TASKS {
            self.background_rows.remove(0);
        }
        self.background_rows.push((id.to_owned(), uid));
    }

    /// The rows of the commands left running in the background that the
    /// tasks `ids` stand for, which have gone: one still running failed.
    fn background_rows_ended(&mut self, ids: &[String]) {
        for id in ids {
            let Some(row) = id.strip_prefix(FROM_TOOL) else {
                continue;
            };
            let Some((turn, at)) = self.background_row(row) else {
                continue;
            };
            let item = &mut self.node.turns[turn].items[at];
            let Item::Tool(tool) = item else {
                continue;
            };
            if tool.status != ToolStatus::Running {
                continue;
            }
            tool.status = ToolStatus::Failed;
            let item = item.clone();
            self.patch_item(turn, at, item);
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
        let mut ids = Vec::new();
        let mut tasks: Vec<AgentTask> = kept.tasks.into_iter().filter_map(task_in).collect();
        for task in (tasks.iter_mut()).filter(|task| !live && task.going()) {
            task.state = AgentTaskState::Lost;
            task.summary = Some(ENDED_BY_UPGRADE.to_owned());
            task.ended = Some(ended);
            ids.push(task.id.clone());
            lost.push(match &task.command {
                Some(command) if *command != task.title => {
                    format!("{} ({})", task.title, command)
                }
                _ => task.title.clone(),
            });
        }
        let gone = bound(&mut tasks);
        self.set_tasks(tasks);
        self.background_rows_ended(&ids);
        self.let_tasks_go(gone);
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
/// go first. Returns those let go.
fn bound(tasks: &mut Vec<AgentTask>) -> Vec<AgentTask> {
    let mut gone = Vec::new();
    while tasks.len() > MOST_TASKS {
        let at = tasks.iter().position(|task| !task.going()).unwrap_or(0);
        gone.push(tasks.remove(at));
    }
    gone
}
