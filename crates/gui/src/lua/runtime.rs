//! The Lua thread: Luau, the client's API, and handlers run as coroutines.
//!
//! One Lua runs for every window of the process: the config loads once, and
//! each window tells it what the world looks like through it. Lua keeps a
//! world for each, by the window's number.
//!
//! Each handler runs in a coroutine of its own. A key's handlers, those of
//! `on("keypress")` and a binding's function, run first, each until it
//! finishes or first waits, as `pane:split` does for the daemon's answer;
//! then whether the key stopped is decided and said. Everything else is
//! background work: a handler going on once its answer comes, one for an
//! event nothing waits on, as a chat's turn ending, and a binding whose
//! keys went on at once. Background work runs a slice at a time
//! ([`SLICE`]), in turn: between slices, any key that has come is decided
//! first, so however long background work takes, a key waits for it a
//! slice at most, and usually not at all, as a key coming asks the slice
//! running to end at once.
//!
//! A slice ends by Luau's interrupt yielding the handler's coroutine, as
//! if it had called `coroutine.yield()`, which only that coroutine does:
//! one of the handler's own, made with `coroutine.create`, would yield to
//! the handler, not here. Luau can't yield inside a metamethod or a call
//! from C, such as `table.sort`'s comparator; there the handler runs on,
//! and the watchdog asks again a slice later. A handler that calls
//! `coroutine.yield()` itself lets other work go first too.
//!
//! A handler that runs too long, as a loop that never ends would, is
//! stopped by a watchdog thread, which only wakes while Lua runs: it's
//! given [`HANDLER_TIME`] of running, counted across its slices, from when
//! it starts or its answer comes until it finishes or waits.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, PaneId, Side, TabId, WorkspaceId};
use mlua::thread::ThreadStatus;
use mlua::{Function, Lua, MultiValue, Table, Thread, Value, VmState};

use super::{
    Chunk, FromLua, KeyInfo, Listen, Origin, Scope, Source, To, ToLua, TurnEnd, Wake, World,
};
use crate::command::Kind;
use crate::keymap::{KeyMode, parse_keys, parse_modes};
use crate::options::provenance::{Place, Source as SetFrom};
use crate::options::{self, Options, SetBy};

/// How long `init.lua` may run before it's stopped.
pub(crate) const INIT_TIME: Duration = Duration::from_secs(2);

/// How long a handler may run, from when it starts or its answer comes
/// until it finishes or waits, before it's stopped. Less than the time a
/// key waits for it.
const HANDLER_TIME: Duration = Duration::from_millis(250);

/// How long background work runs before letting other work go first.
const SLICE: Duration = Duration::from_millis(2);

const PRELUDE: &str = include_str!("prelude.luau");

/// The last request any Lua of the process made. Numbered across them, so
/// an answer to one a Lua run before a `:reload` made never resumes a
/// coroutine of the Lua after.
static REQUESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A request's number, never one used before in the process.
fn next_request() -> u64 {
    REQUESTS.fetch_add(1, Ordering::Relaxed) + 1
}

/// What the core functions share.
struct State {
    /// The world through each window, by its number.
    worlds: RefCell<BTreeMap<u32, World>>,
    /// The options every window shares, as the windows last said, and as
    /// set since.
    options: RefCell<Options>,
    to_windows: Sender<(To, Origin, FromLua)>,
    wake: Wake,
    bindings: RefCell<HashMap<u64, Function>>,
    handlers: RefCell<HashMap<u64, (Scope, Listen, Function)>>,
    next: Cell<u64>,
    /// Which of the config's files is loading, while one is.
    loading: Cell<Option<SetFrom>>,
    /// The window whose event is being handled, while one is.
    window: Cell<Option<u32>>,
    /// Whether that event is guarded (see [`Origin`]).
    guarded: Cell<bool>,
    /// The files of Lua that have set something, each kept once.
    files: RefCell<Vec<Arc<str>>>,
}

impl State {
    fn new(options: Options, to_windows: Sender<(To, Origin, FromLua)>, wake: Wake) -> Self {
        Self {
            worlds: RefCell::default(),
            options: RefCell::new(options),
            to_windows,
            wake,
            bindings: RefCell::default(),
            handlers: RefCell::default(),
            next: Cell::new(0),
            loading: Cell::new(None),
            window: Cell::new(None),
            guarded: Cell::new(false),
            files: RefCell::default(),
        }
    }

    fn tell(&self, to: To, message: FromLua) {
        let from = Origin {
            window: self.window.get(),
            guarded: self.guarded.get(),
        };
        if self.to_windows.send((to, from, message)).is_ok() {
            (self.wake)();
        }
    }

    /// Who's told what's done for the event being handled: its window, or
    /// with none, `otherwise`.
    fn here(&self, otherwise: To) -> To {
        self.window.get().map_or(otherwise, To::Window)
    }

    fn next_id(&self) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        id
    }

    /// `file`, as kept once.
    fn file(&self, file: &str) -> Arc<str> {
        let mut files = self.files.borrow_mut();
        if let Some(kept) = files.iter().find(|kept| ***kept == *file) {
            return kept.clone();
        }
        let kept: Arc<str> = file.into();
        files.push(kept.clone());
        kept
    }

    /// What's setting something now, from Lua: the config's file that's
    /// loading, or the config later, as a handler; and the lines of the
    /// user's code on the stack, from Luau's debug info. Worked out only
    /// when something's set.
    fn set_by(&self, lua: &Lua) -> SetBy {
        /// Deeper than any config's calls go, to stop at.
        const DEEPEST: usize = 200;
        let mut inner = None;
        let mut at = None;
        for level in 0..DEEPEST {
            let Some(frame) = lua.inspect_stack(level, |debug| {
                // The config's files are named `@path`; the prelude and
                // the core's functions, which aren't the user's, aren't.
                let file = self.file(debug.source().source?.strip_prefix('@')?);
                let line = u32::try_from(debug.current_line()?).ok()?;
                Some(Place { file, line })
            }) else {
                break;
            };
            if let Some(place) = frame {
                if inner.is_none() {
                    inner = Some(place.clone());
                }
                at = Some(place);
            }
        }
        let inner = inner.filter(|inner| Some(inner) != at.as_ref());
        let loading = self.loading.get();
        SetBy {
            source: loading.unwrap_or(SetFrom::Config),
            loading: loading.is_some(),
            at,
            inner,
        }
    }
}

