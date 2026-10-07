//! The end-to-end harness: drives the real `clackatoo` binary the way a
//! user would, and prints one table of results.
//!
//! It runs as the `e2e` bench of the `clackatoo` package, so `cargo bench`
//! always builds a fresh release binary for it to measure. Every daemon it
//! starts has a runtime directory of its own, so it never touches yours.
//!
//! The protocol can't push changes to clients yet, so the harness learns
//! that a terminal has caught up by polling: either the terminal's title,
//! which a script sets when it finishes, or its screen.

// Sizes and counts here are far below 2^52, where converting to f64 starts
// losing precision.
#![allow(clippy::cast_precision_loss)]

mod daemon;
mod pty;
mod report;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::Context;
use clackatoo_daemon::Client;
use clackatoo_protocol::{NewTerminal, Size, TerminalId};

use crate::workloads::{self, Workload};
use daemon::DaemonProcess;
use report::{Baselines, Report, Unit};

/// Waits for Enter, then prints a file. It announces each step in the
/// terminal's title, which shows up in a terminal list without fetching the
/// whole screen.
const FLOOD_SCRIPT: &str =
    r#"printf '\033]0;ready\007'; read -r _; cat "$1"; printf '\033]0;done\007'"#;
/// The title [`FLOOD_SCRIPT`] sets when it is ready to start.
const READY_TITLE: &str = "ready";
/// The title [`FLOOD_SCRIPT`] sets when it has printed everything.
const DONE_TITLE: &str = "done";

/// Echoes every byte it's sent straight back, like a full-screen program
/// that draws each key as it's pressed.
const ECHO_SCRIPT: &str = r"stty raw -echo; printf '\033]0;ready\007'; exec cat";

/// How much each throughput run pushes through the terminal.
const FLOOD_LEN: usize = 16 << 20;
/// Repeats of each throughput run.
const FLOOD_RUNS: usize = 5;
/// Keypresses timed for the round-trip latency, after a warm-up.
const KEYPRESSES: usize = 1000;
/// Repeats of each startup measurement.
const STARTUP_RUNS: usize = 20;
/// Fresh daemons the memory measurements are repeated on.
const MEMORY_RUNS: usize = 3;
/// Terminals opened to measure the memory each one takes.
const MEMORY_TERMINALS: usize = 20;
/// One-second windows the idle daemon is watched for.
const IDLE_SECONDS: usize = 3;
/// How long to wait for anything before deciding it's stuck.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The paths the harness needs, which only the bench target knows.
#[derive(Debug)]
pub struct Paths<'a> {
    /// The `clackatoo` binary to measure.
    pub binary: &'a Path,
    /// Where saved results live.
    pub results: &'a Path,
    /// The repository, to note which commit was measured.
    pub repo: &'a Path,
}

