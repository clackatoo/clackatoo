//! Runs the daemon's config, in Luau: `daemon/settings.lua`, then
//! `daemon/init.lua`, in a sandbox, for what they set.
//!
//! The sandbox has no `io` or `os`, nor anything that loads code but
//! `require`, which finds modules only in `daemon/lua/` and `lua/`, by
//! names of letters, digits, `_` and `-`, with dots for folders. Every
//! table it starts with is read-only. A run may take [`TIME`], call or
//! loop [`MOST_STEPS`] times and hold [`MEMORY`], or it's stopped.
//!
//! It runs on a thread of its own, never the daemon's loop: what it
//! gives back is plain data, [`Settings`].

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mlua::chunk::ChunkMode;
use mlua::{Lua, LuaOptions, StdLib, Table, Value as LuaValue, VmState};

use super::options::{self, ALL};
use super::{Failure, Settings};
use crate::agents::CustomAgent;
use crate::profiles::{Change, Limits};
use crate::schema::{Info, Kind, Place, SetBy, Source, Value};

/// The longest a run of the config may take as the daemon starts.
pub(crate) const TIME: Duration = Duration::from_millis(500);

/// The longest it may take as the daemon upgrades, or as a build is asked
/// what it takes: the build before waits on it, and the terminals with it.
pub(crate) const UPGRADE_TIME: Duration = Duration::from_millis(200);

/// What an agent's table, `daemon.agents.NAME`, may have.
pub(crate) const AGENT_FIELDS: &[&str] = &["name", "command", "mode", "tasks"];

/// What a profile's table, `daemon.profiles.NAME`, may have.
pub(crate) const PROFILE_FIELDS: &[&str] =
    &["about", "prompt", "limits", "agent", "model", "effort"];

/// The most times a run may call a function or loop back, as Luau's
/// interrupt counts them: far beyond any config's, and short of [`TIME`]
/// on a slow machine.
pub(crate) const MOST_STEPS: u64 = 20_000_000;

/// The most memory a run may hold.
pub(crate) const MEMORY: usize = 64 << 20;

/// The largest file of config, or module, read.
pub(crate) const MOST_FILE: u64 = 1 << 20;

/// What may be in the sandbox's globals, and nothing else: Luau's own,
/// less what loads code or reaches the machine, and the config's.
#[cfg(test)]
pub(crate) const GLOBALS: &[&str] = &[
    "_G",
    "_VERSION",
    "assert",
    "bit32",
    "buffer",
    "collectgarbage",
    "coroutine",
    "daemon",
    "error",
    "gcinfo",
    "getmetatable",
    "ipairs",
    "math",
    "newproxy",
    "next",
    "pairs",
    "pcall",
    "print",
    "rawequal",
    "rawget",
    "rawlen",
    "rawset",
    "require",
    "select",
    "setmetatable",
    "string",
    "table",
    "tonumber",
    "tostring",
    "type",
    "typeof",
    "unpack",
    "utf8",
    "vector",
    "xpcall",
];

/// A file of config to run.
#[derive(Clone, Debug)]
pub(crate) struct Chunk {
    /// Where it is, as its lines are said.
    pub(crate) path: PathBuf,
    pub(crate) text: String,
    /// Whose it is: clackatoo's `settings.lua`, or the user's own.
    pub(crate) source: Source,
}

/// What a run keeps, as the config sets it.
struct State {
    options: RefCell<BTreeMap<&'static str, (Value, SetBy)>>,
    /// The agents and profiles set, in the order first set, with what set
    /// each last. Their tables are in the Lua's own store.
    agents: RefCell<Vec<(String, SetBy)>>,
    profiles: RefCell<Vec<(String, SetBy)>>,
    /// Whose file is running.
    source: Cell<Source>,
    files: RefCell<Vec<Arc<str>>>,
    /// What went wrong in the API, as it's said, with where: each time,
    /// as `pcall` may have caught some, and the run gone on.
    failed: RefCell<Vec<Failure>>,
}