/// What Lua starts with.
pub(super) struct Start<'a> {
    pub(super) source: &'a Source,
    /// The config's files, `settings.lua` then `init.lua`.
    pub(super) chunks: &'a [Chunk],
    /// The options, as the windows share them.
    pub(super) options: Options,
    /// The windows open as it starts.
    pub(super) windows: Vec<u32>,
}

/// Runs the config, as `start` says, then whatever the windows ask, until
/// they let go of their end. `slice` asks background work to let other
/// work go first, as the windows set it when a key comes.
pub(super) fn run(
    start: Start<'_>,
    (from_windows, to_windows): (Receiver<ToLua>, Sender<(To, Origin, FromLua)>),
    slice: Arc<AtomicBool>,
    wake: Wake,
) {
    let Start {
        source,
        chunks,
        options,
        windows,
    } = start;
    let state = Rc::new(State::new(options, to_windows, wake));
    // Windows open now are there as the config loads, though nothing's
    // shown in them yet.
    for window in windows {
        state.worlds.borrow_mut().insert(window, World::default());
    }
    let mut runtime = match Runtime::new(source, &state, slice) {
        Ok(runtime) => runtime,
        Err(err) => {
            state.tell(To::All, FromLua::Error(format!("Lua didn't start: {err}")));
            state.tell(To::Client, FromLua::Ready);
            return;
        }
    };
    runtime.load(chunks);
    state.tell(To::Client, FromLua::Ready);
    loop {
        // Whatever's come goes first: a key is decided before background
        // work goes on.
        loop {
            match from_windows.try_recv() {
                Ok(message) => runtime.handle(message),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(job) = runtime.background.pop_front() {
            runtime.run_slice(job);
            continue;
        }
        match from_windows.recv() {
            Ok(message) => runtime.handle(message),
            Err(_) => return,
        }
    }
}

/// A handler's coroutine, as background work, or waiting on a request.
struct Job {
    thread: Thread,
    /// What it's resumed with next: its event, to begin with; an answer;
    /// or nothing, after a slice.
    args: MultiValue,
    /// The window whose event it handles, if any, and whether that's
    /// guarded.
    origin: Origin,
    /// The config's file it is, if it's one still loading.
    loading: Option<SetFrom>,
    /// How long it's run since it started or its answer came.
    ran: Duration,
}

impl Job {
    fn new(thread: Thread, args: MultiValue, origin: Origin) -> Self {
        Self {
            thread,
            args,
            origin,
            loading: None,
            ran: Duration::ZERO,
        }
    }
}

/// The Lua thread's Luau, and what it's waiting on.
struct Runtime {
    lua: Lua,
    state: Rc<State>,
    /// What the prelude gives the runtime: `request`, `event`, `decide`.
    prelude: Table,
    /// Coroutines waiting on requests, by request.
    waiting: HashMap<u64, Job>,
    /// Work to run a slice at a time, in turn, once no key waits.
    background: VecDeque<Job>,
    watchdog: Arc<Watchdog>,
    /// The coroutine being run a slice at a time, while one is: the only
    /// one the interrupt yields.
    sliced: Rc<Cell<*mut mlua::lua_State>>,
    /// Set to end the slice running.
    slice: Arc<AtomicBool>,
    /// How long a handler may run: [`HANDLER_TIME`], but in tests.
    budget: Duration,
}

impl Runtime {
    fn new(source: &Source, state: &Rc<State>, slice: Arc<AtomicBool>) -> mlua::Result<Self> {
        let lua = Lua::new();
        // `client` changes under Luau: its fields, such as `client.opt.number`
        // and `client.windows`, are read through metatables each time. Luau
        // would otherwise read a global's fields once, as a chunk loads, and
        // keep what it read.
        lua.set_compiler(
            mlua::chunk::Compiler::new()
                .set_optimization_level(2)
                .set_mutable_globals(["client"]),
        );
        let watchdog = Watchdog::start(slice.clone());
        let stop = watchdog.stop.clone();
        let sliced: Rc<Cell<*mut mlua::lua_State>> = Rc::new(Cell::new(std::ptr::null_mut()));
        let (ending, coroutine) = (slice.clone(), sliced.clone());
        lua.set_interrupt(move |lua| {
            if stop.swap(false, Ordering::Relaxed) {
                return Err(mlua::Error::runtime("ran too long, so it was stopped"));
            }
            // Only the coroutine being run a slice at a time yields here,
            // compared by its Luau state, which is only compared: never a
            // coroutine of the handler's own, which would yield to the
            // handler, which would take it for its own `yield` and go
            // wrong. mlua yields only where Luau can, and otherwise lets
            // it run on; the watchdog sets `ending` again a slice later.
            if ending.load(Ordering::Relaxed) && lua.state() == coroutine.get() {
                ending.store(false, Ordering::Relaxed);
                return Ok(VmState::Yield);
            }
            Ok(VmState::Continue)
        });
        let core = core(&lua, state)?;
        let prelude: Table = lua.load(PRELUDE).set_name("=prelude").call(core)?;
        let globals = lua.globals();
        globals.set("client", prelude.get::<Table>("client")?)?;
        globals.set("require", require(&lua, source)?)?;
        globals.set(
            "print",
            lua.create_function(|_, values: MultiValue| {
                let words: Vec<String> = values
                    .iter()
                    .map(|value| value.to_string().unwrap_or_else(|_| "?".into()))
                    .collect();
                // Logged, so it reaches the window's log as well as
                // stderr: a window in the process of windows has no
                // stderr to see.
                log::info!(target: "clackatoo::init.lua", "{}", words.join("\t"));
                Ok(())
            })?,
        )?;
        lua.sandbox(true)?;
        Ok(Self {
            lua,
            state: state.clone(),
            prelude,
            waiting: HashMap::new(),
            background: VecDeque::new(),
            watchdog,
            sliced,
            slice,
            budget: HANDLER_TIME,
        })
    }

    /// Runs the config's `chunks` in turn, each in a coroutine of its own.
    /// One that waits, as `pane:split` does, loads on when its answer comes.
    /// A mistake in one stops only that one: what clackatoo saved doesn't
    /// keep the user's own config from running, nor the other way round.
    fn load(&mut self, chunks: &[Chunk]) {
        for chunk in chunks {
            let text = match &chunk.text {
                Ok(text) => text,
                Err(why) => {
                    let said = format!("couldn't read {}: {why}", chunk.path);
                    log::error!(target: "clackatoo::init.lua", "{said}");
                    self.state.tell(To::All, FromLua::Error(said));
                    continue;
                }
            };
            let thread = (self.lua.load(text))
                .set_name(format!("@{}", chunk.path))
                .into_function()
                .and_then(|function| self.lua.create_thread(function));
            let thread = match thread {
                Ok(thread) => thread,
                Err(err) => {
                    self.report(&err);
                    continue;
                }
            };
            let job = Job {
                loading: Some(chunk.from),
                ..Job::new(thread, MultiValue::new(), Origin::default())
            };
            self.run(job, INIT_TIME, false);
        }
    }

    fn handle(&mut self, message: ToLua) {
        match message {
            ToLua::Open { window } => {
                self.state
                    .worlds
                    .borrow_mut()
                    .insert(window, World::default());
            }
            ToLua::Closed { window } => {
                self.state.worlds.borrow_mut().remove(&window);
                (self.state.handlers.borrow_mut())
                    .retain(|_, (scope, _, _)| scope.window() != Some(window));
            }
            ToLua::World { window, world } => {
                // A pane's or tab's handlers go with it.
                (self.state.handlers.borrow_mut()).retain(|_, (scope, _, _)| match *scope {
                    Scope::Tab(of, tab) if of == window => world.workspaces.find_tab(tab).is_some(),
                    Scope::Pane(of, pane) if of == window => {
                        world.workspaces.find_pane(pane).is_some()
                    }
                    _ => true,
                });
                let mut worlds = self.state.worlds.borrow_mut();
                // Not one that's closed, as a world can cross with that.
                if let Some(was) = worlds.get_mut(&window) {
                    *was = world;
                }
            }
            ToLua::Key { seq, key, handlers } => {
                let handlers: Vec<Function> = {
                    let all = self.state.handlers.borrow();
                    (handlers.iter())
                        .filter_map(|id| all.get(id).map(|(_, _, handler)| handler.clone()))
                        .collect()
                };
                let stop = self.run_event(&key, false, &handlers);
                self.state
                    .tell(To::Window(key.window), FromLua::Decided { seq, stop });
            }
            ToLua::Bound { seq, key, binding } => {
                let handler = self.state.bindings.borrow().get(&binding).cloned();
                let stop = self.run_event(&key, true, handler.as_slice());
                self.state
                    .tell(To::Window(key.window), FromLua::Decided { seq, stop });
            }
            ToLua::Run { key, binding } => {
                let handler = self.state.bindings.borrow().get(&binding).cloned();
                if let Some(handler) = handler {
                    let origin = Origin {
                        window: Some(key.window),
                        guarded: key.guarded,
                    };
                    self.later(&handler, |runtime| runtime.decided_event(&key), origin);
                }
            }
            ToLua::Answer { request, result } => {
                let Some(mut job) = self.waiting.remove(&request) else {
                    return;
                };
                job.args = match result {
                    Ok(pane) => self.args(pane.0),
                    Err(err) => self.args((Value::Nil, err)),
                };
                // A new stretch of its running, from its answer.
                job.ran = Duration::ZERO;
                self.background.push_back(job);
            }
            ToLua::Option { option, value } => {
                // The windows' own value, so it suits the option.
                let _ = self.state.options.borrow_mut().set(option, value);
            }
            ToLua::TurnEnded(turn) => {
                let mut handlers: Vec<(u64, Function)> = (self.state.handlers.borrow().iter())
                    .filter(|(_, (_, listen, _))| *listen == Listen::TurnEnd)
                    .map(|(&id, (_, _, handler))| (id, handler.clone()))
                    .collect();
                // In the order they were added.
                handlers.sort_by_key(|&(id, _)| id);
                for (_, handler) in handlers {
                    let origin = Origin {
                        window: Some(turn.window),
                        guarded: turn.guarded,
                    };
                    self.later(&handler, |runtime| runtime.turn_event(&turn), origin);
                }
            }
        }
    }

    /// `values`, to resume a coroutine with.
    fn args(&self, values: impl mlua::IntoLuaMulti) -> MultiValue {
        values.into_lua_multi(&self.lua).unwrap_or_default()
    }

    /// Queues `handler` as background work, for the event from `origin`,
    /// called with the event `event` makes.
    fn later(
        &mut self,
        handler: &Function,
        event: impl FnOnce(&Self) -> mlua::Result<Table>,
        origin: Origin,
    ) {
        let made = event(self).and_then(|event| {
            let thread = self.lua.create_thread(handler.clone())?;
            Ok(Job::new(thread, self.args(event), origin))
        });
        match made {
            Ok(job) => self.background.push_back(job),
            Err(err) => self.report(&err),
        }
    }

    /// Runs `handlers` on an event for `key`, stopped to begin with or
    /// not, each until it finishes or waits, stopping at one that stops
    /// the event. Returns whether it was stopped.
    fn run_event(&mut self, key: &KeyInfo, stop: bool, handlers: &[Function]) -> bool {
        let event = match self.event(key, stop) {
            Ok(event) => event,
            Err(err) => {
                self.report(&err);
                return stop;
            }
        };
        let prelude = self.prelude.clone();
        let ask = |name: &str| -> bool {
            (prelude.get::<Function>(name))
                .and_then(|function| function.call::<bool>(&event))
                .unwrap_or(stop)
        };
        for handler in handlers {
            match self.lua.create_thread(handler.clone()) {
                Ok(thread) => {
                    let origin = Origin {
                        window: Some(key.window),
                        guarded: key.guarded,
                    };
                    let job = Job::new(thread, self.args(&event), origin);
                    self.run(job, self.budget, false);
                }
                Err(err) => self.report(&err),
            }
            if !stop && ask("stopped") {
                break;
            }
        }
        // From now on, stopping it or letting it go on is an error.
        ask("decide")
    }

    fn event(&self, key: &KeyInfo, stop: bool) -> mlua::Result<Table> {
        let info = self.lua.create_table()?;
        info.set("key", key.key.as_str())?;
        info.set("keys", key.keys.as_deref())?;
        info.set("text", key.text.as_deref())?;
        info.set("mode", mode_letter(key.mode))?;
        info.set("window", key.window)?;
        info.set("pane", key.pane.map(|pane| pane.0))?;
        info.set("count", key.count)?;
        let event: Function = self.prelude.get("event")?;
        event.call((info, stop))
    }

    /// An event for `key`, whose binding let it go on already: stopping it
    /// or letting it go on is an error.
    fn decided_event(&self, key: &KeyInfo) -> mlua::Result<Table> {
        let event = self.event(key, false)?;
        self.prelude
            .get::<Function>("decide")?
            .call::<bool>(&event)?;
        Ok(event)
    }

    fn turn_event(&self, turn: &TurnEnd) -> mlua::Result<Table> {
        let info = self.lua.create_table()?;
        info.set("chat", turn.chat.0)?;
        info.set("title", turn.title.as_str())?;
        info.set("agent", turn.agent.as_str())?;
        info.set("failed", turn.failed)?;
        info.set("window", turn.window)?;
        info.set("pane", turn.pane.map(|pane| pane.0))?;
        self.prelude.get::<Function>("turnend")?.call(info)
    }

    /// Runs `job` once, at most `limit` (for the config's files as they
    /// load) or what's left of [`HANDLER_TIME`], a slice at most if
    /// `sliced`, then keeps it as what it's come to: finished, waiting on
    /// a request, or to go on as background work.
    fn run(&mut self, mut job: Job, limit: Duration, sliced: bool) {
        // Not resumed at all once it's had its time, which a slice that
        // ran to the end of it, or a run of yields, can come to.
        self.state.window.set(job.origin.window);
        self.state.guarded.set(job.origin.guarded);
        if job.ran >= limit {
            self.report(&mlua::Error::runtime("ran too long, so it was stopped"));
            self.state.window.set(None);
            self.state.guarded.set(false);
            return;
        }
        self.state.loading.set(job.loading);
        if sliced {
            self.sliced.set(job.thread.state());
        }
        // Whatever asked a slice to end did so of one before.
        self.slice.store(false, Ordering::Relaxed);
        let args = std::mem::take(&mut job.args);
        let started = Instant::now();
        self.watchdog.watch(limit.saturating_sub(job.ran), sliced);
        let resumed = job.thread.resume::<MultiValue>(args);
        self.watchdog.done();
        job.ran += started.elapsed();
        self.sliced.set(std::ptr::null_mut());
        self.state.loading.set(None);
        self.state.window.set(None);
        self.state.guarded.set(false);
        self.settle(job, resumed);
    }

    /// Runs a slice of background work.
    fn run_slice(&mut self, job: Job) {
        self.run(job, self.budget, true);
    }

    /// After `job` was resumed: keeps it to resume again if it waits on a
    /// request, or as background work if it let others go first, and
    /// reports it if it failed.
    fn settle(&mut self, job: Job, resumed: mlua::Result<MultiValue>) {
        let values = match resumed {
            Ok(values) => values,
            Err(err) => return self.report(&err),
        };
        if job.thread.status() != ThreadStatus::Resumable {
            return;
        }
        let request = self.prelude.get::<Table>("request").ok();
        let mut values = values.into_iter();
        match (values.next(), values.next()) {
            (Some(Value::Table(first)), Some(Value::Integer(id)))
                if request.is_some_and(|request| request == first) =>
            {
                #[allow(clippy::cast_sign_loss)]
                self.waiting.insert(id as u64, job);
            }
            // A slice ended, or it called `coroutine.yield()` itself: it
            // goes on after the rest.
            (None, _) => self.background.push_back(job),
            _ => self.state.tell(
                self.state.here(To::All),
                FromLua::Error("a handler can only wait on the client, as pane:split does".into()),
            ),
        }
    }

    fn report(&self, err: &mlua::Error) {
        let text = err.to_string();
        log::error!(target: "clackatoo::init.lua", "{text}");
        let first = text.lines().next().unwrap_or_default();
        let first = (["runtime error: ", "syntax error: "].iter())
            .find_map(|kind| first.strip_prefix(kind))
            .unwrap_or(first);
        self.state
            .tell(self.state.here(To::All), FromLua::Error(first.to_owned()));
    }
}

fn mode_letter(mode: KeyMode) -> &'static str {
    match mode {
        KeyMode::Normal => "n",
        KeyMode::Insert => "i",
        KeyMode::Visual => "v",
        KeyMode::Prompt => "c",
    }
}

/// The functions the prelude builds the API from.
fn core(lua: &Lua, state: &Rc<State>) -> mlua::Result<Table> {
    let core = lua.create_table()?;
    listening(lua, state, &core)?;
    reading(lua, state, &core)?;
    acting(lua, state, &core)?;
    setting(lua, state, &core)?;
    Ok(core)
}
/// `get_option` and `set_option`, which `client.opt` reads and writes
/// through.
fn setting(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "get_option",
        lua.create_function(move |lua, name: String| {
            let option = options::find(&name).map_err(mlua::Error::runtime)?;
            // In a handler of a window's event, as that window has it, as
            // Neovim's `vim.o` reads the current window's.
            let own = (s.window.get()).and_then(|window| {
                let worlds = s.worlds.borrow();
                let world = worlds.get(&window)?;
                let own = world.own.iter().find(|(info, _)| info.name == option.name);
                own.map(|(_, value)| value.clone())
            });
            let value = own.unwrap_or_else(|| s.options.borrow().get(option));
            Ok(match value {
                options::Value::Bool(value) => Value::Boolean(value),
                options::Value::Number(value) => Value::Integer(value.into()),
                options::Value::Decimal(value) => Value::Number(value.as_f64()),
                options::Value::List(numbers) => Value::Table(lua.create_sequence_from(numbers)?),
                options::Value::Choice(word) => Value::String(lua.create_string(word)?),
                options::Value::Text(text) => Value::String(lua.create_string(text)?),
            })
        })?,
    )?;
    let s = state.clone();
    core.set(
        "set_option",
        lua.create_function(move |lua, (name, value): (String, Value)| {
            let set = || -> Result<_, String> {
                let option = options::find(&name)?;
                option.settable_now()?;
                let value = option_value(option, &value)?;
                s.options.borrow_mut().set(option, value.clone())?;
                Ok((option, value))
            };
            let by = s.set_by(lua);
            match set() {
                Ok((option, value)) => {
                    let window = s.window.get();
                    // As `:set` in that window: its own value goes, so
                    // what's read next is what was set.
                    if let Some(window) = window.filter(|_| !by.loading)
                        && let Some(world) = s.worlds.borrow_mut().get_mut(&window)
                    {
                        world.own.retain(|(info, _)| info.name != option.name);
                    }
                    s.tell(
                        To::Client,
                        FromLua::Set {
                            option,
                            value,
                            by,
                            window,
                        },
                    );
                    Ok(())
                }
                // settings.lua is clackatoo's, written for whichever build
                // wrote it: one that can't be set is said, and the rest
                // are set all the same.
                Err(err) if by.source == SetFrom::Settings && by.loading => {
                    let at = (by.at).map(|at| format!("{}:{}: ", at.file, at.line));
                    let said = format!("{}{err}", at.unwrap_or_default());
                    s.tell(To::All, FromLua::Error(said));
                    Ok(())
                }
                Err(err) => Err(mlua::Error::runtime(err)),
            }
        })?,
    )?;
    Ok(())
}

