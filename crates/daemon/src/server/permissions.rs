//! Bounded rule evaluation off the daemon loop, with no execution on workers.
//!
//! A check is queued for the one worker thread, which follows links on
//! disk, and its verdict is taken back on the loop, where the continuation
//! given with the check runs. Every continuation runs, once: with the
//! rules' verdict, or cancelled, if the chat or its turn moved on while
//! the check waited.
//!
//! A change of rules cancels nothing. A check still waiting is evaluated
//! again, against the new rules. A question a check already put to the
//! user is checked again too: it stays open unless the new rules now
//! allow or refuse what it asks about.
//!
//! The worker can hang, as on a mount that has gone. A check it doesn't
//! answer in time asks the user, and the worker is left to it: the checks
//! behind it go to a fresh one.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{PermissionRecord, TerminalId};
use mio::Waker;

use super::Daemon;
use super::chats::AgentId;
use crate::chat::TurnUid;
use crate::permissions::{
    Action, Cancelled, Checked, Config, Context, Decision, Engine, Identity, Rule, Verdict,
};
use agent_client_protocol_schema::v1::{PermissionOption, PermissionOptionKind, ToolCallUpdate};
use serde_json::Value;

/// An approval request an agent's adapter sent, and who to answer.
pub(super) struct ProviderRequest {
    /// The running agent that asked.
    pub(super) agent: AgentId,
    /// The agent's name, as chats know it.
    pub(super) name: String,
    /// The session it asked in.
    pub(super) session: String,
    /// The JSON-RPC ID of its request, which the answer carries.
    pub(super) request: Value,
    /// The tool call it asks about.
    pub(super) tool: Box<ToolCallUpdate>,
    /// The answers it offers.
    pub(super) options: Vec<PermissionOption>,
    /// The IDs of the answers whose kind this build doesn't know.
    pub(super) unknown: Vec<String>,
}

impl ProviderRequest {
    /// The answers it offers whose meaning is known.
    fn known_options(&self) -> impl Iterator<Item = &PermissionOption> {
        self.options
            .iter()
            .filter(|option| !self.unknown.iter().any(|id| *option.option_id.0 == **id))
    }

    /// How the rules' `decision` answers it.
    fn answer_for(&self, decision: Decision) -> RuleAnswer {
        let kind = match decision {
            Decision::Allow => PermissionOptionKind::AllowOnce,
            Decision::Deny => PermissionOptionKind::RejectOnce,
            Decision::Ask => return RuleAnswer::AskUser,
        };
        let option = self
            .known_options()
            .find(|option| option.kind == kind)
            .map(|option| option.option_id.0.to_string());
        match (option, decision) {
            (Some(option), _) => RuleAnswer::Pick(option),
            (None, Decision::Deny) => RuleAnswer::Cancel,
            (None, _) => RuleAnswer::AskUser,
        }
    }
}

/// How the rules answer an agent's approval request.
#[derive(Debug, PartialEq, Eq)]
enum RuleAnswer {
    /// With the option that allows it once, or rejects it once.
    Pick(String),
    /// With no option, which cancels it: a Deny the agent offered no
    /// "reject once" for.
    Cancel,
    /// Not at all, so the user is asked: the rules say to, or they allow
    /// it and the agent offered no "allow once". "Always" is never picked
    /// for the user.
    AskUser,
}

/// What runs on the loop once a check has ended.
type Resume = Box<dyn FnOnce(&mut Daemon, Checked) + Send>;

/// How the worker evaluates a check: by [`Config::decide`], but in tests.
type Evaluate = Arc<dyn Fn(&Config, &Action, &Context) -> Verdict + Send + Sync>;

/// The most checks waiting for the worker at once. One more asks the user.
const MOST_WAITING: usize = 64;

/// The most decisions kept in the history.
const MOST_KEPT: usize = 64;

/// How long a check may wait for the worker before the user is asked.
const WAIT: Duration = Duration::from_secs(3);

/// The most workers left hanging at once. With so many, no more are
/// started, and every check asks the user until one comes back.
const MOST_STUCK: usize = 4;

/// The most questions asked under rules that are kept to check again when
/// the rules change. One let go here stays open for the user to answer.
const MOST_ASKED: usize = 64;

/// The checks waiting for the worker, and what they decided lately.
pub(super) struct Permissions {
    /// The latest decisions, the newest first, for the settings page.
    pub(super) history: VecDeque<PermissionRecord>,
    /// The number the next check gets.
    next: u64,
    /// The thread that evaluates checks, once one has been needed.
    worker: Option<Worker>,
    /// Workers that didn't answer in time, until they come back and end.
    stuck: Vec<JoinHandle<()>>,
    /// The checks waiting for a verdict, by their numbers.
    pending: BTreeMap<u64, Pending>,
    /// The questions checks put to the user, as far as they're still
    /// open, to check again when the rules change.
    asked: Vec<Asked>,
    /// How long a check waits for the worker: [`WAIT`], but in tests.
    wait: Duration,
    /// How the worker evaluates a check.
    evaluate: Evaluate,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            history: VecDeque::new(),
            next: 0,
            worker: None,
            stuck: Vec::new(),
            pending: BTreeMap::new(),
            asked: Vec::new(),
            wait: WAIT,
            evaluate: Arc::new(Config::decide),
        }
    }
}

impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permissions")
            .field("pending", &self.pending.len())
            .field("asked", &self.asked.len())
            .field("history", &self.history.len())
            .finish_non_exhaustive()
    }
}

