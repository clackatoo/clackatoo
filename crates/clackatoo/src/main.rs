//! The `clackatoo` command: runs the daemon, and drives it from the shell.

#![forbid(unsafe_code)]

mod config;
mod ping;
mod render;

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_daemon::remote::{self, Remote, RemoteError};
use clackatoo_daemon::{Client, ClientError, DaemonConfig, RuntimeDir, launch, upgrade};
use clackatoo_protocol::{NewTerminal, Size, TerminalId};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// A fast, lightweight terminal workspace.
///
/// With no command, opens a window on your workspaces, starting the daemon
/// first if it isn't running. With none yet, the window makes one, showing
/// your newest terminal or a new one. With `--remote`, the workspaces are
/// on another machine.
#[derive(Debug, Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    window: WindowArgs,
}

/// The window's options. Each defaults to the setting in `client.toml`
/// (see `clackatoo config`).
#[derive(Debug, Args)]
struct WindowArgs {
    /// The font family to draw text with. Defaults to your system's
    /// monospace font.
    #[arg(long, value_name = "FAMILY", env = "CLACKATOO_FONT")]
    font: Option<String>,
    /// The text size, in points. Defaults to 12.
    #[arg(long, value_name = "POINTS", env = "CLACKATOO_FONT_SIZE")]
    font_size: Option<f32>,
    /// The graphics API to draw with. Defaults to Vulkan.
    #[arg(long, value_name = "API", env = "CLACKATOO_RENDERER")]
    renderer: Option<RendererArg>,
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
    /// Type a command into a terminal and press Enter.
    Run(Run),
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
    /// Show or change the window's settings in client.toml. With no
    /// arguments, shows them all.
    Config(ConfigArgs),
    /// Print the handover versions this build can take over from, for a
    /// daemon checking it before upgrading to it.
    #[command(name = "handover-versions", hide = true)]
    HandoverVersions,
    /// Relay between standard input and output and the daemon, starting it
    /// if it isn't running. `--remote` runs this on the remote machine,
    /// through ssh.
    Bridge(BridgeArgs),
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
    /// When scrollback is compacted to save memory: `lazy` once output
    /// pauses, `eager` as each line scrolls off, or `off` for never, which
    /// is fastest but takes about 8 bytes per column for every line.
    #[arg(long, value_name = "WHEN", default_value = "lazy")]
    scrollback_compaction: Compaction,
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
struct Ping {
    /// How many rounds to time.
    #[arg(short, long, default_value_t = 10)]
    count: u32,
    /// The pause before each request and each key, in milliseconds, like
    /// the gap between keys when typing.
    #[arg(short, long, value_name = "MS", default_value_t = 200)]
    interval: u64,
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

fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Daemon(args) => run_daemon(&args),
        Command::List(connection) => list(&connection),
        Command::New(new) => new_terminal(new),
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
        Command::HandoverVersions => {
            let versions: Vec<String> = upgrade::readable_versions()
                .iter()
                .map(u32::to_string)
                .collect();
            println!("{}", versions.join(" "));
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
    }
}

fn open_window(args: &WindowArgs) -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let config = config::load();
    clackatoo_gui::run(clackatoo_gui::Options {
        runtime: RuntimeDir::for_current_user(),
        remote: args.remote.remote(),
        font: clackatoo_gui::FontOptions {
            family: args.font.clone().or(config.font),
            size: args.font_size.or(config.font_size).unwrap_or(12.0),
        },
        graphics_api: args
            .renderer
            .map(Into::into)
            .or(config.renderer)
            .unwrap_or_default(),
    })
}

fn run_daemon(args: &DaemonArgs) -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let runtime = RuntimeDir::for_current_user();
    let mut config = DaemonConfig::from_env();
    config.terminal.scrollback_compaction = args.scrollback_compaction.into();
    // An upgrade runs the next build with the same arguments.
    let rerun: Vec<OsString> = std::env::args_os()
        .skip(1)
        .filter(|arg| arg != upgrade::RESUME_FLAG)
        .collect();
    upgrade::serve(&runtime, &config, args.resume, &rerun)
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
            terminal.pid,
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

fn send(target: &Target, bytes: &[u8]) -> anyhow::Result<()> {
    connect(&target.connection)?.send_input(TerminalId(target.terminal), bytes)?;
    Ok(())
}

fn capture(capture: &Capture) -> anyhow::Result<()> {
    let target = &capture.target;
    let view = connect(&target.connection)?.capture(TerminalId(target.terminal))?;
    let grid = view
        .grid()
        .context("the terminal shows something other than a grid")?;
    let text = if capture.ansi {
        render::ansi(grid)
    } else {
        render::plain(grid)
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
}