/// `value`, from Lua, as a value for `option`: true or false, or a whole
/// number, or for a decimal, any number to two places, or either as a
/// string. A list takes a table of numbers, a string as `:set` takes one
/// (`"80,120"`), or one number alone; a choice, one of its words; text, a
/// string.
fn option_value(option: &options::Info, value: &Value) -> Result<options::Value, String> {
    // Luau passes a whole number as an integer; anything else, such as
    // 2.5, isn't one.
    let number = |value: &Value| match value {
        Value::Integer(value) => u32::try_from(*value).ok(),
        _ => None,
    };
    let list = matches!(option.kind, options::Kind::List { .. });
    // A number can be a string, as `:set` writes it: `"10.5"`.
    let named = matches!(
        option.kind,
        options::Kind::Choice(_)
            | options::Kind::Text
            | options::Kind::Number { .. }
            | options::Kind::Decimal { .. }
    );
    let decimal = matches!(option.kind, options::Kind::Decimal { .. });
    let value = match value {
        Value::String(text) if named => return option.parse(&text.to_string_lossy()),
        Value::Integer(number) if decimal => u32::try_from(*number)
            .ok()
            .map(|number| options::Value::Decimal(options::Decimal::whole(number))),
        Value::Number(number) if decimal => {
            options::Decimal::exact(*number).map(options::Value::Decimal)
        }
        Value::Boolean(value) => Some(options::Value::Bool(*value)),
        Value::Integer(_) if list => number(value).map(|number| options::Value::List(vec![number])),
        Value::Integer(_) => number(value).map(options::Value::Number),
        Value::String(text) if list => return option.parse(&text.to_string_lossy()),
        Value::Table(table) if list => (table.sequence_values::<Value>())
            .map(|value| value.ok().as_ref().and_then(number))
            .collect::<Option<_>>()
            .map(options::Value::List),
        _ => None,
    };
    value.ok_or_else(|| option.wants())
}

