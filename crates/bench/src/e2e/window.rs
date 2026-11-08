//! The window: how soon it shows a terminal, alone or beside another
//! window, how long it takes to draw, and that it sleeps when nothing
//! happens.
//!
//! These need a display to open a window on, so they're skipped without
//! one. The window opens on a daemon of the harness's own, through
//! `XDG_RUNTIME_DIR`, and logs what's measured: its first frame, and with
//! `clackatoo_gui::frame=trace`, how long each frame took to draw.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Context;

use super::daemon::DaemonProcess;
use super::report::{Report, Unit};
use super::{
    DONE_TITLE, IDLE_SECONDS, READY_TITLE, TIMEOUT, new_terminal, seconds, wait_for_title,
    write_workload,
};
use crate::workloads;

/// Windows opened to time the first frame.
const OPEN_RUNS: usize = 5;
/// How much the frame-time run's workload holds. It's printed over and
/// over for two seconds, which the window draws as it goes.
const FRAME_FLOOD_LEN: usize = 4 << 20;

/// Like [`FLOOD_SCRIPT`], but printing the file over and over for two
/// seconds.
const TIMED_FLOOD_SCRIPT: &str = r#"printf '\033]0;ready\007'; read -r _; timeout 2 sh -c 'while :; do cat "$1"; done' sh "$1"; printf '\033]0;done\007'"#;

/// The window's own threads. Others belong to the GPU driver, which on
/// some drivers wake up on their own (see the benchmarks docs).
const OWN_THREADS: [&str; 2] = ["clackatoo", "session"];

pub(crate) fn measure(binary: &Path, report: &mut Report) -> anyhow::Result<()> {
    let Some(display) = Display::from_env() else {
        println!("window: skipped, no display to open a window on");
        return Ok(());
    };
    first_frame(binary, &display, report)?;
    another_window(binary, &display, report)?;
    frame_time(binary, &display, report)?;
    idle(binary, &display, report)
}

/// From starting the window to its first frame, on a running daemon with
/// a quiet terminal.
fn first_frame(binary: &Path, display: &Display, report: &mut Report) -> anyhow::Result<()> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
    let mut samples = Vec::new();
    for _ in 0..OPEN_RUNS {
        let started = Instant::now();
        let window = Window::open(binary, &daemon, display, "clackatoo_gui=info")?;
        window.wait_for("first frame")?;
        samples.push(started.elapsed());
    }
    report.record(
        "window/open",
        "start to first frame",
        Unit::Seconds,
        &seconds(&samples),
    );
    Ok(())
}

/// From starting `clackatoo` to the first frame of the window it asks
/// the process with a window open for, which opens it with the GPU it set
/// up already.
fn another_window(binary: &Path, display: &Display, report: &mut Report) -> anyhow::Result<()> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
    // Its log, in a state directory of its own.
    let state = tempfile::tempdir()?;
    let ask = || -> anyhow::Result<()> {
        let asked = Window::command(binary, &daemon, display, "clackatoo_gui=info")
            .env("XDG_STATE_HOME", state.path())
            .env_remove(OFF_VAR)
            .stderr(Stdio::null())
            .status()
            .context("could not ask for a window")?;
        anyhow::ensure!(asked.success(), "asking for a window failed: {asked}");
        Ok(())
    };
    let log = state.path().join("clackatoo/window.log");
    let frames =
        || std::fs::read_to_string(&log).map_or(0, |log| log.matches("first frame").count());
    // Until the next window has drawn, from `started`.
    let drawn = |started: Instant, before: usize| -> anyhow::Result<Duration> {
        while frames() == before {
            anyhow::ensure!(started.elapsed() < TIMEOUT, "a window never drew");
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(started.elapsed())
    };
    // The first window forks the process of windows, apart from the
    // harness: timed as it opens, then the process ended, each time.
    let mut first = Vec::new();
    for _ in 0..OPEN_RUNS {
        let before = frames();
        let started = Instant::now();
        ask()?;
        first.push(drawn(started, before)?);
        Host::listening_in(daemon.runtime_dir())?.end()?;
    }
    report.record(
        "window/first-hosted",
        "start to first frame, first window of a process of windows",
        Unit::Seconds,
        &seconds(&first),
    );
    ask()?;
    let host = Host::listening_in(daemon.runtime_dir())?;
    drawn(Instant::now(), frames().saturating_sub(1))?;
    let mut samples = Vec::new();
    for _ in 0..OPEN_RUNS {
        let before = frames();
        let started = Instant::now();
        ask()?;
        samples.push(drawn(started, before)?);
    }
    host.end()?;
    report.record(
        "window/another",
        "start to first frame, another window",
        Unit::Seconds,
        &seconds(&samples),
    );
    Ok(())
}

