//! The `clackatoo` command: runs the daemon, and drives it from the shell.

#![forbid(unsafe_code)]

mod config;
mod connections;
mod desktop;
mod display;
mod language;
mod logs;
mod ping;
mod render;
mod update;
mod web;
mod web_app;

use std::ffi::OsString;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_daemon::remote::{self, Remote, RemoteError};
use clackatoo_daemon::{Client, ClientError, DaemonConfig, RuntimeDir, launch, upgrade};
use clackatoo_protocol::{
    NewTerminal, OpenFile, PaneTerminal, Request, Response, Size, TerminalId,
};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// This build's version, the commit it's from, and the day it was built:
/// `2026.10.0 (1a2b3c4d5e 2026-10-13)`. See `build.rs`.
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("CLACKATOO_GIT_HASH"),
    " ",
    env!("CLACKATOO_BUILD_DATE"),
    ")"
);

/// A fast, lightweight terminal workspace.
///
/// With no command, opens a window on your workspaces, starting the daemon
/// first if it isn't running. With none yet, the window makes one, showing
/// your newest terminal or a new one. With `--remote`, the workspaces are
/// on another machine.
#[derive(Debug, Parser)]
#[command(version = VERSION, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    window: WindowArgs,
}

/// The window's options. Each defaults to the setting in
/// `client/startup.toml` (see `clackatoo config`); the font and its size,
/// to the `font` and `fontsize` options in `client/init.lua` first.
#[derive(Debug, Args)]
struct WindowArgs {
    /// The font family to draw text with, outdoing `client.opt.font` in
    /// config. Defaults to your system's monospace font.
    #[arg(long, value_name = "FAMILY", env = "CLACKATOO_FONT")]
    font: Option<String>,
    /// The text size, in points, outdoing `client.opt.fontsize` in config.
    /// Defaults to 12.
    #[arg(long, value_name = "POINTS", env = "CLACKATOO_FONT_SIZE")]
    font_size: Option<f32>,
    /// The graphics API to draw with. Defaults to Vulkan.
    #[arg(long, value_name = "API", env = "CLACKATOO_RENDERER")]
    renderer: Option<RendererArg>,
    /// Open as a picker of the machine to connect to, as `--remote` does:
    /// one connected to recently, one from your ssh config, a Tailscale
    /// peer that's online, or one typed as `user@host:port`. The app
    /// launcher's "Connect to…" runs this.
    #[arg(long, conflicts_with = "remote")]
    connect_picker: bool,
    #[command(flatten)]
    remote: RemoteArgs,
}

/// [`clackatoo_gui::GraphicsApi`], as the command line spells it.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum RendererArg {
    Vulkan,
    #[value(name = "opengl")]
    OpenGl,
}

impl From<RendererArg> for clackatoo_gui::GraphicsApi {
    fn from(renderer: RendererArg) -> Self {
        match renderer {
            RendererArg::Vulkan => Self::Vulkan,
            RendererArg::OpenGl => Self::OpenGl,
        }
    }
}

#[derive(Debug, Args)]
struct RemoteArgs {
    /// Use the daemon on another machine, reached with ssh, starting it
    /// there if it isn't running. HOST is anything ssh takes: `host`,
    /// `user@host`, or a name from your ssh config.
    #[arg(long, value_name = "HOST")]
    remote: Option<String>,
    /// The shell command that runs clackatoo on the remote machine, if it
    /// isn't on the PATH there.
    #[arg(
        long,
        value_name = "COMMAND",
        default_value = "clackatoo",
        env = "CLACKATOO_REMOTE_COMMAND"
    )]
    remote_command: String,
}

