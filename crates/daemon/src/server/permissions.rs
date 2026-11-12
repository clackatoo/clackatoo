//! Bounded rule evaluation off the daemon loop, with no execution on workers.
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clackatoo_protocol::{PermissionRecord, TerminalId};

use super::Daemon;
use super::chats::AgentId;
use crate::permissions::{Action, Config, Context, Decision, Engine, Verdict};
use agent_client_protocol_schema::v1::{PermissionOption, PermissionOptionKind, ToolCallUpdate};
use serde_json::Value;

pub(super) struct ProviderRequest {
    pub(super) agent: AgentId,
    pub(super) name: String,
    pub(super) session: String,
    pub(super) request: Value,
    pub(super) tool: Box<ToolCallUpdate>,
    pub(super) options: Vec<PermissionOption>,
    pub(super) unknown: Vec<String>,
}

type Resume = Box<dyn FnOnce(&mut Daemon, Verdict) + Send>;
const LIMIT: usize = 64;
const WAIT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct Permissions {
    pub(super) history: VecDeque<PermissionRecord>,
    next: u64,
    jobs: Option<mpsc::SyncSender<Job>>,
    results: Option<mpsc::Receiver<(u64, Verdict)>>,
    pending: BTreeMap<u64, Pending>,
}

impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permissions")
            .field("pending", &self.pending.len())
            .field("history", &self.history.len())
            .finish_non_exhaustive()
    }
}
struct Job {
    id: u64,
    config: Config,
    action: Action,
    context: Context,
}
struct Pending {
    chat: TerminalId,
    session: Option<String>,
    agent: String,
    turn: Option<u32>,
    tool: String,
    config: Config,
    due: Instant,
    resume: Resume,
}