/// What a binding's options say, as `map` was given them.
#[derive(Default)]
struct BindingOptions {
    /// What it does, for the panel of keys.
    desc: Option<String>,
    /// Its keys go on at once, and its function runs after.
    propagate: bool,
    /// What's wrong with the rest, to say.
    wrong: Vec<String>,
}

/// What a binding says it does, from the options `map` was given, as
/// Neovim's `vim.keymap.set` takes them: `{desc = "…"}`, and
/// `{propagate = true}` of clackatoo's own, and what's wrong with the
/// rest, each said for the binding of `keys`. Options that only say what
/// clackatoo's bindings do anyway, as `silent` and `noremap` do, are
/// passed over, so a binding written for Neovim loads as it is; any other
/// is left out with a warning, and the binding is made all the same, so
/// the rest of `init.lua` runs.
fn binding_options(keys: &str, options: Option<Table>) -> mlua::Result<BindingOptions> {
    let mut said = BindingOptions::default();
    let Some(options) = options else {
        return Ok(said);
    };
    let BindingOptions {
        desc,
        propagate,
        wrong,
    } = &mut said;
    for pair in options.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let name = key.as_string().map(mlua::LuaString::to_string_lossy);
        match (name.as_deref(), value) {
            (Some("desc"), Value::String(text)) => *desc = Some(text.to_string_lossy()),
            (Some("propagate"), Value::Boolean(on)) => *propagate = on,
            (Some("propagate"), _) => wrong.push(format!(
                "the binding of `{keys}` leaves out its `propagate`, which is true or false"
            )),
            // Neovim's, passed over: bindings here are always quiet and
            // never set off others, and the rest change nothing that
            // matters to a binding that loads.
            (Some("desc"), Value::Nil)
            | (Some("silent" | "noremap" | "nowait" | "unique"), _)
            | (Some("remap"), Value::Boolean(false) | Value::Nil) => {}
            (Some("desc"), _) => wrong.push(format!(
                "the binding of `{keys}` leaves out its `desc`, which is a string"
            )),
            (Some("remap"), _) => wrong.push(format!(
                "the binding of `{keys}` leaves out `remap`: keys bound to keys never set off other bindings"
            )),
            (Some(name), _) => wrong.push(format!(
                "the binding of `{keys}` leaves out `{name}`, which isn't one of its options: desc, propagate"
            )),
            (None, _) => wrong.push(format!(
                "the binding of `{keys}` leaves out options that aren't named, as {{desc = \"…\"}} is"
            )),
        }
    }
    Ok(said)
}

