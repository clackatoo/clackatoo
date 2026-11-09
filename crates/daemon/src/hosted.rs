//! A terminal the daemon is running: the emulator, plus the PTY and process
//! behind it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;

use clackatoo_protocol::{
    Error, NewTerminal, Node, Size, TerminalId, TerminalInfo, TerminalStatus, View,
};
use clackatoo_term::{ClipboardRead, ClipboardRequest, Damage, Terminal, recording};

use crate::DaemonConfig;
use crate::handover::{self, Fds, HandoverError};
use crate::pty::{self, Process, Program, Pty};
use crate::shell;

/// How much input may wait for a program that isn't reading it. Pastes far
/// larger than any real one fit.
const MAX_PENDING_INPUT: usize = 8 * 1024 * 1024;

/// How much input may wait before replies to the program's queries are
/// dropped, whole: a program asking faster than it reads the answers, as
/// one behind ssh to a host that has stopped reading, would otherwise
/// have them kept without end. An answer behind this many unread is stale
/// whatever it says.
const MAX_PENDING_REPLIES: usize = 64 * 1024;

/// What happened when output was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadOutcome {
    /// Everything available was read.
    Drained,
    /// The budget ran out, or the caller asked to stop, before the PTY was
    /// drained, so more may be waiting. The PTY is edge-triggered and won't
    /// signal again, so the caller must come back without waiting for an
    /// event.
    BudgetExhausted,
    /// The PTY is closed: nothing holds its slave end any more.
    Closed,
}

/// A running (or finished) terminal.
#[derive(Debug)]
pub(crate) struct HostedTerminal {
    id: TerminalId,
    emulator: Terminal,
    /// `None` once the PTY has closed.
    pty: Option<Pty>,
    pid: u32,
    /// `None` once the program has been reaped.
    process: Option<Process>,
    status: TerminalStatus,
    /// Input written by clients (and replies to the program's queries) that
    /// the PTY hasn't accepted yet.
    input: Vec<u8>,
    /// Whether replies have been dropped since the last that weren't, to
    /// say so once.
    dropping_replies: bool,
    recording: Option<BufWriter<File>>,
    /// The secret the shell's integration adds to its marks, if it has
    /// one (see [`shell`]).
    secret: Option<shell::Secret>,
    /// The device and inode of the program it was started as, once it had
    /// started: another, as after `exec prog`, is no longer its shell.
    /// `None` if it couldn't be read.
    exe: Option<(u64, u64)>,
    /// For a terminal no program runs in, which the daemon writes what an
    /// agent's command printed into (see [`HostedTerminal::fed`]), where
    /// that stands.
    fed: Option<Fed>,
    /// The directory its program was last found in, kept for once it has
    /// ended, when `/proc` has none (see [`HostedTerminal::own_directory`]).
    directory: std::cell::RefCell<Option<String>>,
    /// Whether an agent asked for it, its program in the agents' leaf (see
    /// [`crate::priority`]). Not handed over: one carried through an
    /// upgrade counts as the user's.
    for_agents: bool,
}

/// Where a terminal the daemon writes an agent's command's output into
/// stands (see [`HostedTerminal::fed`]).
#[derive(Debug)]
struct Fed {
    /// Whether it has been told the command ended, or that nothing more
    /// will come, which it says once.
    ended: bool,
}

/// What a terminal the daemon writes an agent's command's output into
/// says to input: no program reads it.
const FED_INPUT: &str = "this pane shows what an agent's command printed: it can't be typed into";

/// How a terminal the daemon wrote an agent's command's output into ended
/// when nothing said how, as when its chat closed or the daemon upgraded:
/// cut off, by signal 0, which is none.
pub(crate) const FED_CUT_OFF: TerminalStatus = TerminalStatus::Killed(0);

/// What's written into a terminal the daemon writes an agent's command's
/// output into before its last line, to put back what the command left
/// set that only a program running there would want, as a shell's prompt
/// would after it: a string or sequence left half sent ends (ST), which
/// would otherwise swallow the line; the alternate screen is left; the
/// scrolling region is the whole screen again, the cursor kept where it
/// is; mouse and focus reports, bracketed paste, application keys and
/// insert mode are off; the character set is plain again; colours are
/// reset; and the cursor is hidden, as nothing's typed there.
const FED_RESET: &[u8] = b"\x1b\\\x1b[?1049l\x1b7\x1b[r\x1b8\
    \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1004l\
    \x1b[?2004l\x1b[?1l\x1b>\x1b[4l\x0f\x1b(B\x1b[0m\x1b[?25l";

