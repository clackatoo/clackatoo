//! Bounded skill discovery off the event loop. A catalogue is ready before
//! its chat's agent connects; Settings and MCP can refresh it later.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Error, PaneId, Response, TerminalId};
use mio::Token;
use serde_json::Value;

use super::{Daemon, Source};
use crate::skills::{Catalogue, Roots};

const DEADLINE: Duration = Duration::from_secs(3);
const MOST_READING: usize = 64;

#[derive(Debug)]
enum Purpose {
    Connect(TerminalId),
    Refresh,
    Browse {
        token: Token,
        ticket: u64,
    },
    BrowseSpecialists {
        token: Token,
        ticket: u64,
        files: bool,
    },
    Tool {
        token: Token,
        id: Value,
        chat: TerminalId,
        secret: Option<String>,
        load: Option<(String, String)>,
    },
}

#[derive(Debug)]
struct Pending {
    reading: u64,
    work: u64,
    epoch: u64,
    purpose: Purpose,
    roots: Roots,
    due: Instant,
}

#[derive(Debug, Default)]
pub(super) struct Skills {
    epoch: u64,
    next: u64,
    pending: BTreeMap<u64, Pending>,
    done: Arc<Mutex<Vec<(u64, Catalogue)>>>,
    catalogues: BTreeMap<TerminalId, Catalogue>,
    announced: BTreeMap<TerminalId, (String, u64)>,
}

impl Daemon {
    pub(super) fn skill_catalogue(&self, chat: TerminalId) -> Option<&Catalogue> {
        self.skills.catalogues.get(&chat)
    }

    pub(super) fn forget_skill_catalogue(&mut self, chat: TerminalId) {
        self.skills.catalogues.remove(&chat);
        self.skills.announced.remove(&chat);
    }
    pub(super) fn skill_roots(&self, cwd: PathBuf) -> Roots {
        Roots {
            cwd,
            home: self.config.home.clone(),
            config: self
                .config
                .own_config
                .as_ref()
                .and_then(|own| own.dirs.daemon.parent().map(std::path::Path::to_owned)),
        }
    }

    fn scan_skills(&mut self, purpose: Purpose, roots: Roots) {
        if self.skills.pending.len() >= MOST_READING {
            self.skills_finished(
                purpose,
                &roots,
                Err("Too many skill requests. Try again shortly.".into()),
            );
            return;
        }
        self.skills.next += 1;
        let job = self.skills.next;
        let priority = if matches!(purpose, Purpose::Refresh) {
            crate::work::Priority::Background
        } else {
            crate::work::Priority::Interactive
        };
        let shared = self
            .skills
            .pending
            .values()
            .find(|pending| {
                pending.epoch == self.skills.epoch
                    && pending.roots == roots
                    && self
                        .work
                        .scheduler
                        .state(pending.work)
                        .is_some_and(|state| {
                            matches!(
                                state,
                                clackatoo_protocol::WorkState::Queued
                                    | clackatoo_protocol::WorkState::Running
                            )
                        })
            })
            .map(|pending| (pending.reading, pending.work));
        let done = Arc::clone(&self.skills.done);
        let reading = roots.clone();
        // Once the last subscriber consumed a result, a new request needs
        // a new key even if that worker has not returned from its final slice.
        let generation = shared.map_or(job, |(reading, _)| reading);
        let key = format!("skills:{}:{generation}", self.skills.epoch);
        let result = self.work.scheduler.submit(
            key,
            "Reading skills",
            Some(roots.cwd.to_string_lossy().into_owned()),
            priority,
            move |context| {
                if context.cancelled() {
                    return Ok(crate::work::Step::Done);
                }
                context.progress("Discovering skills and specialists", 0, None);
                let catalogue = Catalogue::discover(&reading);
                if !context.cancelled() {
                    done.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((job, catalogue));
                }
                Ok(crate::work::Step::Done)
            },
        );
        match result {
            Ok(work) => {
                let reading = shared
                    .filter(|(_, previous)| *previous == work)
                    .map_or(job, |(reading, _)| reading);
                self.skills.pending.insert(
                    job,
                    Pending {
                        reading,
                        work,
                        epoch: self.skills.epoch,
                        purpose,
                        roots,
                        due: Instant::now() + DEADLINE,
                    },
                );
            }
            Err(err) => self.skills_finished(purpose, &roots, Err(err)),
        }
    }