impl RemoteArgs {
    fn remote(&self) -> Option<Remote> {
        self.remote.as_ref().map(|host| Remote {
            host: host.clone(),
            command: self.remote_command.clone(),
        })
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the daemon in the foreground. It owns every terminal, so
    /// terminals live exactly as long as it does.
    Daemon(DaemonArgs),
    /// List terminals.
    List(Connection),
    /// Start a terminal and print its ID.
    New(New),
    /// Edit a file, or browse a folder as its listing, in a new tab of the
    /// workspace last used.
    Edit(Edit),
    /// Type a command into a terminal and press Enter.
    Run(Run),
    /// Start a command in a new background tab of a workspace, where it
    /// keeps running out of the tab bar, and print its terminal's ID. It's
    /// typed at a new shell's prompt, as you would type it.
    Bg(Bg),
    /// Send text to a terminal exactly as given, without pressing Enter.
    Send(Send),
    /// Print what a terminal is showing.
    Capture(Capture),
    /// Close a terminal, hanging up its program.
    Close(Target),
    /// Time the daemon's answers, and keys coming back from a program, as
    /// a window would type them. Most useful with `--remote`.
    Ping(Ping),
    /// Change a terminal's size. Its program is told, and lines on screen
    /// are rewrapped to the new width.
    Resize(Resize),
    /// Stop the daemon, closing every terminal and hanging up their
    /// programs. Works on a daemon from any version of clackatoo.
    Stop,
    /// Move the running daemon onto this build of clackatoo, keeping its
    /// terminals and their programs running. Daemons from before protocol
    /// 11 can't be upgraded, only stopped.
    Upgrade,
    /// Install the latest release in place of this build, checking it's
    /// signed with clackatoo's release key, then upgrade the daemon onto
    /// it. The build it replaces is kept, for `--rollback`.
    Update(update::UpdateArgs),
    /// Show or change the window's settings in client/startup.toml. With no
    /// arguments, shows them all.
    Config(ConfigArgs),
    /// The languages files are parsed and coloured in.
    #[command(subcommand)]
    Language(language::LanguageCommand),
    /// Put clackatoo in your desktop's app launcher, or take it out.
    #[command(subcommand)]
    Desktop(desktop::DesktopCommand),
    /// The web app phones reach the daemon through, set up in
    /// daemon/web.toml.
    #[command(subcommand)]
    Web(web::WebCommand),
    /// List the devices paired with the web app, or allow one asking to
    /// pair, turn them down, or unpair one.
    Devices(web::DevicesArgs),
    /// Relay between standard input and output and the daemon, starting it
    /// if it isn't running. `--remote` runs this on the remote machine,
    /// through ssh.
    Bridge(BridgeArgs),
    /// Serve a chat's tools to its agent, as an MCP server on standard
    /// input and output. The agent runs this itself.
    #[command(hide = true)]
    Mcp(McpArgs),
    /// Connect the daemon to the daemon on another machine, over ssh,
    /// answering ssh's questions here, and keep it connected, trying again
    /// whenever the connection is lost, until `disconnect`.
    #[command(hide = true)]
    Connect(connections::ConnectArgs),
    /// End the daemon's connection to another machine.
    #[command(hide = true)]
    Disconnect(connections::DisconnectArgs),
    /// Forget the identity key the daemon knows another machine by, as
    /// after that machine's clackatoo chose a new one: the key it shows
    /// next is taken, and kept.
    Forget(connections::DisconnectArgs),
    /// List the daemon's connections to other machines, and how each is
    /// going.
    #[command(hide = true)]
    Connections(Connection),
    /// Print which machine the daemon is: its name and its ID.
    #[command(hide = true)]
    Machine(Connection),
    /// List the workspaces this daemon lent the daemons on other machines,
    /// holding the panes they show of this machine's, or give one back.
    Lent(connections::LentArgs),
    /// Show the desktop's display the daemon on this machine gives the
    /// programs it starts (`WAYLAND_DISPLAY` and the like), and where it came
    /// from: the desktop you're logged in to, the daemon's own
    /// environment, or none.
    Display,
}

#[derive(Debug, Args)]
struct McpArgs {
    /// The chat whose agent the tools are for.
    #[arg(long, value_name = "ID")]
    chat: u32,
    /// The daemon's socket. Defaults to the one in your runtime directory.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    /// The setting: font, font-size or renderer.
    name: Option<String>,
    /// The value to set it to. Without one, prints its value.
    #[arg(conflicts_with = "unset")]
    value: Option<String>,
    /// Remove the setting, so it takes its default.
    #[arg(long, requires = "name")]
    unset: bool,
}

#[derive(Debug, Args)]
struct BridgeArgs {
    /// Stop the daemon that's running first, closing its terminals, as
    /// when it's from another version of clackatoo.
    #[arg(long)]
    replace: bool,
}

#[derive(Debug, Args)]
struct DaemonArgs {
    /// Take over from an older build, which exec'd this one and handed
    /// over its state on standard input.
    #[arg(long, hide = true)]
    resume: bool,
    /// Print the handover versions this build can take over from, and
    /// that it takes signals held back for it, instead of running the
    /// daemon: an older build asks before upgrading to it.
    #[arg(long, hide = true)]
    handover_versions: bool,
    /// When scrollback is compacted to save memory: `lazy` once output
    /// pauses, `eager` as each line scrolls off, or `off` for never, which
    /// is fastest but takes about 8 bytes per column for every line.
    #[arg(long, value_name = "WHEN", default_value = "lazy")]
    scrollback_compaction: Compaction,
    /// Start with no workspaces, this once, rather than making again the
    /// workspaces, tabs and panes kept from the daemon's last run (in
    /// `workspaces`, in clackatoo's state directory). That file is left as
    /// it is until something changes; from then on, what this daemon has
    /// is kept instead.
    #[arg(long)]
    fresh: bool,
}

/// [`clackatoo_term::Compaction`], as the command line spells it.
#[derive(Clone, Copy, Debug, ValueEnum)]
enum Compaction {
    Off,
    Lazy,
    Eager,
}

impl From<Compaction> for clackatoo_term::Compaction {
    fn from(compaction: Compaction) -> Self {
        match compaction {
            Compaction::Off => Self::Off,
            Compaction::Lazy => Self::Lazy,
            Compaction::Eager => Self::Eager,
        }
    }
}

#[derive(Debug, Args)]
struct Connection {
    /// The daemon's socket. Defaults to the one in your runtime directory.
    #[arg(long, value_name = "PATH", conflicts_with = "remote")]
    socket: Option<PathBuf>,
    #[command(flatten)]
    remote: RemoteArgs,
}

#[derive(Debug, Args)]
struct Target {
    /// The terminal's ID, as printed by `new` and `list`.
    terminal: u32,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
struct New {
    /// The terminal's size in columns and rows, like 80x24.
    #[arg(long, default_value = "80x24", value_parser = parse_size)]
    size: Size,
    /// The working directory. Defaults to the current one.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// Record everything the program prints to this file, for tests and
    /// benchmarks. The path is on the daemon's machine.
    #[arg(long, value_name = "FILE")]
    record: Option<PathBuf>,
    /// The program to run, and its arguments. Defaults to the daemon's $SHELL.
    #[arg(last = true, value_name = "COMMAND")]
    command: Vec<String>,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
struct Bg {
    /// The workspace, by its name. Defaults to the one last used.
    #[arg(long, value_name = "NAME")]
    workspace: Option<String>,
    /// The working directory. Defaults to the current one.
    #[arg(long, value_name = "DIR")]
    cwd: Option<PathBuf>,
    /// The terminal's size in columns and rows, like 80x24.
    #[arg(long, default_value = "80x24", value_parser = parse_size)]
    size: Size,
    /// The command line to type. Several arguments are joined with spaces.
    #[arg(last = true, required = true, value_name = "COMMAND")]
    command: Vec<String>,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
struct Edit {
    /// The file, or folder. A relative path is from the current directory,
    /// which is the folder shown if there's none.
    #[arg(default_value = ".")]
    path: PathBuf,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
struct Ping {
    /// How many rounds to time.
    #[arg(short, long, default_value_t = 10)]
    count: u32,
    /// The pause before each request and each key, in milliseconds, like
    /// the gap between keys when typing.
    #[arg(short, long, value_name = "MS", default_value_t = 200)]
    interval: u64,
    /// Keep going until interrupted, typing a key each interval, logging
    /// each echo's time, how the connection goes, and every drop with its
    /// reason, and connecting again after one. For testing how a network
    /// treats a connection to another machine.
    #[arg(long, conflicts_with = "count")]
    soak: bool,
    #[command(flatten)]
    connection: Connection,
}

#[derive(Debug, Args)]
struct Run {
    #[command(flatten)]
    target: Target,
    /// The command line to type. Several arguments are joined with spaces.
    #[arg(required = true, value_name = "COMMAND")]
    words: Vec<String>,
}

#[derive(Debug, Args)]
struct Send {
    #[command(flatten)]
    target: Target,
    /// The text to send, byte for byte. Use your shell's escapes for control
    /// keys, such as `\e` for Escape in fish or `$'\e'` in bash.
    text: String,
}

#[derive(Debug, Args)]
struct Resize {
    #[command(flatten)]
    target: Target,
    /// The new size in columns and rows, like 120x40.
    #[arg(value_parser = parse_size)]
    size: Size,
}

#[derive(Debug, Args)]
struct Capture {
    #[command(flatten)]
    target: Target,
    /// Keep colours and attributes, as ANSI escape sequences.
    #[arg(long)]
    ansi: bool,
}

fn main() -> ExitCode {
    // Run by ssh as its askpass program, for a window: the question is the
    // only argument, and the window answers it.
    if let Some(socket) = std::env::var_os(remote::ASKPASS_VAR) {
        let question = std::env::args().nth(1).unwrap_or_default();
        return remote::ask(Path::new(&socket), &question);
    }
    let cli = Cli::parse();
    let result = match cli.command {
        Some(command) => run(command),
        None => open_window(&cli.window),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("clackatoo: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::too_many_lines, reason = "one arm per command")]
fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Daemon(args) => run_daemon(&args),
        Command::List(connection) => list(&connection),
        Command::New(new) => new_terminal(new),
        Command::Bg(bg) => start_in_background(bg),
        Command::Edit(edit) => edit_file(edit),
        Command::Language(command) => language::run(command),
        Command::Desktop(command) => desktop::run(command),
        Command::Web(command) => web::run(&command),
        Command::Devices(args) => web::devices(&args),
        Command::Run(run) => {
            let mut line = run.words.join(" ").into_bytes();
            line.push(b'\r');
            send(&run.target, &line)
        }
        Command::Send(send_args) => send(&send_args.target, send_args.text.as_bytes()),
        Command::Capture(capture_args) => capture(&capture_args),
        Command::Close(target) => {
            connect(&target.connection)?.close_terminal(TerminalId(target.terminal))?;
            Ok(())
        }
        Command::Resize(resize) => {
            let target = &resize.target;
            connect(&target.connection)?.resize(TerminalId(target.terminal), resize.size)?;
            Ok(())
        }
        Command::Stop => {
            if !launch::stop(&RuntimeDir::for_current_user())? {
                eprintln!("clackatoo: no daemon is running");
            }
            Ok(())
        }
        Command::Upgrade => {
            let mut client = launch::upgrade(&RuntimeDir::for_current_user())?;
            let terminals = client.list_terminals()?.len();
            println!(
                "the daemon is now running {}, and kept its {terminals} terminal{}",
                std::env::current_exe()?.display(),
                if terminals == 1 { "" } else { "s" }
            );
            Ok(())
        }
        Command::Update(args) => update::run(&args),
        Command::Ping(args) if args.soak => {
            logs::command("info");
            let daemon =
                (args.connection.remote.remote.clone()).unwrap_or_else(|| "the daemon".into());
            ping::soak(
                || connect(&args.connection),
                &daemon,
                Duration::from_millis(args.interval),
            )
        }
        Command::Ping(args) => {
            let started = Instant::now();
            let mut client = connect(&args.connection)?;
            let daemon = args
                .connection
                .remote
                .remote
                .clone()
                .unwrap_or_else(|| "the daemon".into());
            println!(
                "connected to {daemon} in {:.1} ms",
                started.elapsed().as_secs_f64() * 1000.0
            );
            ping::ping(
                &mut client,
                &daemon,
                &ping::Rounds {
                    count: args.count,
                    interval: Duration::from_millis(args.interval),
                },
            )
        }
        Command::Config(args) => {
            let path = config::path().context("neither $XDG_CONFIG_HOME nor $HOME is set")?;
            match (args.name, args.value) {
                (None, _) => print!("{}", config::show_all(&path)?),
                (Some(name), _) if args.unset => config::unset(&path, &name)?,
                (Some(name), None) => println!("{}", config::get(&path, &name)?),
                (Some(name), Some(value)) => config::set(&path, &name, &value)?,
            }
            Ok(())
        }
        Command::Bridge(args) => {
            remote::bridge(&RuntimeDir::for_current_user(), args.replace)?;
            Ok(())
        }
        Command::Connect(args) => connections::connect(&args),
        Command::Disconnect(args) => connections::disconnect(&args),
        Command::Forget(args) => connections::forget(&args),
        Command::Connections(connection) => connections::list(&connection),
        Command::Lent(args) => connections::lent(&args),
        Command::Machine(connection) => connections::machine(&connection),
        Command::Display => display::show(),
        Command::Mcp(args) => {
            let socket =
                (args.socket).unwrap_or_else(|| RuntimeDir::for_current_user().socket_path());
            // The raw descriptors: the standard library's stdout holds
            // bytes back until a newline.
            let input = std::fs::File::from(io::stdin().as_fd().try_clone_to_owned()?);
            let output = std::fs::File::from(io::stdout().as_fd().try_clone_to_owned()?);
            clackatoo_daemon::mcp::relay(&socket, TerminalId(args.chat), input, output)?;
            Ok(())
        }
    }
}

/// The command agents start to reach the tools clackatoo gives them: this
/// program's `mcp`, unless `$CLACKATOO_AGENT_TOOLS` is `off`.
fn agent_tools() -> Option<Vec<OsString>> {
    if std::env::var_os("CLACKATOO_AGENT_TOOLS").is_some_and(|tools| tools == "off") {
        return None;
    }
    let program = std::env::current_exe()
        .inspect_err(|err| log::warn!("agents get no tools: this program can't be found: {err}"))
        .ok()?;
    Some(vec![program.into_os_string(), "mcp".into()])
}

/// What the window draws text with as it opens, from the command line
/// (or environment), else startup.toml, else the defaults; and which of
/// those the command line gave, which `client.opt` in config doesn't
/// change. Config's own come between: over startup.toml's, under the
/// command line's.
fn window_font(
    args: &WindowArgs,
    config: &config::ClientConfig,
) -> (clackatoo_gui::FontOptions, clackatoo_gui::FontFixed) {
    let font = clackatoo_gui::FontOptions {
        family: args.font.clone().or_else(|| config.font.clone()),
        size: args.font_size.or(config.font_size).unwrap_or(12.0),
    };
    let fixed = clackatoo_gui::FontFixed {
        family: args.font.is_some(),
        size: args.font_size.is_some(),
    };
    (font, fixed)
}

fn open_window(args: &WindowArgs) -> anyhow::Result<()> {
    logs::window();
    let config = config::load();
    let (font, font_fixed) = window_font(args, &config);
    clackatoo_gui::run(clackatoo_gui::Options {
        runtime: RuntimeDir::for_current_user(),
        remote: args.remote.remote(),
        pick_remote: args.connect_picker,
        remote_command: args.remote.remote_command.clone(),
        font,
        font_fixed,
        graphics_api: args
            .renderer
            .map(Into::into)
            .or(config.renderer)
            .unwrap_or_default(),
        config: config::dir(),
    })
}

fn run_daemon(args: &DaemonArgs) -> anyhow::Result<()> {
    if args.handover_versions {
        println!("{}", upgrade::versions_answer());
        return Ok(());
    }
    // Before any thread starts, so its shells don't inherit a launcher's
    // spent token.
    clackatoo_gui::forget_launch_token();
    logs::daemon();
    let runtime = RuntimeDir::for_current_user();
    let mut config = DaemonConfig::from_env();
    config.terminal.scrollback_compaction = args.scrollback_compaction.into();
    config.mcp = agent_tools();
    // ssh asks its questions, connecting to other daemons, through this
    // program, as it does for a window.
    if let Some(links) = &mut config.links {
        links.askpass = std::env::current_exe()
            .inspect_err(|err| log::warn!("ssh can't ask its questions: {err}"))
            .ok();
    }
    if let Some(web) = &mut config.web {
        web.assets = web_app::ASSETS;
    }
    // An upgrade runs the next build with the same arguments, but for
    // `--fresh`, which was for this start alone.
    let rerun: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != upgrade::RESUME_FLAG && arg != "--fresh")
        .collect();
    upgrade::serve(&runtime, &config, args.resume, !args.fresh, &rerun)
        .with_context(|| format!("could not serve {}", runtime.socket_path().display()))
}

/// Why a daemon of another version can't be used, and what to do about it.
fn other_version(daemon: u32, ours: u32, on_host: &str) -> String {
    let what = format!(
        "the daemon{on_host} is from another version of clackatoo (protocol {daemon}, this is \
         {ours})"
    );
    if daemon > ours {
        format!("{what}; use a newer clackatoo")
    } else if on_host.is_empty() && launch::can_upgrade(daemon) {
        format!("{what}; `clackatoo upgrade` moves it onto this one, keeping its terminals")
    } else {
        format!("{what}; `clackatoo stop`{on_host} stops it, closing its terminals")
    }
}

fn connect(connection: &Connection) -> anyhow::Result<Client> {
    if let Some(remote) = connection.remote.remote() {
        // ssh asks for any password on this terminal.
        return match remote::connect(&remote, false, None) {
            Ok(client) => Ok(client),
            Err(RemoteError::Client(ClientError::VersionMismatch { daemon, ours })) => {
                anyhow::bail!(other_version(daemon, ours, &format!(" on {}", remote.host)))
            }
            Err(err) => Err(err.into()),
        };
    }
    let socket = connection
        .socket
        .clone()
        .unwrap_or_else(|| RuntimeDir::for_current_user().socket_path());
    match Client::connect(&socket) {
        Ok(client) => Ok(client),
        Err(ClientError::Connect(err))
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            anyhow::bail!(
                "no daemon is listening on {}; start one with `clackatoo daemon`",
                socket.display()
            )
        }
        Err(ClientError::VersionMismatch { daemon, ours }) => {
            anyhow::bail!(other_version(daemon, ours, ""))
        }
        Err(err) => Err(err.into()),
    }
}

fn list(connection: &Connection) -> anyhow::Result<()> {
    let terminals = connect(connection)?.list_terminals()?;
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{:>4}  {:>8}  {:>9}  {:<16}  TITLE",
        "ID", "PID", "SIZE", "STATUS"
    )?;
    for terminal in terminals {
        writeln!(
            out,
            "{:>4}  {:>8}  {:>9}  {:<16}  {}",
            terminal.id,
            terminal
                .pid
                .map_or_else(|| "-".to_owned(), |pid| pid.to_string()),
            terminal.size.to_string(),
            terminal.status.to_string(),
            terminal.title
        )?;
    }
    Ok(())
}

fn new_terminal(new: New) -> anyhow::Result<()> {
    let cwd = match new.cwd {
        Some(cwd) => Some(cwd),
        None => Some(std::env::current_dir().context("could not read the current directory")?),
    };
    let request = NewTerminal {
        size: new.size,
        command: (!new.command.is_empty()).then_some(new.command),
        cwd: cwd.map(|cwd| path_string(cwd, "--cwd")).transpose()?,
        record_to: new
            .record
            .map(|path| path_string(path, "--record"))
            .transpose()?,
    };
    let id = connect(&new.connection)?.new_terminal(request)?;
    println!("{id}");
    Ok(())
}

fn start_in_background(bg: Bg) -> anyhow::Result<()> {
    let cwd = match bg.cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().context("could not read the current directory")?,
    };
    let mut client = connect(&bg.connection)?;
    if !client.daemon_has(clackatoo_protocol::wire::BACKGROUND) {
        anyhow::bail!(
            "the daemon is too old for background tabs: update it, and run `clackatoo upgrade`"
        );
    }
    let workspaces = client.watch_workspaces()?;
    // This daemon's own: another machine's keeps its background tabs.
    let own = |workspace: &&clackatoo_protocol::Workspace| {
        workspace.home.is_none() && workspace.lent.is_none()
    };
    let workspace = match &bg.workspace {
        Some(name) => (workspaces.list.iter().filter(own))
            .find(|workspace| workspace.name == *name)
            .ok_or_else(|| anyhow::anyhow!("there's no workspace called {name:?}"))?,
        None => (workspaces.active)
            .and_then(|active| workspaces.get(active))
            .filter(own)
            .or_else(|| workspaces.list.iter().find(own))
            .ok_or_else(|| anyhow::anyhow!("there's no workspace yet: open a window first"))?,
    };
    let start = clackatoo_protocol::BackgroundAction::Start {
        workspace: workspace.id,
        command: bg.command.join(" "),
        cwd: Some(path_string(cwd, "--cwd")?),
        pane: None,
        size: bg.size,
    };
    let pane = match client.request(&Request::Background(start))? {
        Response::PaneCreated(pane) => pane,
        Response::Error(err) => anyhow::bail!("{err}"),
        other => anyhow::bail!("the daemon answered {other:?}"),
    };
    let workspaces = client.watch_workspaces()?;
    let terminal = (workspaces.find_background_pane(pane))
        .and_then(|(_, _, pane)| pane.content.terminal())
        .ok_or_else(|| anyhow::anyhow!("its terminal closed at once"))?;
    println!("{terminal}");
    Ok(())
}

fn edit_file(edit: Edit) -> anyhow::Result<()> {
    let cwd = std::env::current_dir().context("could not read the current directory")?;
    let file = PaneTerminal::File(OpenFile {
        path: path_string(edit.path, "the path")?,
        cwd: Some(path_string(cwd, "the current directory")?),
    });
    let mut client = connect(&edit.connection)?;
    let workspaces = client.watch_workspaces()?;
    let active =
        (workspaces.active).or_else(|| workspaces.list.first().map(|workspace| workspace.id));
    let pane = match active {
        Some(workspace) => client.new_tab(workspace, file)?,
        None => client.new_workspace(None, file)?,
    };
    client.focus(pane)?;
    Ok(())
}

fn send(target: &Target, bytes: &[u8]) -> anyhow::Result<()> {
    connect(&target.connection)?.send_input(TerminalId(target.terminal), bytes)?;
    Ok(())
}

fn capture(capture: &Capture) -> anyhow::Result<()> {
    let target = &capture.target;
    let view = connect(&target.connection)?.capture(TerminalId(target.terminal))?;
    let text = match (view.grid(), view.chat()) {
        (Some(grid), _) if capture.ansi => render::ansi(grid),
        (Some(grid), _) => render::plain(grid),
        // A chat's turns, as text.
        (None, Some(chat)) => chat.text(),
        (None, None) => anyhow::bail!("the terminal shows something this build can't print"),
    };
    io::stdout().lock().write_all(text.as_bytes())?;
    Ok(())
}

/// Paths travel as UTF-8 strings, because the daemon may be on another
/// machine with different path conventions.
fn path_string(path: PathBuf, option: &str) -> anyhow::Result<String> {
    path.into_os_string().into_string().map_err(|path| {
        anyhow::anyhow!(
            "{option} {} is not valid UTF-8",
            PathBuf::from(path).display()
        )
    })
}

fn parse_size(text: &str) -> Result<Size, String> {
    let (cols, rows) = text
        .split_once('x')
        .ok_or_else(|| format!("{text:?} is not COLSxROWS, like 80x24"))?;
    let number = |part: &str| {
        part.parse::<u16>()
            .map_err(|_| format!("{part:?} is not a number"))
    };
    Size::new(number(cols)?, number(rows)?).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("80x24"), Ok(Size::new(80, 24).unwrap()));
        assert!(parse_size("80").is_err());
        assert!(parse_size("0x24").is_err());
        assert!(parse_size("ax24").is_err());
    }