/// `map`, `unmap`, `group`, `on` and `off`.
#[allow(
    clippy::too_many_lines,
    reason = "one core function after another, each short"
)]
fn listening(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "map",
        lua.create_function(
            move |lua, (modes, keys, handler, options): (String, String, Value, Option<Table>)| {
                let by = s.set_by(lua);
                let modes = parse_modes(&modes).map_err(mlua::Error::runtime)?;
                let shown = keys.clone();
                let BindingOptions {
                    desc,
                    propagate,
                    mut wrong,
                } = binding_options(&keys, options)?;
                let keys = parse_keys(&keys).map_err(mlua::Error::runtime)?;
                // Said once the binding's made, which it is all the same.
                let warn = |s: &State, wrong: Vec<String>| {
                    for warning in wrong {
                        s.tell(s.here(To::All), FromLua::Error(warning));
                    }
                };
                let handler = match handler {
                    Value::Function(handler) => handler,
                    // Keys to type, as Vim's `noremap` maps to.
                    Value::String(to) => {
                        let to = parse_keys(&to.to_string_lossy()).map_err(mlua::Error::runtime)?;
                        if propagate {
                            wrong.push(format!(
                                "the binding of `{shown}` leaves out `propagate`: keys bound to \
                                 keys are typed in place of them"
                            ));
                        }
                        s.tell(
                            To::Client,
                            FromLua::Remap {
                                modes,
                                keys,
                                to,
                                desc,
                                by,
                            },
                        );
                        warn(&s, wrong);
                        return Ok(());
                    }
                    _ => {
                        return Err(mlua::Error::runtime(
                            "keys are bound to a function, or to keys to type, as a string",
                        ));
                    }
                };
                let binding = s.next_id();
                s.bindings.borrow_mut().insert(binding, handler);
                s.tell(
                    To::Client,
                    FromLua::Map {
                        modes,
                        keys,
                        binding,
                        desc,
                        propagate,
                        by,
                    },
                );
                warn(&s, wrong);
                Ok(())
            },
        )?,
    )?;
    let s = state.clone();
    core.set(
        "unmap",
        lua.create_function(move |lua, (modes, keys): (String, String)| {
            let modes = parse_modes(&modes).map_err(mlua::Error::runtime)?;
            let keys = parse_keys(&keys).map_err(mlua::Error::runtime)?;
            let by = s.set_by(lua);
            s.tell(To::Client, FromLua::Unmap { modes, keys, by });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "group",
        lua.create_function(
            move |_, (modes, keys, name): (String, String, Option<String>)| {
                let modes = parse_modes(&modes).map_err(mlua::Error::runtime)?;
                let keys = parse_keys(&keys).map_err(mlua::Error::runtime)?;
                s.tell(To::Client, FromLua::Group { modes, keys, name });
                Ok(())
            },
        )?,
    )?;
    let s = state.clone();
    core.set(
        "on",
        lua.create_function(
            move |_, (kind, window, id, event, handler): (String, u32, u32, String, Function)| {
                let listen = match event.as_str() {
                    "keypress" => Listen::Keypress,
                    "turnend" => Listen::TurnEnd,
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "there's no event called `{event}`: the events are keypress and turnend"
                        )));
                    }
                };
                let scope = match kind.as_str() {
                    "client" => Scope::Client,
                    "window" => Scope::Window(window),
                    "tab" => Scope::Tab(window, TabId(id)),
                    _ => Scope::Pane(window, PaneId(id)),
                };
                if listen == Listen::TurnEnd && scope != Scope::Client {
                    return Err(mlua::Error::runtime(
                        "turnend is heard with client.on alone: a chat's turn ends for every window",
                    ));
                }
                let handler_id = s.next_id();
                (s.handlers.borrow_mut()).insert(handler_id, (scope, listen, handler));
                s.tell(
                    To::Client,
                    FromLua::Subscribe {
                        handler: handler_id,
                        scope,
                        listen,
                    },
                );
                Ok(handler_id)
            },
        )?,
    )?;
    let s = state.clone();
    core.set(
        "off",
        lua.create_function(move |_, handler: u64| {
            if s.handlers.borrow_mut().remove(&handler).is_some() {
                s.tell(To::Client, FromLua::Unsubscribe { handler });
            }
            Ok(())
        })?,
    )?;
    Ok(())
}