/// Runs the harness with the process's arguments, printing the results.
#[must_use]
pub fn main(paths: &Paths<'_>) -> ExitCode {
    match run(paths) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("e2e: {err:#}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage: cargo bench -p clackatoo --bench e2e -- [OPTIONS] [GROUP...]

Measures the release clackatoo binary end to end. Without GROUPs, runs
every group: size, startup, latency, throughput (with cpu), memory,
idle.

options:
  --save-baseline NAME  compare with baseline NAME, then replace it
  --baseline NAME       compare with baseline NAME, without saving
  --binary PATH         measure another clackatoo binary, such as one
                        built from a different branch
  -h, --help            show this help

Without either option, compares with the previous run and replaces it.";

#[derive(Debug, Default)]
struct Options {
    save: Option<String>,
    compare: Option<String>,
    /// A different `clackatoo` binary to measure, such as another branch's.
    binary: Option<PathBuf>,
    groups: Vec<String>,
}

impl Options {
    fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Option<Self>> {
        let mut options = Self::default();
        while let Some(arg) = args.next() {
            let mut value = |option: &str| {
                let name = args
                    .next()
                    .with_context(|| format!("{option} needs a baseline name"))?;
                anyhow::ensure!(
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                    "baseline names may only use letters, digits, - and _"
                );
                Ok(name)
            };
            match arg.as_str() {
                // cargo passes this to every bench target.
                "--bench" => {}
                "--save-baseline" => options.save = Some(value("--save-baseline")?),
                "--baseline" => options.compare = Some(value("--baseline")?),
                "--binary" => {
                    let path = args.next().context("--binary needs a path")?;
                    options.binary = Some(PathBuf::from(path));
                }
                "-h" | "--help" => return Ok(None),
                option if option.starts_with('-') => {
                    anyhow::bail!("unknown option {option}\n\n{USAGE}")
                }
                _ => options.groups.push(arg),
            }
        }
        Ok(Some(options))
    }

    fn wants(&self, group: &str) -> bool {
        self.groups.is_empty() || self.groups.iter().any(|wanted| wanted == group)
    }
}

fn run(paths: &Paths<'_>) -> anyhow::Result<()> {
    let Some(options) = Options::parse(std::env::args().skip(1))? else {
        println!("{USAGE}");
        return Ok(());
    };
    let binary = options.binary.as_deref().unwrap_or(paths.binary);
    let measured = match &options.binary {
        Some(path) => format!("binary {}", path.display()),
        None => commit(paths.repo),
    };
    println!("clackatoo end-to-end benchmarks");
    println!("binary: {}", binary.display());
    let baselines = Baselines::new(
        paths.results,
        options.save.as_deref(),
        options.compare.as_deref(),
    );
    let mut report = Report::start(baselines, measured)?;

    if options.wants("size") {
        let size = std::fs::metadata(binary)?.len();
        report.record("size/binary", "release binary", Unit::Bytes, &[size as f64]);
    }
    if options.wants("startup") {
        startup(binary, &mut report)?;
    }
    if options.wants("latency") {
        latency(binary, &mut report)?;
    }
    if options.wants("throughput") {
        throughput(binary, &mut report)?;
    }
    if options.wants("memory") {
        memory(binary, &mut report)?;
    }
    if options.wants("idle") {
        idle(binary, &mut report)?;
    }
    report.finish()
}

fn startup(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let samples = repeat(STARTUP_RUNS, || Ok(DaemonProcess::start(binary)?.1))?;
    report.record(
        "startup/daemon",
        "daemon start to accepting clients",
        Unit::Seconds,
        &samples,
    );

    let (daemon, _) = DaemonProcess::start(binary)?;
    let samples = repeat(STARTUP_RUNS, || {
        let started = Instant::now();
        let status = Command::new(binary)
            .arg("list")
            .arg("--socket")
            .arg(daemon.socket())
            .stdout(Stdio::null())
            .status()?;
        anyhow::ensure!(status.success(), "clackatoo list failed: {status}");
        Ok(started.elapsed())
    })?;
    report.record(
        "startup/cli",
        "CLI command, start to exit (list)",
        Unit::Seconds,
        &samples,
    );

    let mut client = daemon.connect()?;
    let samples = repeat(STARTUP_RUNS, || {
        let started = Instant::now();
        let terminal = new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
        let took = started.elapsed();
        client.close_terminal(terminal)?;
        Ok(took)
    })?;
    report.record(
        "startup/terminal",
        "new terminal, request to running",
        Unit::Seconds,
        &samples,
    );

    let samples = repeat(STARTUP_RUNS / 2, || {
        let started = Instant::now();
        let terminal = client.new_terminal(NewTerminal {
            size: workloads::generated_size(),
            command: None,
            cwd: None,
            record_to: None,
        })?;
        poll(|| {
            let screen = client.capture(terminal)?;
            Ok(screen
                .lines
                .iter()
                .any(|line| !line.text().trim().is_empty()))
        })?;
        let took = started.elapsed();
        client.close_terminal(terminal)?;
        Ok(took)
    })?;
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let shell = shell.rsplit('/').next().unwrap_or("sh");
    report.record(
        "startup/shell",
        &format!("new terminal to first text ({shell})"),
        Unit::Seconds,
        &samples,
    );
    Ok(())
}

fn latency(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    let terminal = new_terminal(
        &mut client,
        workloads::generated_size(),
        &["sh", "-c", ECHO_SCRIPT],
    )?;
    wait_for_title(&mut client, terminal, READY_TITLE)?;

    // Each keypress returns to the start of the line and types a letter, so
    // the letter shows up at the top left. Consecutive letters differ.
    let mut keypresses = Vec::new();
    for i in 0..KEYPRESSES + KEYPRESSES / 10 {
        let letter = char::from(b'a' + u8::try_from(i % 26)?);
        let started = Instant::now();
        client.send_input(terminal, format!("\r{letter}").as_bytes())?;
        poll_busy(|| {
            let screen = client.capture(terminal)?;
            Ok(screen
                .lines
                .first()
                .is_some_and(|line| line.text().starts_with(letter)))
        })?;
        keypresses.push(started.elapsed());
    }
    let keypresses = seconds(&keypresses[KEYPRESSES / 10..]);
    report.record(
        "latency/echo",
        "keypress to echo in the grid",
        Unit::Seconds,
        &keypresses,
    );

    let captures = repeat(KEYPRESSES, || {
        let started = Instant::now();
        client.capture(terminal)?;
        Ok(started.elapsed())
    })?;
    report.record(
        "latency/capture",
        "screen capture round trip",
        Unit::Seconds,
        &captures,
    );
    Ok(())
}

fn throughput(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    let mut ceiling_file = None;
    let mut cpu = Vec::new();
    for workload in workloads::generated(FLOOD_LEN) {
        let file = write_workload(dir.path(), &workload)?;
        let mib = workload.bytes.len() as f64 / f64::from(1 << 20);
        let mut rates = Vec::new();
        let mut cpu_per_mib = Vec::new();
        for _ in 0..FLOOD_RUNS {
            let run = flood(&daemon, &mut client, &workload, &file)?;
            rates.push(workload.bytes.len() as f64 / run.elapsed.as_secs_f64());
            cpu_per_mib.push(run.cpu.as_secs_f64() / mib);
        }
        report.record(
            &format!("throughput/{}", workload.name),
            workload.name,
            Unit::BytesPerSecond,
            &rates,
        );
        cpu.push((workload.name, cpu_per_mib));
        if ceiling_file.is_none() {
            ceiling_file = Some((file, workload.bytes.len(), workload.size));
        }
    }

    let (file, len, size) = ceiling_file.context("there are no workloads")?;
    let samples = repeat(FLOOD_RUNS, || pty::flood(&file, size))?;
    let rates: Vec<f64> = samples.iter().map(|secs| len as f64 / secs).collect();
    report.record(
        "throughput/pty",
        "ascii through a bare PTY (the ceiling)",
        Unit::BytesPerSecond,
        &rates,
    );

    // Throughput is capped by the PTY, so this is where the daemon's own
    // speed shows: what each MiB costs it.
    for (name, cpu_per_mib) in cpu {
        report.record(
            &format!("cpu/{name}"),
            &format!("{name}, daemon CPU per MiB"),
            Unit::Seconds,
            &cpu_per_mib,
        );
    }
    Ok(())
}

/// One run of [`FLOOD_SCRIPT`]: how long it took, and how much CPU time the
/// daemon spent on it.
struct Flood {
    elapsed: Duration,
    cpu: Duration,
}

/// Times one run of [`FLOOD_SCRIPT`] printing `file`, from pressing Enter
/// until the daemon has processed everything.
fn flood(
    daemon: &DaemonProcess,
    client: &mut Client,
    workload: &Workload,
    file: &Path,
) -> anyhow::Result<Flood> {
    let file = file.to_str().context("temporary paths are UTF-8")?;
    let terminal = new_terminal(
        client,
        workload.size,
        &["sh", "-c", FLOOD_SCRIPT, "sh", file],
    )?;
    wait_for_title(client, terminal, READY_TITLE)?;
    let cpu_before = daemon.cpu_time()?;
    let started = Instant::now();
    client.send_input(terminal, b"\r")?;
    wait_for_title(client, terminal, DONE_TITLE)?;
    let elapsed = started.elapsed();
    let cpu = daemon.cpu_time()?.saturating_sub(cpu_before);
    client.close_terminal(terminal)?;
    Ok(Flood { elapsed, cpu })
}

fn memory(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let mut at_rest = Vec::new();
    let mut per_terminal = Vec::new();
    let mut per_line = Vec::new();
    let dir = tempfile::tempdir()?;
    let scrollback = clackatoo_term::Config::default().scrollback_lines;
    let lines = scrollback_workload(scrollback);
    let lines_file = write_workload(dir.path(), &lines)?;

    for _ in 0..MEMORY_RUNS {
        let (daemon, _) = DaemonProcess::start(binary)?;
        let mut client = daemon.connect()?;
        at_rest.push(daemon.resident_memory()? as f64);

        // The first terminal also pulls in code and buffers every terminal
        // shares, which isn't what this measures.
        let warm_up = new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
        client.close_terminal(warm_up)?;
        let before = daemon.resident_memory()?;
        for _ in 0..MEMORY_TERMINALS {
            new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
        }
        let after = daemon.resident_memory()?;
        per_terminal.push(after.saturating_sub(before) as f64 / MEMORY_TERMINALS as f64);

        let terminal = new_terminal(
            &mut client,
            lines.size,
            &[
                "sh",
                "-c",
                FLOOD_SCRIPT,
                "sh",
                lines_file.to_str().context("UTF-8 path")?,
            ],
        )?;
        wait_for_title(&mut client, terminal, READY_TITLE)?;
        let before = daemon.resident_memory()?;
        client.send_input(terminal, b"\r")?;
        wait_for_title(&mut client, terminal, DONE_TITLE)?;
        let after = daemon.resident_memory()?;
        per_line.push(after.saturating_sub(before) as f64 / scrollback as f64);
    }

    report.record("memory/daemon", "daemon at rest", Unit::Bytes, &at_rest);
    report.record(
        "memory/terminal",
        &format!("per terminal ({}, empty)", workloads::generated_size()),
        Unit::Bytes,
        &per_terminal,
    );
    report.record(
        "memory/scrollback",
        &format!("per scrollback line ({} cols, full)", lines.size.cols()),
        Unit::Bytes,
        &per_line,
    );
    Ok(())
}

/// Exactly enough full-width lines to fill an 80x24 terminal's scrollback.
fn scrollback_workload(scrollback: usize) -> Workload {
    let size = Size::new(80, 24).expect("80x24 is a valid size");
    let line_count = scrollback + usize::from(size.rows());
    let mut bytes = Vec::with_capacity(line_count * 82);
    for line in 0..line_count {
        // Vary the text a little, so nothing could share rows.
        let letter = b'a' + u8::try_from(line % 26).expect("below 26");
        bytes.extend(std::iter::repeat_n(letter, usize::from(size.cols())));
        bytes.extend_from_slice(b"\r\n");
    }
    Workload {
        name: "scrollback",
        description: "full lines filling the scrollback",
        size,
        bytes,
    }
}

fn idle(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    // A connected client and a few terminals with nothing to say: the state
    // a daemon spends most of its life in.
    let mut client = daemon.connect()?;
    for _ in 0..3 {
        new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
    }
    std::thread::sleep(Duration::from_millis(200));
    let mut samples = Vec::new();
    let mut last = daemon.context_switches()?;
    for _ in 0..IDLE_SECONDS {
        std::thread::sleep(Duration::from_secs(1));
        let now = daemon.context_switches()?;
        samples.push((now - last) as f64);
        last = now;
    }
    report.record(
        "idle/wakeups",
        "wakeups, with 3 quiet terminals",
        Unit::PerSecond,
        &samples,
    );
    Ok(())
}

fn new_terminal(client: &mut Client, size: Size, command: &[&str]) -> anyhow::Result<TerminalId> {
    Ok(client.new_terminal(NewTerminal {
        size,
        command: Some(command.iter().map(|&arg| arg.to_owned()).collect()),
        cwd: None,
        record_to: None,
    })?)
}

fn write_workload(dir: &Path, workload: &Workload) -> anyhow::Result<PathBuf> {
    let path = dir.join(workload.name);
    std::fs::write(&path, &workload.bytes)?;
    Ok(path)
}

/// Waits until `terminal`'s title is `title`.
fn wait_for_title(client: &mut Client, terminal: TerminalId, title: &str) -> anyhow::Result<()> {
    poll(|| {
        let terminals = client.list_terminals()?;
        let info = terminals
            .iter()
            .find(|info| info.id == terminal)
            .context("the terminal disappeared")?;
        Ok(info.title == title)
    })
    .with_context(|| format!("waiting for the title {title:?}"))
}

/// Calls `done` every 200µs until it says yes. The pause keeps the polling
/// from competing with the daemon for its own event loop.
fn poll(mut done: impl FnMut() -> anyhow::Result<bool>) -> anyhow::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    while !done()? {
        anyhow::ensure!(Instant::now() < deadline, "timed out after {TIMEOUT:?}");
        std::thread::sleep(Duration::from_micros(200));
    }
    Ok(())
}

/// Calls `done` back to back until it says yes, for latencies too short to
/// pause between checks.
fn poll_busy(mut done: impl FnMut() -> anyhow::Result<bool>) -> anyhow::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    while !done()? {
        anyhow::ensure!(Instant::now() < deadline, "timed out after {TIMEOUT:?}");
    }
    Ok(())
}

