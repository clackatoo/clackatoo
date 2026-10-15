//! The Lua thread: Luau, the client's API, and handlers run as coroutines.
//!
//! Each handler runs in a coroutine of its own until it finishes or first
//! waits, as `pane:split` does for the daemon's answer. Then the next one
//! runs, and once they all have, whether the key stopped is decided. A
//! waiting coroutine is resumed when its answer comes.
//!
//! A resume that runs too long, as a loop that never ends would, is
//! stopped by a watchdog thread, which only wakes while Lua runs.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clackatoo_protocol::{Content, PaneId, Side, TabId, WorkspaceId};
use mlua::thread::ThreadStatus;
use mlua::{Function, Lua, MultiValue, Table, Thread, Value, VmState};

use super::{FromLua, KeyInfo, Scope, Source, ToLua, WINDOW, Wake, World};
use crate::command::Kind;
use crate::keymap::{KeyMode, parse_keys, parse_modes};
use crate::options::{self, Options};

/// How long `init.lua` may run before it's stopped.
pub(crate) const INIT_TIME: Duration = Duration::from_secs(2);

/// How long a handler may run, each time it's resumed, before it's
/// stopped. Less than the time a key waits for it.
const HANDLER_TIME: Duration = Duration::from_millis(250);

const PRELUDE: &str = include_str!("prelude.luau");

/// What the core functions share.
struct State {
    world: RefCell<World>,
    /// The options, as the window last said, and as set since.
    options: RefCell<Options>,
    to_window: Sender<FromLua>,
    wake: Wake,
    bindings: RefCell<HashMap<u64, Function>>,
    handlers: RefCell<HashMap<u64, (Scope, Function)>>,
    next: Cell<u64>,
}

impl State {
    fn tell(&self, message: FromLua) {
        if self.to_window.send(message).is_ok() {
            (self.wake)();
        }
    }

    fn next_id(&self) -> u64 {
        let id = self.next.get() + 1;
        self.next.set(id);
        id
    }
}

/// Runs `init.lua`, whose path and text are `init`, with `options` as the
/// window's are, then whatever the window asks, until the window lets go
/// of its end.
pub(super) fn run(
    source: &Source,
    (path, text): (&str, &str),
    options: Options,
    (from_window, to_window): (&Receiver<ToLua>, Sender<FromLua>),
    wake: Wake,
) {
    let state = Rc::new(State {
        world: RefCell::default(),
        options: RefCell::new(options),
        to_window,
        wake,
        bindings: RefCell::default(),
        handlers: RefCell::default(),
        next: Cell::new(0),
    });
    let mut runtime = match Runtime::new(source, &state) {
        Ok(runtime) => runtime,
        Err(err) => {
            state.tell(FromLua::Error(format!("Lua didn't start: {err}")));
            state.tell(FromLua::Ready);
            return;
        }
    };
    match runtime
        .lua
        .load(text)
        .set_name(format!("@{path}"))
        .into_function()
    {
        Ok(init) => runtime.start(&init, (), INIT_TIME),
        Err(err) => runtime.report(&err),
    }
    state.tell(FromLua::Ready);
    while let Ok(message) = from_window.recv() {
        runtime.handle(message);
    }
}

/// The Lua thread's Luau, and what it's waiting on.
struct Runtime {
    lua: Lua,
    state: Rc<State>,
    /// What the prelude gives the runtime: `request`, `event`, `decide`.
    prelude: Table,
    /// Coroutines waiting on requests, by request.
    waiting: HashMap<u64, Thread>,
    watchdog: Arc<Watchdog>,
}

