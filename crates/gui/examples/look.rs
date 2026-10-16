//! Looks at the window without a desktop: runs a script of keys, clicks
//! and screenshots against the real window, drawn offscreen, connected to
//! a daemon of its own, and writes each screenshot as a PNG, with how long
//! each step took to show in `timings.txt` beside them.
//!
//! ```text
//! cargo build -p clackatoo-daemon --example fake_agent --example fake_language_server
//! cargo run -p clackatoo-gui --example look -- crates/gui/looks/*.look target/looks
//! ```
//!
//! Its timings are a debug build's, and count reading each frame back
//! from the GPU: good for comparing steps, not for latency. Building both
//! commands with `--profile quick` brings them nearer a release build's.
//!
//! The last argument is the folder to write into: one script writes into
//! it, and several each into a folder of it named after the script. Under
//! `target/`, they stay out of the repository. Each script's project, the
//! folder its first shell starts in, is a folder of the home named after
//! it.
//!
//! Each script runs in a process of its own, in a temporary directory made
//! for it and deleted after: a home, config, data, state and runtime
//! directories of its own, so nothing touches the daemon or the config of
//! whoever runs it, and no script sees what another left. The daemon runs
//! in that process, its shells are bash, its chats talk to
//! the daemon tests' fake agent (see `crates/daemon/examples/fake_agent.rs`,
//! whose messages choose what it does), and `.fk` files have the fake
//! language server, which says every `bad` is an error. `LOOK_LOG=info`
//! shows what the window and daemon log; warnings show anyway.
//!
//! A script has a command a line; `#` starts a comment.
//!
//! | Command | Does |
//! |---|---|
//! | `size WxH` | Makes the window W × H points (1280 × 800 to start) |
//! | `scale S` | Draws it at S pixels to a point (1 to start) |
//! | `theme dark` | The window's colours; it has only the one theme so far |
//! | `file PATH` | Makes an empty file in the project, the folder the first shell starts in |
//! | `file PATH <<END`, lines, `END` | Makes a file of those lines |
//! | `config <<END`, lines, `END` | Makes the window's `init.lua` of those lines, which it runs as it opens |
//! | `kept <<END`, lines, `END` | The workspaces a daemon kept (see `clackatoo_daemon`'s `saved`), with `$PROJECT` for the project's folder: the daemon makes them again as it starts, in place of a shell in the project |
//! | `open PATH` | Opens the project's file in an editor, from normal mode: `<C-;>:Editor PATH<CR>` |
//! | `keys KEYS` | Presses keys, in Vim's notation: `:Explore<CR>`, `<Space>ff`, `<C-w>v` |
//! | `type TEXT` | Types the text, a key a character |
//! | `shell COMMAND` | Types the command and Enter, as into a shell |
//! | `move X Y` | Moves the mouse to X, Y, in points from the top left |
//! | `click X Y [right\|middle]` | Clicks there, with the left button unless said |
//! | `drag X1 Y1 X2 Y2` | Drags with the left button |
//! | `scroll X Y N` | Turns the wheel N notches there: up for more than none, down for fewer |
//! | `wait-for TEXT` | Waits until the window shows TEXT (see `look::Window::text`) |
//! | `timeout MS` | How long `wait-for` and settling wait at most (10000 to start) |
//! | `quiet MS` | How long nothing must change for the window to have settled (300 to start) |
//! | `sleep MS` | Lets the window carry on by itself for MS milliseconds |
//! | `shot NAME` | Writes what the window shows to `NAME.png`, once it's settled |
//!
//! After each key, click, scroll or `open`, the window is left to settle,
//! and how long that took to show is noted for `timings.txt`. The window
//! opens at the first command that needs it, so `size`, `scale` and
//! `file` before then are how it opens.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use clackatoo_daemon::{Client, CustomAgent, Daemon, DaemonConfig, RuntimeDir, ShutdownHandle};
use clackatoo_gui::look::{Timing, Window};
use clackatoo_protocol::{NewTerminal, PaneTerminal, Size};
use winit::event::MouseButton;

