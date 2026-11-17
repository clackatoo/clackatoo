//! Project index ownership, watcher readiness and asynchronous MCP replies.

mod gui;

use std::collections::VecDeque;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clackatoo_protocol::{AgentTools, BufferId, TerminalId};
use mio::{Interest, Token, unix::SourceFd};
use serde_json::Value;

use super::{Daemon, Source};
use crate::outline::{Overlay, Project, Query, Scan};
use crate::work::{Asked, Outcome, Priority, Question};

pub(super) use crate::outline::Overlays;

/// The most projects whose indexes are kept loaded, and how long one
/// stays loaded unused.
const MOST_PROJECTS: usize = 4;
const UNUSED: Duration = Duration::from_mins(10);

/// How often a project whose changes aren't all heard of is looked over
/// whole.
const RECONCILE: Duration = Duration::from_mins(2);

/// The most outline questions that may wait on workers at once, of
/// agents' and of windows' each.
const MOST_ASKED: usize = 16;

/// How long a query may take on a worker, and how long its answer is
/// waited for: longer, as it may queue behind the project's indexing.
const QUERY_BUDGET: Duration = Duration::from_secs(3);
const QUERY_WAIT: Duration = Duration::from_secs(10);

/// The most unsaved buffers, and bytes of them, one query reads in place
/// of their files.
const MOST_OVERLAYS: usize = 32;
const MOST_OVERLAY_BYTES: usize = 16 * 1024 * 1024;

/// A project whose index is loaded.
struct Entry {
    project: Arc<Project>,
    /// Where the loop hears of its watch, if it could listen.
    token: Option<Token>,
    /// The indexing job under way.
    work: Option<u64>,
    /// The user cancelled indexing: it isn't started again unasked.
    paused: bool,
    /// Set by the indexing job as it ends well.
    finished: Arc<AtomicBool>,
    used: Instant,
    reconcile: Instant,
}

/// The buffers a query read, each at the version it read: if any has
/// changed or closed by the time the answer is back, the answer is of
/// text that's gone.
type Versions = Vec<(BufferId, u64)>;

/// An agent's outline call waiting on a worker.
struct ToolAsk {
    token: Token,
    /// The tool connection's secret, so the answer never goes to another
    /// that took its place.
    secret: Option<String>,
    chat: TerminalId,
    id: Value,
    versions: Versions,
}

/// The projects whose indexes are loaded, and the outline questions
/// waiting on workers.
#[derive(Default)]
pub(super) struct Outlines {
    projects: VecDeque<Entry>,
    /// Numbers indexing jobs, so each has a key of its own.
    next: u64,
    asked: Asked<ToolAsk, Result<String, String>>,
    gui: gui::Gui,
}

impl std::fmt::Debug for Outlines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outlines")
            .field("projects", &self.projects.len())
            .field("asked", &self.asked)
            .finish_non_exhaustive()
    }
}

impl Daemon {
    /// Where this daemon's outline indexes are kept: under its state
    /// folder, by its socket.
    fn outline_cache(&self) -> PathBuf {
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
        cache.join(crate::outline::hex(&namespace.as_ref()[..8]))
    }

    /// The index of the project at `root` if it's loaded, as used now.
    /// Never loads one: for what only wants an index that's already
    /// there.
    pub(super) fn outline_loaded(&mut self, root: &Path) -> Option<Arc<Project>> {
        let at = self
            .outlines
            .projects
            .iter()
            .position(|entry| entry.project.root == root)?;
        let mut entry = self.outlines.projects.remove(at)?;
        entry.used = Instant::now();
        let project = Arc::clone(&entry.project);
        self.outlines.projects.push_back(entry);
        Some(project)
    }