/// Why a terminal's shell doesn't have it to itself, to be typed into:
/// see [`HostedTerminal::shell_has_terminal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    /// A process group other than the shell's is in front: a program it
    /// runs, with job control, or another shell it started.
    Program,
    /// The shell runs a program in its own process group, as a shell
    /// without job control does.
    Child(u32),
    /// The shell has become another program, as with `exec prog`.
    Exec,
    /// Its program has ended, or it's no terminal of a shell's.
    Ended,
}

impl Taken {
    /// What it says, in a clause: why a command wasn't typed.
    pub(crate) fn why(self) -> &'static str {
        match self {
            Self::Program => {
                "a program other than the shell was in front of its prompt; if the daemon's \
                 `$SHELL` starts another shell, as `sh -c fish` does, set `$SHELL` to that one"
            }
            Self::Child(_) => "a program the shell started was still running",
            Self::Exec => "the shell had become another program, as `exec` makes it",
            Self::Ended => "the shell had ended",
        }
    }
}

/// The device and inode of process `pid`'s program.
fn exe_of(pid: u32) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
    Some((meta.dev(), meta.ino()))
}

impl HostedTerminal {
    /// Starts terminal `id` as `request` says, in the daemon's `leaf` for
    /// whoever asked for it (see [`crate::priority`]).
    pub(crate) fn start(
        id: TerminalId,
        request: &NewTerminal,
        config: &DaemonConfig,
        display: Option<&crate::display::Env>,
        leaf: crate::priority::Leaf,
    ) -> Result<Self, Error> {
        // Open the recording first: failing then leaves nothing to clean up.
        let recording = match &request.record_to {
            Some(path) => Some(start_recording(Path::new(path), request).map_err(|err| {
                Error::RecordingFailed {
                    path: path.clone(),
                    reason: err.to_string(),
                }
            })?),
            None => None,
        };

        // The user's own shell gets its integration, if it has one, and
        // a secret for it, if the integration takes one.
        let mut integration = match (&request.command, &config.shell_integration) {
            (None, Some(scripts)) => shell::integration(
                &config.shell,
                scripts,
                std::env::var_os("ZDOTDIR").as_deref(),
            ),
            _ => shell::Integration::default(),
        };
        let secret = (config.shell_integration.as_deref())
            .filter(|_| !integration.is_empty() && shell::takes_secret(&config.shell))
            .and_then(|scripts| {
                shell::Secret::new(scripts, id.0)
                    .map_err(|err| log::warn!("terminal {id}: its shell has no secret: {err}"))
                    .ok()
            });
        if let Some(file) = secret.as_ref().and_then(|secret| secret.file.as_ref()) {
            integration.tell_secret(file.path());
        }
        let (path, args) = match request.command.as_deref() {
            Some([program, args @ ..]) => (program.as_ref(), args),
            Some([]) => return Err(Error::SpawnFailed("the command is empty".to_owned())),
            None => (config.shell.as_os_str(), integration.args.as_slice()),
        };
        let cwd = request
            .cwd
            .as_deref()
            .map_or(config.home.as_path(), Path::new);
        let program = Program {
            path,
            args,
            env: &integration.env,
            display,
            cwd,
            leaf,
        };
        let spawned = pty::spawn(&program, request.size)
            .map_err(|err| Error::SpawnFailed(err.to_string()))?;
        let pid = spawned.process.pid().as_raw_pid().cast_unsigned();
        log::info!("terminal {id} started: pid {pid}");
        // Spawning waited for it to start its program, so this is it.
        let exe = exe_of(pid);

        let mut emulator = Terminal::new(request.size, &config.terminal);
        emulator.expect_secret(secret.as_ref().map(|secret| secret.text.as_str()));
        Ok(Self {
            id,
            emulator,
            pty: Some(spawned.pty),
            pid,
            process: Some(spawned.process),
            status: TerminalStatus::Running,
            input: Vec::new(),
            dropping_replies: false,
            recording,
            secret,
            exe,
            directory: std::cell::RefCell::default(),
            for_agents: leaf == crate::priority::Leaf::Agents,
            fed: None,
        })
    }

