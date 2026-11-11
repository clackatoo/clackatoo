//! Shared, bounded, cooperative background execution. Features own their
//! data and freshness checks; this module owns scheduling and presentation.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use clackatoo_protocol::{WorkItem, WorkState};

const WORKERS: usize = 2;
const MOST_ACTIVE: usize = 64;
const MOST_HISTORY: usize = 16;
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

type Task = Box<dyn FnMut(&Context) -> Result<Step, String> + Send>;

struct Entry {
    key: String,
    priority: Priority,
    ready: Instant,
    item: WorkItem,
    task: Option<Task>,
}

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

    fn changed(&self, state: &mut State) {
        state.revision += 1;
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
        if entry.item.state != WorkState::Running {
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
        self.shared.changed(&mut state);
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
                priority,
                ready: Instant::now(),
                task: Some(Box::new(task)),
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
        self.shared.changed(&mut state);
        self.shared.ready.notify_all();
        Ok(id)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    pub(crate) fn snapshot(&self) -> (u64, Vec<WorkItem>) {
        let state = self.shared.lock();
        self.shared.dirty.store(false, Ordering::Release);
        (
            state.revision,
            state
                .entries
                .values()
                .map(|entry| entry.item.clone())
                .collect(),
        )
    }

    pub(crate) fn state(&self, id: u64) -> Option<WorkState> {
        self.shared
            .lock()
            .entries
            .get(&id)
            .map(|entry| entry.item.state)
    }

    pub(crate) fn cancel(&self, id: u64) -> Result<(), String> {
        let mut state = self.shared.lock();
        let Some(entry) = state.entries.get_mut(&id) else {
            return Err("No such background job.".into());
        };
        match entry.item.state {
            // Yielded jobs can own expensive resources. Let a worker close
            // them inside their scope instead of dropping them on the caller.
            WorkState::Queued | WorkState::Running => entry.item.state = WorkState::Cancelling,
            _ => return Ok(()),
        }
        self.shared.changed(&mut state);
        self.shared.ready.notify_all();
        Ok(())
    }

    pub(crate) fn dismiss(&self, id: u64) -> Result<(), String> {
        let mut state = self.shared.lock();
        let Some(entry) = state.entries.get(&id) else {
            return Ok(());
        };
        if entry.item.state.active() {
            return Err("Cancel this job before dismissing it.".into());
        }
        state.entries.remove(&id);
        state.history.retain(|old| *old != id);
        self.shared.changed(&mut state);
        Ok(())
    }

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

fn finish(state: &mut State, id: u64, outcome: WorkState) {
    let entry = state.entries.get_mut(&id).expect("a submitted job");
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
                    state.last_scope.clone_from(&scope);
                    state.running.insert(id, scope);
                    shared.changed(&mut state);
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
        shared.changed(&mut state);
        shared.ready.notify_all();
    }
}

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