impl State {
    fn file(&self, file: &str) -> Arc<str> {
        let mut files = self.files.borrow_mut();
        if let Some(kept) = files.iter().find(|kept| ***kept == *file) {
            return kept.clone();
        }
        let kept: Arc<str> = file.into();
        files.push(kept.clone());
        kept
    }

    /// What's setting something now: the file running, and the lines of
    /// config on the stack, from Luau's debug info, the outermost being
    /// the user's own line and the innermost where it was really set.
    fn set_by(&self, lua: &Lua) -> SetBy {
        /// Deeper than any config's calls go, to stop at.
        const DEEPEST: usize = 200;
        let mut inner = None;
        let mut at = None;
        for level in 0..DEEPEST {
            let Some(frame) = lua.inspect_stack(level, |debug| {
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
        SetBy {
            source: self.source.get(),
            loading: true,
            at,
            inner,
        }
    }

    /// Fails the run with `said`, at the line setting something now.
    fn fail(&self, lua: &Lua, said: String) -> mlua::Error {
        let at = self.set_by(lua).at;
        self.fail_at(at, said)
    }

    fn fail_at(&self, at: Option<Place>, said: String) -> mlua::Error {
        let said = match &at {
            Some(at) => format!("{}:{}: {said}", at.file, at.line),
            None => said,
        };
        let error = mlua::Error::runtime(said.clone());
        self.failed.borrow_mut().push(Failure { said, at });
        error
    }
}

/// Runs `chunks` in turn, each finding modules in `dirs`, taking `limit`
/// at most in all, for what they set: or what went wrong, which stopped
/// it.
pub(crate) fn run(
    chunks: &[Chunk],
    dirs: &[PathBuf],
    limit: Duration,
) -> Result<Settings, Failure> {
    let state = Rc::new(State {
        options: RefCell::default(),
        agents: RefCell::default(),
        profiles: RefCell::default(),
        source: Cell::new(Source::Settings),
        files: RefCell::default(),
        failed: RefCell::default(),
    });
    let (lua, store) = sandbox(&state, dirs, limit).map_err(|err| Failure {
        said: format!("the daemon's config couldn't start: {err}"),
        at: None,
    })?;
    for chunk in chunks {
        state.source.set(chunk.source);
        let ran = (lua.load(&chunk.text))
            .set_name(format!("@{}", chunk.path.display()))
            .set_mode(ChunkMode::Text)
            .exec();
        if let Err(err) = ran {
            return Err(ended_by(&state, &err));
        }
    }
    let settings = gather(&state, &store);
    // What's checked once everything's set fails at the line that set it.
    settings.and_then(|settings| settings.checked().map(|()| settings))
}

/// What ended a run with `err`: what the API said of it, with where, if
/// it was the API's, not one `pcall` caught before; or else what Luau
/// says.
fn ended_by(state: &State, err: &mlua::Error) -> Failure {
    let text = err.to_string();
    let failed = state.failed.take();
    (failed.into_iter().rev())
        .find(|failure| text.contains(&failure.said))
        .unwrap_or_else(|| failure(err))
}

/// What `err`, from Luau, says, and where, if it says.
fn failure(err: &mlua::Error) -> Failure {
    let text = match err {
        mlua::Error::CallbackError { cause, .. } => cause.to_string(),
        err => err.to_string(),
    };
    let first = text.lines().next().unwrap_or_default();
    let said = (["runtime error: ", "syntax error: "].iter())
        .find_map(|kind| first.strip_prefix(kind))
        .unwrap_or(first)
        .to_owned();
    // `path:line: what`, as Luau says where.
    let at = said.split_once(": ").and_then(|(place, _)| {
        let (file, line) = place.rsplit_once(':')?;
        Some(Place {
            file: file.into(),
            line: line.parse().ok()?,
        })
    });
    Failure { said, at }
}

/// A Luau in its sandbox, with the config's API, and the store the agents
/// and profiles it sets are kept in.
fn sandbox(state: &Rc<State>, dirs: &[PathBuf], limit: Duration) -> mlua::Result<(Lua, Table)> {
    let libs = StdLib::COROUTINE
        | StdLib::TABLE
        | StdLib::STRING
        | StdLib::UTF8
        | StdLib::BIT
        | StdLib::MATH
        | StdLib::BUFFER
        | StdLib::VECTOR;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_compiler(
        mlua::chunk::Compiler::new()
            .set_optimization_level(2)
            // Read through metatables each time, as its values change.
            .set_mutable_globals(["daemon"]),
    );
    lua.set_memory_limit(MEMORY)?;
    let started = Instant::now();
    let steps = Cell::new(0u64);
    let stopped = Cell::new(false);
    lua.set_interrupt(move |_| {
        let step = steps.get() + 1;
        steps.set(step);
        // Once stopped, stopped at every step after, so a `pcall` that
        // caught the error can't carry on.
        if stopped.get()
            || step > MOST_STEPS
            || (step.is_multiple_of(64) && started.elapsed() > limit)
        {
            stopped.set(true);
            return Err(mlua::Error::runtime("it ran too long, so it was stopped"));
        }
        Ok(VmState::Continue)
    });
    let globals = lua.globals();
    // Nothing that loads code, but `require` from the config's folders.
    for name in ["loadstring", "getfenv", "setfenv", "require"] {
        globals.raw_set(name, LuaValue::Nil)?;
    }
    globals.raw_set("require", require(&lua, state, dirs.to_vec())?)?;
    globals.raw_set(
        "print",
        lua.create_function(|_, values: mlua::MultiValue| {
            let words: Vec<String> = (values.iter())
                .map(|value| value.to_string().unwrap_or_else(|_| "?".into()))
                .collect();
            log::info!(target: "clackatoo::daemon.lua", "{}", words.join("\t"));
            Ok(())
        })?,
    )?;
    let store = lua.create_table()?;
    globals.raw_set("daemon", api(&lua, state, &store)?)?;
    lua.sandbox(true)?;
    Ok((lua, store))
}

/// `daemon`: a table for each group of options, and `agents` and
/// `profiles`, whose tables are kept in `store`.
fn api(lua: &Lua, state: &Rc<State>, store: &Table) -> mlua::Result<Table> {
    // Kept apart from `daemon` itself, so setting any of them is refused
    // in the same words, not as a read-only table.
    let fields = lua.create_table()?;
    for group in options::groups() {
        fields.raw_set(group, options_group(lua, state, group)?)?;
    }
    for (name, list) in [("agents", Named::Agents), ("profiles", Named::Profiles)] {
        let kept = lua.create_table()?;
        store.raw_set(name, &kept)?;
        fields.raw_set(name, named(lua, state, list, kept)?)?;
    }
    let daemon = lua.create_table()?;
    let meta = lua.create_table()?;
    let names: Vec<&str> = options::groups().chain(["agents", "profiles"]).collect();
    let listed = names.join(", ");
    let reading = state.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |lua, (_, key): (Table, LuaValue)| {
            match fields.raw_get::<LuaValue>(&key)? {
                LuaValue::Nil => {
                    let key = key.to_string().unwrap_or_default();
                    Err(reading.fail(lua, format!("there's no daemon.{key}: there's {listed}")))
                }
                field => Ok(field),
            }
        })?,
    )?;
    let setting = state.clone();
    meta.raw_set(
        "__newindex",
        lua.create_function(
            move |lua, (_, key): (Table, LuaValue)| -> mlua::Result<()> {
                let key = key.to_string().unwrap_or_default();
                Err(setting.fail(
                    lua,
                    format!("daemon.{key} can't be set: set what's in it, as daemon.web.mode"),
                ))
            },
        )?,
    )?;
    meta.raw_set("__metatable", false)?;
    daemon.set_metatable(Some(meta))?;
    Ok(daemon)
}

/// `daemon.GROUP`: reads and sets the options in `group`, checking what
/// it sets and noting where.
fn options_group(lua: &Lua, state: &Rc<State>, group: &'static str) -> mlua::Result<Table> {
    let find = move |key: &LuaValue| -> Result<&'static Info, String> {
        let key = key.to_string().unwrap_or_default();
        let name = format!("{group}.{key}");
        options::find(&name).ok_or_else(|| {
            let known: Vec<&str> = (ALL.iter())
                .filter_map(|info| info.name.strip_prefix(group)?.strip_prefix('.'))
                .collect();
            format!(
                "there's no setting daemon.{name}: daemon.{group} has {}",
                known.join(", ")
            )
        })
    };
    let proxy = lua.create_table()?;
    let meta = lua.create_table()?;
    let reading = state.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |lua, (_, key): (Table, LuaValue)| {
            let info = find(&key).map_err(|said| reading.fail(lua, said))?;
            let value = (reading.options.borrow().get(info.name))
                .map_or_else(|| options::default_of(info), |(value, _)| value.clone());
            into_lua(lua, &value)
        })?,
    )?;
    let setting = state.clone();
    meta.raw_set(
        "__newindex",
        lua.create_function(move |lua, (_, key, value): (Table, LuaValue, LuaValue)| {
            let info = find(&key).map_err(|said| setting.fail(lua, said))?;
            if value.is_nil() {
                setting.options.borrow_mut().remove(info.name);
                return Ok(());
            }
            let value = from_lua(info, &value)
                .map_err(|said| setting.fail(lua, format!("daemon.{said}")))?;
            let by = setting.set_by(lua);
            setting.options.borrow_mut().insert(info.name, (value, by));
            Ok(())
        })?,
    )?;
    meta.raw_set("__metatable", false)?;
    proxy.set_metatable(Some(meta))?;
    Ok(proxy)
}