    /// Holds a new connection while its catalogue is read.
    pub(super) fn reading_skills(&mut self, chat: TerminalId) -> bool {
        if self
            .chat_ref(chat)
            .is_some_and(|c| c.side_parent().is_some())
        {
            return false;
        }
        if self.skills.catalogues.contains_key(&chat) {
            return false;
        }
        if !self
            .skills
            .pending
            .values()
            .any(|pending| matches!(pending.purpose, Purpose::Connect(id) if id == chat))
        {
            let Some(own) = self.chat_ref(chat) else {
                return false;
            };
            let roots = self.skill_roots(own.real_cwd());
            self.scan_skills(Purpose::Connect(chat), roots);
        }
        true
    }

    /// A kept runtime needs its host catalogue back without opening its
    /// already-live session again.
    pub(super) fn refresh_chat_skills(&mut self, chat: TerminalId) {
        if let Some(own) = self.chat_ref(chat) {
            let roots = self.skill_roots(own.real_cwd());
            self.scan_skills(Purpose::Refresh, roots);
        }
    }

    pub(super) fn browse_skills(&mut self, token: Token, pane: Option<PaneId>) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Err(err) = self.refuse_lent(token, &clackatoo_protocol::Request::Skills { pane }) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        if self.is_web_device(token) {
            self.hand_answer_with(
                token,
                ticket,
                Response::Error(Error::Other(
                    "Browse shared skills in a clackatoo window.".into(),
                )),
            );
            return;
        }
        let cwd = pane
            .and_then(|pane| self.working_directory(pane))
            .map_or_else(|| self.config.home.clone(), PathBuf::from);
        self.scan_skills(Purpose::Browse { token, ticket }, self.skill_roots(cwd));
    }

    pub(super) fn browse_specialists(&mut self, token: Token, pane: Option<PaneId>) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Some(err) = self.not_a_window(token) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        let cwd = pane
            .and_then(|p| self.working_directory(p))
            .map_or_else(|| self.config.home.clone(), PathBuf::from);
        self.scan_skills(
            Purpose::BrowseSpecialists {
                token,
                ticket,
                files: false,
            },
            self.skill_roots(cwd),
        );
    }

    pub(super) fn browse_specialist_files(&mut self, token: Token, pane: Option<PaneId>) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Some(err) = self.not_a_window(token) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        let cwd = pane
            .and_then(|p| self.working_directory(p))
            .map_or_else(|| self.config.home.clone(), PathBuf::from);
        self.scan_skills(
            Purpose::BrowseSpecialists {
                token,
                ticket,
                files: true,
            },
            self.skill_roots(cwd),
        );
    }
    pub(super) fn refresh_specialist_catalogue(&mut self, roots: &Roots, catalogue: Catalogue) {
        self.skills.epoch += 1;
        self.update_skill_catalogues(roots, &Ok(catalogue), &Purpose::Refresh);
    }

    /// Polling MCP Settings only invalidates an outstanding discovery if
    /// the effective server configuration actually changed.
    pub(super) fn refresh_mcp_catalogue(&mut self, roots: &Roots, catalogue: Catalogue) {
        let changed = self.terminals.keys().any(|id| {
            let Some(chat) = self.chat_ref(*id).filter(|c| c.real_cwd() == roots.cwd) else {
                return false;
            };
            let next = chat
                .specialist_definition()
                .map_or_else(|| Ok(catalogue.clone()), |s| catalogue.scoped(s));
            next.is_ok_and(|next| {
                self.skills
                    .catalogues
                    .get(id)
                    .is_none_or(|old| old.managed_mcp != next.managed_mcp)
            })
        });
        if changed {
            self.skills.epoch += 1;
        }
        self.update_skill_catalogues(roots, &Ok(catalogue), &Purpose::Refresh);
    }

    pub(super) fn skill_tool(
        &mut self,
        token: Token,
        id: Value,
        chat: TerminalId,
        load: Option<(String, String)>,
    ) {
        let Some(own) = self.chat_ref(chat) else {
            self.answer_tool(token, &id, Err("The chat has closed.".into()));
            return;
        };
        let roots = self.skill_roots(own.real_cwd());
        let secret = match self.sources.get(token.0) {
            Some(Source::Tools(connection)) if connection.chat == chat => connection.token.clone(),
            _ => return,
        };
        self.scan_skills(
            Purpose::Tool {
                token,
                id,
                chat,
                secret,
                load,
            },
            roots,
        );
    }

    fn update_skill_catalogues(
        &mut self,
        roots: &Roots,
        result: &Result<Catalogue, String>,
        purpose: &Purpose,
    ) -> std::collections::HashSet<TerminalId> {
        let mut blocked = std::collections::HashSet::new();
        if let Ok(catalogue) = result {
            let chats: Vec<_> = self
                .terminals
                .keys()
                .copied()
                .filter(|id| {
                    self.chat_ref(*id)
                        .is_some_and(|chat| chat.real_cwd() == roots.cwd)
                })
                .collect();
            for id in chats {
                let definition = self
                    .chat_ref(id)
                    .and_then(|chat| chat.specialist_definition())
                    .cloned();
                let scoped = definition
                    .as_ref()
                    .map_or_else(|| Ok(catalogue.clone()), |s| catalogue.scoped(s));
                let scoped = match scoped {
                    Ok(scoped) => scoped,
                    Err(why) => {
                        blocked.insert(id);
                        let opening = matches!(purpose, Purpose::Connect(chat) if *chat == id);
                        if let Some(chat) = self.chat(id) {
                            if opening {
                                chat.failed_to_connect(&why);
                            } else {
                                chat.notify(why);
                            }
                        }
                        self.forget_skill_catalogue(id);
                        if let Some(chat) = self.chat(id) {
                            chat.offer_skills(Vec::new());
                        }
                        self.collect_damage(id);
                        continue;
                    }
                };
                let changed = self.skills.catalogues.get(&id).is_none_or(|old| {
                    old.infos() != scoped.infos() || old.specialists != scoped.specialists
                });
                self.skills.catalogues.insert(id, scoped.clone());
                if changed {
                    self.skills.announced.remove(&id);
                }
                if let Some(chat) = self.chat(id) {
                    chat.offer_skills(scoped.commands());
                }
                let specialists = self.specialists_for(id);
                if let Some(chat) = self.chat(id) {
                    chat.offer_specialists(&specialists);
                }
                self.collect_damage(id);
            }
        }
        blocked
    }

    fn skills_finished(
        &mut self,
        purpose: Purpose,
        roots: &Roots,
        result: Result<Catalogue, String>,
    ) {
        let blocked = self.update_skill_catalogues(roots, &result, &purpose);
        match purpose {
            Purpose::Refresh => {
                if let Err(why) = result {
                    log::warn!("refreshing shared skills: {why}");
                }
            }
            Purpose::Connect(chat) => {
                if blocked.contains(&chat) {
                    return;
                }
                if let Err(why) = result {
                    if self
                        .chat_ref(chat)
                        .is_some_and(|own| own.specialist_definition().is_some())
                    {
                        self.forget_skill_catalogue(chat);
                        if let Some(own) = self.chat(chat) {
                            own.failed_to_connect(&why);
                        }
                        self.collect_damage(chat);
                        return;
                    }
                    self.skills.catalogues.insert(chat, Catalogue::default());
                    if let Some(own) = self.chat(chat) {
                        own.notify(why);
                    }
                }
                if self.chat_ref(chat).is_some() {
                    self.connect(chat);
                }
            }
            Purpose::Browse { token, ticket } => {
                let response = match result {
                    Ok(catalogue) => Response::Skills(catalogue.infos()),
                    Err(why) => Response::Error(Error::Other(why)),
                };
                self.hand_answer_with(token, ticket, response);
            }
            Purpose::BrowseSpecialists {
                token,
                ticket,
                files,
            } => {
                let response = match result {
                    Err(why) => Response::Error(Error::Other(why)),
                    Ok(catalogue) => {
                        let entries = Self::specialist_file_definitions(roots, &catalogue);
                        if files {
                            Response::SpecialistFiles(entries)
                        } else {
                            Response::Specialists(
                                entries.into_iter().map(|f| f.definition).collect(),
                            )
                        }
                    }
                };
                self.hand_answer_with(token, ticket, response);
            }
            Purpose::Tool {
                token,
                id,
                chat,
                secret,
                load,
            } => {
                // A token can be reused after disconnect; never answer a different session.
                if !matches!(self.sources.get(token.0), Some(Source::Tools(connection)) if connection.chat == chat && connection.token == secret)
                {
                    return;
                }
                if self
                    .chat_ref(chat)
                    .is_none_or(|chat| chat.tools() == clackatoo_protocol::AgentTools::Off)
                {
                    self.answer_tool(
                        token,
                        &id,
                        Err("The chat's tools have been turned off.".into()),
                    );
                    return;
                }
                let response = result.and_then(|catalogue| {
                    let catalogue = self
                        .chat_ref(chat)
                        .and_then(|chat| chat.specialist_definition())
                        .map_or_else(|| Ok(catalogue.clone()), |s| catalogue.scoped(s))?;
                    match load {
                        Some((name, arguments)) => catalogue.load(&name, &arguments, false),
                        None => {
                            serde_json::to_string(&catalogue.infos()).map_err(|err| err.to_string())
                        }
                    }
                });
                self.answer_tool(token, &id, response);
            }
        }
    }

    pub(super) fn next_skills_due(&self) -> Option<Instant> {
        self.skills
            .pending
            .values()
            .map(|pending| pending.due)
            .min()
    }

    /// Workers don't survive exec. Finish their RPCs before tool sockets
    /// and ordered client replies are handed to the next build.
    pub(super) fn finish_skill_reads_for_upgrade(&mut self) {
        let pending = std::mem::take(&mut self.skills.pending);
        for (_, pending) in pending {
            let _ = self.work.scheduler.cancel(pending.work);
            if !matches!(&pending.purpose, Purpose::Connect(_) | Purpose::Refresh) {
                self.skills_finished(
                    pending.purpose,
                    &pending.roots,
                    Err(
                        "The daemon is upgrading. Retry the skill request after it reconnects."
                            .into(),
                    ),
                );
            }
        }
    }

    pub(super) fn take_skills(&mut self) {
        let done = std::mem::take(
            &mut *self
                .skills
                .done
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (reading, catalogue) in done {
            let jobs: Vec<_> = self
                .skills
                .pending
                .iter()
                .filter_map(|(&job, pending)| (pending.reading == reading).then_some(job))
                .collect();
            for job in jobs {
                let pending = self.skills.pending.remove(&job).expect("listed above");
                if self
                    .work
                    .scheduler
                    .state(pending.work)
                    .is_some_and(|state| {
                        matches!(
                            state,
                            clackatoo_protocol::WorkState::Cancelling
                                | clackatoo_protocol::WorkState::Cancelled
                        )
                    })
                {
                    self.skills_finished(
                        pending.purpose,
                        &pending.roots,
                        Err("Reading skills was cancelled. Try again.".into()),
                    );
                } else if pending.epoch == self.skills.epoch {
                    self.skills_finished(pending.purpose, &pending.roots, Ok(catalogue.clone()));
                } else {
                    self.scan_skills(pending.purpose, pending.roots);
                }
            }
        }
        let stopped: Vec<_> = self
            .skills
            .pending
            .iter()
            .filter_map(|(&job, pending)| {
                let state = self.work.scheduler.state(pending.work)?;
                matches!(
                    state,
                    clackatoo_protocol::WorkState::Cancelled
                        | clackatoo_protocol::WorkState::Cancelling
                        | clackatoo_protocol::WorkState::Failed
                )
                .then_some(job)
            })
            .collect();
        for job in stopped {
            let pending = self.skills.pending.remove(&job).expect("listed above");
            self.skills_finished(
                pending.purpose,
                &pending.roots,
                Err(
                    "Reading skills was cancelled or the background worker failed. Try again."
                        .into(),
                ),
            );
        }
        let expired: Vec<_> = self
            .skills
            .pending
            .iter()
            .filter_map(|(&job, pending)| (pending.due <= Instant::now()).then_some(job))
            .collect();
        for job in expired {
            let pending = self.skills.pending.remove(&job).expect("pending above");
            if !self
                .skills
                .pending
                .values()
                .any(|other| other.work == pending.work)
            {
                let _ = self.work.scheduler.cancel(pending.work);
            }
            self.skills_finished(
                pending.purpose,
                &pending.roots,
                Err("Reading skill folders took too long. Try again shortly.".into()),
            );
        }
        self.skills
            .catalogues
            .retain(|id, _| self.terminals.contains_key(id));
        self.skills
            .announced
            .retain(|id, _| self.terminals.contains_key(id));
    }

    pub(super) fn skills_told(&self, chat: TerminalId) -> String {
        self.skills
            .catalogues
            .get(&chat)
            .map(Catalogue::instructions)
            .unwrap_or_default()
    }

    pub(super) fn skill_brief_for(
        &mut self,
        chat: TerminalId,
        action: &crate::chat::Action,
        session: Option<&str>,
    ) -> Option<String> {
        use crate::chat::Action;
        let text = match action {
            Action::Prompt(prompt)
            | Action::Briefed { text: prompt, .. }
            | Action::Carry { text: prompt, .. } => &prompt.text,
            _ => return None,
        };
        if clackatoo_protocol::agent_command(text, &self.chat_ref(chat)?.node().commands) {
            return None;
        }
        let brief = self.skill_brief(chat, session?);
        if matches!(action, Action::Briefed { .. }) {
            let skills = if self.chat_ref(chat)?.tools() == clackatoo_protocol::AgentTools::Off {
                String::new()
            } else {
                self.skills_told(chat)
            };
            (!skills.is_empty()).then_some(skills)
        } else {
            brief
        }
    }

    pub(super) fn is_shared_skill(&self, chat: TerminalId, text: &str) -> bool {
        self.skills
            .catalogues
            .get(&chat)
            .is_some_and(|catalogue| catalogue.has_command(text))
    }

    /// A session's first ordinary prompt carries the catalogue too, for
    /// runtimes that don't put MCP server instructions into model context.
    pub(super) fn skill_brief(&mut self, chat: TerminalId, session: &str) -> Option<String> {
        let own = self.chat_ref(chat)?;
        let tools = own.tools() != clackatoo_protocol::AgentTools::Off;
        let epoch = own.context_epoch;
        if self
            .skills
            .announced
            .get(&chat)
            .is_some_and(|(seen, before)| seen == session && *before == epoch)
        {
            return None;
        }
        self.skills.announced.insert(chat, (session.into(), epoch));
        let mut told = if tools {
            self.skills_told(chat)
        } else {
            String::new()
        };
        if let Some(specialist) = self
            .chat_ref(chat)
            .and_then(|chat| chat.specialist_definition())
        {
            let limits = self.chat_ref(chat).map_or(
                crate::specialists::Limits::None,
                crate::chat::HostedChat::limits,
            );
            if let Some(role) =
                crate::chat::role_for(&specialist.name, &specialist.instruction_text(), limits)
            {
                told.insert_str(0, &format!("{role}\n"));
            }
        }
        (!told.is_empty()).then_some(told)
    }
}