/// A check as the worker gets it.
struct Job {
    /// The check's number, which its verdict comes back with.
    id: u64,
    /// The rules as they were when it was sent.
    config: Config,
    /// What is checked.
    action: Action,
    /// The directories its paths and the rules' are told by.
    context: Context,
}

/// The thread that evaluates checks, and the channels to it.
struct Worker {
    /// Where checks are sent to it.
    jobs: mpsc::SyncSender<Job>,
    /// Where its verdicts come back, each with its check's number.
    verdicts: mpsc::Receiver<(u64, Verdict)>,
    /// The thread itself, to tell whether it still runs.
    thread: JoinHandle<()>,
}

impl Worker {
    /// Starts one, which evaluates jobs with `evaluate` and wakes the
    /// loop with `waker` as each is done. It ends when its channels are
    /// dropped, once it's through with the job it has.
    fn start(evaluate: Evaluate, waker: Arc<Waker>) -> std::io::Result<Self> {
        let (jobs, queued) = mpsc::sync_channel::<Job>(MOST_WAITING);
        let (done, verdicts) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("permission-rules".into())
            .spawn(move || {
                while let Ok(job) = queued.recv() {
                    let evaluated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        evaluate(&job.config, &job.action, &job.context)
                    }));
                    let verdict =
                        evaluated.unwrap_or_else(|_| Verdict::ask("Rule evaluation failed"));
                    if done.send((job.id, verdict)).is_err() {
                        break;
                    }
                    let _ = waker.wake();
                }
            })?;
        Ok(Self {
            jobs,
            verdicts,
            thread,
        })
    }
}

/// A check waiting for the worker's verdict.
struct Pending {
    /// The chat whose agent asked, and the session, agent and turn it
    /// asked in: the verdict is for those only.
    chat: TerminalId,
    session: Option<String>,
    agent: String,
    turn: Option<TurnUid>,
    /// Whether that turn was being answered when it asked. One that has
    /// ended since is not gone on with.
    turn_open: bool,
    /// What is checked, kept to evaluate again.
    action: Action,
    context: Context,
    /// The rules it was last sent to the worker with.
    rules: Vec<Rule>,
    /// When the worker has had long enough.
    due: Instant,
    /// Whether it checks a question already put to the user again.
    again: bool,
    /// What runs once it has ended.
    resume: Resume,
}

/// A question put to the user because the rules said to ask, or decided
/// nothing.
struct Asked {
    /// The chat whose user is asked.
    chat: TerminalId,
    /// Which of its questions.
    question: Question,
    /// What it asks about, to check against new rules.
    action: Action,
}

/// Which question of a chat's a check put to the user.
pub(super) enum Question {
    /// The agent's own approval request.
    Provider(Box<ProviderRequest>),
    /// About a call of one of clackatoo's tools, or of a managed MCP
    /// server, by the server's number for the call.
    Tool(u64),
}

impl Daemon {
    /// An agent asks whether it may use a tool. `checked` is the rules'
    /// verdict, once they have been asked, for a chat that uses them.
    pub(super) fn provider_permission(
        &mut self,
        mut asked: ProviderRequest,
        checked: Option<Verdict>,
    ) {
        // The agent that asked went while the rules were checked. No one
        // waits for an answer, and a question now would outlive it: its
        // answer would go to whichever agent has its name by then.
        if checked.is_some() && !self.agents.running.contains_key(&asked.agent) {
            return;
        }
        let serving = self
            .chat_in(&asked.name, &asked.session)
            .map(|(id, _)| id)
            .is_some_and(|id| self.serves_tools(id));
        let Some((id, chat)) = self
            .chat_in(&asked.name, &asked.session)
            .filter(|(_, chat)| !chat.told_again())
        else {
            self.answer_provider(&asked, None);
            return;
        };
        if chat.side_parent().is_some() {
            self.answer_provider(&asked, None);
            return;
        }
        let rules = chat.permission_engine() == Engine::Clackatoo;
        let interaction =
            asked.name == "antigravity" && asked.tool.tool_call_id.0.starts_with("interaction_");
        let known: Vec<_> = asked.known_options().cloned().collect();
        let answered = if interaction {
            None
        } else {
            chat.turned_down(&asked.tool, &known)
                .or_else(|| chat.about_own_tools(&asked.tool, &known, serving))
        };
        let cwd = chat.real_cwd();
        // A managed MCP server's call asks the user itself, through
        // clackatoo's proxy, when it must (see `about_managed_call`).
        let answered = answered.or_else(|| {
            (!interaction)
                .then(|| self.about_managed_call(id, &asked.tool, &known))
                .flatten()
        });
        if let Some(option) = answered {
            self.answer_provider(&asked, Some(&option));
            self.collect_damage(id);
            return;
        }
        let action = Action::provider(&asked.name, &cwd.to_string_lossy(), &asked.tool);
        if rules && checked.is_none() && !interaction {
            self.check_permission(id, action, move |daemon, checked| match checked {
                Checked::Decided(verdict) => daemon.provider_permission(asked, Some(verdict)),
                // Nothing was decided: the request is cancelled, not
                // rejected.
                Checked::Cancelled(_) => daemon.answer_provider(&asked, None),
            });
            return;
        }
        if let Some(verdict) = checked {
            match asked.answer_for(verdict.decision) {
                RuleAnswer::Pick(option) => {
                    self.answer_provider(&asked, Some(&option));
                    return;
                }
                RuleAnswer::Cancel => {
                    self.answer_provider(&asked, None);
                    return;
                }
                RuleAnswer::AskUser => {}
            }
        }
        if rules {
            asked.options.retain(|option| {
                option.kind != PermissionOptionKind::AllowAlways
                    || self
                        .chat_ref(id)
                        .is_some_and(|chat| chat.permission_plan_choice(&option.option_id.0))
            });
        }
        let accepted = self.chat(id).is_some_and(|chat| {
            chat.ask(
                asked.request.clone(),
                &asked.tool,
                &asked.options,
                &asked.unknown,
            )
        });
        if !accepted {
            self.answer_provider(&asked, None);
        } else if rules && !interaction {
            self.asked_under_rules(id, Question::Provider(Box::new(asked)), action);
        }
        self.collect_damage(id);
    }