impl Runtime {
    fn new(source: &Source, state: &Rc<State>) -> mlua::Result<Self> {
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
        let watchdog = Watchdog::start();
        let stop = watchdog.stop.clone();
        lua.set_interrupt(move |_| {
            if stop.swap(false, Ordering::Relaxed) {
                Err(mlua::Error::runtime("ran too long, so it was stopped"))
            } else {
                Ok(VmState::Continue)
            }
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
                eprintln!("clackatoo: init.lua: {}", words.join("\t"));
                Ok(())
            })?,
        )?;
        lua.sandbox(true)?;
        Ok(Self {
            lua,
            state: state.clone(),
            prelude,
            waiting: HashMap::new(),
            watchdog,
        })
    }

    fn handle(&mut self, message: ToLua) {
        match message {
            ToLua::World(world) => {
                // A pane's or tab's handlers go with it.
                self.state
                    .handlers
                    .borrow_mut()
                    .retain(|_, (scope, _)| match *scope {
                        Scope::Client | Scope::Window(_) => true,
                        Scope::Tab(tab) => world.workspaces.find_tab(tab).is_some(),
                        Scope::Pane(pane) => world.workspaces.find_pane(pane).is_some(),
                    });
                *self.state.world.borrow_mut() = world;
            }
            ToLua::Key { seq, key, handlers } => {
                let handlers: Vec<Function> = {
                    let all = self.state.handlers.borrow();
                    (handlers.iter())
                        .filter_map(|id| all.get(id).map(|(_, handler)| handler.clone()))
                        .collect()
                };
                let stop = self.run_event(&key, false, &handlers);
                self.state.tell(FromLua::Decided { seq, stop });
            }
            ToLua::Bound { seq, key, binding } => {
                let handler = self.state.bindings.borrow().get(&binding).cloned();
                let stop = self.run_event(&key, true, handler.as_slice());
                self.state.tell(FromLua::Decided { seq, stop });
            }
            ToLua::Answer { request, result } => {
                let Some(thread) = self.waiting.remove(&request) else {
                    return;
                };
                let resumed = match result {
                    Ok(pane) => self.watched(HANDLER_TIME, || thread.resume(pane.0)),
                    Err(err) => self.watched(HANDLER_TIME, || thread.resume((Value::Nil, err))),
                };
                self.settle(&thread, resumed);
            }
            ToLua::Option { option, value } => {
                // The window's own value, so it suits the option.
                let _ = self.state.options.borrow_mut().set(option, value);
            }
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
            self.start(handler, &event, HANDLER_TIME);
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
        info.set("window", WINDOW)?;
        info.set("pane", key.pane.map(|pane| pane.0))?;
        let event: Function = self.prelude.get("event")?;
        event.call((info, stop))
    }

    /// Runs `function` with `args` in a coroutine of its own, until it
    /// finishes or waits.
    fn start(&mut self, function: &Function, args: impl mlua::IntoLuaMulti, limit: Duration) {
        let thread = match self.lua.create_thread(function.clone()) {
            Ok(thread) => thread,
            Err(err) => return self.report(&err),
        };
        let resumed = self.watched(limit, || thread.resume(args));
        self.settle(&thread, resumed);
    }

    /// Runs `resume` with the watchdog watching.
    fn watched(
        &self,
        limit: Duration,
        resume: impl FnOnce() -> mlua::Result<MultiValue>,
    ) -> mlua::Result<MultiValue> {
        self.watchdog.watch(limit);
        let resumed = resume();
        self.watchdog.done();
        resumed
    }

    /// After `thread` was resumed: keeps it to resume again if it waits on
    /// a request, and reports it if it failed.
    fn settle(&mut self, thread: &Thread, resumed: mlua::Result<MultiValue>) {
        let values = match resumed {
            Ok(values) => values,
            Err(err) => return self.report(&err),
        };
        if thread.status() != ThreadStatus::Resumable {
            return;
        }
        let request = self.prelude.get::<Table>("request").ok();
        let mut values = values.into_iter();
        match (values.next(), values.next()) {
            (Some(Value::Table(first)), Some(Value::Integer(id)))
                if request.is_some_and(|request| request == first) =>
            {
                #[allow(clippy::cast_sign_loss)]
                self.waiting.insert(id as u64, thread.clone());
            }
            _ => self.state.tell(FromLua::Error(
                "a handler can only wait on the client, as pane:split does".into(),
            )),
        }
    }

    fn report(&self, err: &mlua::Error) {
        let text = err.to_string();
        eprintln!("clackatoo: {text}");
        let first = text.lines().next().unwrap_or_default();
        let first = (["runtime error: ", "syntax error: "].iter())
            .find_map(|kind| first.strip_prefix(kind))
            .unwrap_or(first);
        self.state.tell(FromLua::Error(first.to_owned()));
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
            Ok(match s.options.borrow().get(option) {
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
        lua.create_function(move |_, (name, value): (String, Value)| {
            let option = options::find(&name).map_err(mlua::Error::runtime)?;
            let value = option_value(option, &value).map_err(mlua::Error::runtime)?;
            s.options
                .borrow_mut()
                .set(option, value.clone())
                .map_err(mlua::Error::runtime)?;
            s.tell(FromLua::Set { option, value });
            Ok(())
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

/// `map`, `unmap`, `on` and `off`.
fn listening(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "map",
        lua.create_function(move |_, (modes, keys, handler): (String, String, Value)| {
            let modes = parse_modes(&modes).map_err(mlua::Error::runtime)?;
            let keys = parse_keys(&keys).map_err(mlua::Error::runtime)?;
            let handler = match handler {
                Value::Function(handler) => handler,
                // Keys to type, as Vim's `noremap` maps to.
                Value::String(to) => {
                    let to = parse_keys(&to.to_string_lossy()).map_err(mlua::Error::runtime)?;
                    s.tell(FromLua::Remap { modes, keys, to });
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
            s.tell(FromLua::Map {
                modes,
                keys,
                binding,
            });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "unmap",
        lua.create_function(move |_, (modes, keys): (String, String)| {
            let modes = parse_modes(&modes).map_err(mlua::Error::runtime)?;
            let keys = parse_keys(&keys).map_err(mlua::Error::runtime)?;
            s.tell(FromLua::Unmap { modes, keys });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "on",
        lua.create_function(
            move |_, (kind, id, event, handler): (String, u64, String, Function)| {
                if event != "keypress" {
                    return Err(mlua::Error::runtime(format!(
                        "there's no event called `{event}`: the events are keypress"
                    )));
                }
                #[allow(clippy::cast_possible_truncation)]
                let scope = match kind.as_str() {
                    "client" => Scope::Client,
                    "window" => Scope::Window(id as u32),
                    "tab" => Scope::Tab(TabId(id as u32)),
                    _ => Scope::Pane(PaneId(id as u32)),
                };
                let handler_id = s.next_id();
                s.handlers.borrow_mut().insert(handler_id, (scope, handler));
                s.tell(FromLua::Subscribe {
                    handler: handler_id,
                    scope,
                });
                Ok(handler_id)
            },
        )?,
    )?;
    let s = state.clone();
    core.set(
        "off",
        lua.create_function(move |_, handler: u64| {
            if s.handlers.borrow_mut().remove(&handler).is_some() {
                s.tell(FromLua::Unsubscribe { handler });
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
        lua.create_function(move |lua, (kind, id): (String, u32)| {
            info(lua, &s.world.borrow(), &kind, id)
        })?,
    )?;
    let s = state.clone();
    core.set(
        "windows",
        lua.create_function(move |_, ()| Ok(vec![WINDOW]))?,
    )?;
    core.set(
        "focused",
        lua.create_function(move |_, ()| Ok(s.world.borrow().shown.map(|shown| shown.pane.0)))?,
    )?;
    Ok(())
}

/// `split`, `close` and `focus`.
fn acting(lua: &Lua, state: &Rc<State>, core: &Table) -> mlua::Result<()> {
    let s = state.clone();
    core.set(
        "split",
        lua.create_function(move |_, (pane, side, kind): (u32, String, String)| {
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
            let request = s.next_id();
            s.tell(FromLua::Split {
                request,
                pane: PaneId(pane),
                side,
                kind,
            });
            Ok(request)
        })?,
    )?;
    let s = state.clone();
    core.set(
        "close",
        lua.create_function(move |_, pane: u32| {
            s.tell(FromLua::Close { pane: PaneId(pane) });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "focus",
        lua.create_function(move |_, pane: u32| {
            s.tell(FromLua::Focus { pane: PaneId(pane) });
            Ok(())
        })?,
    )?;
    let s = state.clone();
    core.set(
        "command",
        lua.create_function(move |_, line: String| {
            s.tell(FromLua::Command(line));
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
            s.tell(FromLua::Forward(spec));
            Ok(())
        })?,
    )?;
    Ok(())
}

/// What Lua reads of a window, workspace, tab or pane, or nil if it's gone.
fn info(lua: &Lua, world: &World, kind: &str, id: u32) -> mlua::Result<Option<Table>> {
    let table = lua.create_table()?;
    let workspaces = &world.workspaces;
    match kind {
        "window" => {
            if id != WINDOW {
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

/// Stops Lua that runs too long, waking only while it runs.
struct Watchdog {
    watch: Mutex<Watch>,
    changed: Condvar,
    /// Set when it's run too long, for the interrupt to see.
    stop: Arc<AtomicBool>,
}

impl Watchdog {
    fn start() -> Arc<Self> {
        let watchdog = Arc::new(Self {
            watch: Mutex::new(Watch::default()),
            changed: Condvar::new(),
            stop: Arc::new(AtomicBool::new(false)),
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
            watch = match watch.deadline {
                None => self.changed.wait(watch).expect("never poisoned"),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.stop.store(true, Ordering::Relaxed);
                        watch.deadline = None;
                        watch
                    } else {
                        (self.changed)
                            .wait_timeout(watch, deadline - now)
                            .expect("never poisoned")
                            .0
                    }
                }
            };
        }
    }

    fn set(&self, change: impl FnOnce(&mut Watch)) {
        change(&mut self.watch.lock().expect("never poisoned"));
        self.changed.notify_one();
    }

    fn watch(&self, limit: Duration) {
        self.stop.store(false, Ordering::Relaxed);
        self.set(|watch| watch.deadline = Some(Instant::now() + limit));
    }

    fn done(&self) {
        self.set(|watch| watch.deadline = None);
    }
}

/// What the watchdog watches.
#[derive(Default)]
struct Watch {
    /// When the run being watched must end by, if one is.
    deadline: Option<Instant>,
    /// Lua has gone, so the watchdog goes too.
    closed: bool,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.watchdog.set(|watch| watch.closed = true);
    }
}