/// Which list of named tables.
#[derive(Clone, Copy)]
enum Named {
    Agents,
    Profiles,
}

impl Named {
    fn name(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::Profiles => "profiles",
        }
    }

    fn list(self, state: &State) -> &RefCell<Vec<(String, SetBy)>> {
        match self {
            Self::Agents => &state.agents,
            Self::Profiles => &state.profiles,
        }
    }
}

/// `daemon.agents` or `daemon.profiles`: tables by name, kept in `kept`,
/// which config may change after setting them, and which are read once
/// it's run. Setting one to nil takes it out.
fn named(lua: &Lua, state: &Rc<State>, list: Named, kept: Table) -> mlua::Result<Table> {
    let proxy = lua.create_table()?;
    let meta = lua.create_table()?;
    let reading = kept.clone();
    meta.raw_set(
        "__index",
        lua.create_function(move |_, (_, key): (Table, LuaValue)| {
            reading.raw_get::<LuaValue>(key)
        })?,
    )?;
    let setting = state.clone();
    let writing = kept.clone();
    meta.raw_set(
        "__newindex",
        lua.create_function(move |lua, (_, key, value): (Table, LuaValue, LuaValue)| {
            let group = list.name();
            let LuaValue::String(key) = key else {
                return Err(setting.fail(lua, format!("daemon.{group} is by name: a string")));
            };
            let name = key.to_str()?.to_owned();
            if name.is_empty() {
                return Err(setting.fail(lua, format!("daemon.{group} needs a name")));
            }
            let mut names = list.list(&setting).borrow_mut();
            match value {
                LuaValue::Nil => names.retain(|(kept, _)| *kept != name),
                LuaValue::Table(ref _table) => {
                    let by = setting.set_by(lua);
                    match names.iter_mut().find(|(kept, _)| *kept == name) {
                        Some((_, was)) => *was = by,
                        None => names.push((name.clone(), by)),
                    }
                }
                _ => {
                    drop(names);
                    return Err(setting.fail(
                        lua,
                        format!("daemon.{group}.{name} is a table, or nil to take it out"),
                    ));
                }
            }
            writing.raw_set(name, value)
        })?,
    )?;
    meta.raw_set(
        "__iter",
        lua.create_function(move |lua, _: LuaValue| {
            // Through the sandbox's globals, whose own table is empty.
            let next: LuaValue = lua.globals().get("next")?;
            Ok((next, kept.clone()))
        })?,
    )?;
    meta.raw_set("__metatable", false)?;
    proxy.set_metatable(Some(meta))?;
    Ok(proxy)
}

