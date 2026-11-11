//! Project index ownership, watcher readiness and asynchronous MCP replies.

mod gui;

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clackatoo_protocol::{AgentTools, TerminalId, WorkState};
use mio::{Interest, Token, unix::SourceFd};
use serde_json::Value;

use super::{Daemon, Source};
use crate::outline::{Overlay, Project, Query, Scan};
use crate::work::Priority;

struct Entry {
    project: Arc<Project>,
    token: Option<Token>,
    work: Option<u64>,
    paused: bool,
    finished: Arc<AtomicBool>,
    used: Instant,
    reconcile: Instant,
}

struct Pending {
    token: Token,
    secret: Option<String>,
    chat: TerminalId,
    id: Value,
    work: u64,
    due: Instant,
    versions: Vec<(PathBuf, u64)>,
    root: PathBuf,
}

type Replies = Arc<Mutex<Vec<(u64, Result<String, String>)>>>;

#[derive(Default)]
pub(super) struct Outlines {
    projects: VecDeque<Entry>,
    next: u64,
    pending: BTreeMap<u64, Pending>,
    done: Replies,
    gui: gui::Gui,
}

impl std::fmt::Debug for Outlines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outlines")
            .field("projects", &self.projects.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl Daemon {
    pub(super) fn outline_project(&mut self, root: &Path) -> Result<Arc<Project>, String> {
        if let Some(at) = self
            .outlines
            .projects
            .iter()
            .position(|entry| entry.project.root == root)
        {
            let mut entry = self.outlines.projects.remove(at).expect("found");
            entry.used = Instant::now();
            entry.paused = false;
            let project = Arc::clone(&entry.project);
            self.outlines.projects.push_back(entry);
            return Ok(project);
        }
        let languages = self.config.languages.clone().ok_or("Outline indexing needs installed language packs. Install one with `clackatoo language install LANGUAGE`.")?;
        while self.outlines.projects.len() >= 4 {
            self.forget_oldest_outline();
        }
        let cache = self
            .config
            .own_config
            .as_ref()
            .and_then(|own| own.dirs.state.clone())
            .unwrap_or_else(|| self.config.home.join(".cache/clackatoo"))
            .join("outline");
        let socket = self
            .listener
            .local_addr()
            .ok()
            .and_then(|address| address.as_pathname().map(Path::to_owned))
            .unwrap_or_else(|| self.config.home.clone());
        let namespace =
            ring::digest::digest(&ring::digest::SHA256, socket.as_os_str().as_encoded_bytes());
        let namespace = crate::outline::hex(&namespace.as_ref()[..8]);
        let project = Arc::new(Project::new(
            root.to_owned(),
            &cache.join(namespace),
            languages,
        ));
        let token = project.watch.as_ref().and_then(|watch| {
            let token = Token(self.sources.insert(Source::Outline(root.to_owned())));
            if self
                .poll
                .registry()
                .register(
                    &mut SourceFd(&watch.fd.as_raw_fd()),
                    token,
                    Interest::READABLE,
                )
                .is_ok()
            {
                Some(token)
            } else {
                self.sources.remove(token.0);
                None
            }
        });
        project.watching(token.is_some());
        self.outlines.projects.push_back(Entry {
            project: Arc::clone(&project),
            token,
            work: None,
            paused: false,
            finished: Arc::new(AtomicBool::new(false)),
            used: Instant::now(),
            reconcile: Instant::now() + Duration::from_mins(2),
        });
        Ok(project)
    }

    fn forget_oldest_outline(&mut self) {
        if let Some(entry) = self.outlines.projects.pop_front() {
            if let Some(work) = entry.work {
                let _ = self.work.scheduler.cancel(work);
            }
            if let Some(token) = entry.token {
                if let Some(watch) = &entry.project.watch {
                    let _ = self
                        .poll
                        .registry()
                        .deregister(&mut SourceFd(&watch.fd.as_raw_fd()));
                }
                self.sources.remove(token.0);
            }
        }
    }

    pub(super) fn outline_changed(&mut self, root: &Path) {
        if let Some(entry) = self
            .outlines
            .projects
            .iter()
            .find(|entry| entry.project.root == root)
        {
            entry.project.notifications();
        }
    }

    /// Reads are scoped to the chat's existing project, never an arbitrary path.
    pub(super) fn outline_tool(
        &mut self,
        token: Token,
        chat: TerminalId,
        id: &Value,
        mut request: Query,
    ) {
        let result = (|| {
            if self.outlines.pending.len() >= 16 {
                return Err("Too many outline queries. Wait for one to finish.".into());
            }
            let root = self
                .chat_ref(chat)
                .ok_or("The chat closed.")?
                .project()
                .to_owned();
            let project = self.outline_project(&root)?;
            project.notifications();
            let mut overlays = Vec::new();
            let mut versions = Vec::new();
            let mut bytes = 0;
            let mut focus = Vec::new();
            for slot in self.buffers.values() {
                let buffer = &slot.buffer;
                let Ok(path) = buffer.path().strip_prefix(&root) else {
                    continue;
                };
                let Some(path) = path.to_str().filter(|path| crate::outline::relative(path)) else {
                    continue;
                };
                if versions.len() >= 512 {
                    return Err("Too many open buffers to validate outline freshness.".into());
                }
                versions.push((buffer.path().to_owned(), buffer.version()));
                if focus.len() < 8 {
                    focus.push(path.to_owned());
                }
                if !buffer.modified() {
                    continue;
                }
                if let Query::File { path: wanted, .. } = &request
                    && wanted != path
                {
                    continue;
                }
                bytes += buffer.text().len_bytes();
                if overlays.len() >= 32 || bytes > 16 * 1024 * 1024 {
                    return Err("Too much unsaved source to overlay safely. Save files or request a single file_outline.".into());
                }
                overlays.push(Overlay {
                    path: path.into(),
                    text: buffer.text().clone(),
                    version: buffer.version(),
                });
            }
            if let Query::Map { focus: wanted, .. } = &mut request
                && wanted.is_empty()
            {
                *wanted = focus;
            }
            self.outlines.next += 1;
            let ticket = self.outlines.next;
            let done = Arc::clone(&self.outlines.done);
            let scope = Some(root.to_string_lossy().into_owned());
            let work = self.work.scheduler.submit(
                format!("outline-query:{ticket}"),
                "Reading code outline",
                scope,
                Priority::Interactive,
                move |context| {
                    let result = crate::outline::query(&project, &request, &overlays, context);
                    let failure = result.as_ref().err().cloned();
                    done.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push((ticket, result));
                    failure.map_or(Ok(crate::work::Step::Done), Err)
                },
            )?;
            let secret = match self.sources.get(token.0) {
                Some(Source::Tools(connection)) => connection.token.clone(),
                _ => None,
            };
            self.outlines.pending.insert(
                ticket,
                Pending {
                    token,
                    secret,
                    chat,
                    id: id.clone(),
                    work,
                    versions,
                    root,
                    due: Instant::now() + Duration::from_secs(10),
                },
            );
            Ok(())
        })();
        if let Err(err) = result {
            self.answer_tool(token, id, Err(err));
        }
    }

    pub(super) fn take_outlines(&mut self) {
        self.take_code_symbols();
        let now = Instant::now();
        for entry in &mut self.outlines.projects {
            if entry.project.unread() {
                entry.project.notifications();
            }
            if let Some(work) = entry.work
                && self
                    .work
                    .scheduler
                    .state(work)
                    .is_none_or(|state| !state.active())
            {
                if !entry.finished.swap(false, Ordering::AcqRel) {
                    entry.project.interrupted(
                        "Outline indexing was cancelled or failed. The next query can retry it.",
                    );
                    entry.paused = true;
                }
                entry.work = None;
            }
            if now >= entry.reconcile {
                entry.project.changed(None);
                entry.reconcile = now + Duration::from_mins(2);
            }
            if entry.work.is_none() && !entry.paused && entry.project.needs_work() {
                let project = Arc::clone(&entry.project);
                let mut scan = None;
                entry.finished.store(false, Ordering::Release);
                let finished = Arc::clone(&entry.finished);
                self.outlines.next += 1;
                match self.work.scheduler.submit(
                    format!("outline-index:{}", self.outlines.next),
                    "Indexing outline",
                    Some(project.root.to_string_lossy().into_owned()),
                    Priority::Background,
                    move |context| {
                        if scan.is_none() {
                            scan = Some(Scan::new(Arc::clone(&project))?);
                        }
                        let result = scan.as_mut().expect("opened").step(context);
                        if matches!(result, Ok(crate::work::Step::Done)) && !context.cancelled() {
                            finished.store(true, Ordering::Release);
                        }
                        result
                    },
                ) {
                    Ok(work) => entry.work = Some(work),
                    Err(err) => {
                        entry.project.interrupted(&err);
                        entry.paused = true;
                    }
                }
            }
        }
        while self
            .outlines
            .projects
            .front()
            .is_some_and(|entry| now.duration_since(entry.used) >= Duration::from_mins(10))
        {
            self.forget_oldest_outline();
        }
        let done = std::mem::take(
            &mut *self
                .outlines
                .done
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for (ticket, result) in done {
            if let Some(pending) = self.outlines.pending.remove(&ticket) {
                self.outline_answer(&pending, result);
            }
        }
        self.expire_outline_queries(now);
    }

    fn expire_outline_queries(&mut self, now: Instant) {
        let expired: Vec<_> = self
            .outlines
            .pending
            .iter()
            .filter_map(|(&ticket, pending)| {
                (now >= pending.due
                    || self
                        .work
                        .scheduler
                        .state(pending.work)
                        .is_some_and(|state| {
                            matches!(
                                state,
                                WorkState::Cancelled | WorkState::Cancelling | WorkState::Failed
                            )
                        }))
                .then_some(ticket)
            })
            .collect();
        for ticket in expired {
            let pending = self.outlines.pending.remove(&ticket).expect("listed");
            let _ = self.work.scheduler.cancel(pending.work);
            self.outline_answer(
                &pending,
                Err("Outline query was cancelled, timed out or failed. Try again.".into()),
            );
        }
    }

    fn outline_answer(&mut self, pending: &Pending, mut result: Result<String, String>) {
        if !matches!(self.sources.get(pending.token.0), Some(Source::Tools(connection))
            if connection.chat == pending.chat && connection.token == pending.secret)
        {
            return;
        }
        if self
            .chat_ref(pending.chat)
            .is_none_or(|chat| chat.tools() == AgentTools::Off)
        {
            result = Err("This chat's tools have been turned off.".into());
        }
        if self
            .work
            .scheduler
            .state(pending.work)
            .is_some_and(|state| matches!(state, WorkState::Cancelled | WorkState::Cancelling))
        {
            result = Err("Outline query cancelled.".into());
        }
        let changed = pending.versions.iter().any(|(path, version)| {
            !self
                .buffers
                .values()
                .any(|slot| slot.buffer.path() == *path && slot.buffer.version() == *version)
        });
        let newly_modified = self.buffers.values().any(|slot| {
            slot.buffer.modified()
                && slot.buffer.path().starts_with(&pending.root)
                && !pending
                    .versions
                    .iter()
                    .any(|(path, _)| *path == slot.buffer.path())
        });
        if changed || newly_modified {
            result = Err(
                "Editor source changed during the query. Retry for the current outline.".into(),
            );
        }
        self.answer_tool(pending.token, &pending.id, result);
    }

    pub(super) fn invalidate_outlines(&self) {
        for entry in &self.outlines.projects {
            entry.project.changed(None);
        }
    }

    pub(super) fn next_outline_due(&self) -> Option<Instant> {
        self.outlines
            .projects
            .iter()
            .map(|entry| {
                if entry.project.unread() {
                    Instant::now()
                } else {
                    entry.reconcile
                }
            })
            .chain(self.outlines.pending.values().map(|pending| pending.due))
            .chain(self.code_symbols_due())
            .min()
    }

    pub(super) fn finish_outlines_for_upgrade(&mut self) {
        self.finish_code_symbols();
        for (_, pending) in std::mem::take(&mut self.outlines.pending) {
            let _ = self.work.scheduler.cancel(pending.work);
            self.outline_answer(
                &pending,
                Err("The daemon is upgrading. Retry after it reconnects.".into()),
            );
        }
        while !self.outlines.projects.is_empty() {
            self.forget_oldest_outline();
        }
    }
}