/// Measures something `runs` times, giving the samples in seconds.
fn repeat(
    runs: usize,
    mut measure: impl FnMut() -> anyhow::Result<Duration>,
) -> anyhow::Result<Vec<f64>> {
    (0..runs).map(|_| Ok(measure()?.as_secs_f64())).collect()
}

fn seconds(durations: &[Duration]) -> Vec<f64> {
    durations.iter().map(Duration::as_secs_f64).collect()
}

/// The commit being measured, marked if there are uncommitted changes.
fn commit(repo: &Path) -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let Some(commit) = git(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".to_owned();
    };
    match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(changes) if changes.is_empty() => commit,
        _ => format!("{commit}+changes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Option<Options>> {
        Options::parse(args.iter().map(|&arg| arg.to_owned()))
    }

    #[test]
    fn options_parse_like_criterion() {
        let options = parse(&["--bench", "--save-baseline", "before", "memory"])
            .unwrap()
            .unwrap();
        assert_eq!(options.save.as_deref(), Some("before"));
        assert_eq!(options.compare, None);
        assert!(options.wants("memory"));
        assert!(!options.wants("startup"));

        let options = parse(&["--binary", "/tmp/other"]).unwrap().unwrap();
        assert_eq!(options.binary.as_deref(), Some(Path::new("/tmp/other")));

        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(parse(&["--frobnicate"]).is_err());
        assert!(parse(&["--baseline"]).is_err());
        assert!(parse(&["--baseline", "../escape"]).is_err());
    }

    #[test]
    fn the_scrollback_workload_fills_the_scrollback_exactly() {
        let workload = scrollback_workload(100);
        let text = std::str::from_utf8(&workload.bytes).unwrap();
        assert_eq!(text.lines().count(), 124);
        assert!(text.lines().all(|line| line.trim_end().len() == 80));
    }
}
