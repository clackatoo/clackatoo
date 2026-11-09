use std::time::Instant;

use super::*;

/// What `text` sets, or what it gets wrong.
fn run(text: &str) -> Result<Settings, Failure> {
    Settings::from_lua(text)
}

/// What `text` gets wrong, as it's said.
fn wrong(text: &str) -> String {
    run(text).unwrap_err().said
}

/// A config directory and a state directory, empty.
fn dirs() -> (tempfile::TempDir, Dirs) {
    let temp = tempfile::tempdir().unwrap();
    let dirs = Dirs {
        daemon: temp.path().join("config/clackatoo/daemon"),
        shared: temp.path().join("config/clackatoo/lua"),
        state: Some(temp.path().join("state")),
    };
    std::fs::create_dir_all(&dirs.daemon).unwrap();
    (temp, dirs)
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn options_are_set_read_back_and_said_where() {
    let settings = run("daemon.machine.name = \"desk\"\n\
         daemon.history.lines = 20000\n\
         assert(daemon.history.lines == 20000)\n\
         assert(daemon.history.every == 30, \"the default reads back\")\n\
         daemon.web.mode = \"tailscale\"\n\
         daemon.web.mode = nil\n")
    .unwrap();
    assert_eq!(settings.machine_name().as_deref(), Some("desk"));
    assert_eq!(settings.value("history.lines"), Value::Number(20_000));
    assert_eq!(
        settings.value("web.mode"),
        Value::Choice("off"),
        "nil unsets"
    );
    let by = settings.set_by("history.lines");
    assert_eq!(by.source, Source::Config);
    assert!(by.loading);
    assert_eq!(by.at.unwrap().line, 2);
    assert_eq!(settings.set_by("web.mode"), SetBy::DEFAULT);
}

#[test]
fn a_value_an_option_cant_hold_is_refused_at_its_line() {
    let failure = run("daemon.machine.name = \"x\"\ndaemon.history.lines = 0\n").unwrap_err();
    assert_eq!(
        failure.said,
        "daemon/init.lua:2: daemon.history.lines is a whole number from 1 to 1000000"
    );
    assert_eq!(failure.at.unwrap().line, 2);
    assert!(wrong("daemon.web.mode = \"open\"").contains("off, tailscale or public"));
    assert!(wrong("daemon.machine.priority = 1").contains("true or false"));
    assert!(wrong("daemon.machine.name = 3").contains("text"));
    assert!(wrong("daemon.history.lines = 1.5").contains("whole number"));
    assert!(wrong("daemon.machine.nmae = \"x\"").contains("daemon.machine has name, priority"));
    assert!(wrong("daemon.mahcine.name = \"x\"").contains("there's no daemon.mahcine"));
    assert!(wrong("daemon.web = {}").contains("can't be set"));
}

/// Where a helper in a module sets an option, the user's line that called
/// it is where it was set, and the helper's where it really was.
#[test]
fn a_module_required_sets_what_it_sets_at_the_users_line() {
    let (_temp, dirs) = dirs();
    write(
        &dirs.shared.join("machines.lua"),
        "return { name = function(name)\n  daemon.machine.name = name\nend }\n",
    );
    write(
        &dirs.daemon.join("lua/web/home.lua"),
        "daemon.web.mode = \"tailscale\"\n",
    );
    write(
        &dirs.daemon.join("init.lua"),
        "local machines = require(\"machines\")\n\nmachines.name(\"desk\")\nrequire(\"web.home\")\n",
    );
    let loaded = load(&dirs, When::Starting);
    assert_eq!(loaded.notes, Vec::<String>::new());
    let by = loaded.settings.set_by("machine.name");
    let at = by.at.unwrap();
    assert!(
        at.file.ends_with("daemon/init.lua") && at.line == 3,
        "{at:?}"
    );
    let inner = by.inner.unwrap();
    assert!(
        inner.file.ends_with("lua/machines.lua") && inner.line == 2,
        "{inner:?}"
    );
    assert_eq!(
        loaded.settings.value("web.mode"),
        Value::Choice("tailscale")
    );
}

/// settings.lua runs first, and init.lua outdoes it.
#[test]
fn init_lua_outdoes_settings_lua() {
    let (_temp, dirs) = dirs();
    write(
        &dirs.daemon.join("settings.lua"),
        "daemon.machine.name = \"saved\"\ndaemon.history.lines = 5\n",
    );
    write(
        &dirs.daemon.join("init.lua"),
        "daemon.machine.name = \"mine\"\n",
    );
    let settings = load(&dirs, When::Starting).settings;
    assert_eq!(settings.machine_name().as_deref(), Some("mine"));
    assert_eq!(settings.set_by("machine.name").source, Source::Config);
    assert_eq!(settings.value("history.lines"), Value::Number(5));
    assert_eq!(settings.set_by("history.lines").source, Source::Settings);
}

/// Nothing in the sandbox reaches the machine, or loads code but
/// `require` from the config's folders.
#[test]
fn the_sandbox_refuses_what_would_escape_it() {
    for escape in [
        "io.open(\"/etc/passwd\")",
        "os.execute(\"touch /tmp/escaped\")",
        "os.getenv(\"HOME\")",
        "load(\"return 1\")",
        "loadstring(\"return 1\")",
        "dofile(\"/etc/passwd\")",
        "loadfile(\"/etc/passwd\")",
        "getfenv(1)",
        "setfenv(1, {})",
        "debug.getinfo(1)",
        "package.loadlib(\"libc.so.6\", \"system\")",
        "require(\"/etc/passwd\")",
        "require(\"../../../etc/passwd\")",
        "require(\"..\")",
        "require(\"a/b\")",
        "require(\"\")",
        "rawset(daemon, \"web\", {})",
        "setmetatable(daemon, nil)",
        "string.rep = nil",
        "_G.daemon = {}",
    ] {
        assert!(run(escape).is_err(), "{escape} ran");
    }
    // A module outside the folders, by any name, is never found.
    let (temp, dirs) = dirs();
    write(
        &temp.path().join("outside.lua"),
        "daemon.machine.name = \"escaped\"\n",
    );
    write(&dirs.daemon.join("init.lua"), "require(\"outside\")\n");
    let loaded = load(&dirs, When::Starting);
    assert_eq!(loaded.settings.machine_name(), None);
    assert!(
        loaded.notes[0].contains("there's no module `outside`"),
        "{:?}",
        loaded.notes
    );
}

/// The globals are only those allowed: anything else would be a way out
/// that no test above thought of.
#[test]
fn the_sandbox_has_only_the_globals_allowed() {
    let settings = run("local names = {}\n\
         for name in pairs(_G) do table.insert(names, name) end\n\
         table.sort(names)\n\
         daemon.machine.name = table.concat(names, \" \")\n")
    .unwrap();
    let names = settings.machine_name().unwrap();
    for name in names.split(' ') {
        assert!(lua::GLOBALS.contains(&name), "{name} is in the sandbox");
    }
    assert!(names.split(' ').any(|name| name == "daemon"), "{names}");
}

/// Luau bytecode is never run: as the config or a module, it's text.
#[test]
fn bytecode_is_refused() {
    let bytecode = mlua::chunk::Compiler::new()
        .compile("daemon.machine.name = \"bytecode\"")
        .unwrap();
    let (_temp, dirs) = dirs();
    std::fs::write(dirs.daemon.join("init.lua"), &bytecode).unwrap();
    let loaded = load(&dirs, When::Starting);
    assert_eq!(loaded.settings.machine_name(), None);
    assert!(!loaded.notes.is_empty());
    std::fs::write(dirs.daemon.join("init.lua"), "require(\"compiled\")").unwrap();
    std::fs::create_dir_all(dirs.daemon.join("lua")).unwrap();
    std::fs::write(dirs.daemon.join("lua/compiled.lua"), &bytecode).unwrap();
    assert_eq!(load(&dirs, When::Starting).settings.machine_name(), None);
}

/// A loop that never ends, a call that never returns, and memory that
/// never stops growing are all stopped.
#[test]
fn a_runaway_config_is_stopped() {
    for runaway in [
        "while true do end",
        "local function f() return f() end f()",
        "local t = {} while true do table.insert(t, t) end",
        "local s = \"x\" while true do s = s .. s end",
        "while true do pcall(function() end) end",
    ] {
        let started = Instant::now();
        assert!(run(runaway).is_err(), "{runaway}");
        assert!(started.elapsed() < Duration::from_secs(5), "{runaway}");
    }
    assert!(wrong("while true do end").contains("ran too long"));
    // As the daemon runs it: a note, and the defaults.
    let (_temp, dirs) = dirs();
    write(
        &dirs.daemon.join("init.lua"),
        "daemon.machine.name = \"x\"\nwhile true do end\n",
    );
    let started = Instant::now();
    let loaded = load(&dirs, When::Starting);
    assert!(started.elapsed() < When::Starting.wait() + Duration::from_secs(1));
    assert_eq!(loaded.settings, Settings::fallback());
    assert!(
        loaded.notes[0].contains("ran too long"),
        "{:?}",
        loaded.notes
    );
}

/// A config that goes wrong keeps what last ran without a mistake, and
/// says so; fixed, it's run again.
#[test]
fn a_broken_config_keeps_the_last_good_values_and_says_so() {
    let (_temp, dirs) = dirs();
    let init = dirs.daemon.join("init.lua");
    write(
        &init,
        "daemon.machine.name = \"good\"\n\
         daemon.agents.mine = { command = { \"mine-acp\" }, tasks = false }\n\
         daemon.profiles.review = { model = { claude = \"haiku\" }, limits = \"read-only\" }\n",
    );
    let good = load(&dirs, When::Starting);
    assert!(good.notes.is_empty(), "{:?}", good.notes);
    let state = dirs.state.as_ref().unwrap();
    assert!(state.join(LAST_GOOD).exists());
    write(
        &init,
        "daemon.machine.name = \"broken\"\ndaemon.history.lines = \"many\"\n",
    );
    let broken = load(&dirs, When::Starting);
    assert_eq!(broken.settings.machine_name().as_deref(), Some("good"));
    assert_eq!(broken.settings.agents(), good.settings.agents());
    let note = &broken.notes[0];
    assert!(
        note.contains("kept its settings from the last time it ran"),
        "{note}"
    );
    assert!(
        note.contains("daemon/init.lua:2: daemon.history.lines"),
        "{note}"
    );
    assert!(
        !note.contains(&*dirs.daemon.to_string_lossy()),
        "said from the config: {note}"
    );
    // A syntax error, too.
    write(&init, "daemon.machine.name = \n");
    assert_eq!(
        load(&dirs, When::Starting)
            .settings
            .machine_name()
            .as_deref(),
        Some("good")
    );
    // Fixed, it's what runs, and what's kept.
    write(&init, "daemon.machine.name = \"fixed\"\n");
    assert_eq!(
        load(&dirs, When::Starting)
            .settings
            .machine_name()
            .as_deref(),
        Some("fixed")
    );
    write(&init, "error(\"no\")\n");
    assert_eq!(
        load(&dirs, When::Starting)
            .settings
            .machine_name()
            .as_deref(),
        Some("fixed")
    );
    // With nothing kept, the defaults, with history and the web app off
    // until a config has run.
    std::fs::remove_file(state.join(LAST_GOOD)).unwrap();
    let defaults = load(&dirs, When::Starting);
    assert_eq!(defaults.settings, Settings::fallback());
    assert!(defaults.settings.history(PathBuf::from("/h")).is_none());
    assert_eq!(defaults.settings.value("web.mode"), Value::Choice("off"));
    assert!(
        defaults.notes[0]
            .contains("went by its defaults, with terminals' history and the web app off"),
        "{:?}",
        defaults.notes
    );
}

/// Once a run has gone on too long, every step after stops it, so a
/// `pcall` that caught it can't carry on.
#[test]
fn a_run_stopped_for_time_stays_stopped() {
    for runaway in [
        "while true do pcall(string.rep, \"x\", 1e9) end",
        "while true do pcall(function() while true do end end) end",
        "for _ = 1, 1e12 do pcall(error, \"no\") end",
    ] {
        let started = Instant::now();
        let failure = run(runaway).unwrap_err();
        assert!(
            failure.said.contains("ran too long"),
            "{runaway}: {failure:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{runaway}");
    }
}

/// What's said is what ended the run, not a mistake a `pcall` caught on
/// the way.
#[test]
fn the_mistake_said_is_the_one_that_ended_it() {
    let failure = run("pcall(function() daemon.history.lines = 0 end)
\
         daemon.machine.nmae = \"x\"\n")
    .unwrap_err();
    assert!(failure.said.contains("nmae"), "{failure:?}");
    assert_eq!(failure.at.unwrap().line, 2);
    let failure =
        run("pcall(function() daemon.history.lines = 0 end)\nerror(\"its own\")\n").unwrap_err();
    assert!(failure.said.contains("its own"), "{failure:?}");
    assert!(!failure.said.contains("history"), "{failure:?}");
    // Caught and gone on from, it's no mistake at all.
    assert!(run("pcall(function() daemon.history.lines = 0 end)").is_ok());
}

/// The build an upgrade is to is answered from what last ran, never the
/// user's own `init.lua`, which may be slow.
#[test]
fn peeking_never_runs_the_users_own_config() {
    let (_temp, dirs) = dirs();
    write(
        &dirs.daemon.join("init.lua"),
        "daemon.upgrade.keep_agents = false
",
    );
    assert!(peek(&dirs).agents().keep_agents, "not run");
    let _ = load(&dirs, When::Starting);
    assert!(!peek(&dirs).agents().keep_agents, "what last ran");
    write(
        &dirs.daemon.join("init.lua"),
        "while true do end
",
    );
    let started = Instant::now();
    assert!(!peek(&dirs).agents().keep_agents);
    assert!(started.elapsed() < Duration::from_millis(500));
    // Before anything has run, what the old file would be moved to.
    let (_temp, dirs) = super::tests::dirs();
    write(
        &dirs.daemon.join("agents.toml"),
        "[upgrade]\nkeep_agents = false\n",
    );
    assert!(!peek(&dirs).agents().keep_agents);
    assert!(
        !dirs.daemon.join("settings.lua").exists(),
        "nothing written"
    );
}

/// A file is written whole, only for the user, never through a link left
/// where its partial copy goes.
#[test]
fn a_private_file_is_written_whole_and_never_through_a_link() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("elsewhere");
    std::fs::write(&target, "theirs").unwrap();
    let path = temp.path().join("web-password");
    std::os::unix::fs::symlink(&target, temp.path().join("web-password.partial")).unwrap();
    write_private(&path, "mine\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "theirs");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert!(!temp.path().join("web-password.partial").exists());
}

/// What's kept as last good sets the same again.
#[test]
fn the_last_good_settings_read_back_the_same() {
    let text = "daemon.machine.name = \"a \\\"quoted\\\" name\"\n\
                daemon.machine.priority = false\n\
                daemon.web.mode = \"tailscale\"\n\
                daemon.images.terminal_mb = 32\n\
                daemon.agents[\"my agent\"] = { name = \"Mine\", command = { \"a\", \"b c\" }, mode = \"m\", tasks = true }\n\
                daemon.profiles.explore = { model = \"inherit\" }\n\
                daemon.profiles.review = { about = \"x\", prompt = \"y\", limits = \"no-commands\", agent = \"codex\", model = { claude = \"haiku\", codex = \"luna\" }, effort = \"low\" }\n";
    let settings = run(text).unwrap();
    let again = run(&settings.to_lua()).unwrap();
    assert_eq!(again.options.len(), settings.options.len());
    for (name, (value, _)) in &settings.options {
        assert_eq!(&again.options[name].0, value, "{name}");
    }
    assert_eq!(again.agents(), settings.agents());
}

/// What's checked between options is refused at the line that set it.
#[test]
fn options_that_cant_be_so_together_are_refused() {
    let failure = run("daemon.images.total_mb = 16\ndaemon.images.terminal_mb = 64\n").unwrap_err();
    assert!(
        failure.said.starts_with("daemon/init.lua:2: "),
        "{failure:?}"
    );
    assert!(
        failure.said.contains("at most daemon.images.total_mb"),
        "{failure:?}"
    );
    assert!(run("daemon.images.terminal_mb = 16\ndaemon.images.total_mb = 16").is_ok());
}

/// The password's hash is kept in the state directory, only for the user.
#[test]
fn the_password_is_kept_in_the_state_directory() {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    assert_eq!(password(&state).unwrap(), None);
    let hash = crate::web::hash_password("correct horse");
    set_password(&state, Some(&hash)).unwrap();
    assert_eq!(password(&state).unwrap().as_deref(), Some(hash.as_str()));
    let mode = std::fs::metadata(state.join(PASSWORD))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    set_password(&state, None).unwrap();
    assert_eq!(password(&state).unwrap(), None);
    set_password(&state, None).unwrap();
}

/// A config file that's a FIFO, which would hold the daemon up, isn't
/// read.
#[test]
fn a_fifo_for_a_config_is_refused_without_waiting() {
    let (_temp, dirs) = dirs();
    let init = dirs.daemon.join("init.lua");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &init,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
    let started = Instant::now();
    let loaded = load(&dirs, When::Starting);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!loaded.notes.is_empty());
}