    /// A terminal no program runs in, `size`, titled `title`, that the
    /// daemon writes what an agent's command printed into, as the agent
    /// tells it (see [`feed`](Self::feed)): it shows it as a terminal
    /// shows any program's output, colours and all, with history, search
    /// and yanks, but can't be typed into. It has no PTY and no process.
    /// Its pid is 0, which no program's is: that's how the next build
    /// knows one handed over to it.
    pub(crate) fn fed(id: TerminalId, size: Size, title: &str, config: &DaemonConfig) -> Self {
        let mut emulator = Terminal::new(size, &config.terminal);
        // Named as a program names its terminal, for its tab, after the
        // command, which is all that names it: what the command prints
        // can't rename the tab or the window, nor say where paths are.
        // With no cursor, as nothing's typed there.
        let title: String = title.chars().filter(|ch| !ch.is_control()).collect();
        emulator.advance(format!("\x1b]2;{title}\x07\x1b[?25l").as_bytes());
        emulator.keep_names();
        log::info!("terminal {id} started: fed what an agent's command printed");
        Self {
            id,
            emulator,
            pty: None,
            pid: 0,
            process: None,
            status: TerminalStatus::Running,
            input: Vec::new(),
            dropping_replies: false,
            recording: None,
            secret: None,
            exe: None,
            directory: std::cell::RefCell::default(),
            for_agents: false,
            fed: Some(Fed { ended: false }),
        }
    }

    /// Whether an agent asked for it, as `run_in_terminal` and
    /// `run_in_background` do.
    pub(crate) fn for_agents(&self) -> bool {
        self.for_agents
    }

    /// Whether no program runs in it, and the daemon writes into it what
    /// an agent's command printed (see [`fed`](Self::fed)).
    pub(crate) fn is_fed(&self) -> bool {
        self.fed.is_some()
    }

    /// Whether it's one the daemon writes into that hasn't been told the
    /// command ended.
    pub(crate) fn is_fed_unended(&self) -> bool {
        self.fed.as_ref().is_some_and(|fed| !fed.ended)
    }

    /// Writes `text`, more of what the command printed, as a terminal
    /// shows a program's output. Commands print to pipes, so a line break
    /// goes to the next line's start, as a terminal's line discipline
    /// takes one. Nothing's answered, as no program reads it, and nothing
    /// it prints reaches a clipboard. Does nothing to a terminal a
    /// program runs in.
    pub(crate) fn feed(&mut self, text: &str) {
        if self.fed.is_none() || text.is_empty() {
            return;
        }
        let mut bytes = Vec::with_capacity(text.len() + text.len() / 16);
        for piece in text.split_inclusive('\n') {
            match piece.strip_suffix('\n') {
                Some(line) => {
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.extend_from_slice(b"\r\n");
                }
                None => bytes.extend_from_slice(piece.as_bytes()),
            }
        }
        self.emulator.advance(&bytes);
        let _ = self.emulator.take_replies();
        let _ = self.emulator.take_clipboard();
    }

    /// Says on a line of its own that the command ended, with its exit
    /// code `exit`, if the agent said it, and takes that as how its
    /// program ended: one that `failed` without saying how as 1, as the
    /// agents' own terminals take it, and otherwise 0. Said once.
    pub(crate) fn end(&mut self, exit: Option<i32>, failed: bool) {
        if !self.is_fed_unended() {
            return;
        }
        self.status = TerminalStatus::Exited(exit.unwrap_or(i32::from(failed)));
        let said = match exit {
            Some(code) => format!("[exited {code}]"),
            None => "[ended]".to_owned(),
        };
        self.stop_feeding(&said);
    }

    /// Says `why`, clackatoo's own words, dim, on a line of its own after
    /// what the command printed, as the last it's written: nothing more
    /// comes, as the command ended, or what it prints can't reach here.
    /// Unless [`end`](Self::end) said how it ended, it's taken as cut off
    /// ([`FED_CUT_OFF`]): it runs no more, whatever the command does.
    pub(crate) fn stop_feeding(&mut self, why: &str) {
        let Some(fed) = self.fed.as_mut().filter(|fed| !fed.ended) else {
            return;
        };
        fed.ended = true;
        if self.status == TerminalStatus::Running {
            self.status = FED_CUT_OFF;
        }
        self.emulator.advance(FED_RESET);
        let start = if self.emulator.cursor().col == 0 {
            ""
        } else {
            "\r\n"
        };
        let note = format!("{start}\x1b[2m{why}\x1b[0m");
        self.emulator.advance(note.as_bytes());
    }