/// `require(name)`: a module from `daemon/lua/`, then `lua/`, by a name
/// of letters, digits, `_` and `-`, a dot being a folder, run once.
fn require(lua: &Lua, state: &Rc<State>, dirs: Vec<PathBuf>) -> mlua::Result<mlua::Function> {
    let loaded = lua.create_table()?;
    let state = state.clone();
    lua.create_function(move |lua, name: String| {
        if let Some(module) = loaded.raw_get::<Option<LuaValue>>(name.as_str())? {
            return Ok(module);
        }
        if !module_name(&name) {
            return Err(state.fail(
                lua,
                format!(
                    "require({name:?}): a module's name is letters, digits, _ and -, with dots \
                     for folders"
                ),
            ));
        }
        let relative = name.replace('.', "/");
        let mut found = None;
        'dirs: for dir in &dirs {
            for file in [format!("{relative}.lua"), format!("{relative}/init.lua")] {
                let path = dir.join(file);
                match read(&path) {
                    Ok(Some(text)) => {
                        found = Some((text, path));
                        break 'dirs;
                    }
                    Ok(None) => {}
                    Err(why) => return Err(state.fail(lua, why)),
                }
            }
        }
        let Some((text, path)) = found else {
            return Err(state.fail(
                lua,
                format!("there's no module `{name}` in daemon/lua/ or lua/"),
            ));
        };
        let module: LuaValue = (lua.load(text))
            .set_name(format!("@{}", path.display()))
            .set_mode(ChunkMode::Text)
            .call(())?;
        let module = if module.is_nil() {
            LuaValue::Boolean(true)
        } else {
            module
        };
        loaded.raw_set(name, &module)?;
        Ok(module)
    })
}