/// Set to `off`, every window has a process of its own, which these
/// measure.
const OFF_VAR: &str = "CLACKATOO_WINDOW_HOST";

/// The process of windows `clackatoo` started, ended when dropped.
struct Host(rustix::process::Pid);

impl Host {
    /// The process listening for windows in `runtime_dir`, as its socket
    /// says.
    fn listening_in(runtime_dir: &Path) -> anyhow::Result<Self> {
        let socket = runtime_dir.join("clackatoo/window.sock");
        let stream = std::os::unix::net::UnixStream::connect(&socket)
            .with_context(|| format!("no process of windows at {}", socket.display()))?;
        let peer = rustix::net::sockopt::socket_peercred(&stream)?;
        Ok(Self(peer.pid))
    }
}

impl Host {
    /// Ends the process, and waits until it has.
    fn end(self) -> anyhow::Result<()> {
        let pid = self.0;
        drop(self);
        let deadline = Instant::now() + TIMEOUT;
        while rustix::process::test_kill_process(pid).is_ok() {
            anyhow::ensure!(
                Instant::now() < deadline,
                "the process of windows never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::TERM);
    }
}

/// How long the window takes to draw each frame, from starting on it to
/// handing it to the compositor, while a terminal floods.
fn frame_time(binary: &Path, display: &Display, report: &mut Report) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let workload = workloads::generated(FRAME_FLOOD_LEN)
        .into_iter()
        .find(|workload| workload.name == "dense")
        .context("there is a dense workload")?;
    let file = write_workload(dir.path(), &workload)?;
    let file = file.to_str().context("temporary paths are UTF-8")?;

    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    let terminal = new_terminal(
        &mut client,
        workload.size,
        &["sh", "-c", TIMED_FLOOD_SCRIPT, "sh", file],
    )?;
    wait_for_title(&mut client, terminal, READY_TITLE)?;
    let window = Window::open(
        binary,
        &daemon,
        display,
        "clackatoo_gui=info,clackatoo_gui::frame=trace",
    )?;
    window.wait_for("first frame")?;
    client.send_input(terminal, b"\r")?;
    wait_for_title(&mut client, terminal, DONE_TITLE)?;
    // Lines logged so far, which the reader thread has collected.
    let frames: Vec<f64> = window
        .lines()
        .iter()
        .filter_map(|line| line.split("frame ").nth(1)?.trim().parse::<f64>().ok())
        .map(|micros| micros / 1e6)
        .collect();
    anyhow::ensure!(
        frames.len() > 10,
        "the window drew only {} frames",
        frames.len()
    );
    report.record(
        "window/frame",
        &format!("frame drawn, during a flood ({})", workload.size),
        Unit::Seconds,
        &frames,
    );
    Ok(())
}

/// How often the window wakes up with nothing to draw.
fn idle(binary: &Path, display: &Display, report: &mut Report) -> anyhow::Result<()> {
    let (daemon, _) = DaemonProcess::start(binary)?;
    let mut client = daemon.connect()?;
    new_terminal(&mut client, workloads::generated_size(), &["cat"])?;
    let window = Window::open(binary, &daemon, display, "clackatoo_gui=info")?;
    window.wait_for("first frame")?;
    std::thread::sleep(Duration::from_millis(500));

    let mut own = Vec::new();
    let mut all = Vec::new();
    let (mut last_own, mut last_all) = window.context_switches()?;
    for _ in 0..IDLE_SECONDS {
        std::thread::sleep(Duration::from_secs(1));
        let (now_own, now_all) = window.context_switches()?;
        own.push((now_own - last_own) as f64);
        all.push((now_all - last_all) as f64);
        (last_own, last_all) = (now_own, now_all);
    }
    report.record(
        "window/idle",
        "wakeups of the window's own threads",
        Unit::PerSecond,
        &own,
    );
    report.record(
        "window/idle-driver",
        "wakeups, the GPU driver's threads too",
        Unit::PerSecond,
        &all,
    );
    Ok(())
}

/// What a window needs to reach the display.
struct Display {
    vars: Vec<(&'static str, OsString)>,
}

impl Display {
    /// The display this process can reach, if any. A relative
    /// `WAYLAND_DISPLAY` names a socket in `XDG_RUNTIME_DIR`, which the
    /// window gets a different one of, so it's made absolute.
    fn from_env() -> Option<Self> {
        let mut vars = Vec::new();
        if let Some(wayland) = std::env::var_os("WAYLAND_DISPLAY").filter(|v| !v.is_empty()) {
            let path = PathBuf::from(&wayland);
            let path = if path.is_absolute() {
                path
            } else {
                PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(path)
            };
            vars.push(("WAYLAND_DISPLAY", path.into_os_string()));
        }
        if let Some(x11) = std::env::var_os("DISPLAY").filter(|v| !v.is_empty()) {
            vars.push(("DISPLAY", x11));
        }
        (!vars.is_empty()).then_some(Self { vars })
    }
}

/// A running window, closed when dropped.
struct Window {
    child: Child,
    /// Each line it logs.
    lines: mpsc::Receiver<String>,
    /// Lines already taken from `lines`.
    seen: std::cell::RefCell<Vec<String>>,
}

impl Window {
    /// Opens a window on `daemon`, which makes a workspace of its newest
    /// terminal if it has none, logging at `log`.
    fn open(
        binary: &Path,
        daemon: &DaemonProcess,
        display: &Display,
        log: &str,
    ) -> anyhow::Result<Self> {
        let mut child = Self::command(binary, daemon, display, log)
            .stderr(Stdio::piped())
            .spawn()
            .context("could not open a window")?;
        let stderr = child.stderr.take().context("stderr is piped")?;
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { return };
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        Ok(Self {
            child,
            lines,
            seen: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// `clackatoo`, opening a window on `daemon` in a process of its own,
    /// logging at `log`.
    fn command(binary: &Path, daemon: &DaemonProcess, display: &Display, log: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .env("CLACKATOO_BOARDS", "off")
            .env("CLACKATOO_RECOVERY", "off")
            .env("CLACKATOO_MACHINE", "off")
            .env("CLACKATOO_DISPLAY", "off")
            .env("XDG_RUNTIME_DIR", daemon.runtime_dir())
            .envs(display.vars.iter().map(|(name, value)| (name, value)))
            .env("RUST_LOG", log)
            // A process of its own, which logs to the harness.
            .env(OFF_VAR, "off")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }

    /// Waits until the window logs a line containing `text`.
    fn wait_for(&self, text: &str) -> anyhow::Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .with_context(|| format!("the window never logged {text:?}"))?;
            let found = line.contains(text);
            self.seen.borrow_mut().push(line);
            if found {
                return Ok(());
            }
        }
    }

    /// Every line logged so far.
    fn lines(&self) -> Vec<String> {
        let mut seen = self.seen.borrow_mut();
        seen.extend(self.lines.try_iter());
        seen.clone()
    }

    /// Context switches so far: of the window's own threads, and of all of
    /// them.
    fn context_switches(&self) -> anyhow::Result<(u64, u64)> {
        let own: HashSet<&str> = OWN_THREADS.into_iter().collect();
        let tasks = PathBuf::from(format!("/proc/{}/task", self.child.id()));
        let (mut mine, mut all) = (0, 0);
        for task in std::fs::read_dir(&tasks).context("could not list the window's threads")? {
            let task = task?.path();
            let name = std::fs::read_to_string(task.join("comm"))?;
            let status = std::fs::read_to_string(task.join("status"))?;
            let switches: u64 = status
                .lines()
                .filter(|line| line.contains("ctxt_switches:"))
                .filter_map(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
                .sum();
            all += switches;
            if own.contains(name.trim()) {
                mine += switches;
            }
        }
        Ok((mine, all))
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