/// The daemon's options are listed in the docs as they're declared.
#[test]
fn the_docs_list_every_daemon_option_as_the_schema_declares_it() {
    let docs = include_str!(
        "../../../../docs/learn/2--developer-tools.shelf/clackatoo.product/2--reference.topic/180--config.md"
    );
    let section = docs
        .split_once("## The daemon's options")
        .expect("the docs have the daemon's options")
        .1;
    let section = section.split("\n## ").next().unwrap();
    let mut listed = Vec::new();
    let mut category = "";
    for line in section.lines() {
        if let Some(title) = line.strip_prefix("### ") {
            category = title;
        } else if let Some(row) = line.strip_prefix("| `daemon.") {
            let cells: Vec<&str> = row.split(" | ").collect();
            let name = cells[0].trim_end_matches('`');
            let starts = cells.last().unwrap().trim_end_matches(" |").trim();
            listed.push((category.to_owned(), name.to_owned(), starts.to_owned()));
        }
    }
    let declared: Vec<(String, String, String)> = (options::ALL.iter())
        .map(|info| {
            let starts = match options::default_of(info) {
                Value::Bool(true) => "on".to_owned(),
                Value::Bool(false) => "off".to_owned(),
                Value::Number(number) => number.to_string(),
                Value::Text(text) if text.is_empty() => "none".to_owned(),
                Value::Text(text) => format!("`{text}`"),
                Value::Choice(word) => format!("`{word}`"),
                other => format!("{other:?}"),
            };
            (
                info.category.title().to_owned(),
                info.name.to_owned(),
                starts,
            )
        })
        .collect();
    // `web.ask`'s default goes by the mode, as the docs say.
    let listed: Vec<(String, String, String)> = (listed.into_iter())
        .map(|(category, name, starts)| {
            let starts = if name == "web.ask" {
                "on".to_owned()
            } else {
                starts
            };
            (category, name, starts)
        })
        .collect();
    assert_eq!(listed, declared);
}