/// Whether `text` compiles as Luau, or why not: nothing in it runs.
pub(crate) fn compiles(text: &str) -> Result<(), String> {
    (mlua::chunk::Compiler::new().compile(text))
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Whether `name` is a module's name `require` takes: letters, digits,
/// `_` and `-`, with dots for folders, so never a path of its own, nor
/// one out of the config's folders.
#[must_use]
pub fn module_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|part| {
            !part.is_empty()
                && (part.bytes()).all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
}

/// The text of the file of config at `path`, or `None` if it isn't
/// there: never a FIFO or a device, which could keep the daemon waiting,
/// nor more than [`MOST_FILE`].
///
/// # Errors
///
/// What's wrong with it, or reading it.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    let file = match crate::regular::open(path, crate::regular::Links::Follow) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{} couldn't be read: {err}", path.display())),
    };
    let mut text = String::new();
    (file.take(MOST_FILE + 1).read_to_string(&mut text))
        .map_err(|err| format!("{} couldn't be read: {err}", path.display()))?;
    if text.len() as u64 > MOST_FILE {
        return Err(format!("{} is over 1 MiB", path.display()));
    }
    Ok(Some(text))
}

/// `value`, as Lua has it.
fn into_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Bool(on) => LuaValue::Boolean(*on),
        Value::Number(number) => LuaValue::Number(f64::from(*number)),
        Value::Decimal(number) => LuaValue::Number(number.as_f64()),
        Value::List(numbers) => {
            LuaValue::Table(lua.create_sequence_from(numbers.iter().map(|n| f64::from(*n)))?)
        }
        Value::Choice(word) => LuaValue::String(lua.create_string(word)?),
        Value::Text(text) => LuaValue::String(lua.create_string(text)?),
    })
}