    /// The index of the project at `root`, loaded if it wasn't, which
    /// starts its indexing, and lets it start again if the user had
    /// cancelled it.
    ///
    /// # Errors
    /// If no language packs are installed, or `root` is the home
    /// directory or above it: that's no project, and far too much to
    /// index.
    pub(super) fn outline_project(&mut self, root: &Path) -> Result<Arc<Project>, String> {
        if let Some(project) = self.outline_loaded(root) {
            if let Some(entry) = self.outlines.projects.back_mut() {
                entry.paused = false;
            }
            return Ok(project);
        }
        if self.config.home.starts_with(root) {
            return Err(
                "The home directory isn't a project, so it has no code outline. Work in a \
                 project's folder to get one."
                    .into(),
            );
        }
        let languages = self.config.languages.clone().ok_or(
            "Outline indexing needs installed language packs. Install one with `clackatoo \
             language install LANGUAGE`.",
        )?;
        while self.outlines.projects.len() >= MOST_PROJECTS {
            self.forget_oldest_outline();
        }
        let cache = self.outline_cache();
        let project = Arc::new(Project::new(root.to_owned(), &cache, languages));
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
            reconcile: Instant::now() + RECONCILE,
        });
        self.prune_outlines(cache);
        Ok(project)
    }

    /// Has a worker clear out of `cache` the indexes nobody uses, as a
    /// project is loaded: the only time the folder grows.
    fn prune_outlines(&mut self, cache: PathBuf) {
        let loaded: Vec<PathBuf> = self
            .outlines
            .projects
            .iter()
            .map(|entry| entry.project.database.clone())
            .collect();
        let queued = self.work.scheduler.submit_hidden(
            "outline-prune".into(),
            None,
            Priority::Background,
            move |_| {
                crate::outline::prune(
                    &cache,
                    &loaded,
                    crate::outline::MOST_UNUSED,
                    crate::outline::MOST_HELD,
                );
                Ok(crate::work::Step::Done)
            },
        );
        if let Err(why) = queued {
            log::debug!("unused outline indexes weren't cleared out: {why}");
        }
    }

    /// Unloads the project used longest ago: its indexing is stopped and
    /// its watch let go. Its index stays on disk.
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

    /// The watch of the project at `root` has something to read.
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

    /// The unsaved buffers under `root` that a query reads in place of
    /// their files: only the one for `only`, if given, and no more than a
    /// query takes.
    pub(super) fn outline_overlays(&self, root: &Path, only: Option<&str>) -> Overlays {
        let mut overlays = Overlays::default();
        let mut bytes = 0;
        for (&id, slot) in &self.buffers {
            let buffer = &slot.buffer;
            if !buffer.modified() {
                continue;
            }
            let Some(path) = project_path(root, buffer.path()) else {
                continue;
            };
            if only.is_some_and(|only| only != path) {
                continue;
            }
            bytes += buffer.text().len_bytes();
            if overlays.list.len() >= MOST_OVERLAYS || bytes > MOST_OVERLAY_BYTES {
                overlays.left_out = true;
                break;
            }
            overlays.list.push(Overlay {
                path: path.to_owned(),
                text: buffer.text().clone(),
                buffer: id,
                version: buffer.version(),
            });
        }
        overlays
    }

    /// What a query's freshness rests on: the buffers it read as
    /// `overlays`, and the buffer open on `target`, the file asked about,
    /// if one is. Typing in any other file doesn't make its answer stale.
    fn outline_versions(&self, root: &Path, overlays: &Overlays, target: Option<&str>) -> Versions {
        let mut versions: Versions = overlays
            .list
            .iter()
            .map(|overlay| (overlay.buffer, overlay.version))
            .collect();
        let Some(target) = target else {
            return versions;
        };
        for (&id, slot) in &self.buffers {
            let on_target = project_path(root, slot.buffer.path()) == Some(target);
            if on_target && !versions.iter().any(|(buffer, _)| *buffer == id) {
                versions.push((id, slot.buffer.version()));
            }
        }
        versions
    }

    /// Whether each buffer in `versions` is still open at that version.
    fn outline_fresh(&self, versions: &Versions) -> bool {
        versions.iter().all(|(id, version)| {
            self.buffers
                .get(id)
                .is_some_and(|slot| slot.buffer.version() == *version)
        })
    }

    /// Reads are scoped to the chat's existing project, never an arbitrary path.
    pub(super) fn outline_tool(
        &mut self,
        token: Token,
        chat: TerminalId,
        id: &Value,
        request: Query,
    ) {
        if let Err(err) = self.ask_outline_tool(token, chat, id, request) {
            self.answer_tool(token, id, Err(err));
        }
    }

    /// Puts an agent's outline call to a worker.
    ///
    /// # Errors
    /// If too many wait, the chat has closed, or its project can't be
    /// indexed.
    fn ask_outline_tool(
        &mut self,
        token: Token,
        chat: TerminalId,
        id: &Value,
        mut request: Query,
    ) -> Result<(), String> {
        if self.outlines.asked.len() >= MOST_ASKED {
            return Err("Too many outline queries. Wait for one to finish.".into());
        }
        let root = self
            .chat_ref(chat)
            .ok_or("The chat closed.")?
            .project()
            .to_owned();
        let project = self.outline_project(&root)?;
        project.notifications();
        let target = match &request {
            Query::File { path, .. } => Some(path.clone()),
            _ => None,
        };
        let overlays = self.outline_overlays(&root, target.as_deref());
        let versions = self.outline_versions(&root, &overlays, target.as_deref());
        if let Query::Map { focus, .. } = &mut request
            && focus.is_empty()
        {
            // With no files named, the map leans towards those open.
            *focus = self
                .buffers
                .values()
                .filter_map(|slot| project_path(&root, slot.buffer.path()))
                .take(8)
                .map(str::to_owned)
                .collect();
        }
        let secret = match self.sources.get(token.0) {
            Some(Source::Tools(connection)) => connection.token.clone(),
            _ => None,
        };
        self.outlines.asked.ask(
            &mut self.work.scheduler,
            Question {
                name: "outline-query",
                scope: Some(root.to_string_lossy().into_owned()),
                wait: QUERY_WAIT,
            },
            ToolAsk {
                token,
                secret,
                chat,
                id: id.clone(),
                versions,
            },
            move |context| {
                crate::outline::query(&project, &request, &overlays, QUERY_BUDGET, &|| {
                    context.cancelled()
                })
            },
        )
    }

    /// The loop's turn: reads the watches, starts indexing what changed,
    /// unloads projects long unused, and answers the agents whose queries
    /// have settled.
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
                entry.project.reconcile();
                entry.reconcile = now + RECONCILE;
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
            .is_some_and(|entry| now.duration_since(entry.used) >= UNUSED)
        {
            self.forget_oldest_outline();
        }
        let settled = self.outlines.asked.take(&self.work.scheduler, now);
        for (ask, outcome) in settled {
            let result = match outcome {
                Outcome::Answered(result) => result,
                Outcome::TimedOut => {
                    Err("The outline query took too long and was stopped. Try again.".into())
                }
                Outcome::Lost => Err("The outline query was stopped. Try again.".into()),
            };
            self.outline_answer(&ask, result);
        }
    }

    /// Answers the agent that asked `ask`, if it's still there to hear.
    fn outline_answer(&mut self, ask: &ToolAsk, mut result: Result<String, String>) {
        if !matches!(self.sources.get(ask.token.0), Some(Source::Tools(connection))
            if connection.chat == ask.chat && connection.token == ask.secret)
        {
            return;
        }
        if self
            .chat_ref(ask.chat)
            .is_none_or(|chat| chat.tools() == AgentTools::Off)
        {
            result = Err("This chat's tools have been turned off.".into());
        } else if result.is_ok() && !self.outline_fresh(&ask.versions) {
            result = Err(
                "Editor source changed during the query. Retry for the current outline.".into(),
            );
        }
        self.answer_tool(ask.token, &ask.id, result);
    }

    /// The language packs changed: every loaded project is scanned again.
    pub(super) fn invalidate_outlines(&self) {
        for entry in &self.outlines.projects {
            entry.project.changed(None);
        }
    }

    /// When the loop must next look at outlines.
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
            .chain(self.outlines.asked.next_due())
            .chain(self.code_symbols_due())
            .min()
    }

    /// The daemon is handing over to a newer one: whoever waits is told,
    /// and every project is unloaded.
    pub(super) fn finish_outlines_for_upgrade(&mut self) {
        self.finish_code_symbols();
        let waiting = self.outlines.asked.withdraw(&self.work.scheduler, |_| true);
        for ask in waiting {
            self.outline_answer(
                &ask,
                Err("The daemon is upgrading. Retry after it reconnects.".into()),
            );
        }
        while !self.outlines.projects.is_empty() {
            self.forget_oldest_outline();
        }
    }
}

/// The file at `path` as the project at `root` names it, if it's inside.
fn project_path<'a>(root: &Path, path: &'a Path) -> Option<&'a str> {
    path.strip_prefix(root)
        .ok()?
        .to_str()
        .filter(|path| crate::outline::relative(path))
}
