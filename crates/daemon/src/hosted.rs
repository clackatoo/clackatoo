//! A terminal the daemon is running: the emulator, plus the PTY and process
//! behind it.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;

use clackatoo_protocol::{
    Error, NewTerminal, Size, TerminalId, TerminalInfo, TerminalStatus, View,
};
use clackatoo_term::{ClipboardRead, ClipboardRequest, Damage, Terminal, recording};

use crate::DaemonConfig;
use crate::handover::{self, Fds, HandoverError};
use crate::pty::{self, Process, Program, Pty};
use crate::shell;

/// How much input may wait for a program that isn't reading it. Pastes far
/// larger than any real one fit.
const MAX_PENDING_INPUT: usize = 8 * 1024 * 1024;

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
    recording: Option<BufWriter<File>>,
    /// The secret the shell's integration adds to its marks, if it has
    /// one (see [`shell`]).
    secret: Option<shell::Secret>,
}

impl HostedTerminal {
    pub(crate) fn start(
        id: TerminalId,
        request: &NewTerminal,
        config: &DaemonConfig,
        display: Option<&crate::display::Env>,
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
        };
        let spawned = pty::spawn(&program, request.size)
            .map_err(|err| Error::SpawnFailed(err.to_string()))?;
        let pid = spawned.process.pid().as_raw_pid().cast_unsigned();
        log::info!("terminal {id} started: pid {pid}");

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
            recording,
            secret,
        })
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
        Ok(Self {
            id: TerminalId(terminal.id),
            emulator,
            pty,
            pid: terminal.pid,
            process,
            status: match terminal.status {
                handover::Status::Running => TerminalStatus::Running,
                handover::Status::Exited(code) => TerminalStatus::Exited(code),
                handover::Status::Killed(signal) => TerminalStatus::Killed(signal),
            },
            input: terminal.input.clone(),
            recording: None,
            secret,
        })
    }

    /// Whether its program is still running, or how it ended.
    pub(crate) fn status(&self) -> TerminalStatus {
        self.status
    }

    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            pid: Some(self.pid),
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

    /// Whether its program is a shell at its prompt, running nothing: the
    /// process group in front is its own.
    pub(crate) fn is_idle_shell(&self) -> bool {
        let front = self.pty.as_ref().and_then(Pty::foreground);
        front.is_some_and(|front| front.as_raw_nonzero().get().cast_unsigned() == self.pid)
    }

    /// The directory the terminal's own program is in: for a shell, where
    /// it last changed to, whatever runs in front of it.
    pub(crate) fn own_directory(&self) -> Option<String> {
        let cwd = std::fs::read_link(format!("/proc/{}/cwd", self.pid)).ok()?;
        cwd.into_os_string().into_string().ok()
    }

    pub(crate) fn view(&self) -> View {
        View::capture(&self.emulator)
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
        // An answer is due whatever's queued, as replies to queries are.
        self.input.extend_from_slice(&read.answer(text));
        self.flush_input();
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
                self.input.extend_from_slice(&replies);
                self.flush_input();
            }
        }
        outcome
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
        if self.pty.is_none() {
            return Err(Error::TerminalExited(self.id));
        }
        if self.input.len() + bytes.len() > MAX_PENDING_INPUT {
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