/// `value`, from Lua, for `info`, if it can hold it: or what it holds.
fn from_lua(info: &Info, value: &LuaValue) -> Result<Value, String> {
    let number = match value {
        LuaValue::Integer(number) => u32::try_from(*number).ok(),
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        LuaValue::Number(number)
            if number.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(number) =>
        {
            Some(*number as u32)
        }
        _ => None,
    };
    let text = match value {
        LuaValue::String(text) => text.to_str().ok().map(|text| text.to_owned()),
        _ => None,
    };
    let value = match (info.kind, value) {
        (Kind::Bool, LuaValue::Boolean(on)) => Some(Value::Bool(*on)),
        (Kind::Number { .. }, _) => number.map(Value::Number),
        (Kind::Choice(words), _) => (text.as_deref())
            .and_then(|text| words.iter().find(|word| **word == text))
            .map(|word| Value::Choice(word)),
        (Kind::Text, _) => text.map(Value::Text),
        _ => None,
    };
    let value = value.ok_or_else(|| info.wants())?;
    info.check(&value)?;
    Ok(value)
}

/// What the run set: its options, and its agents' and profiles' tables,
/// read now they're set, each wrong one failing at the line that set it.
fn gather(state: &State, store: &Table) -> Result<Settings, Failure> {
    let options = state.options.take();
    let read = |list: Named| tables(state, store, list);
    let wrong = |by: &SetBy, said: String| Failure {
        said: match &by.at {
            Some(at) => format!("{}:{}: {said}", at.file, at.line),
            None => said,
        },
        at: by.at.clone(),
    };
    let mut agents = Vec::new();
    for (name, table, by) in read(Named::Agents)? {
        let agent = agent_from(&name, &table).map_err(|said| wrong(&by, said))?;
        agents.push((agent, by));
    }
    let mut profiles = Vec::new();
    for (name, table, by) in read(Named::Profiles)? {
        let profile = profile_from(&name, &table).map_err(|said| wrong(&by, said))?;
        profiles.push((profile, by));
    }
    Ok(Settings {
        options,
        agents,
        profiles,
    })
}

/// The tables set in `list`, by name, with what set each.
fn tables(
    state: &State,
    store: &Table,
    list: Named,
) -> Result<Vec<(String, Table, SetBy)>, Failure> {
    let kept: Table = store.raw_get(list.name()).map_err(|err| failure(&err))?;
    let mut tables = Vec::new();
    for (name, by) in list.list(state).borrow().iter() {
        let table: Table = kept.raw_get(name.as_str()).map_err(|err| failure(&err))?;
        tables.push((name.clone(), table, by.clone()));
    }
    Ok(tables)
}

/// The fields of `table`, `daemon.GROUP.NAME`, which may have only
/// `known`: or which it has that it can't.
fn fields(
    group: &str,
    name: &str,
    table: &Table,
    known: &[&str],
) -> Result<BTreeMap<String, LuaValue>, String> {
    let mut fields = BTreeMap::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair.map_err(|err| err.to_string())?;
        let key = match key {
            LuaValue::String(key) => key.to_str().map_err(|err| err.to_string())?.to_owned(),
            other => {
                return Err(format!(
                    "daemon.{group}.{name} has {}, but its fields are named",
                    other.type_name()
                ));
            }
        };
        if !known.contains(&key.as_str()) {
            return Err(format!(
                "daemon.{group}.{name}.{key} isn't something it has: it has {}",
                known.join(", ")
            ));
        }
        fields.insert(key, value);
    }
    Ok(fields)
}