    /// Answers the agent's approval request `asked` with `option`, or as
    /// cancelled with none, if the agent still runs.
    ///
    /// The answer is written at once. One that follows a check comes when
    /// the agent has said nothing new, so nothing else would write it.
    fn answer_provider(&mut self, asked: &ProviderRequest, option: Option<&str>) {
        if let Some(slot) = self.agents.running.get_mut(&asked.agent) {
            slot.agent.answer_permission(&asked.request, option);
        }
        self.flush_agent(asked.agent);
    }

    /// Whether the chat `id` has clackatoo's rules answer its approval
    /// requests.
    pub(super) fn uses_permission_rules(&self, id: TerminalId) -> bool {
        self.chat_ref(id)
            .is_some_and(|chat| chat.permission_engine() == Engine::Clackatoo)
    }

    /// Checks `action`, which the chat `chat`'s agent asks to do, against
    /// the rules, off the loop, then runs `resume` on it with how the
    /// check ended. `resume` always runs, once.
    ///
    /// Only executable inputs enter the policy. Each verdict is tied to
    /// the chat's session and turn as they were when it asked, and to the
    /// rules in force when it comes.
    pub(super) fn check_permission(
        &mut self,
        chat: TerminalId,
        action: Action,
        resume: impl FnOnce(&mut Self, Checked) + Send + 'static,
    ) {
        self.queue_check(chat, action, false, Box::new(resume));
    }

    /// Queues a check, as [`Self::check_permission`] does: `again` for
    /// one of a question the user is already asked.
    fn queue_check(&mut self, chat: TerminalId, action: Action, again: bool, resume: Resume) {
        let Some(own) = self.chat_ref(chat) else {
            resume(self, Checked::Cancelled(Cancelled::ChatChanged));
            return;
        };
        if self.permissions.pending.len() >= MOST_WAITING {
            let busy = Verdict::ask("Too many rule evaluations are waiting");
            resume(self, Checked::Decided(busy));
            return;
        }
        let own_dirs = self.config.own_config.iter().flat_map(|own| {
            std::iter::once(own.dirs.config().to_owned()).chain(own.dirs.state.clone())
        });
        let pending = Pending {
            chat,
            session: own.session().map(str::to_owned),
            agent: own.agent().to_owned(),
            turn: own.last_uid(),
            turn_open: own.answering(),
            action,
            context: Context {
                project: own.project().to_owned(),
                home: self.config.home.clone(),
                protected: own_dirs.collect(),
            },
            rules: Vec::new(),
            due: Instant::now(),
            again,
            resume,
        };
        let id = self.permissions.next;
        self.permissions.next += 1;
        self.permissions.pending.insert(id, pending);
        if let Err(why) = self.send_to_rules_worker(id) {
            self.permission_settled(id, Verdict::ask(why));
        }
    }