pub(super) fn words<'a>(before: &[&'a str], brief: Option<&'a str>, text: &'a str) -> Vec<&'a str> {
    before
        .iter()
        .copied()
        .chain(brief)
        .chain(std::iter::once(text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::agent_files::tests::daemon_with_chat;

    #[test]
    fn shared_reads_complete_all_consumers_and_a_consumed_read_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, _) = daemon_with_chat(dir.path());
        let roots = daemon.skill_roots(dir.path().to_owned());
        // Simulate the final slice of a worker whose result was already
        // consumed. Its old key must not absorb a new request's closure.
        let (release, wait) = std::sync::mpsc::channel();
        let (began, started) = std::sync::mpsc::channel();
        let previous = daemon
            .work
            .scheduler
            .submit(
                "skills:0:1".into(),
                "Reading skills",
                Some(roots.cwd.to_string_lossy().into_owned()),
                crate::work::Priority::Interactive,
                move |_| {
                    began.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(crate::work::Step::Done)
                },
            )
            .unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        daemon.skills.next = 1;
        daemon.scan_skills(Purpose::Refresh, roots.clone());
        daemon.scan_skills(Purpose::Refresh, roots);
        let work: Vec<_> = daemon
            .skills
            .pending
            .values()
            .map(|pending| pending.work)
            .collect();
        assert_eq!(work.len(), 2);
        assert_eq!(work[0], work[1], "consumers share one catalogue scan");
        assert_ne!(work[0], previous, "the consumed result cannot be reused");
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !daemon.skills.pending.is_empty() {
            daemon.take_skills();
            assert!(
                Instant::now() < until,
                "catalogue consumers did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_kept_session_refreshes_its_skillsets_without_reopening_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let bundle = dir.path().join(".agents/skillsets/release");
        std::fs::create_dir_all(bundle.join("skills/check")).unwrap();
        std::fs::write(
            bundle.join("skillset.toml"),
            "format = 1\nname = \"release\"\ndescription = \"Release checks\"\n",
        )
        .unwrap();
        std::fs::write(
            bundle.join("skills/check/SKILL.md"),
            "---\nname: check\ndescription: Check a release.\n---\nCheck it.\n",
        )
        .unwrap();
        daemon.refresh_chat_skills(chat);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !daemon.is_shared_skill(chat, "/check") {
            daemon.take_skills();
            assert!(Instant::now() < deadline, "catalogue did not refresh");
            std::thread::sleep(Duration::from_millis(5));
        }
        let own = daemon.chat(chat).unwrap();
        assert!(own.is_connected());
        assert_eq!(own.session(), Some("s1"));
        assert!(
            own.node()
                .commands
                .iter()
                .any(|command| command.name == "check")
        );
    }

    #[test]
    fn missing_dependencies_cannot_be_bypassed_by_retrying_or_losing_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        daemon
            .chat(chat)
            .unwrap()
            .remember_specialist(crate::specialists::Specialist {
                name: "reviewer".into(),
                extra: crate::specialists::Extras {
                    skills: Some(vec!["required-check".into()]),
                    ..crate::specialists::Extras::default()
                },
                ..crate::specialists::Specialist::default()
            });
        let roots = daemon.skill_roots(dir.path().to_owned());
        let missing = Ok(Catalogue::discover(&roots));
        for _ in 0..2 {
            let blocked = daemon.update_skill_catalogues(&roots, &missing, &Purpose::Connect(chat));
            assert!(blocked.contains(&chat));
            assert!(daemon.skill_catalogue(chat).is_none());
        }
        daemon.skills_finished(Purpose::Connect(chat), &roots, Err("scan timed out".into()));
        assert!(daemon.skill_catalogue(chat).is_none());
        let folder = dir.path().join(".agents/skills/required-check");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join("SKILL.md"),
            "---\nname: required-check\ndescription: Required\n---\nCheck\n",
        )
        .unwrap();
        let fixed = Ok(Catalogue::discover(&roots));
        assert!(
            daemon
                .update_skill_catalogues(&roots, &fixed, &Purpose::Connect(chat))
                .is_empty()
        );
        assert!(daemon.is_shared_skill(chat, "/required-check"));
    }

    #[test]
    fn active_role_returns_after_compaction_even_with_tools_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut daemon, chat) = daemon_with_chat(dir.path());
        let own = daemon.chat(chat).unwrap();
        own.remember_specialist(crate::specialists::Specialist {
            name: "reviewer".into(),
            instructions: "ACTIVE REVIEW ROLE".into(),
            ..crate::specialists::Specialist::default()
        });
        own.act(clackatoo_protocol::ChatAction::Set {
            setting: "clackatoo.tools".into(),
            value: "off".into(),
        });
        assert!(
            daemon
                .skill_brief(chat, "s1")
                .unwrap()
                .contains("ACTIVE REVIEW ROLE")
        );
        assert!(daemon.skill_brief(chat, "s1").is_none());
        daemon.chat(chat).unwrap().context_epoch += 1;
        assert!(
            daemon
                .skill_brief(chat, "s1")
                .unwrap()
                .contains("ACTIVE REVIEW ROLE")
        );
    }
}