    /// Hands the terminal over to the next build, adding its fds to `fds`,
    /// with its shell's secret, if it has one. A recording stops: the next
    /// build can't know where it was.
    pub(crate) fn hand_over(self, fds: &mut Fds) -> (handover::Terminal, Option<handover::Shell>) {
        if self.recording.is_some() {
            log::info!("terminal {}: recording stopped by the upgrade", self.id);
        }
        let shell = self.secret.map(|secret| handover::Shell {
            terminal: self.id.0,
            secret: secret.text,
            file: (secret.file).map(|file| file.keep().into_os_string().into_vec()),
            trusted: self.emulator.trusted_block(),
        });
        let (screen, links) = self.emulator.snapshot_with_links();
        let terminal = handover::Terminal {
            id: self.id.0,
            process: self.process.map(|process| {
                let (pid, pidfd) = process.into_parts();
                handover::Process {
                    pid: pid.as_raw_pid(),
                    pidfd: fds.add(pidfd),
                }
            }),
            pid: self.pid,
            pty: self.pty.map(|pty| fds.add(pty.into_fd())),
            status: match self.status {
                TerminalStatus::Running => handover::Status::Running,
                TerminalStatus::Exited(code) => handover::Status::Exited(code),
                TerminalStatus::Killed(signal) => handover::Status::Killed(signal),
            },
            input: self.input,
            screen,
            blocks: self.emulator.blocks_snapshot(),
            links,
        };
        (terminal, shell)
    }

    /// A terminal the last build handed over, with its shell's secret if
    /// it had one, taking its fds from `fds`.
    pub(crate) fn resume(
        terminal: &handover::Terminal,
        shell: Option<&handover::Shell>,
        fds: &mut Fds,
        config: &DaemonConfig,
    ) -> Result<Self, HandoverError> {
        // First, so a file the shell hasn't read is removed if the rest
        // fails.
        let secret = shell.map(|shell| shell::Secret {
            text: shell.secret.clone(),
            file: (shell.file.clone())
                .map(|file| shell::SecretFile::kept(OsString::from_vec(file).into())),
        });
        let process = terminal
            .process
            .as_ref()
            .map(|process| resume_process(process, fds))
            .transpose()?;
        let pty = terminal
            .pty
            .map(|pty| fds.take(pty).map(Pty::from_fd))
            .transpose()?;
        let mut emulator =
            Terminal::restore_with_links(&terminal.screen, &terminal.links, &config.terminal)?;
        emulator.restore_blocks(&terminal.blocks);
        emulator.expect_secret(secret.as_ref().map(|secret| secret.text.as_str()));
        if let Some(block) = shell.and_then(|shell| shell.trusted) {
            emulator.trust_block(block);
        }
        // One the daemon wrote an agent's command's output into, as only
        // those have pid 0. What the command prints next can't reach it:
        // which tool of which chat it showed isn't handed over. So it has
        // ended, however the last build left it, and its names are still
        // its own.
        let fed = terminal.pid == 0 && terminal.pty.is_none() && terminal.process.is_none();
        let status = match terminal.status {
            handover::Status::Running if fed => FED_CUT_OFF,
            handover::Status::Running => TerminalStatus::Running,
            handover::Status::Exited(code) => TerminalStatus::Exited(code),
            handover::Status::Killed(signal) => TerminalStatus::Killed(signal),
        };
        if fed {
            emulator.keep_names();
        }
        Ok(Self {
            id: TerminalId(terminal.id),
            emulator,
            pty,
            pid: terminal.pid,
            process,
            status,
            input: terminal.input.clone(),
            dropping_replies: false,
            recording: None,
            secret,
            exe: None,
            directory: std::cell::RefCell::default(),
            for_agents: false,
            fed: fed.then_some(Fed { ended: true }),
        })
    }

    /// Its emulator, to feed what a program would print, in a test.
    #[cfg(test)]
    pub(crate) fn emulator_mut(&mut self) -> &mut Terminal {
        &mut self.emulator
    }

    /// Says what colours the terminal is drawn in, for programs that ask
    /// (see [`Terminal::set_palette`]).
    pub(crate) fn set_palette(&mut self, palette: Option<clackatoo_term::Palette>) {
        self.emulator.set_palette(palette);
    }

    /// Puts back what the program asked of the terminal it was handed
    /// over with, beyond its modes (see [`Terminal::restore_asked`]).
    pub(crate) fn restore_asked(&mut self, asked: &clackatoo_term::AskedSnapshot) {
        self.emulator.restore_asked(asked);
    }

    /// Takes `blocks` as those whose commands the shell's integration said,
    /// as the build that handed this terminal over knew they were.
    pub(crate) fn trust_blocks(&mut self, blocks: &[u32]) {
        for &block in blocks {
            self.emulator.trust_block(block);
        }
    }