/// Set in the environment of the process that runs a script, which this
/// one starts with a home and the like of its own, to the temporary
/// directory they're all in.
const ISOLATED: &str = "CLACKATOO_LOOK_ISOLATED";

/// The environment that would reach outside the run: the desktop, the
/// session's bus and ssh's agent. Everything starting `CLACKATOO_`, which
/// is clackatoo's own settings, is left out too.
const LEFT_OUT: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "SSH_AUTH_SOCK",
];

fn main() -> ExitCode {
    if log::set_logger(&Stderr).is_ok() {
        let level = (std::env::var("LOOK_LOG").ok())
            .and_then(|level| level.parse().ok())
            .unwrap_or(log::LevelFilter::Warn);
        log::set_max_level(level);
    }
    let result = if let Some(root) = std::env::var_os(ISOLATED) {
        run_isolated(Path::new(&root))
    } else {
        isolate()
    };
    match result {
        Ok(code) => code,
        Err(err) => {
            eprintln!("look: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs this program again for each script, in a temporary directory of
/// the script's own that's home, config, data, state and runtime
/// directory to it, so no script sees what one before it left, then
/// deletes the directory.
fn isolate() -> anyhow::Result<ExitCode> {
    let mut args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let (Some(out), false) = (args.pop(), args.is_empty()) else {
        bail!("usage: look SCRIPT... OUT_DIR");
    };
    let several = args.len() > 1;
    let mut failed = false;
    for script in &args {
        let out = if several {
            out.join(script.file_stem().context("a script with no name")?)
        } else {
            out.clone()
        };
        println!("{}", script.display());
        if !isolated(script, &out)? {
            failed = true;
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Runs this program again for `script`, writing into `out`, in a
/// temporary directory of its own. Returns whether it succeeded.
fn isolated(script: &Path, out: &Path) -> anyhow::Result<bool> {
    let dir = tempfile::Builder::new()
        .prefix("look-")
        .tempdir()
        .context("no temporary directory")?;
    let root = dir.path();
    let home = root.join("home");
    let run = root.join("run");
    for dir in [&home, &run] {
        std::fs::create_dir_all(dir)?;
    }
    // The fonts installed for this user, so the window can draw in the
    // ones it would. Linked, not copied, as they can be big: the window
    // and the daemon only ever read fonts.
    if let Some(fonts) =
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/fonts"))
        && fonts.is_dir()
    {
        std::fs::create_dir_all(home.join(".local/share"))?;
        std::os::unix::fs::symlink(&fonts, home.join(".local/share/fonts"))?;
    }
    // A prompt that says where the shell is, as most people's do.
    std::fs::write(
        home.join(".bashrc"),
        "PS1='\\[\\e[1;34m\\]\\W\\[\\e[0m\\] $ '\n",
    )?;
    let socket = RuntimeDir::at(run.join("clackatoo")).socket_path();
    if socket.as_os_str().len() > 100 {
        bail!(
            "the daemon's socket would be at {}, too long a path for a socket: set TMPDIR to a shorter one",
            socket.display()
        );
    }
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .arg(script)
        .arg(out)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", &run)
        .env("SHELL", "/bin/bash")
        .env("TERM", "xterm-256color");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("CLACKATOO_") {
            command.env_remove(name);
        }
    }
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.env(ISOLATED, root);
    let status = command.status().context("couldn't run the script")?;
    drop(dir);
    Ok(status.success())
}

/// Runs the script given, writing into the folder given, in the
/// temporary directory [`isolated`] made, `root`.
fn run_isolated(root: &Path) -> anyhow::Result<ExitCode> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [script, out] = args.as_slice() else {
        bail!("usage: look SCRIPT OUT_DIR, in a home of its own");
    };
    // Never in a real home, whatever the environment says.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if !home.is_some_and(|home| home.starts_with(root) && root.starts_with(std::env::temp_dir())) {
        bail!("{ISOLATED} is set, but HOME isn't in the temporary directory it names");
    }
    match run_script(script, out) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(err) => {
            eprintln!("look: {}: {err:#}", script.display());
            Ok(ExitCode::FAILURE)
        }
    }
}

/// One line of a script.
#[derive(Debug)]
struct Step {
    /// Its number, from 1.
    line: usize,
    /// As written.
    text: String,
    command: Command,
}

#[derive(Debug)]
enum Command {
    Size([u32; 2]),
    Scale(f64),
    Theme,
    File(PathBuf, String),
    Config(String),
    Kept(String),
    Open(String),
    Keys(String),
    Type(String),
    Shell(String),
    Move([f32; 2]),
    Click([f32; 2], MouseButton),
    Drag([f32; 2], [f32; 2]),
    Scroll([f32; 2], f32),
    WaitFor(String),
    Timeout(Duration),
    Quiet(Duration),
    Sleep(Duration),
    Shot(String),
}

/// The steps of `script`.
#[allow(clippy::too_many_lines, reason = "one arm per command, each short")]
fn parse(script: &str) -> anyhow::Result<Vec<Step>> {
    let mut steps = Vec::new();
    let mut lines = script.lines().enumerate();
    while let Some((index, line)) = lines.next() {
        let line_number = index + 1;
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let (word, rest) = text.split_once(' ').unwrap_or((text, ""));
        let rest = rest.trim();
        let numbers = |count: usize| -> anyhow::Result<Vec<f32>> {
            let numbers: Vec<f32> = (rest.split_whitespace().take(count))
                .map(str::parse)
                .collect::<Result<_, _>>()
                .map_err(|_| anyhow!("line {line_number}: `{word}` takes {count} numbers"))?;
            if numbers.len() == count {
                Ok(numbers)
            } else {
                bail!("line {line_number}: `{word}` takes {count} numbers")
            }
        };
        let millis = || -> anyhow::Result<Duration> {
            rest.parse()
                .map(Duration::from_millis)
                .map_err(|_| anyhow!("line {line_number}: `{word}` takes milliseconds"))
        };
        let command = match word {
            "size" => {
                let (width, height) = (rest.split_once('x'))
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .with_context(|| format!("line {line_number}: `size` takes WIDTHxHEIGHT"))?;
                Command::Size([width, height])
            }
            "scale" => Command::Scale(
                (rest.parse().ok())
                    .filter(|scale: &f64| *scale > 0.0)
                    .with_context(|| format!("line {line_number}: `scale` takes a number"))?,
            ),
            "theme" if rest == "dark" => Command::Theme,
            "theme" => bail!("line {line_number}: the window has only the dark theme so far"),
            "file" | "config" | "kept" => {
                let (path, until) = match rest.split_once("<<") {
                    Some((path, until)) => (path.trim(), Some(until.trim())),
                    None => (rest, None),
                };
                let mut contents = String::new();
                if let Some(until) = until {
                    loop {
                        let (_, line) = lines.next().with_context(|| {
                            format!("line {line_number}: no `{until}` after the file")
                        })?;
                        if line.trim_end() == until {
                            break;
                        }
                        contents.push_str(line);
                        contents.push('\n');
                    }
                }
                if word == "config" || word == "kept" {
                    if !path.is_empty() || until.is_none() {
                        bail!("line {line_number}: `{word}` takes its lines: `{word} <<END`");
                    }
                    if word == "kept" {
                        Command::Kept(contents)
                    } else {
                        Command::Config(contents)
                    }
                } else {
                    let path = PathBuf::from(path);
                    if path.is_absolute() || path.components().any(|part| part.as_os_str() == "..")
                    {
                        bail!("line {line_number}: files go in the project");
                    }
                    Command::File(path, contents)
                }
            }
            "open" => Command::Open(rest.to_owned()),
            "keys" => Command::Keys(rest.to_owned()),
            // Spaces at the ends of text to type are kept.
            "type" => Command::Type(
                line.trim_start()[word.len()..]
                    .strip_prefix(' ')
                    .unwrap_or("")
                    .to_owned(),
            ),
            "shell" => Command::Shell(rest.to_owned()),
            "move" => {
                let at = numbers(2)?;
                Command::Move([at[0], at[1]])
            }
            "click" => {
                let at = numbers(2)?;
                let button = match rest.split_whitespace().nth(2) {
                    None | Some("left") => MouseButton::Left,
                    Some("right") => MouseButton::Right,
                    Some("middle") => MouseButton::Middle,
                    Some(other) => bail!("line {line_number}: no button called {other}"),
                };
                Command::Click([at[0], at[1]], button)
            }
            "drag" => {
                let at = numbers(4)?;
                Command::Drag([at[0], at[1]], [at[2], at[3]])
            }
            "scroll" => {
                let at = numbers(3)?;
                Command::Scroll([at[0], at[1]], at[2])
            }
            "wait-for" => Command::WaitFor(rest.to_owned()),
            "timeout" => Command::Timeout(millis()?),
            "quiet" => Command::Quiet(millis()?),
            "sleep" => Command::Sleep(millis()?),
            "shot" if !rest.is_empty() && !rest.contains('/') => Command::Shot(rest.to_owned()),
            "shot" => bail!("line {line_number}: `shot` takes a name, with no `/`"),
            _ => bail!("line {line_number}: no command called `{word}`"),
        };
        steps.push(Step {
            line: line_number,
            text: text.to_owned(),
            command,
        });
    }
    Ok(steps)
}

/// The daemon a script's window connects to, running on a thread of this
/// process.
struct Running {
    shutdown: ShutdownHandle,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Held while it runs, as a daemon holds it.
    _lock: clackatoo_daemon::DaemonLock,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shutdown.shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where the programs the daemon's tests use are: beside this one, as
/// cargo builds examples.
fn example(name: &str) -> anyhow::Result<PathBuf> {
    let program = std::env::current_exe()?.with_file_name(name);
    if program.is_file() {
        Ok(program)
    } else {
        bail!(
            "no {} to run: build it first, with `cargo build -p clackatoo-daemon --example \
             fake_agent --example fake_language_server`, and the profile this was built with",
            program.display()
        )
    }
}

/// Starts a daemon in the runtime directory the environment names, with
/// the fake agent and the fake language server, and a workspace with a
/// shell in `project`, or else the workspaces `kept`, made again as a
/// daemon starting afresh makes those it kept.
fn start_daemon(project: &Path, kept: Option<&str>) -> anyhow::Result<Running> {
    let runtime = RuntimeDir::for_current_user();
    let lock = runtime.lock()?;
    let home = PathBuf::from(std::env::var_os("HOME").context("no HOME")?);
    let agent = example("fake_agent")?;
    let mut config = DaemonConfig::from_env();
    config.udp_ports = None;
    config.web = None;
    config.workspaces = match kept {
        Some(kept) => {
            let dir = clackatoo_daemon::state_dir().context("no state directory")?;
            std::fs::create_dir_all(&dir)?;
            let file = dir.join("workspaces");
            std::fs::write(
                &file,
                kept.replace("$PROJECT", &project.display().to_string()),
            )?;
            Some(file)
        }
        None => None,
    };
    config.browser = None;
    config.servers = None;
    config.agents.custom = vec![CustomAgent {
        name: "claude".into(),
        title: None,
        command: vec![
            agent.clone().into(),
            "--log".into(),
            home.join("agent.log").into(),
        ],
        starting_mode: None,
    }];
    config.mcp = Some(vec![agent.into(), "mcp".into()]);
    config.languages = Some(languages(&home.join("languages"))?);
    let mut daemon = Daemon::bind(&runtime.socket_path(), config)?;
    daemon.install_shell_integration(runtime.path().join("shell"));
    if kept.is_some() {
        daemon.restore_workspaces();
    }
    let shutdown = daemon.shutdown_handle();
    let thread = std::thread::Builder::new()
        .name("daemon".into())
        .spawn(move || {
            if let Err(err) = daemon.run() {
                eprintln!("look: the daemon failed: {err}");
            }
        })?;
    let running = Running {
        shutdown,
        thread: Some(thread),
        _lock: lock,
    };
    if kept.is_some() {
        return Ok(running);
    }
    // The window opens on this workspace, rather than make one with a
    // shell at home.
    let mut client = Client::connect(&runtime.socket_path())?;
    client.new_workspace(
        None,
        PaneTerminal::New(NewTerminal {
            size: Size::new(80, 24).expect("80x24 is a size"),
            command: None,
            cwd: Some(project.display().to_string()),
            record_to: None,
        }),
    )?;
    Ok(running)
}

/// Makes the language packs files are parsed and coloured with in `dir`,
/// from the syntax tests' grammars: JSON, and `.fk` files read as the
/// curly test language, with the fake language server, and more of what
/// it says coloured than its tests need.
fn languages(dir: &Path) -> anyhow::Result<PathBuf> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../syntax/tests/fixtures/languages");
    // Copied, not linked: a pack is a directory, not a link to one.
    let copy = |from: &str, to: &str, files: &[&str]| -> anyhow::Result<()> {
        std::fs::create_dir_all(dir.join(to))?;
        for file in files {
            std::fs::copy(fixtures.join(from).join(file), dir.join(to).join(file))
                .with_context(|| format!("no {from}/{file} in {}", fixtures.display()))?;
        }
        Ok(())
    };
    copy(
        "json",
        "json",
        &["language.toml", "grammar.wasm", "highlights.scm"],
    )?;
    copy(
        "curly",
        "fake",
        &["grammar.wasm", "indents.scm", "textobjects.scm"],
    )?;
    std::fs::write(
        dir.join("fake/highlights.scm"),
        "[\"fn\" \"let\" \"if\" \"else\"] @keyword\n\
         (number) @number\n\
         (function name: (identifier) @function)\n\
         (call function: (identifier) @function.call)\n\
         (parameter) @variable.parameter\n\
         [\"(\" \")\" \"{\" \"}\"] @punctuation.bracket\n\
         [\",\" \";\"] @punctuation.delimiter\n\
         \"=\" @operator\n",
    )?;
    let server = example("fake_language_server")?;
    std::fs::write(
        dir.join("fake/language.toml"),
        format!(
            "grammar = \"curly\"\nextensions = [\"fk\"]\nserver = [{:?}]\nroots = [\"project\"]\n",
            server.display().to_string()
        ),
    )?;
    Ok(dir.to_owned())
}

/// What a script has set for the window, and the window once it's open.
struct Look {
    size: [u32; 2],
    scale: f64,
    timeout: Duration,
    quiet: Duration,
    project: PathBuf,
    out: PathBuf,
    window: Option<Window>,
    /// The config directory a `config` step wrote `init.lua` in, for the
    /// window to run; none until one does.
    config: Option<PathBuf>,
    timings: String,
}

impl Look {
    /// The window, opened as the script has set it so far if it isn't
    /// yet, once it shows the daemon's workspaces.
    fn window(&mut self) -> anyhow::Result<&mut Window> {
        if self.window.is_none() {
            let runtime = RuntimeDir::for_current_user().path().to_owned();
            let config = self.config.clone();
            let mut window = Window::open(runtime, self.size, self.scale, config)?;
            if !window.connect(self.timeout) {
                bail!(
                    "the window never showed the daemon's workspaces:\n{}",
                    window.text()
                );
            }
            window.settle(self.quiet, self.timeout);
            self.window = Some(window);
        }
        Ok(self.window.as_mut().expect("opened"))
    }

    /// Carries out `step`.
    #[allow(clippy::too_many_lines, reason = "one arm per command, each short")]
    fn run(&mut self, step: &Step) -> anyhow::Result<()> {
        let mut timed = true;
        match &step.command {
            Command::Size(size) => {
                self.size = *size;
                timed = false;
                if let Some(window) = &mut self.window {
                    window.resize(*size);
                }
            }
            Command::Scale(scale) => {
                self.scale = *scale;
                timed = false;
                if let Some(window) = &mut self.window {
                    window.rescale(*scale);
                }
            }
            // The workspaces kept were made again as the daemon started.
            Command::Theme | Command::Kept(_) => timed = false,
            Command::File(path, contents) => {
                timed = false;
                let path = self.project.join(path);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, contents)?;
            }
            Command::Config(contents) => {
                timed = false;
                if self.window.is_some() {
                    bail!("`config` comes before the window opens");
                }
                let dir = clackatoo_daemon::config_dir().context("no config directory")?;
                std::fs::create_dir_all(dir.join("client"))?;
                std::fs::write(dir.join("client/init.lua"), contents)?;
                self.config = Some(dir);
            }
            Command::Open(path) => {
                let notation = format!("<C-;>:Editor {}<CR>", path.replace('<', "<lt>"));
                self.window()?.keys(&notation).map_err(|err| anyhow!(err))?;
            }
            Command::Keys(keys) => self.window()?.keys(keys).map_err(|err| anyhow!(err))?,
            Command::Type(text) => self.window()?.type_text(text),
            Command::Shell(command) => {
                let window = self.window()?;
                window.type_text(command);
                window.keys("<CR>").map_err(|err| anyhow!(err))?;
            }
            Command::Move(at) => self.window()?.move_to(*at),
            Command::Click(at, button) => self.window()?.click(*at, *button),
            Command::Drag(from, to) => self.window()?.drag(*from, *to),
            Command::Scroll(at, notches) => self.window()?.scroll(*at, *notches),
            Command::WaitFor(text) => {
                timed = false;
                let timeout = self.timeout;
                let window = self.window()?;
                if !window.wait_for(text, timeout) {
                    bail!(
                        "line {}: the window never showed {text:?}. It shows:\n{}",
                        step.line,
                        window.text()
                    );
                }
            }
            Command::Timeout(time) => {
                timed = false;
                self.timeout = *time;
            }
            Command::Quiet(time) => {
                timed = false;
                self.quiet = *time;
            }
            Command::Sleep(time) => {
                timed = false;
                self.window()?.wait(*time);
            }
            Command::Shot(name) => {
                timed = false;
                let (quiet, timeout) = (self.quiet, self.timeout);
                let path = self.out.join(format!("{name}.png"));
                let window = self.window()?;
                let timing = window.settle(quiet, timeout);
                if !timing.settled {
                    eprintln!(
                        "look: line {}: the window never settled for {name}",
                        step.line
                    );
                }
                let picture = window.picture().context("nothing drawn")?;
                write_png(&path, picture.size, &picture.rgba)?;
                println!("  {}", path.display());
            }
        }
        if timed {
            let (quiet, timeout) = (self.quiet, self.timeout);
            let timing = self.window()?.settle(quiet, timeout);
            note(&mut self.timings, step, &timing);
        }
        if self.window.as_ref().is_some_and(Window::closed) {
            bail!("line {}: the window closed", step.line);
        }
        Ok(())
    }
}

impl Look {
    /// Writes what the window shows to `failed-LINE.png`, if it's open,
    /// to see why `step` failed.
    fn failed(&mut self, step: &Step) {
        let Some(window) = &mut self.window else {
            return;
        };
        window.settle(self.quiet, self.timeout);
        if let Some(picture) = window.picture() {
            let path = self.out.join(format!("failed-{}.png", step.line));
            match write_png(&path, picture.size, &picture.rgba) {
                Ok(()) => eprintln!("look: what the window showed is in {}", path.display()),
                Err(err) => eprintln!("look: couldn't write {}: {err:#}", path.display()),
            }
        }
    }
}

/// Notes how long `step` took to show, as a line of `timings.txt`.
fn note(timings: &mut String, step: &Step, timing: &Timing) {
    let ms = |time: Option<Duration>| {
        time.map_or_else(
            || "     -".to_owned(),
            |time| format!("{:6.1}", time.as_secs_f64() * 1000.0),
        )
    };
    let _ = writeln!(
        timings,
        "{:4}  {:>6}  {}  {}  {:3}  {}  {}{}",
        step.line,
        ms(Some(timing.input)),
        ms(timing.first),
        ms(timing.done),
        timing.frames,
        ms(Some(timing.slowest)),
        step.text,
        if timing.settled {
            ""
        } else {
            "  (never settled)"
        },
    );
}

/// Runs `script`, writing its shots and timings into `out`.
fn run_script(script: &Path, out: &Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(script)?;
    let steps = parse(&text)?;
    let home = PathBuf::from(std::env::var_os("HOME").context("no HOME")?);
    let project = home.join(script.file_stem().context("a script with no name")?);
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(out)?;
    let kept = steps.iter().find_map(|step| match &step.command {
        Command::Kept(kept) => Some(kept.as_str()),
        _ => None,
    });
    let daemon = start_daemon(&project, kept)?;
    let mut look = Look {
        size: [1280, 800],
        scale: 1.0,
        timeout: Duration::from_secs(10),
        quiet: Duration::from_millis(300),
        project,
        out: out.to_owned(),
        window: None,
        config: None,
        timings: format!(
            "# {}\n\
             # How long each step took to show, in milliseconds, from its first key\n\
             # or click. input: handing it all over; first: to the first frame that looked\n\
             # different; done: to the last one before the window settled; frames: drawn\n\
             # meanwhile; slowest: the longest of those to draw, on the CPU.\n\
             # Each frame is read back from the GPU to see whether it changed, and that's\n\
             # counted in; and this is a {} build. Compare steps with these, as the\n\
             # most each could take, not as what a person would see; a release build\n\
             # draws faster, and `--profile quick` comes near it.\n\
             # line   input   first    done  frames slowest  step\n",
            script.display(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "optimised"
            },
        ),
    };
    let started = Instant::now();
    let result = steps.iter().try_for_each(|step| {
        let ran = look.run(step);
        if ran.is_err() {
            look.failed(step);
        }
        ran
    });
    std::fs::write(out.join("timings.txt"), &look.timings)?;
    // The window goes before its daemon, as a window closing does.
    drop(look);
    drop(daemon);
    result?;
    println!("  took {:.1?}", started.elapsed());
    Ok(())
}

/// Writes `rgba`, `size` pixels, to `path` as a PNG.
fn write_png(path: &Path, size: [u32; 2], rgba: &[u8]) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), size[0], size[1]);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// Writes what's logged to stderr, a line each: warnings and errors,
/// unless `LOOK_LOG` names another level, such as `info` or `debug`.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // The GPU's drivers warn of every device they can't open, which
        // says nothing of the window.
        if metadata.target().starts_with("wgpu") {
            metadata.level() <= log::Level::Error
        } else {
            metadata.level() <= log::max_level()
        }
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

// Run with `cargo test -p clackatoo-gui --example look`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_takes_its_lines_and_nothing_else() {
        let steps = parse("config <<END\nclient.opt.wrap = false\nEND\nshot a\n").unwrap();
        assert!(matches!(
            &steps[0].command,
            Command::Config(text) if text == "client.opt.wrap = false\n"
        ));
        assert!(matches!(&steps[1].command, Command::Shot(_)));
        assert!(parse("config\n").is_err(), "no lines");
        assert!(parse("config init.lua <<END\nEND\n").is_err(), "a path");
        assert!(parse("config <<END\nx\n").is_err(), "no end");
    }
}
