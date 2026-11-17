//! Specialists as Settings sees and saves them: each with the file that
//! defines it, and saves made one at a time, away from the daemon's
//! event loop.
use super::Daemon;
use crate::skills::{Catalogue, Roots, specialist_files};
use clackatoo_protocol::{Error, PaneId, Response, SpecialistDefinition, SpecialistScope};
use mio::Token;
use std::collections::VecDeque;
use std::sync::mpsc;

/// A save asked for, and who by.
#[derive(Debug)]
struct Job {
    token: Token,
    ticket: u64,
    edit: SpecialistDefinition,
    scope: SpecialistScope,
    path: Option<String>,
    roots: Roots,
}

/// The saves waiting, and the one being made.
#[derive(Debug, Default)]
pub(super) struct Saving {
    queue: VecDeque<Job>,
    running: Option<(Job, mpsc::Receiver<Result<(), String>>)>,
}

/// The most saves waiting at once.
const MOST_SAVES: usize = 16;

impl Daemon {
    /// The specialists there are with `catalogue`, read for `roots`, as
    /// Settings lists them: each with its file, and whether Settings
    /// changes that file or makes a copy.
    pub(super) fn specialist_file_definitions(
        roots: &Roots,
        catalogue: &Catalogue,
    ) -> Vec<clackatoo_protocol::SpecialistFile> {
        let file = |specialist: &crate::specialists::Specialist| {
            let name = &specialist.name;
            let mut definition = crate::skills::specialist_edit::definition(specialist);
            let includes = (catalogue.specialist_includes.get(name)).filter(|i| !i.is_empty());
            if let Some(includes) = includes {
                let mut settings: serde_json::Value =
                    serde_json::from_str(&definition.settings).expect("serialized specialist");
                settings["includes"] = serde_json::json!(includes);
                definition.settings = settings.to_string();
            }
            let source = catalogue.specialist_sources.get(name);
            definition.saved =
                source.and_then(|path| catalogue.specialist_texts.get(path).cloned());
            clackatoo_protocol::SpecialistFile {
                definition,
                path: source.map_or_else(String::new, |path| path.display().to_string()),
                scope: source.map_or(SpecialistScope::Personal, |path| {
                    specialist_files::source_scope(roots, path)
                }),
                editable: source.is_some_and(|path| specialist_files::editable(path, name)),
            }
        };
        (super::specialists::with_built_in(Some(catalogue)).iter())
            .map(file)
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
            let said = "Upgrade this window before editing specialists; this daemon supports \
                        managed MCP selections.";
            self.hand_answer_with(token, ticket, Response::Error(Error::Other(said.into())));
            return;
        }
        if self.specialist_files.queue.len() >= MOST_SAVES {
            let refusal = "Too many specialist saves are waiting".to_owned();
            self.hand_answer_with(token, ticket, Response::Error(Error::Other(refusal)));
            return;
        }
        let cwd = pane
            .and_then(|p| self.working_directory(p))
            .map_or_else(|| self.config.home.clone(), Into::into);
        self.specialist_files.queue.push_back(Job {
            token,
            ticket,
            edit,
            scope,
            path,
            roots: self.skill_roots(cwd),
        });
        self.take_specialist_files();
    }

    /// Answers the save that was being made, if it's done, and starts the
    /// next. A save changes what every chat's catalogue would find, a
    /// personal one in every project: they're all read again.
    pub(super) fn take_specialist_files(&mut self) {
        if let Some((_, answer)) = &self.specialist_files.running {
            let result = match answer.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Err(
                    "Specialist save worker stopped. Reopen Settings to check the file before retrying."
                        .into(),
                ),
            };
            let (job, _) = self.specialist_files.running.take().expect("worker exists");
            let response = match result {
                Ok(()) => {
                    self.skills_changed();
                    Response::SpecialistSaved
                }
                Err(why) => Response::Error(Error::Other(why)),
            };
            self.hand_answer_with(job.token, job.ticket, response);
        }
        let Some(job) = self.specialist_files.queue.pop_front() else {
            return;
        };
        let (roots, edit, path, scope) = (
            job.roots.clone(),
            job.edit.clone(),
            job.path.clone(),
            job.scope,
        );
        let (send, answer) = mpsc::channel();
        let waker = std::sync::Arc::clone(&self.waker);
        let save = move || {
            let saved = specialist_files::save(&roots, &edit, scope, path.as_deref());
            let _ = send.send(saved.map(|_| ()));
            let _ = waker.wake();
        };
        let spawned = std::thread::Builder::new()
            .name("specialist-save".into())
            .spawn(save);
        match spawned {
            Ok(_) => self.specialist_files.running = Some((job, answer)),
            Err(err) => {
                let failed = Response::Error(Error::Other(err.to_string()));
                self.hand_answer_with(job.token, job.ticket, failed);
            }
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
            let said = "The daemon upgraded during this save. Reopen Settings to check its file \
                        before retrying.";
            self.hand_answer_with(
                job.token,
                job.ticket,
                Response::Error(Error::Other(said.into())),
            );
        }
    }
}