/// The string `value`, if it's one.
fn string(
    value: Option<&LuaValue>,
    wrong: impl FnOnce() -> String,
) -> Result<Option<String>, String> {
    match value {
        None | Some(LuaValue::Nil) => Ok(None),
        Some(LuaValue::String(text)) => Ok(Some(text.to_str().map_err(|_| wrong())?.to_owned())),
        Some(_) => Err(wrong()),
    }
}

/// The agent `table`, `daemon.agents.NAME`, adds.
fn agent_from(name: &str, table: &Table) -> Result<CustomAgent, String> {
    let fields = fields("agents", name, table, AGENT_FIELDS)?;
    let text = |key: &str| {
        string(fields.get(key), || {
            format!("daemon.agents.{name}.{key} is a string")
        })
    };
    let wrong = || format!("daemon.agents.{name} needs a command, a list of strings");
    let Some(LuaValue::Table(command)) = fields.get("command") else {
        return Err(wrong());
    };
    let command = (command.clone().sequence_values::<LuaValue>())
        .map(|word| match word {
            Ok(LuaValue::String(word)) => word.to_str().ok().map(|word| word.to_owned().into()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .filter(|command: &Vec<std::ffi::OsString>| !command.is_empty())
        .ok_or_else(wrong)?;
    let tasks = match fields.get("tasks") {
        None | Some(LuaValue::Nil) => None,
        Some(LuaValue::Boolean(tasks)) => Some(*tasks),
        Some(_) => return Err(format!("daemon.agents.{name}.tasks is true or false")),
    };
    Ok(CustomAgent {
        name: name.to_owned(),
        title: text("name")?,
        command,
        starting_mode: text("mode")?,
        tasks,
    })
}

/// The profile `table`, `daemon.profiles.NAME`, adds, or changes of the
/// built-in one of its name.
fn profile_from(name: &str, table: &Table) -> Result<Change, String> {
    let fields = fields("profiles", name, table, PROFILE_FIELDS)?;
    let text = |key: &str| {
        string(fields.get(key), || {
            format!("daemon.profiles.{name}.{key} is a string")
        })
    };
    // A string for any agent, or a table of them by agent.
    let by_agent = |key: &str| -> Result<Option<Vec<(String, String)>>, String> {
        let wrong =
            || format!("daemon.profiles.{name}.{key} is a string, or a table of them by agent");
        match fields.get(key) {
            None | Some(LuaValue::Nil) => Ok(None),
            Some(LuaValue::String(value)) => Ok(Some(vec![(
                "*".to_owned(),
                value.to_str().map_err(|_| wrong())?.to_owned(),
            )])),
            Some(LuaValue::Table(values)) => {
                let mut pairs = Vec::new();
                for pair in values.pairs::<LuaValue, LuaValue>() {
                    match pair.map_err(|_| wrong())? {
                        (LuaValue::String(agent), LuaValue::String(value)) => pairs.push((
                            agent.to_str().map_err(|_| wrong())?.to_owned(),
                            value.to_str().map_err(|_| wrong())?.to_owned(),
                        )),
                        _ => return Err(wrong()),
                    }
                }
                // As written, whatever order Luau keeps them in.
                pairs.sort();
                Ok(Some(pairs))
            }
            Some(_) => Err(wrong()),
        }
    };
    let limits = (text("limits")?)
        .map(|limits| {
            Limits::named(&limits).ok_or_else(|| {
                format!(
                    "daemon.profiles.{name}.limits is \"read-only\", \"no-commands\" or \"none\""
                )
            })
        })
        .transpose()?;
    Ok(Change {
        name: name.to_owned(),
        about: text("about")?,
        prompt: text("prompt")?,
        limits,
        agent: text("agent")?,
        models: by_agent("model")?,
        efforts: by_agent("effort")?,
    })
}
