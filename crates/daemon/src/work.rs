//! Shared, bounded, cooperative background execution. Features own their
//! data and freshness checks; this module owns scheduling and presentation.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clackatoo_protocol::{WorkItem, WorkState};

mod asked;
pub(crate) use asked::{Asked, Outcome, Question};

/// How many threads run jobs.
const WORKERS: usize = 2;
/// The most jobs queued or running at once, hidden ones included.
const MOST_ACTIVE: usize = 64;
/// The most finished jobs whose outcomes are kept for the list.
const MOST_HISTORY: usize = 16;
/// How long a background job waits before it counts as urgent as an
/// interactive one.
const AGE: Duration = Duration::from_secs(2);

/// Interactive demand can promote an already queued background job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Priority {
    Background,
    Interactive,
}

/// A long operation yields after a bounded slice, retaining its closure's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Again,
    Done,
}

/// Whether a job is the user's to watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Show {
    /// It's in the work list, and its outcome stays there until dismissed.
    Listed,
    /// It's the plumbing of a question someone is waiting on (a search, a
    /// preview, a lookup): never in the list, its history or the badge.
    /// Whoever asked hears how it went, so it leaves nothing behind.
    Hidden,
}

/// A job's work: called for each slice until it says it's done.
type Task = Box<dyn FnMut(&Context) -> Result<Step, String> + Send>;

/// A job, from when it's queued until its outcome is dismissed.
struct Entry {
    key: String,
    show: Show,
    priority: Priority,
    ready: Instant,
    item: WorkItem,
    task: Option<Task>,
}

/// What the workers and the loop share, under one lock.
#[derive(Default)]
struct State {
    next: u64,
    revision: u64,
    stopped: bool,
    entries: BTreeMap<u64, Entry>,
    queue: VecDeque<u64>,
    running: BTreeMap<u64, Option<String>>,
    history: VecDeque<u64>,
    last_scope: Option<String>,
}

/// The scheduler's state, and how its workers and the loop are woken.
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    wake: Box<dyn Fn() + Send + Sync>,
    dirty: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wakes the loop, once until it next looks, as a job changed. The
    /// list's revision moves only for a job `show`n in it, so hidden jobs
    /// never send windows a list that reads the same as the last.
    fn changed(&self, state: &mut State, show: Show) {
        if show == Show::Listed {
            state.revision += 1;
        }
        if !self.dirty.swap(true, Ordering::AcqRel) {
            (self.wake)();
        }
    }
}

/// Passed to each slice; no daemon or feature data is held under this lock.
pub(crate) struct Context {
    shared: Arc<Shared>,
    id: u64,
}

impl Context {
    /// Whether the job has been told to stop, or the scheduler has.
    pub(crate) fn cancelled(&self) -> bool {
        let state = self.shared.lock();
        state.stopped
            || state.entries.get(&self.id).is_none_or(|entry| {
                matches!(
                    entry.item.state,
                    WorkState::Cancelling | WorkState::Cancelled
                )
            })
    }

    /// A phase can have an unknown total; no invented percentage is needed.
    pub(crate) fn progress(&self, detail: &str, completed: u64, total: Option<u64>) {
        let mut state = self.shared.lock();
        let Some(entry) = state.entries.get_mut(&self.id) else {
            return;
        };
        if entry.item.state != WorkState::Running || entry.show == Show::Hidden {
            return;
        }
        let detail = shown(detail);
        let completed = total.map_or(completed, |total| completed.min(total));
        if (
            entry.item.detail.as_str(),
            entry.item.completed,
            entry.item.total,
        ) == (detail.as_str(), completed, total)
        {
            return;
        }
        entry.item.detail = detail;
        entry.item.completed = completed;
        entry.item.total = total;
        self.shared.changed(&mut state, Show::Listed);
    }
}

