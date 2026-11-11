//! Serialized specialist file saves, away from the daemon's event loop.
use super::Daemon;
use crate::skills::{Catalogue, Roots, specialist_files};
use clackatoo_protocol::{Error, PaneId, Response, SpecialistDefinition, SpecialistScope};
use mio::Token;
use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc;

#[derive(Debug)]
struct Job {
    token: Token,
    ticket: u64,
    edit: SpecialistDefinition,
    scope: SpecialistScope,
    path: Option<String>,
    roots: Vec<Roots>,
}
type Saved = Result<Vec<Catalogue>, String>;

#[derive(Debug, Default)]
pub(super) struct Saving {
    queue: VecDeque<Job>,
    running: Option<(Job, mpsc::Receiver<Saved>)>,
}
impl Daemon {
    pub(super) fn specialist_file_definitions(
        roots: &Roots,
        catalogue: &Catalogue,
    ) -> Vec<clackatoo_protocol::SpecialistFile> {
        let mut all = crate::specialists::built_in();
        for specialist in catalogue.specialists.values() {
            if let Some(old) = all.iter_mut().find(|s| s.name == specialist.name) {
                *old = specialist.clone();
            } else {
                all.push(specialist.clone());
            }
        }
        all.iter()
            .map(|s| {
                let mut definition = crate::skills::specialist_edit::definition(s);
                if let Some(includes) = catalogue.specialist_includes.get(&s.name)
                    && !includes.is_empty()
                {
                    let mut v: serde_json::Value =
                        serde_json::from_str(&definition.settings).expect("serialized specialist");
                    v["includes"] = serde_json::json!(includes);
                    definition.settings = v.to_string();
                }
                let source = catalogue.specialist_sources.get(&s.name);
                let path = source.map_or_else(String::new, |p| p.display().to_string());
                definition.saved = source.and_then(|p| catalogue.specialist_texts.get(p).cloned());
                let scope = specialist_files::source_scope(roots, std::path::Path::new(&path));
                let editable = source.is_some_and(|p| specialist_files::editable(p, &s.name));
                clackatoo_protocol::SpecialistFile {
                    definition,
                    path,
                    scope,
                    editable,
                }
            })
            .collect()
    }
    pub(super) fn save_specialist_file(
        &mut self,
        token: Token,
        pane: Option<PaneId>,
        edit: SpecialistDefinition,
        scope: SpecialistScope,
        path: Option<String>,
    ) {
        let Some(ticket) = self.take_place(token) else {
            return;
        };
        if let Some(err) = self.not_a_window(token) {
            self.hand_answer_with(token, ticket, Response::Error(err));
            return;
        }
        if self
            .client_wire(token)
            .is_some_and(|wire| wire < clackatoo_protocol::wire::MANAGED_MCP)
        {
            self.hand_answer_with(token, ticket, Response::Error(Error::Other("Upgrade this window before editing specialists; this daemon supports managed MCP selections.".into())));
            return;
        }
        if self.specialist_files.queue.len() >= 16 {
            self.hand_answer_with(
                token,
                ticket,
                Response::Error(Error::Other("Too many specialist saves are waiting".into())),
            );
            return;
        }
        let cwd = pane
            .and_then(|p| self.working_directory(p))
            .map_or_else(|| self.config.home.clone(), Into::into);
        let mut roots = vec![self.skill_roots(cwd)];
        let mut seen = BTreeSet::from([roots[0].cwd.clone()]);
        // Saving a personal role also refreshes already-open chats in other
        // projects, respecting each project's own higher-priority definitions.
        for own in self
            .terminals
            .values()
            .filter_map(|s| s.terminal.chat_ref())
        {
            let cwd = own.real_cwd().clone();
            if seen.insert(cwd.clone()) {
                roots.push(self.skill_roots(cwd));
            }
        }
        self.specialist_files.queue.push_back(Job {
            token,
            ticket,
            edit,
            scope,
            path,
            roots,
        });
        self.take_specialist_files();
    }
    pub(super) fn take_specialist_files(&mut self) {
        if let Some((_, answer)) = &self.specialist_files.running {
            let result=match answer.try_recv() {
                Ok(result)=>result,
                Err(mpsc::TryRecvError::Empty)=>return,
                Err(mpsc::TryRecvError::Disconnected)=>Err("Specialist save worker stopped. Reopen Settings to check the file before retrying.".into()),
            };
            let (job, _) = self.specialist_files.running.take().expect("worker exists");
            let response = match result {
                Ok(catalogues) => {
                    for (roots, catalogue) in job.roots.iter().zip(catalogues) {
                        self.refresh_specialist_catalogue(roots, catalogue);
                    }
                    Response::SpecialistSaved
                }
                Err(why) => Response::Error(Error::Other(why)),
            };
            self.hand_answer_with(job.token, job.ticket, response);
        }
        let Some(job) = self.specialist_files.queue.pop_front() else {
            return;
        };
        let roots = job.roots.clone();
        let edit = job.edit.clone();
        let path = job.path.clone();
        let scope = job.scope;
        let (send, answer) = mpsc::channel();
        let waker = std::sync::Arc::clone(&self.waker);
        match std::thread::Builder::new()
            .name("specialist-save".into())
            .spawn(move || {
                let result = specialist_files::save(&roots[0], &edit, scope, path.as_deref())
                    .map(|_| roots.iter().map(Catalogue::discover).collect());
                let _ = send.send(result);
                let _ = waker.wake();
            }) {
            Ok(_) => self.specialist_files.running = Some((job, answer)),
            Err(e) => self.hand_answer_with(
                job.token,
                job.ticket,
                Response::Error(Error::Other(e.to_string())),
            ),
        }
    }
    pub(super) fn specialist_files_busy(&self) -> bool {
        self.specialist_files.running.is_some()
    }
    pub(super) fn finish_specialist_files(&mut self) {
        let mut jobs = std::mem::take(&mut self.specialist_files.queue);
        if let Some((job, _)) = self.specialist_files.running.take() {
            jobs.push_front(job);
        }
        for job in jobs {
            self.hand_answer_with(job.token,job.ticket,Response::Error(Error::Other("The daemon upgraded during this save. Reopen Settings to check its file before retrying.".into())));
        }
    }
}
