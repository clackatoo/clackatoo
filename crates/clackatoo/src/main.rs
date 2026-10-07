//! The `clackatoo` command: runs the daemon, and drives it from the shell.

#![forbid(unsafe_code)]

mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clackatoo_daemon::{Client, ClientError, Daemon, DaemonConfig, RuntimeDir};
use clackatoo_protocol::{NewTerminal, Size, TerminalId};
use clap::{Args, Parser, Subcommand, ValueEnum};

/// A fast, lightweight terminal workspace.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    /// Change a terminal's size. Its program is told, and lines on screen
    /// are rewrapped to the new width.
    Resize(Resize),
}

#[derive(Debug, Args)]
struct DaemonArgs {
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
    /// Point it at an SSH-forwarded socket to drive a remote daemon.
    #[arg(long, value_name = "PATH")]
    socket: Option<PathBuf>,
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
    let cli = Cli::parse();
    match run(cli.command) {
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
    }
}

fn run_daemon(args: &DaemonArgs) -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let runtime = RuntimeDir::for_current_user();
    let _lock = runtime.lock()?;
    let mut config = DaemonConfig::from_env();
    config.terminal.scrollback_compaction = args.scrollback_compaction.into();
    let mut daemon = Daemon::bind(&runtime.socket_path(), config)
        .with_context(|| format!("could not listen on {}", runtime.socket_path().display()))?;
    daemon.run()?;
    Ok(())
}

fn connect(connection: &Connection) -> anyhow::Result<Client> {
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