    #[test]
    fn the_cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn the_font_on_the_command_line_outdoes_startup_toml_and_is_fixed_for_config() {
        use clap::CommandFactory;

        // Made, not parsed, as parsing reads the environment, which may
        // have CLACKATOO_FONT in it.
        let window = |args: &[&str]| {
            let (mut font, mut font_size) = (None, None);
            for pair in args.chunks(2) {
                match pair {
                    ["--font", family] => font = Some((*family).to_owned()),
                    ["--font-size", size] => font_size = Some(size.parse().unwrap()),
                    _ => unreachable!(),
                }
            }
            WindowArgs {
                font,
                font_size,
                renderer: None,
                connect_picker: false,
                remote: RemoteArgs {
                    remote: None,
                    remote_command: "clackatoo".into(),
                },
            }
        };
        let file = config::ClientConfig {
            font: Some("Iosevka".into()),
            font_size: Some(13.5),
            renderer: None,
        };
        let (font, fixed) = window_font(&window(&[]), &file);
        assert_eq!((font.family.as_deref(), font.size), (Some("Iosevka"), 13.5));
        assert_eq!(fixed, clackatoo_gui::FontFixed::default());

        let (font, fixed) = window_font(&window(&["--font", "JetBrains Mono"]), &file);
        assert_eq!(
            (font.family.as_deref(), font.size),
            (Some("JetBrains Mono"), 13.5)
        );
        assert!(fixed.family && !fixed.size);

        let (font, fixed) = window_font(
            &window(&["--font-size", "15"]),
            &config::ClientConfig::default(),
        );
        assert_eq!((font.family, font.size), (None, 15.0));
        assert!(!fixed.family && fixed.size);

        let (font, _) = window_font(&window(&[]), &config::ClientConfig::default());
        assert_eq!((font.family, font.size), (None, 12.0));

        // The environment gives them as the flags do, so they're fixed too.
        let command = Cli::command();
        let env = |id: &str| {
            let arg = command.get_arguments().find(|arg| arg.get_id() == id);
            arg.and_then(clap::Arg::get_env)
                .map(|env| env.to_string_lossy().into_owned())
        };
        assert_eq!(env("font").as_deref(), Some("CLACKATOO_FONT"));
        assert_eq!(env("font_size").as_deref(), Some("CLACKATOO_FONT_SIZE"));
    }

    #[test]
    fn the_connect_picker_is_a_window_option_that_names_no_machine() {
        let cli = Cli::try_parse_from(["clackatoo", "--connect-picker"]).unwrap();
        assert!(cli.window.connect_picker && cli.command.is_none());
        assert!(Cli::try_parse_from(["clackatoo", "--connect-picker", "--remote", "x"]).is_err());
    }
}