    /// Sends the waiting check `id` to the worker, with the rules as they
    /// are now, starting the worker if there's none. Fails with why the
    /// user must be asked instead.
    fn send_to_rules_worker(&mut self, id: u64) -> Result<(), &'static str> {
        let wait = self.permissions.wait;
        let config = self.config.permissions.clone();
        let Some(pending) = self.permissions.pending.get_mut(&id) else {
            return Ok(());
        };
        pending.rules.clone_from(&config.rules);
        pending.due = Instant::now() + wait;
        let job = Job {
            id,
            config,
            action: pending.action.clone(),
            context: pending.context.clone(),
        };
        // A worker that ended by itself, which none should, takes no
        // more checks: another is started in its place.
        let ended = |worker: &Worker| worker.thread.is_finished();
        if self.permissions.worker.as_ref().is_some_and(ended) {
            self.permissions.worker = None;
        }
        if self.permissions.worker.is_none() {
            self.permissions
                .stuck
                .retain(|worker| !worker.is_finished());
            if self.permissions.stuck.len() >= MOST_STUCK {
                return Err("The rules worker is stuck, as on a disk that doesn't answer");
            }
            let evaluate = Arc::clone(&self.permissions.evaluate);
            let worker = Worker::start(evaluate, Arc::clone(&self.waker))
                .map_err(|_| "The rules worker could not start")?;
            self.permissions.worker = Some(worker);
        }
        let sent = self
            .permissions
            .worker
            .as_ref()
            .is_some_and(|worker| worker.jobs.try_send(job).is_ok());
        if sent {
            Ok(())
        } else {
            Err("The rules worker is unavailable")
        }
    }

    /// When the loop must next look at the checks: when the first of
    /// them has waited long enough.
    pub(super) fn next_permission_due(&self) -> Option<Instant> {
        self.permissions
            .pending
            .values()
            .map(|pending| pending.due)
            .min()
    }

    /// Takes the verdicts the worker has reached, and asks the user about
    /// the checks it has had too long.
    pub(super) fn take_permissions(&mut self) {
        let reached: Vec<_> = self
            .permissions
            .worker
            .iter()
            .flat_map(|worker| worker.verdicts.try_iter())
            .collect();
        for (id, verdict) in reached {
            self.permission_evaluated(id, verdict);
        }
        let now = Instant::now();
        let overdue: Vec<u64> = self
            .permissions
            .pending
            .iter()
            .filter(|(_, pending)| pending.due <= now)
            .map(|(id, _)| *id)
            .collect();
        if overdue.is_empty() {
            return;
        }
        // The worker hangs on one of them. It's left to it, so its place
        // is free: the checks behind it go to a fresh worker.
        if let Some(worker) = self.permissions.worker.take() {
            self.permissions.stuck.push(worker.thread);
        }
        for id in overdue {
            self.permission_settled(id, Verdict::ask("Rule evaluation timed out"));
        }
        let behind: Vec<u64> = self.permissions.pending.keys().copied().collect();
        for id in behind {
            if let Err(why) = self.send_to_rules_worker(id) {
                self.permission_settled(id, Verdict::ask(why));
            }
        }
    }

    /// The worker evaluated the check `id` as `verdict`. If the rules
    /// changed since it was sent, that was by the old ones, and it's
    /// evaluated again.
    fn permission_evaluated(&mut self, id: u64, verdict: Verdict) {
        let stale = self
            .permissions
            .pending
            .get(&id)
            .is_some_and(|pending| pending.rules != self.config.permissions.rules);
        if !stale {
            self.permission_settled(id, verdict);
        } else if let Err(why) = self.send_to_rules_worker(id) {
            self.permission_settled(id, Verdict::ask(why));
        }
    }

    /// The check `id` has ended, with `verdict` unless the chat or its
    /// turn has moved on: notes it in the history and runs what waited
    /// for it.
    fn permission_settled(&mut self, id: u64, verdict: Verdict) {
        let Some(pending) = self.permissions.pending.remove(&id) else {
            return;
        };
        let checked = if let Some(cancelled) = self.check_cancelled(&pending) {
            Checked::Cancelled(cancelled)
        } else if self.uses_permission_rules(pending.chat) {
            Checked::Decided(verdict)
        } else {
            Checked::Decided(Verdict::ask(
                "clackatoo's rules no longer answer this chat's requests",
            ))
        };
        // A question checked again is noted only once the rules answer
        // it (see [`Self::answer_by_rule`]).
        if !pending.again {
            let (decision, reason) = match &checked {
                Checked::Decided(verdict) => (verdict.decision.name(), verdict.reason.as_str()),
                Checked::Cancelled(cancelled) => (Cancelled::NAME, cancelled.reason()),
            };
            self.note_permission(pending.chat, &pending.action.tool, decision, reason);
        }
        (pending.resume)(self, checked);
    }

    /// Why the check `pending` decides nothing, if the chat it was for
    /// has moved on since it asked.
    fn check_cancelled(&self, pending: &Pending) -> Option<Cancelled> {
        let Some(chat) = self.chat_ref(pending.chat) else {
            return Some(Cancelled::ChatChanged);
        };
        if chat.session() != pending.session.as_deref() || chat.agent() != pending.agent {
            return Some(Cancelled::ChatChanged);
        }
        // A turn the agent takes later by itself is another turn: only
        // the turn that was stopped stays stopped.
        let ended = pending.turn_open && !chat.answering();
        if ended || chat.last_uid() != pending.turn {
            return Some(Cancelled::TurnEnded);
        }
        if chat.permission_stopped() {
            return Some(Cancelled::Stopped);
        }
        None
    }

    /// Notes in the history what became of the chat `chat`'s use of
    /// `tool`, and why: never its arguments.
    fn note_permission(&mut self, chat: TerminalId, tool: &Identity, decision: &str, reason: &str) {
        self.permissions.history.push_front(PermissionRecord {
            chat,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            tool: crate::skills::line(&tool.to_string(), 160),
            decision: decision.into(),
            reason: crate::skills::line(reason, 320),
        });
        self.permissions.history.truncate(MOST_KEPT);
    }

    /// Outstanding requests become human questions before handover. Late
    /// worker results have no continuation and can never replay an action.
    pub(super) fn finish_permissions(&mut self) {
        let ids: Vec<_> = self.permissions.pending.keys().copied().collect();
        for id in ids {
            self.permission_settled(id, Verdict::ask("Daemon upgrade requires a fresh approval"));
        }
        self.permissions.asked.clear();
    }

    /// Notes that the user of the chat `chat` is asked `question` about
    /// `action` because of the rules, so it can be checked again if they
    /// change. Questions since answered are forgotten here.
    pub(super) fn asked_under_rules(
        &mut self,
        chat: TerminalId,
        question: Question,
        action: Action,
    ) {
        let open: Vec<bool> = (self.permissions.asked.iter())
            .map(|asked| self.question_open(asked))
            .collect();
        let mut open = open.into_iter();
        self.permissions
            .asked
            .retain(|_| open.next().unwrap_or(false));
        if self.permissions.asked.len() >= MOST_ASKED {
            self.permissions.asked.remove(0);
        }
        self.permissions.asked.push(Asked {
            chat,
            question,
            action,
        });
    }

    /// Whether the user is still asked `asked`.
    ///
    /// An agent's request is known by its JSON-RPC ID, which a restarted
    /// agent counts from the start again. So it's the agent's that asked
    /// only while that very agent still runs, in the session it asked in:
    /// a question asked since under the same ID is another's.
    fn question_open(&self, asked: &Asked) -> bool {
        let Some(chat) = self.chat_ref(asked.chat) else {
            return false;
        };
        match &asked.question {
            Question::Provider(request) => {
                self.agents.running.contains_key(&request.agent)
                    && chat.agent() == request.name
                    && chat.session() == Some(request.session.as_str())
                    && chat.asks_agent(&request.request)
            }
            Question::Tool(call) => chat.asks_tool(*call),
        }
    }

    /// The rules changed. The checks waiting are evaluated again as their
    /// verdicts come (see [`Self::permission_evaluated`]); this checks
    /// the questions already put to the user again. None is cancelled:
    /// each stays open unless the new rules decide it.
    pub(super) fn permission_rules_changed(&mut self) {
        for asked in std::mem::take(&mut self.permissions.asked) {
            if !self.question_open(&asked) {
                continue;
            }
            let (chat, action) = (asked.chat, asked.action.clone());
            let resume = move |daemon: &mut Self, checked| daemon.asked_again(asked, &checked);
            self.queue_check(chat, action, true, Box::new(resume));
        }
    }

    /// The question `asked` was checked against new rules, which ended as
    /// `checked`. Rules that now allow or refuse what it asks about answer
    /// it in the user's place; otherwise it stays theirs to answer.
    fn asked_again(&mut self, asked: Asked, checked: &Checked) {
        if !self.question_open(&asked) {
            return;
        }
        let answered = match checked {
            Checked::Decided(verdict) => self.answer_by_rule(&asked, verdict),
            Checked::Cancelled(_) => false,
        };
        if !answered {
            self.permissions.asked.push(asked);
        }
    }

    /// Answers the open question `asked` as the rules' `verdict` says, if
    /// it says more than to ask, and takes the question back. Returns
    /// whether it did.
    fn answer_by_rule(&mut self, asked: &Asked, verdict: &Verdict) -> bool {
        match &asked.question {
            Question::Provider(request) => {
                let option = match request.answer_for(verdict.decision) {
                    RuleAnswer::Pick(option) => Some(option),
                    RuleAnswer::Cancel => None,
                    RuleAnswer::AskUser => return false,
                };
                if let Some(chat) = self.chat(asked.chat) {
                    chat.withdraw_request(&request.request);
                }
                self.answer_provider(request, option.as_deref());
            }
            Question::Tool(call) => {
                if verdict.decision == Decision::Ask {
                    return false;
                }
                if let Some(chat) = self.chat(asked.chat) {
                    chat.withdraw(*call);
                }
                if verdict.decision == Decision::Allow {
                    self.permitted(*call, true);
                } else {
                    self.refused_by_rule(*call, verdict);
                }
            }
        }
        let decision = verdict.decision.name();
        self.note_permission(asked.chat, &asked.action.tool, decision, &verdict.reason);
        self.collect_damage(asked.chat);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;
    use clackatoo_protocol::ChatAction;
    use serde_json::json;
    use std::sync::Mutex;

    /// How long a test waits for the worker: far longer than it takes,
    /// so a busy machine never fails one.
    const LONG: Duration = Duration::from_mins(2);

    /// Waits for every check to end, however long the worker takes,
    /// taking each verdict as the loop would.
    fn settle(daemon: &mut Daemon) {
        while !daemon.permissions.pending.is_empty() {
            let worker = daemon.permissions.worker.as_ref().expect("a worker");
            let (id, verdict) = worker.verdicts.recv_timeout(LONG).expect("a verdict");
            daemon.permission_evaluated(id, verdict);
        }
    }

    /// Checks `action` for the chat `chat` and waits for how it ends.
    fn checked(daemon: &mut Daemon, chat: TerminalId, action: Action) -> Checked {
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action, move |_, checked| {
            send.send(checked).unwrap();
        });
        settle(daemon);
        receive.try_recv().expect("the continuation ran")
    }

    /// Starts an agent for the chat's, which says nothing: one that runs,
    /// to ask and be answered.
    fn running_agent(daemon: &mut Daemon) -> AgentId {
        let spec = crate::AgentConfig::default().find("claude").unwrap();
        daemon.start_agent(spec, vec!["cat".into()]).unwrap()
    }

    /// What `checked` decided, if it wasn't cancelled.
    fn decision(checked: &Checked) -> Option<Decision> {
        match checked {
            Checked::Decided(verdict) => Some(verdict.decision),
            Checked::Cancelled(_) => None,
        }
    }

    fn action() -> Action {
        Action::managed(
            "docs",
            "tools/call",
            &json!({"name":"search"}),
            json!({"secret":"not-in-history"}),
        )
    }

    const ALLOW_SEARCH: &str =
        r#"[{"decision":"allow","kind":"tool","target":"managed_docs/search"}]"#;
    const DENY_SEARCH: &str =
        r#"[{"decision":"deny","kind":"tool","target":"managed_docs/search"}]"#;

    /// Sets the daemon's rules to those in `text`, as a config reload does.
    fn set_rules(daemon: &mut Daemon, text: &str) {
        daemon.config.permissions.rules = crate::permissions::read_rules(text).unwrap();
        daemon.permission_rules_changed();
    }

    /// Ends the chat's turn, has it use clackatoo's rules, never waiting
    /// on the clock for the worker, and allows the docs search.
    fn rules(daemon: &mut Daemon, chat: TerminalId) {
        daemon.permissions.wait = LONG;
        daemon
            .chat(chat)
            .unwrap()
            .answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
        daemon.chat(chat).unwrap().act(ChatAction::Set {
            setting: crate::chat::permissions::SETTING.into(),
            value: "clackatoo".into(),
        });
        assert!(daemon.uses_permission_rules(chat));
        set_rules(daemon, ALLOW_SEARCH);
    }

    /// The request of Claude Code, running as `agent`, to run `command`,
    /// numbered `request`.
    fn bash_request(agent: AgentId, request: u64, command: &str) -> ProviderRequest {
        let tool = json!({"toolCallId":format!("t{request}"),"name":"Bash","kind":"execute",
            "title":command,"rawInput":{"command":command}});
        // "Always" comes first, as the option a careless pick would take.
        let options = json!([
            {"optionId":"always","name":"Always allow","kind":"allow_always"},
            {"optionId":"allow","name":"Allow","kind":"allow_once"},
            {"optionId":"reject","name":"Reject","kind":"reject_once"}
        ]);
        ProviderRequest {
            agent,
            name: "claude".into(),
            session: "s1".into(),
            request: json!(request),
            tool: Box::new(serde_json::from_value(tool).unwrap()),
            options: serde_json::from_value(options).unwrap(),
            unknown: Vec::new(),
        }
    }

    #[test]
    fn worker_decisions_return_on_the_loop_and_history_never_keeps_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, checked| {
            send.send(checked).unwrap();
        });
        assert!(
            receive.try_recv().is_err(),
            "workers never execute continuations"
        );
        settle(&mut daemon);
        assert_eq!(
            decision(&receive.try_recv().unwrap()),
            Some(Decision::Allow)
        );
        let record = daemon.permissions.history.front().unwrap();
        assert_eq!(record.decision, "Allow");
        assert_eq!(record.tool, "managed_docs/search");
        assert!(!format!("{record:?}").contains("not-in-history"));
    }

    #[test]
    fn a_check_waiting_as_the_rules_change_is_evaluated_against_the_new_rules() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        for (rules, wanted, noted) in [
            (DENY_SEARCH, Decision::Deny, "Rule 1: Deny Tool"),
            ("[]", Decision::Ask, "No permission rule matched"),
            (ALLOW_SEARCH, Decision::Allow, "Rule 1: Allow Tool"),
        ] {
            set_rules(&mut daemon, ALLOW_SEARCH);
            let (send, receive) = mpsc::channel();
            daemon.check_permission(chat, action(), move |_, checked| {
                send.send(checked).unwrap();
            });
            // Whether or not the worker has evaluated it by now, its
            // verdict hasn't been taken: it was by the old rules.
            set_rules(&mut daemon, rules);
            settle(&mut daemon);
            let checked = receive.try_recv().unwrap();
            assert_eq!(decision(&checked), Some(wanted), "{rules}");
            let record = daemon.permissions.history.front().unwrap();
            assert_eq!(record.decision, wanted.name(), "{rules}");
            assert_eq!(record.reason, noted, "{rules}");
        }
    }

    #[test]
    fn a_question_stays_open_as_the_rules_change_unless_they_now_decide_it() {
        let allow_ls = r#"[{"decision":"allow","kind":"command","target":"ls -la"}]"#;
        let deny_ls = r#"[{"decision":"deny","kind":"command","target":"ls"}]"#;
        let ask_ls = r#"[{"decision":"ask","kind":"command","target":"ls"}]"#;
        let unrelated = r#"[{"decision":"deny","kind":"command","target":"git push"}]"#;
        for (rules_after, open_after, noted) in [
            (unrelated, true, None),
            (ask_ls, true, None),
            (allow_ls, false, Some("Allow")),
            (deny_ls, false, Some("Deny")),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (mut daemon, chat) = daemon_with_chat(dir.path());
            rules(&mut daemon, chat);
            let agent = running_agent(&mut daemon);
            daemon.provider_permission(bash_request(agent, 7, "ls -la"), None);
            settle(&mut daemon);
            let open = |daemon: &mut Daemon| daemon.chat(chat).unwrap().asks_agent(&json!(7));
            assert!(open(&mut daemon), "no rule matched, so the user is asked");
            let before = daemon.permissions.history.len();
            set_rules(&mut daemon, rules_after);
            settle(&mut daemon);
            assert_eq!(open(&mut daemon), open_after, "{rules_after}");
            let noted_now = (daemon.permissions.history.len() > before)
                .then(|| daemon.permissions.history.front().unwrap().decision.clone());
            assert_eq!(noted_now.as_deref(), noted, "{rules_after}");
            // A question left open is still checked by the next change.
            set_rules(&mut daemon, deny_ls);
            settle(&mut daemon);
            assert!(!open(&mut daemon), "{rules_after}");
        }
    }

    #[test]
    fn a_question_about_a_tool_call_is_taken_back_once_new_rules_decide_it() {
        let run = |command: &str| Action {
            tool: crate::permissions::Identity::clackatoo("run_in_terminal"),
            arguments: json!({"command":command}),
            command: Some(command.into()),
            cwd: None,
            file: None,
            search: false,
            consent: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        for (call, command) in [(1, "make"), (2, "git push")] {
            let hosted = daemon.chat(chat).unwrap();
            assert!(hosted.ask_for_tool(call, "run it", None, crate::chat::Choices::Once));
            daemon.asked_under_rules(chat, Question::Tool(call), run(command));
        }
        set_rules(
            &mut daemon,
            r#"[{"decision":"deny","kind":"command","target":"git push"}]"#,
        );
        settle(&mut daemon);
        let hosted = daemon.chat(chat).unwrap();
        assert!(hosted.asks_tool(1), "the new rule isn't about it");
        assert!(!hosted.asks_tool(2), "the new rule refuses it");
        let record = daemon.permissions.history.front().unwrap();
        assert_eq!(
            (record.tool.as_str(), record.decision.as_str()),
            ("clackatoo/run_in_terminal", "Deny")
        );
    }

    #[test]
    fn handover_asks_about_waiting_checks_and_never_reuses_an_allow() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, checked| {
            send.send(checked).unwrap();
        });
        daemon.finish_permissions();
        assert_eq!(decision(&receive.try_recv().unwrap()), Some(Decision::Ask));
        assert!(daemon.permissions.pending.is_empty());
    }

    #[test]
    fn a_stopped_turn_cancels_its_checks_and_a_turn_the_agent_takes_later_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        daemon.chat(chat).unwrap().act(ChatAction::Stop);
        let stopped = checked(&mut daemon, chat, action());
        assert_eq!(stopped, Checked::Cancelled(Cancelled::Stopped));
        assert!(Cancelled::Stopped.told().contains("Do not try it again"));
        let record = daemon.permissions.history.front().unwrap();
        assert_eq!(record.decision, "Cancelled", "never noted as a Deny");
        // Something the agent ran in the background ends, and it takes a
        // turn by itself: the Stop was of the turn before.
        assert!(daemon.chat(chat).unwrap().wake(None));
        let woken = checked(&mut daemon, chat, action());
        assert_eq!(decision(&woken), Some(Decision::Allow));
    }

    #[test]
    fn every_continuation_runs_cancelled_if_the_chat_or_its_turn_moved_on() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let gone = checked(&mut daemon, TerminalId(99), action());
        assert_eq!(gone, Checked::Cancelled(Cancelled::ChatChanged));

        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, checked| {
            send.send(checked).unwrap();
        });
        daemon.chat(chat).unwrap().connected(crate::agent::Opened {
            session: "s2".into(),
            settings: None,
        });
        settle(&mut daemon);
        assert_eq!(
            receive.try_recv().unwrap(),
            Checked::Cancelled(Cancelled::ChatChanged)
        );

        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, checked| {
            send.send(checked).unwrap();
        });
        assert!(daemon.chat(chat).unwrap().wake(None));
        settle(&mut daemon);
        assert_eq!(
            receive.try_recv().unwrap(),
            Checked::Cancelled(Cancelled::TurnEnded)
        );
    }

    #[test]
    fn a_worker_that_hangs_is_abandoned_and_the_checks_behind_it_are_answered() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        // One check hangs until the test lets it go, as a path on a mount
        // that has gone would.
        let slow = Action::managed("docs", "tools/call", &json!({"name":"slow"}), json!({}));
        let (release, held) = mpsc::channel::<()>();
        let held = Mutex::new(held);
        daemon.permissions.evaluate = Arc::new(move |config, action, context| {
            if action.tool.to_string() == "managed_docs/slow" {
                let _ = held.lock().unwrap().recv();
            }
            config.decide(action, context)
        });
        daemon.permissions.wait = Duration::ZERO;
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, slow, move |_, checked| {
            send.send(checked).unwrap();
        });
        daemon.take_permissions();
        let timed_out = receive.try_recv().expect("it asks rather than wait");
        assert_eq!(decision(&timed_out), Some(Decision::Ask));
        assert!(daemon.permissions.pending.is_empty(), "its place is free");
        assert_eq!(daemon.permissions.stuck.len(), 1);

        // More checks than the queue holds are all answered, by a fresh
        // worker, while the first still hangs.
        daemon.permissions.wait = LONG;
        for _ in 0..=MOST_WAITING {
            assert_eq!(
                decision(&checked(&mut daemon, chat, action())),
                Some(Decision::Allow)
            );
        }
        release.send(()).unwrap();
    }

    #[test]
    fn with_too_many_workers_hanging_every_check_asks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let (release, held) = mpsc::channel::<()>();
        let held = Mutex::new(held);
        daemon.permissions.evaluate = Arc::new(move |config, action, context| {
            let _ = held.lock().unwrap().recv();
            config.decide(action, context)
        });
        daemon.permissions.wait = Duration::ZERO;
        for _ in 0..MOST_STUCK {
            daemon.check_permission(chat, action(), |_, _| {});
            daemon.take_permissions();
        }
        assert_eq!(daemon.permissions.stuck.len(), MOST_STUCK);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, checked| {
            send.send(checked).unwrap();
        });
        assert_eq!(decision(&receive.try_recv().unwrap()), Some(Decision::Ask));
        drop(release);
    }

    #[test]
    fn a_rule_answers_an_agent_with_once_and_never_with_always() {
        let with = |kinds: &[&str]| {
            let mut request = bash_request(AgentId(0), 1, "ls");
            let options: Vec<Value> = (kinds.iter())
                .map(|kind| json!({"optionId":kind,"name":kind,"kind":kind}))
                .collect();
            request.options = serde_json::from_value(json!(options)).unwrap();
            request
        };
        let pick = |id: &str| RuleAnswer::Pick(id.into());
        let all = ["allow_always", "reject_always", "allow_once", "reject_once"];
        for (kinds, decision, wanted) in [
            (&all[..], Decision::Allow, pick("allow_once")),
            (&all[..], Decision::Deny, pick("reject_once")),
            (&all[..], Decision::Ask, RuleAnswer::AskUser),
            // With no "allow once", the user is asked: "always" is theirs
            // alone to pick.
            (
                &["allow_always", "reject_once"][..],
                Decision::Allow,
                RuleAnswer::AskUser,
            ),
            // A Deny with no "reject once" cancels the request.
            (
                &["allow_once", "reject_always"][..],
                Decision::Deny,
                RuleAnswer::Cancel,
            ),
            (&[][..], Decision::Deny, RuleAnswer::Cancel),
        ] {
            assert_eq!(with(kinds).answer_for(decision), wanted, "{kinds:?}");
        }
        // An option of a kind this build doesn't know is never picked.
        let mut unknown = with(&["allow_once"]);
        unknown.unknown = vec!["allow_once".into()];
        assert_eq!(unknown.answer_for(Decision::Allow), RuleAnswer::AskUser);
    }

    #[test]
    fn a_question_from_an_agent_that_restarted_is_not_taken_for_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        // The first agent asks, in its request 0, and the user answers.
        let first = running_agent(&mut daemon);
        daemon.provider_permission(bash_request(first, 0, "cargo build"), None);
        settle(&mut daemon);
        assert!(daemon.chat(chat).unwrap().withdraw_request(&json!(0)));
        // It restarts, and counts its requests from 0 again.
        daemon.agents.running.remove(&first);
        let second = running_agent(&mut daemon);
        daemon.provider_permission(bash_request(second, 0, "git push -f"), None);
        settle(&mut daemon);
        assert!(daemon.chat(chat).unwrap().asks_agent(&json!(0)));
        // A rule that would have answered the first is not about the
        // second, whose question stays, and whose agent stays asked.
        set_rules(
            &mut daemon,
            r#"[{"decision":"allow","kind":"command","target":"cargo build"}]"#,
        );
        settle(&mut daemon);
        assert!(daemon.chat(chat).unwrap().asks_agent(&json!(0)));
        assert_eq!(daemon.permissions.asked.len(), 1, "the first is forgotten");
        // One that is about the second answers it.
        set_rules(
            &mut daemon,
            r#"[{"decision":"deny","kind":"command","target":"git push"}]"#,
        );
        settle(&mut daemon);
        assert!(!daemon.chat(chat).unwrap().asks_agent(&json!(0)));
    }

    #[test]
    fn a_question_the_user_answers_while_it_is_checked_again_is_left_answered() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let agent = running_agent(&mut daemon);
        daemon.provider_permission(bash_request(agent, 7, "ls -la"), None);
        settle(&mut daemon);
        let noted = daemon.permissions.history.len();
        // The rules change so as to allow it, and before the worker's
        // verdict is taken, the user answers the question themselves.
        daemon.config.permissions.rules = crate::permissions::read_rules(
            r#"[{"decision":"allow","kind":"command","target":"ls -la"}]"#,
        )
        .unwrap();
        daemon.permission_rules_changed();
        assert!(!daemon.permissions.pending.is_empty());
        assert!(daemon.chat(chat).unwrap().withdraw_request(&json!(7)));
        settle(&mut daemon);
        // The rule answers nothing: the agent has its one answer, and
        // nothing is noted as allowed that the user decided.
        assert!(daemon.permissions.asked.is_empty());
        assert_eq!(daemon.permissions.history.len(), noted);
    }

    #[test]
    fn a_question_is_never_asked_for_an_agent_that_went_or_a_turn_that_ended() {
        let asks = |daemon: &mut Daemon, chat| daemon.chat(chat).unwrap().asks_agent(&json!(7));
        // The agent exits while its request is checked.
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let agent = running_agent(&mut daemon);
        daemon.provider_permission(bash_request(agent, 7, "ls -la"), None);
        daemon.agents.running.remove(&agent);
        settle(&mut daemon);
        assert!(!asks(&mut daemon, chat), "no one would get its answer");
        assert!(daemon.permissions.asked.is_empty());

        // The turn it asked in ends while its request is checked.
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let agent = running_agent(&mut daemon);
        assert!(daemon.chat(chat).unwrap().wake(None));
        daemon.provider_permission(bash_request(agent, 7, "ls -la"), None);
        daemon
            .chat(chat)
            .unwrap()
            .answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
        settle(&mut daemon);
        assert!(!asks(&mut daemon, chat), "its turn is over");
        let record = daemon.permissions.history.front().unwrap();
        assert_eq!(record.decision, "Cancelled");

        // While both go on, it is asked.
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let agent = running_agent(&mut daemon);
        assert!(daemon.chat(chat).unwrap().wake(None));
        daemon.provider_permission(bash_request(agent, 7, "ls -la"), None);
        settle(&mut daemon);
        assert!(asks(&mut daemon, chat));
    }
}