/// `info`, `windows` and `focused`.
fn reading(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "info",
        lua.create_function(move |lua, (kind, window, id): (String, u32, u32)| {
            match s.worlds.borrow().get(&window) {
                Some(world) => info(lua, world, &kind, window, id),
                None => Ok(None),
            }
        })?,
    )?;
    let s = state.clone();
    core.set(
        "windows",
        lua.create_function(move |_, ()| {
            Ok(s.worlds.borrow().keys().copied().collect::<Vec<u32>>())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "focused",
        // The pane shown in the window with the desktop's focus, or else
        // the first window's.
        lua.create_function(move |_, ()| {
            let worlds = s.worlds.borrow();
            let window =
                (worlds.iter().find(|(_, world)| world.focused)).or_else(|| worlds.iter().next());
            Ok(match window {
                Some((&window, world)) => (Some(window), world.shown.map(|shown| shown.pane.0)),
                None => (None, None),
            })
        })?,
    )?;
    Ok(())
}

/// `split`, `close`, `focus`, `command`, `forward`, `connect` and
/// `disconnect`.
#[allow(
    clippy::too_many_lines,
    reason = "one core function after another, each short"
)]
fn acting(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "split",
        lua.create_function(
            move |_, (window, pane, side, kind): (u32, u32, String, String)| {
                let side = match side.as_str() {
                    "right" => Side::Right,
                    "left" => Side::Left,
                    "below" => Side::Below,
                    "above" => Side::Above,
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "a pane splits to the right, left, below or above, not `{side}`"
                        )));
                    }
                };
                let kind = match kind.as_str() {
                    "terminal" => Kind::Terminal,
                    "chat" => Kind::Agent,
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "a pane is a terminal or a chat, not `{kind}`"
                        )));
                    }
                };
                let request = next_request();
                s.tell(
                    To::Window(window),
                    FromLua::Split {
                        request,
                        pane: PaneId(pane),
                        side,
                        kind,
                    },
                );
                Ok(request)
            },
        )?,
    )?;
    let s = state.clone();
    core.set(
        "close",
        lua.create_function(move |_, (window, pane): (u32, u32)| {
            s.tell(To::Window(window), FromLua::Close { pane: PaneId(pane) });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "focus",
        lua.create_function(move |_, (window, pane): (u32, u32)| {
            s.tell(To::Window(window), FromLua::Focus { pane: PaneId(pane) });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "command",
        lua.create_function(move |lua, line: String| {
            let by = s.set_by(lua);
            s.tell(s.here(To::Focused), FromLua::Command { line, by });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "forward",
        lua.create_function(move |_, spec: String| {
            let spec = spec.parse().map_err(|why: String| {
                mlua::Error::runtime(format!(
                    "{why}: client.forward takes PORT, LOCAL:PORT or LOCAL:HOST:PORT"
                ))
            })?;
            s.tell(To::All, FromLua::Forward(spec));
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "connect",
        lua.create_function(move |_, (host, options): (String, Option<Table>)| {
            let host = machine(&host, "connect")?;
            let share = match options.map(|options| options.get::<mlua::Value>("share")) {
                None | Some(Ok(mlua::Value::Nil)) => None,
                Some(Ok(mlua::Value::Boolean(share))) => Some(share),
                Some(_) => {
                    return Err(mlua::Error::runtime(
                        "client.connect's share is true or false: client.connect(\"server\", \
                         {share = false})",
                    ));
                }
            };
            s.tell(To::All, FromLua::Connect { host, share });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "disconnect",
        lua.create_function(move |_, host: String| {
            s.tell(To::All, FromLua::Disconnect(machine(&host, "disconnect")?));
            Ok(())
        })?,
    )?;
    Ok(())
}

/// `host`, a machine `client.connect` or `client.disconnect`, `name`,
/// was given, if it can be one: one word.
fn machine(host: &str, name: &str) -> mlua::Result<String> {
    let host = host.trim();
    if host.is_empty() || host.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(mlua::Error::runtime(format!(
            "client.{name} takes a machine, as ssh names it: \"desk\", \"jack@desk\""
        )));
    }
    Ok(host.to_owned())
}

/// What Lua reads of a window, or a workspace, tab or pane through
/// `window`, whose world is `world`, or nil if it's gone.
fn info(lua: &Lua, world: &World, kind: &str, window: u32, id: u32) -> mlua::Result<Option<Table>> {
    let table = lua.create_table()?;
    let workspaces = &world.workspaces;
    match kind {
        "window" => {
            if id != window {
                return Ok(None);
            }
            let shown = world.shown;
            table.set("workspace", shown.map(|shown| shown.workspace.0))?;
            table.set("tab", shown.map(|shown| shown.tab.0))?;
            table.set("pane", shown.map(|shown| shown.pane.0))?;
        }
        "workspace" => {
            let Some(workspace) = workspaces.get(WorkspaceId(id)) else {
                return Ok(None);
            };
            table.set("name", workspace.name.as_str())?;
            let tabs: Vec<u32> = workspace.tabs.iter().map(|tab| tab.id.0).collect();
            table.set("tabs", tabs)?;
        }
        "tab" => {
            let Some((workspace, tab)) = workspaces.find_tab(TabId(id)) else {
                return Ok(None);
            };
            table.set("name", tab.name.as_deref())?;
            let panes: Vec<u32> = tab.layout.panes().iter().map(|pane| pane.id.0).collect();
            table.set("panes", panes)?;
            table.set("focused", tab.focused.0)?;
            table.set("workspace", workspace.id.0)?;
        }
        _ => {
            let Some((_, tab, pane)) = workspaces.find_pane(PaneId(id)) else {
                return Ok(None);
            };
            let known = world.panes.get(&pane.id);
            let kind = match pane.content {
                Content::Buffer(_) => "editor",
                Content::Files(_) => "explorer",
                Content::Folder(_) => "folder",
                Content::Board(_) => "board",
                Content::Review(_) => "review",
                Content::Browser(_) => "browser",
                Content::Picture(_) => "image",
                Content::Other { .. } => "other",
                Content::Terminal(_) if known.is_some_and(|known| known.chat) => "chat",
                Content::Terminal(_) => "terminal",
            };
            table.set("kind", kind)?;
            table.set("title", known.and_then(|known| known.title.as_deref()))?;
            table.set("tab", tab.id.0)?;
        }
    }
    Ok(Some(table))
}

/// `require`, finding modules in `client/lua/` and then the shared `lua/`,
/// a dot in a module's name being a directory.
fn require(lua: &Lua, source: &Source) -> mlua::Result<Function> {
    let dirs = match source {
        Source::Dir(dir) => vec![dir.join("client/lua"), dir.join("lua")],
        #[cfg(test)]
        Source::Text(_) => Vec::new(),
    };
    let loaded = lua.create_table()?;
    lua.create_function(move |lua, name: String| {
        if let Some(module) = loaded.get::<Option<Value>>(name.as_str())? {
            return Ok(module);
        }
        let relative = name.replace('.', "/");
        let found = dirs.iter().find_map(|dir| {
            [format!("{relative}.lua"), format!("{relative}/init.lua")]
                .into_iter()
                .map(|file| dir.join(file))
                .find_map(|path| Some((std::fs::read_to_string(&path).ok()?, path)))
        });
        let Some((text, path)) = found else {
            return Err(mlua::Error::runtime(format!(
                "there's no module `{name}` in client/lua/ or lua/"
            )));
        };
        let module: Value = (lua.load(text))
            .set_name(format!("@{}", path.display()))
            .call(())?;
        // A module that returns nothing is loaded all the same.
        let module = if module.is_nil() {
            Value::Boolean(true)
        } else {
            module
        };
        loaded.set(name, &module)?;
        Ok(module)
    })
}

/// Stops Lua that runs too long, and ends background work's slices,
/// waking only while Lua runs.
struct Watchdog {
    watch: Mutex<Watch>,
    changed: Condvar,
    /// Set when it's run too long, for the interrupt to see.
    stop: Arc<AtomicBool>,
    /// Set when a slice has run long enough, for the interrupt to see.
    slice: Arc<AtomicBool>,
}

impl Watchdog {
    fn start(slice: Arc<AtomicBool>) -> Arc<Self> {
        let watchdog = Arc::new(Self {
            watch: Mutex::new(Watch::default()),
            changed: Condvar::new(),
            stop: Arc::new(AtomicBool::new(false)),
            slice,
        });
        let watching = watchdog.clone();
        let spawned = std::thread::Builder::new()
            .name("lua-watchdog".into())
            .spawn(move || watching.run());
        if let Err(err) = spawned {
            log::warn!("couldn't start Lua's watchdog: {err}");
        }
        watchdog
    }

    fn run(&self) {
        let mut watch = self.watch.lock().expect("never poisoned");
        while !watch.closed {
            let now = Instant::now();
            if watch.deadline.is_some_and(|deadline| now >= deadline) {
                self.stop.store(true, Ordering::Relaxed);
                watch.deadline = None;
                watch.slice = None;
            }
            // Asked again each slice it runs on, in case it couldn't
            // yield where it was asked.
            if let Some(slice) = watch.slice
                && now >= slice
            {
                self.slice.store(true, Ordering::Relaxed);
                watch.slice = Some(now + SLICE);
            }
            watch = match watch.deadline.into_iter().chain(watch.slice).min() {
                None => self.changed.wait(watch).expect("never poisoned"),
                Some(next) => {
                    (self.changed)
                        .wait_timeout(watch, next.saturating_duration_since(now))
                        .expect("never poisoned")
                        .0
                }
            };
        }
    }

    fn set(&self, change: impl FnOnce(&mut Watch)) {
        change(&mut self.watch.lock().expect("never poisoned"));
        self.changed.notify_one();
    }

    /// Watches a run that may go on for `limit`, and if `sliced`, ends
    /// its slice after [`SLICE`].
    fn watch(&self, limit: Duration, sliced: bool) {
        self.stop.store(false, Ordering::Relaxed);
        let now = Instant::now();
        self.set(|watch| {
            watch.deadline = Some(now + limit);
            watch.slice = sliced.then_some(now + SLICE);
        });
    }

    /// The run being watched has ended. A stop it asked for too late to
    /// stop it is taken back, under the lock the watchdog asks it under,
    /// so it never stops what runs next unwatched, as the prelude's calls
    /// do, nor the next handler.
    fn done(&self) {
        self.set(|watch| {
            watch.deadline = None;
            watch.slice = None;
            self.stop.store(false, Ordering::Relaxed);
        });
    }
}

/// What the watchdog watches.
#[derive(Default)]
struct Watch {
    /// When the run being watched must end by, if one is.
    deadline: Option<Instant>,
    /// When its slice ends, if it's run a slice at a time.
    slice: Option<Instant>,
    /// Lua has gone, so the watchdog goes too.
    closed: bool,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.watchdog.set(|watch| watch.closed = true);
    }
}

#[cfg(test)]
mod tests;