impl Daemon {
    pub(super) fn provider_permission(
        &mut self,
        mut asked: ProviderRequest,
        checked: Option<Verdict>,
    ) {
        let serving = self
            .chat_in(&asked.name, &asked.session)
            .map(|(id, _)| id)
            .is_some_and(|id| self.serves_tools(id));
        let Some((id, chat)) = self
            .chat_in(&asked.name, &asked.session)
            .filter(|(_, chat)| !chat.told_again())
        else {
            if let Some(slot) = self.agents.running.get_mut(&asked.agent) {
                slot.agent.answer_permission(&asked.request, None);
            }
            return;
        };
        if chat.side_parent().is_some() {
            if let Some(slot) = self.agents.running.get_mut(&asked.agent) {
                slot.agent.answer_permission(&asked.request, None);
            }
            return;
        }
        let rules = chat.permission_engine() == Engine::Clackatoo;
        let interaction =
            asked.name == "antigravity" && asked.tool.tool_call_id.0.starts_with("interaction_");
        let known: Vec<_> = asked
            .options
            .iter()
            .filter(|option| !asked.unknown.iter().any(|id| *option.option_id.0 == **id))
            .cloned()
            .collect();
        let answered = if interaction {
            None
        } else {
            chat.turned_down(&asked.tool, &known)
                .or_else(|| chat.about_own_tools(&asked.tool, &known, serving))
        };
        if let Some(option) = answered {
            if let Some(slot) = self.agents.running.get_mut(&asked.agent) {
                slot.agent.answer_permission(&asked.request, Some(&option));
            }
            self.collect_damage(id);
            return;
        }
        if rules && checked.is_none() && !interaction {
            let action = Action::provider(&asked.name, &asked.tool);
            self.check_permission(id, action, move |daemon, verdict| {
                daemon.provider_permission(asked, Some(verdict));
            });
            return;
        }
        if let Some(verdict) = checked {
            let kind = match verdict.decision {
                Decision::Allow => Some(PermissionOptionKind::AllowOnce),
                Decision::Deny => Some(PermissionOptionKind::RejectOnce),
                Decision::Ask => None,
            };
            if let Some(kind) = kind {
                let option = known
                    .iter()
                    .find(|option| option.kind == kind)
                    .map(|option| option.option_id.0.to_string());
                // A deny with no reject_once cancels. An allow with no
                // allow_once must still ask; never pick allow_always.
                if option.is_some() || verdict.decision == Decision::Deny {
                    if let Some(slot) = self.agents.running.get_mut(&asked.agent) {
                        slot.agent
                            .answer_permission(&asked.request, option.as_deref());
                    }
                    return;
                }
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
        if !accepted && let Some(slot) = self.agents.running.get_mut(&asked.agent) {
            slot.agent.answer_permission(&asked.request, None);
        }
        self.collect_damage(id);
    }

    pub(super) fn uses_permission_rules(&self, id: TerminalId) -> bool {
        self.chat_ref(id)
            .is_some_and(|chat| chat.permission_engine() == Engine::Clackatoo)
    }

    /// Only executable inputs enter the policy. Each result is tied to the
    /// original chat session and current policy before its continuation runs.
    pub(super) fn check_permission(
        &mut self,
        chat: TerminalId,
        action: Action,
        resume: impl FnOnce(&mut Self, Verdict) + Send + 'static,
    ) {
        let Some(own) = self.chat_ref(chat) else {
            return;
        };
        let config = self.config.permissions.clone();
        let session = own.session().map(str::to_owned);
        let agent = own.agent().to_owned();
        let turn = own.permission_turn();
        let context = Context {
            cwd: own.working_directory().into(),
            project: own.project().to_owned(),
            home: self.config.home.clone(),
            protected: self
                .config
                .own_config
                .iter()
                .flat_map(|own| {
                    std::iter::once(own.dirs.config().to_owned()).chain(own.dirs.state.clone())
                })
                .collect(),
        };
        let tool = crate::skills::line(&action.tool, 160);
        if self.permissions.pending.len() >= LIMIT {
            resume(self, Verdict::ask("Too many rule evaluations are waiting"));
            return;
        }
        if self.permissions.jobs.is_none() {
            let (send, receive) = mpsc::sync_channel::<Job>(LIMIT);
            let (done, results) = mpsc::channel();
            let waker = Arc::clone(&self.waker);
            let spawned = std::thread::Builder::new()
                .name("permission-rules".into())
                .spawn(move || {
                    while let Ok(job) = receive.recv() {
                        let verdict =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                job.config.decide(&job.action, &job.context)
                            }))
                            .unwrap_or_else(|_| Verdict::ask("Rule evaluation failed"));
                        if done.send((job.id, verdict)).is_err() {
                            break;
                        }
                        let _ = waker.wake();
                    }
                });
            if spawned.is_err() {
                resume(self, Verdict::ask("The rules worker could not start"));
                return;
            }
            self.permissions.jobs = Some(send);
            self.permissions.results = Some(results);
        }
        let id = self.permissions.next;
        self.permissions.next += 1;
        let job = Job {
            id,
            config: config.clone(),
            action,
            context,
        };
        if self
            .permissions
            .jobs
            .as_ref()
            .is_none_or(|send| send.try_send(job).is_err())
        {
            resume(self, Verdict::ask("The rules worker is unavailable"));
            return;
        }
        self.permissions.pending.insert(
            id,
            Pending {
                chat,
                session,
                agent,
                turn,
                tool,
                config,
                due: Instant::now() + WAIT,
                resume: Box::new(resume),
            },
        );
    }

    pub(super) fn next_permission_due(&self) -> Option<Instant> {
        self.permissions
            .pending
            .values()
            .map(|pending| pending.due)
            .min()
    }

    pub(super) fn take_permissions(&mut self) {
        let mut done: Vec<_> = self
            .permissions
            .results
            .iter()
            .flat_map(mpsc::Receiver::try_iter)
            .collect();
        done.extend(
            self.permissions
                .pending
                .iter()
                .filter(|(_, p)| p.due <= Instant::now())
                .map(|(id, _)| (*id, Verdict::ask("Rule evaluation timed out"))),
        );
        for (id, verdict) in done {
            self.permission_done(id, verdict);
        }
    }

    fn permission_done(&mut self, id: u64, mut verdict: Verdict) {
        let Some(pending) = self.permissions.pending.remove(&id) else {
            return;
        };
        let valid = self.chat_ref(pending.chat).is_some_and(|chat| {
            chat.session() == pending.session.as_deref() && chat.agent() == pending.agent
        });
        if !valid {
            return;
        }
        if !self.uses_permission_rules(pending.chat)
            || self.config.permissions.rules != pending.config.rules
        {
            verdict = Verdict {
                decision: Decision::Deny,
                reason: "Permission rules changed; this pending request was cancelled".into(),
            };
        }
        if self
            .chat_ref(pending.chat)
            .is_some_and(|chat| chat.permission_turn() != pending.turn || chat.permission_stopped())
        {
            verdict = Verdict {
                decision: Decision::Deny,
                reason: "The originating turn ended or was stopped".into(),
            };
        }
        self.permissions.history.push_front(PermissionRecord {
            chat: pending.chat,
            at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            tool: pending.tool,
            decision: verdict.decision.name().into(),
            reason: crate::skills::line(&verdict.reason, 320),
        });
        self.permissions.history.truncate(LIMIT);
        (pending.resume)(self, verdict);
    }

    /// Outstanding requests become human questions before handover. Late
    /// worker results have no continuation and can never replay an action.
    pub(super) fn finish_permissions(&mut self) {
        let ids: Vec<_> = self.permissions.pending.keys().copied().collect();
        for id in ids {
            self.permission_done(id, Verdict::ask("Daemon upgrade requires a fresh approval"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;
    use serde_json::json;

    fn action() -> Action {
        Action {
            tool: "managed_docs/search".into(),
            arguments: json!({"secret":"not-in-history"}),
            command: None,
            cwd: None,
            file: None,
            always_ask: false,
        }
    }
    fn rules(daemon: &mut Daemon, chat: TerminalId) {
        daemon
            .chat(chat)
            .unwrap()
            .answered(Ok(agent_client_protocol_schema::v1::StopReason::EndTurn));
        daemon
            .chat(chat)
            .unwrap()
            .act(clackatoo_protocol::ChatAction::Set {
                setting: crate::chat::permissions::SETTING.into(),
                value: "clackatoo".into(),
            });
        assert!(daemon.uses_permission_rules(chat));
        daemon.config.permissions.rules = crate::permissions::read_rules(
            r#"[{"decision":"allow","kind":"tool","target":"managed_docs/search"}]"#,
        )
        .unwrap();
    }
    fn take(daemon: &mut Daemon, answer: &mpsc::Receiver<Verdict>) -> Verdict {
        let due = Instant::now() + Duration::from_secs(3);
        loop {
            daemon.take_permissions();
            if let Ok(verdict) = answer.try_recv() {
                return verdict;
            }
            assert!(Instant::now() < due, "permission worker did not answer");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_decisions_return_on_the_loop_and_history_never_keeps_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, verdict| {
            send.send(verdict).unwrap();
        });
        assert!(
            receive.try_recv().is_err(),
            "workers never execute continuations"
        );
        assert_eq!(take(&mut daemon, &receive).decision, Decision::Allow);
        let record = daemon.permissions.history.front().unwrap();
        assert_eq!(record.decision, "Allow");
        assert!(!format!("{record:?}").contains("not-in-history"));
    }

    #[test]
    fn changed_policy_and_handover_cannot_reuse_an_earlier_allow() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        rules(&mut daemon, chat);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, verdict| {
            send.send(verdict).unwrap();
        });
        daemon.config.permissions.rules.clear();
        assert_eq!(take(&mut daemon, &receive).decision, Decision::Deny);
        let (send, receive) = mpsc::channel();
        daemon.check_permission(chat, action(), move |_, verdict| {
            send.send(verdict).unwrap();
        });
        daemon.finish_permissions();
        assert_eq!(receive.recv().unwrap().decision, Decision::Ask);
        assert!(daemon.permissions.pending.is_empty());
    }
}