/// Lazily starts a fixed worker pool. Queued and running work is bounded,
/// each scope runs at most once concurrently, and outcomes have bounded history.
pub(crate) struct Scheduler {
    shared: Arc<Shared>,
    started: bool,
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl Scheduler {
    /// A scheduler that calls `wake` when the loop should look at it.
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                ready: Condvar::new(),
                wake: Box::new(wake),
                dirty: AtomicBool::new(false),
            }),
            started: false,
        }
    }

    /// Keys must include the feature namespace and all inputs affecting the
    /// result, including its revision. A duplicate promotes and shares work.
    pub(crate) fn submit(
        &mut self,
        key: String,
        title: &str,
        scope: Option<String>,
        priority: Priority,
        task: impl FnMut(&Context) -> Result<Step, String> + Send + 'static,
    ) -> Result<u64, String> {
        self.submit_as(Show::Listed, key, title, scope, priority, Box::new(task))
    }

    /// Queues a job the user never sees: the plumbing of a question
    /// someone is waiting on, who hears how it went. It's in no list, and
    /// is forgotten as it ends, however it ends. Keys are as
    /// [`submit`](Self::submit)'s.
    pub(crate) fn submit_hidden(
        &mut self,
        key: String,
        scope: Option<String>,
        priority: Priority,
        task: impl FnMut(&Context) -> Result<Step, String> + Send + 'static,
    ) -> Result<u64, String> {
        self.submit_as(Show::Hidden, key, "", scope, priority, Box::new(task))
    }

    /// Queues a job, `show`n or hidden, or joins one already queued with
    /// the same key.
    fn submit_as(
        &mut self,
        show: Show,
        key: String,
        title: &str,
        scope: Option<String>,
        priority: Priority,
        task: Task,
    ) -> Result<u64, String> {
        if !self.started {
            for _ in 0..WORKERS {
                let shared = Arc::clone(&self.shared);
                if let Err(err) = std::thread::Builder::new()
                    .name("background-work".into())
                    .spawn(move || run(&shared))
                {
                    self.stop();
                    return Err(format!("Could not start background worker: {err}"));
                }
            }
            self.started = true;
        }
        let mut state = self.shared.lock();
        if state.stopped {
            return Err("Background scheduler is stopping.".into());
        }
        if let Some((&id, entry)) = state.entries.iter_mut().find(|(_, entry)| {
            entry.key == key
                && entry.item.state.active()
                && entry.item.state != WorkState::Cancelling
        }) {
            entry.priority = entry.priority.max(priority);
            return Ok(id);
        }
        if state
            .entries
            .values()
            .filter(|entry| entry.item.state.active())
            .count()
            >= MOST_ACTIVE
        {
            return Err("Background work queue is full. Try again shortly.".into());
        }
        state.next += 1;
        let id = state.next;
        state.entries.insert(
            id,
            Entry {
                key,
                show,
                priority,
                ready: Instant::now(),
                task: Some(task),
                item: WorkItem {
                    id,
                    title: shown(title),
                    scope: scope.map(|scope| shown(&scope)),
                    state: WorkState::Queued,
                    detail: String::new(),
                    completed: 0,
                    total: None,
                    cancellable: true,
                },
            },
        );
        state.queue.push_back(id);
        self.shared.changed(&mut state, show);
        self.shared.ready.notify_all();
        Ok(id)
    }

    /// The work list's revision, which moves as a listed job changes. The
    /// loop asks each turn, and that's it looking: a change after this
    /// wakes it again. (Only [`snapshot`](Self::snapshot) used to count as
    /// looking, so a job that finished while the list was held back for
    /// its quarter second woke nobody, and its answer waited that long.)
    pub(crate) fn revision(&self) -> u64 {
        let state = self.shared.lock();
        self.shared.dirty.store(false, Ordering::Release);
        state.revision
    }

    /// The jobs the user may watch, with the list's revision.
    pub(crate) fn snapshot(&self) -> (u64, Vec<WorkItem>) {
        let state = self.shared.lock();
        self.shared.dirty.store(false, Ordering::Release);
        (
            state.revision,
            state
                .entries
                .values()
                .filter(|entry| entry.show == Show::Listed)
                .map(|entry| entry.item.clone())
                .collect(),
        )
    }

    /// A job's state: `None` once it's dismissed, or for a hidden job once
    /// it has ended.
    pub(crate) fn state(&self, id: u64) -> Option<WorkState> {
        self.shared
            .lock()
            .entries
            .get(&id)
            .map(|entry| entry.item.state)
    }

    /// Asks a job to stop, for the feature that queued it.
    pub(crate) fn cancel(&self, id: u64) -> Result<(), String> {
        self.cancel_if(id, |_| true)
    }

    /// Asks a job in the work list to stop, for the user. A hidden job
    /// isn't theirs to stop: it's as if there were none.
    pub(crate) fn cancel_listed(&self, id: u64) -> Result<(), String> {
        self.cancel_if(id, |show| show == Show::Listed)
    }

    /// Asks the job `id` to stop, if `may` says one shown as it is may be.
    fn cancel_if(&self, id: u64, may: impl Fn(Show) -> bool) -> Result<(), String> {
        let mut state = self.shared.lock();
        let Some(entry) = state.entries.get_mut(&id).filter(|entry| may(entry.show)) else {
            return Err("No such background job.".into());
        };
        match entry.item.state {
            // Yielded jobs can own expensive resources. Let a worker close
            // them inside their scope instead of dropping them on the caller.
            WorkState::Queued | WorkState::Running => entry.item.state = WorkState::Cancelling,
            _ => return Ok(()),
        }
        let show = entry.show;
        self.shared.changed(&mut state, show);
        self.shared.ready.notify_all();
        Ok(())
    }

    /// Takes a finished job's outcome out of the work list, for the user.
    pub(crate) fn dismiss(&self, id: u64) -> Result<(), String> {
        let mut state = self.shared.lock();
        let Some(entry) = state
            .entries
            .get(&id)
            .filter(|entry| entry.show == Show::Listed)
        else {
            return Ok(());
        };
        if entry.item.state.active() {
            return Err("Cancel this job before dismissing it.".into());
        }
        state.entries.remove(&id);
        state.history.retain(|old| *old != id);
        self.shared.changed(&mut state, Show::Listed);
        Ok(())
    }

    /// Tells every worker to end.
    fn stop(&self) {
        let mut state = self.shared.lock();
        state.stopped = true;
        self.shared.ready.notify_all();
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Ends the job `id` as `outcome`. A listed job's outcome joins the
/// history; a hidden job is forgotten.
fn finish(state: &mut State, id: u64, outcome: WorkState) {
    let entry = state.entries.get_mut(&id).expect("a submitted job");
    if entry.show == Show::Hidden {
        state.entries.remove(&id);
        return;
    }
    entry.item.state = outcome;
    entry.item.cancellable = false;
    entry.task = None;
    state.history.push_back(id);
    while state.history.len() > MOST_HISTORY {
        let at = state
            .history
            .iter()
            .position(|old| state.entries[old].item.state != WorkState::Failed)
            .unwrap_or(0);
        if let Some(old) = state.history.remove(at) {
            state.entries.remove(&old);
        }
    }
}

/// Where in the queue the job to run next is: one whose project has
/// nothing running, urgent before not, from another project than the
/// last if there is one, and the longest queued of those.
fn pick(state: &State) -> Option<usize> {
    let now = Instant::now();
    state
        .queue
        .iter()
        .enumerate()
        .filter(|(_, id)| {
            let scope = &state.entries[id].item.scope;
            !state.running.values().any(|running| running == scope)
        })
        .max_by_key(|(at, id)| {
            let entry = &state.entries[id];
            // Aging prevents a stream of interactive requests starving maintenance.
            let urgent =
                entry.priority == Priority::Interactive || now.duration_since(entry.ready) >= AGE;
            let different = entry.item.scope != state.last_scope;
            (urgent, different, std::cmp::Reverse(*at))
        })
        .map(|(at, _)| at)
}

/// A worker: takes jobs and runs a slice of each, until the scheduler
/// stops.
fn run(shared: &Arc<Shared>) {
    loop {
        let (id, mut task) = {
            let mut state = shared.lock();
            loop {
                if state.stopped {
                    return;
                }
                if let Some(at) = pick(&state) {
                    let id = state.queue.remove(at).expect("queued");
                    let entry = state.entries.get_mut(&id).expect("queued entry");
                    if entry.item.state != WorkState::Cancelling {
                        entry.item.state = WorkState::Running;
                    }
                    let scope = entry.item.scope.clone();
                    let task = entry.task.take().expect("queued task");
                    let show = entry.show;
                    state.last_scope.clone_from(&scope);
                    state.running.insert(id, scope);
                    shared.changed(&mut state, show);
                    break (id, task);
                }
                state = shared
                    .ready
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let context = Context {
            shared: Arc::clone(shared),
            id,
        };
        let result = if context.cancelled() {
            Ok(Step::Done)
        } else {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(&context)))
                .unwrap_or_else(|_| Err("Background worker panicked.".into()))
        };
        let mut task = Some(task);
        let mut state = shared.lock();
        let cancelled = state.stopped || state.entries[&id].item.state == WorkState::Cancelling;
        if cancelled || !matches!(result, Ok(Step::Again)) {
            // Resource teardown is part of a job's scope. In particular, a
            // SQLite connection must close before another job in that scope
            // can open its cache, while cleanup must not hold the queue lock.
            drop(state);
            drop(task.take());
            state = shared.lock();
        }
        state.running.remove(&id);
        let cancelled = state.stopped || state.entries[&id].item.state == WorkState::Cancelling;
        let show = state.entries[&id].show;
        match result {
            _ if cancelled => finish(&mut state, id, WorkState::Cancelled),
            Ok(Step::Again) => {
                let entry = state.entries.get_mut(&id).expect("running");
                entry.item.state = WorkState::Queued;
                entry.task = task.take();
                entry.ready = Instant::now();
                state.queue.push_back(id);
            }
            Ok(Step::Done) => finish(&mut state, id, WorkState::Completed),
            Err(err) => {
                state.entries.get_mut(&id).expect("running").item.detail = shown(&err);
                finish(&mut state, id, WorkState::Failed);
            }
        }
        shared.changed(&mut state, show);
        shared.ready.notify_all();
    }
}

/// `text` as the work list shows it: one line, of no more than 240
/// characters.
fn shown(text: &str) -> String {
    clackatoo_term::visible(
        text.chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch }),
        240,
    )
    .trim()
    .to_owned()
}

#[cfg(test)]
mod tests;