    /// Whether its program is still running, or how it ended.
    pub(crate) fn status(&self) -> TerminalStatus {
        self.status
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            // One the daemon writes into runs no program.
            pid: (self.fed.is_none()).then_some(self.pid),
            size: self.emulator.size(),
            title: self.emulator.title().to_owned(),
            status: self.status,
            cwd: self.emulator.cwd().map(str::to_owned),
            command: (self.emulator.running_command()).map(str::to_owned),
        }
    }

    /// The directory the program in front is in: a nested shell's or an
    /// editor's, say, else the terminal's own program's. For a shell, it's
    /// where it last changed to.
    pub(crate) fn working_directory(&self) -> Option<String> {
        let cwd = |pid: i32| std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
        // The front process may have exited, or be another user's, as
        // under sudo.
        let cwd = self
            .pty
            .as_ref()
            .and_then(Pty::foreground)
            .and_then(|pid| cwd(pid.as_raw_nonzero().get()))
            .or_else(|| cwd(self.pid.cast_signed()))?;
        cwd.into_os_string().into_string().ok()
    }

    /// Whether its shell has the terminal to itself, to be typed into, as
    /// its prompt says it's waiting: its process group is in front, it's
    /// still the program it was started as, and it runs no program in its
    /// own group, as a shell without job control does. A child in a group
    /// of its own, as a job sent to the background, can't read the
    /// terminal. Reads `/proc`, so asked only as something's to be typed,
    /// or a command's end taken for one, not as output is read.
    pub(crate) fn shell_has_terminal(&self) -> Result<(), Taken> {
        if self.pty.is_none() || self.status != TerminalStatus::Running {
            return Err(Taken::Ended);
        }
        if !self.is_idle_shell() {
            return Err(Taken::Program);
        }
        if let Some(started) = self.exe
            && exe_of(self.pid).is_some_and(|now| now != started)
        {
            return Err(Taken::Exec);
        }
        let proc = Path::new("/proc");
        let in_its_group = (crate::ports::children_of(proc, self.pid).into_iter()).find(|&child| {
            crate::ports::stat_of(proc, child).is_some_and(|stat| stat.pgrp == self.pid)
        });
        match in_its_group {
            Some(child) => Err(Taken::Child(child)),
            None => Ok(()),
        }
    }

    /// Whether its program is the user's shell, one whose integration
    /// signs its marks (bash, zsh or fish), with the terminal to itself
    /// (see [`shell_has_terminal`](Self::shell_has_terminal)): what's typed
    /// goes to its prompt, and is read as it reads a command line.
    pub(crate) fn shell_at_prompt(&self) -> bool {
        self.secret.is_some() && self.shell_has_terminal().is_ok()
    }

    /// The device and inode of the program it was started as, for the next
    /// build to know.
    pub(crate) fn exe(&self) -> Option<(u64, u64)> {
        self.exe
    }

    /// Takes `exe` as the program it was started as, as the build that
    /// handed it over knew.
    pub(crate) fn started_as(&mut self, exe: Option<(u64, u64)>) {
        self.exe = exe;
    }

    /// Whether its program is a shell at its prompt, running nothing: the
    /// process group in front is its own.
    pub(crate) fn is_idle_shell(&self) -> bool {
        let front = self.pty.as_ref().and_then(Pty::foreground);
        front.is_some_and(|front| front.as_raw_nonzero().get().cast_unsigned() == self.pid)
    }

    /// When the terminal's own program started, as the kernel says, to
    /// the second or so: its start, in clock ticks after the machine
    /// booted, which Linux counts 100 to the second whatever its own tick
    /// (`USER_HZ`), and the boot, to the second. `None` if it's gone.
    pub(crate) fn started_at(&self) -> Option<std::time::SystemTime> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        // After the command's name, in brackets, which may hold anything:
        // the state is the 3rd field, the start the 22nd.
        let fields = stat.rsplit_once(')')?.1;
        let ticks: u64 = fields.split_whitespace().nth(19)?.parse().ok()?;
        let boot: u64 = (std::fs::read_to_string("/proc/stat").ok()?.lines())
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        let since_boot = std::time::Duration::from_millis(ticks.checked_mul(10)?);
        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(boot) + since_boot)
    }

    /// The directory the terminal's own program is in: for a shell, where
    /// it last changed to, whatever runs in front of it. Once it has
    /// ended, where it was last found, as the workspaces were last kept,
    /// if it was: so a shell a shutdown ended comes back where it was,
    /// even one that doesn't say where it is (CLACKA-236).
    pub(crate) fn own_directory(&self) -> Option<String> {
        if self.process.is_none() {
            return self.directory.borrow().clone();
        }
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok()?;
        let cwd = cwd.into_os_string().into_string().ok()?;
        self.directory.replace(Some(cwd.clone()));
        Some(cwd)
    }

    pub(crate) fn view(&self) -> View {
        let mut view = View::capture(&self.emulator);
        if let Node::Grid(grid) = &mut view.root {
            grid.state.fed = self.fed.is_some();
        }
        view
    }

    pub(crate) fn emulator(&self) -> &Terminal {
        &self.emulator
    }

    pub(crate) fn take_damage(&mut self, into: &mut Damage) {
        self.emulator.take_damage(into);
    }

    pub(crate) fn pty_fd(&self) -> Option<RawFd> {
        self.pty.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(crate) fn exit_fd(&self) -> Option<RawFd> {
        self.process.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(crate) fn has_pending_input(&self) -> bool {
        !self.input.is_empty()
    }

    /// Whether scrollback has rows to compact, or memory freed by history
    /// being cleared to hand back.
    pub(crate) fn scrollback_needs_compaction(&mut self) -> bool {
        // Both or-ed, so the flag is always taken.
        self.emulator.take_history_cleared() | self.emulator.scrollback_needs_compaction()
    }

    pub(crate) fn compact_scrollback(&mut self) {
        self.emulator.compact_scrollback();
    }

    /// Takes what the program has asked of the clipboard since the last
    /// call.
    pub(crate) fn take_clipboard(&mut self) -> Vec<ClipboardRequest> {
        self.emulator.take_clipboard()
    }

    /// Tells the program what's on the clipboard it asked about, or that
    /// there's nothing, with `None`.
    pub(crate) fn answer_clipboard(&mut self, read: ClipboardRead, text: Option<&str>) {
        if self.pty.is_none() {
            return;
        }
        // Dropped, as replies to queries are, while the program leaves
        // too many unread.
        self.queue_replies(&read.answer(text));
    }

    /// Reads and processes the program's output until the PTY has nothing
    /// more, `budget` bytes have been read, or `stop` says to, whichever
    /// comes first. `stop` is asked after each read.
    pub(crate) fn read_output(
        &mut self,
        buffer: &mut [u8],
        budget: usize,
        mut stop: impl FnMut() -> bool,
    ) -> ReadOutcome {
        // Taken out so processing can borrow the rest of `self`. Put back
        // unless the PTY turns out to be closed.
        let Some(pty) = self.pty.take() else {
            return ReadOutcome::Closed;
        };
        let mut total = 0;
        let outcome = loop {
            match pty.read(buffer) {
                Ok(0) => break ReadOutcome::Closed,
                Ok(n) => {
                    self.process_output(&buffer[..n]);
                    total += n;
                    if total >= budget || stop() {
                        break ReadOutcome::BudgetExhausted;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break ReadOutcome::Drained,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                // EIO is how Linux reports that the slave side has closed.
                Err(err) => {
                    if err.raw_os_error() != Some(rustix::io::Errno::IO.raw_os_error()) {
                        log::warn!("terminal {}: reading output failed: {err}", self.id);
                    }
                    break ReadOutcome::Closed;
                }
            }
        };

        if let Some(recording) = &mut self.recording
            && let Err(err) = recording.flush()
        {
            log::warn!("terminal {}: recording stopped: {err}", self.id);
            self.recording = None;
        }
        // The shell has read its secret, if it was going to, by the time
        // it marks anything: its integration reads it first thing.
        if let Some(secret) = &mut self.secret
            && secret.file.is_some()
            && self.emulator.latest_block().is_some()
        {
            secret.file = None;
        }
        if outcome == ReadOutcome::Closed {
            log::info!("terminal {}: PTY closed", self.id);
            self.input.clear();
        } else {
            self.pty = Some(pty);
            let replies = self.emulator.take_replies();
            if !replies.is_empty() {
                self.queue_replies(&replies);
            }
        }
        outcome
    }

    /// Queues `replies`, whole replies to the program's queries, unless
    /// they'd make more than [`MAX_PENDING_REPLIES`] wait: then they're
    /// dropped, all of them, as a cut one is worse than none. With nothing
    /// waiting they go however long, as a clipboard's contents may be.
    fn queue_replies(&mut self, replies: &[u8]) {
        self.flush_input();
        if !self.input.is_empty() && self.input.len() + replies.len() > MAX_PENDING_REPLIES {
            if !std::mem::replace(&mut self.dropping_replies, true) {
                log::debug!(
                    "terminal {}: replies dropped: the program isn't reading them",
                    self.id
                );
            }
            return;
        }
        self.dropping_replies = false;
        self.input.extend_from_slice(replies);
        self.flush_input();
    }

    fn process_output(&mut self, bytes: &[u8]) {
        if let Some(recording) = &mut self.recording
            && let Err(err) = recording.write_all(bytes)
        {
            log::warn!("terminal {}: recording stopped: {err}", self.id);
            self.recording = None;
        }
        self.emulator.advance(bytes);
    }

    /// Queues input for the program and writes as much as the PTY accepts.
    /// Changes the terminal's size: the emulator's, and the one its program
    /// sees.
    pub(crate) fn resize(&mut self, size: Size) {
        if size == self.emulator.size() {
            return;
        }
        // Resize the emulator first, so output written for the new size
        // after SIGWINCH lands on a screen of that size.
        self.emulator.resize(size);
        if let Some(pty) = &self.pty
            && let Err(err) = pty.resize(size)
        {
            log::warn!(
                "terminal {}: setting the window size failed: {err}",
                self.id
            );
        }
        // Recordings have one size, in their header, so a replay of what
        // follows would be wrong.
        if self.recording.take().is_some() {
            log::info!("terminal {}: recording stopped by a resize", self.id);
        }
    }

    pub(crate) fn send_input(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.fed.is_some() {
            return Err(Error::Other(FED_INPUT.to_owned()));
        }
        if self.pty.is_none() {
            return Err(Error::TerminalExited(self.id));
        }
        // Replies waiting are never counted against the user's typing,
        // so a program flooding queries can't keep keys out.
        if self.input.len() + bytes.len() > MAX_PENDING_INPUT + MAX_PENDING_REPLIES {
            return Err(Error::InputBacklogFull(self.id));
        }
        self.input.extend_from_slice(bytes);
        self.flush_input();
        Ok(())
    }

    /// Writes queued input until the PTY stops accepting it.
    pub(crate) fn flush_input(&mut self) {
        let Some(pty) = &self.pty else {
            self.input.clear();
            return;
        };
        let mut written = 0;
        while written < self.input.len() {
            match pty.write(&self.input[written..]) {
                Ok(n) => written += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => {
                    // The PTY is going away. Reading will notice and close it.
                    log::debug!("terminal {}: writing input failed: {err}", self.id);
                    written = self.input.len();
                }
            }
        }
        self.input.drain(..written);
    }

    /// Collects the child's exit status, if it has exited. Returns whether it
    /// has.
    pub(crate) fn reap(&mut self) -> bool {
        let Some(process) = &self.process else {
            return true;
        };
        match process.try_wait() {
            Ok(Some(status)) => {
                self.status = match (status.exit_status(), status.terminating_signal()) {
                    (Some(code), _) => TerminalStatus::Exited(code),
                    (None, Some(signal)) => TerminalStatus::Killed(signal),
                    (None, None) => unreachable!("an exited process has a code or a signal"),
                };
                log::info!("terminal {}: program {}", self.id, self.status);
                self.process = None;
                if let Some(secret) = &mut self.secret {
                    secret.file = None;
                }
                true
            }
            Ok(None) => false,
            Err(err) => {
                log::warn!(
                    "terminal {}: waiting for the program failed: {err}",
                    self.id
                );
                false
            }
        }
    }

    /// Closes the terminal. Closing the PTY sends its programs SIGHUP, as when
    /// a terminal window closes. Returns the program if it is still running,
    /// so the caller can reap it once it exits.
    pub(crate) fn close(self) -> Option<Orphan> {
        // Dropping the PTY here is what hangs up the program.
        self.process.map(|process| Orphan { process })
    }
}

/// A closed terminal's program that hasn't exited yet. It's kept so it can be
/// reaped and not left as a zombie.
#[derive(Debug)]
pub(crate) struct Orphan {
    process: Process,
}

impl Orphan {
    pub(crate) fn new(process: Process) -> Self {
        Self { process }
    }

    pub(crate) fn hand_over(self, fds: &mut Fds) -> handover::Process {
        let (pid, pidfd) = self.process.into_parts();
        handover::Process {
            pid: pid.as_raw_pid(),
            pidfd: fds.add(pidfd),
        }
    }

    pub(crate) fn resume(
        process: &handover::Process,
        fds: &mut Fds,
    ) -> Result<Self, HandoverError> {
        Ok(Self {
            process: resume_process(process, fds)?,
        })
    }

    pub(crate) fn exit_fd(&self) -> RawFd {
        self.process.as_raw_fd()
    }

    /// Returns whether the process has exited and been reaped.
    pub(crate) fn reap(&mut self) -> bool {
        matches!(self.process.try_wait(), Ok(Some(_)) | Err(_))
    }
}

fn resume_process(process: &handover::Process, fds: &mut Fds) -> Result<Process, HandoverError> {
    let pid = rustix::process::Pid::from_raw(process.pid)
        .ok_or(HandoverError::Damaged("a pid isn't one"))?;
    Ok(Process::from_parts(pid, fds.take(process.pidfd)?))
}

fn start_recording(path: &Path, request: &NewTerminal) -> io::Result<BufWriter<File>> {
    let mut file = BufWriter::new(File::create(path)?);
    file.write_all(recording::header(request.size).as_bytes())?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// A terminal running `script` in `sh`.
    fn running(script: &str) -> HostedTerminal {
        let request = NewTerminal {
            size: Size::new(80, 24).unwrap(),
            command: Some(vec!["sh".into(), "-c".into(), script.into()]),
            cwd: Some("/".into()),
            record_to: None,
        };
        HostedTerminal::start(
            TerminalId(1),
            &request,
            &DaemonConfig::from_env(),
            None,
            crate::priority::Leaf::Terminals,
        )
        .unwrap()
    }

    /// Reads `terminal`'s output until there's been none for a while,
    /// noting the most input that waited for it meanwhile.
    fn read_all(terminal: &mut HostedTerminal) -> usize {
        let mut buffer = vec![0; 4096];
        let (mut most, mut quiet) = (0, 0);
        let give_up = Instant::now() + Duration::from_secs(20);
        while quiet < 50 && Instant::now() < give_up {
            match terminal.read_output(&mut buffer, 4096, || false) {
                ReadOutcome::Drained => {
                    quiet += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                ReadOutcome::BudgetExhausted => quiet = 0,
                ReadOutcome::Closed => break,
            }
            most = most.max(terminal.input.len());
        }
        most
    }

    #[test]
    fn a_fed_terminal_comes_through_an_upgrade_ended_and_named_as_it_was() {
        let config = DaemonConfig::from_env();
        let mut fed = HostedTerminal::fed(TerminalId(4), Size::new(20, 4).unwrap(), "ls", &config);
        fed.feed("one\n");
        assert_eq!(fed.status(), TerminalStatus::Running);
        let mut fds = Fds::default();
        let (handed, shell) = fed.hand_over(&mut fds);
        assert!(shell.is_none());
        let mut resumed = HostedTerminal::resume(&handed, None, &mut fds, &config).unwrap();
        assert!(resumed.is_fed() && !resumed.is_fed_unended());
        assert_eq!(resumed.status(), FED_CUT_OFF);
        assert_eq!(resumed.info().pid, None);
        resumed.emulator_mut().advance(b"\x1b]2;renamed\x07");
        assert_eq!(resumed.emulator().title(), "ls");
        assert!(resumed.send_input(b"x").is_err());
    }

    #[test]
    fn replies_to_a_program_not_reading_them_stop_at_a_limit() {
        // Queries, each answered with several times its length, from a
        // program that never reads its terminal, as ssh to a host that
        // has stopped reading.
        let mut terminal = running(
            "stty raw -echo; yes \"$(printf '\\033[>q')\" | tr -d '\\n' | head -c 400000; \
             exec sleep 60",
        );
        let most = read_all(&mut terminal);
        assert!(most <= MAX_PENDING_REPLIES, "{most} bytes waited");
        assert!(most > MAX_PENDING_REPLIES / 2, "only {most} bytes waited");
        // Nor does an answer about the clipboard, which may be long.
        terminal.emulator.advance(b"\x1b]52;c;?\x07");
        let Some(ClipboardRequest::Read(read)) = terminal.take_clipboard().pop() else {
            panic!("a read");
        };
        let waiting = terminal.input.len();
        terminal.answer_clipboard(read, Some(&"x".repeat(100_000)));
        assert_eq!(terminal.input.len(), waiting);
        terminal.process.as_ref().unwrap().kill_group();
        // The user's own typing still goes in, all of it the limit allows.
        terminal.send_input(&vec![b'x'; MAX_PENDING_INPUT]).unwrap();
    }
}
